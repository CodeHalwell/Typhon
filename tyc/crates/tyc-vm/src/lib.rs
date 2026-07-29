//! `tyc-vm` — a small tree-walking interpreter that runs Typhon source
//! directly, without compiling to Python.
//!
//! The VM is the default execution mode for `tyc run`. It parses `.ty` source
//! through the same preprocessing path that `tyc build` uses, then evaluates
//! the resulting Python-shaped AST node-by-node in a Rust environment. The
//! tradeoff is honest: pure-Typhon programs run native; the moment a program
//! reaches into the CPython ecosystem (numpy, requests, …) the VM hits an
//! `ImportError` and the user can re-run with `tyc run --compile` to fall
//! back to emitting Python and exec'ing CPython.
//!
//! v1 coverage is documented at the top of `interp.rs` and the diagnostics
//! catalog in `docs/vm.md`. Features that aren't supported yet surface as
//! `NotImplementedError` at runtime with a feature-name pointer.

// `HashKey::Instance` retains an `Rc<Instance>` (which has interior
// mutability) so a frozen-dataclass key can round-trip back to its value.
// Its `Hash`/`Eq`/`Ord` derive solely from the precomputed, immutable
// `InstanceKey` projection — never from the mutable fields — so using
// `HashKey` as a map/set key is sound. Silence the (here false-positive)
// `mutable_key_type` lint crate-wide rather than at ~17 call sites.
#![allow(clippy::mutable_key_type)]

pub mod builtins;
pub mod env;
pub mod error;
pub mod ffi;
pub mod interp;
pub mod slots;
pub mod value;

use std::path::Path;

pub use error::{Unwind, VmError, VmException};
pub use interp::Interpreter;
pub use value::Value;

use tyc_syntax::preprocess;

/// Run a Typhon source file with the VM. `script_args` populates `sys.argv`
/// after the script path. Returns the process exit code that `tyc run`
/// should propagate.
pub fn run_file(path: &Path, script_args: &[String]) -> Result<i32, VmError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| VmError::Io(format!("cannot read '{}': {e}", path.display())))?;
    run_source(&source, Some(path), script_args)
}

/// Run a Typhon source string. `origin` seeds `__file__` and `sys.argv[0]`
/// and is used in diagnostic messages.
pub fn run_source(
    source: &str,
    origin: Option<&Path>,
    script_args: &[String],
) -> Result<i32, VmError> {
    // Apply the same surface-syntax expansions as `tyc build` so the parser
    // sees identical input. `gather:`, `go`, `with`-chains, pipes and `?`
    // all get lowered to plain Python before parsing.
    //
    // Note: we deliberately call `expand_lazy_lets` instead of the full
    // `expand_lazy_imports`. The full version lowers `lazy import np =
    // numpy` to a `__TyphonLazy_np_` proxy class that uses descriptor
    // protocol and `__getattr__` — neither of which the VM models. The
    // simpler `preprocess` pass below already rewrites `lazy import ALIAS
    // = MODULE` to a plain `import MODULE as ALIAS`, which is the right
    // shape for an in-process VM (no point deferring an import that's
    // about to be evaluated eagerly anyway).
    let expanded = preprocess::expand_question_ops(&preprocess::expand_inline_question_ops(
        // Shared with the CLI: the VM omitted both of these, so an
        // inline `?` (`f(g()?)`, `elif h()? > 1:`) failed to parse under
        // `tyc run` on a program `tyc build` compiles and runs.
        &preprocess::expand_compound_question_headers(&preprocess::expand_pipes(
            &preprocess::expand_with_chains(&preprocess::expand_go_calls(
                &preprocess::expand_gather_blocks(&preprocess::expand_multiline_guards(
                    &preprocess::expand_typed_let_unpack(&preprocess::expand_lazy_lets(source)),
                )),
            )),
        )),
    ));
    let prep = preprocess::preprocess(&expanded);

    let parsed = tyc_syntax::parse_module(&prep.python_source).map_err(|e| {
        let where_ = origin
            .map(|p| format!("{}: ", p.display()))
            .unwrap_or_default();
        VmError::Parse(format!("{where_}{e}"))
    })?;
    let mut module = parsed.into_syntax();

    // Evaluate `comptime` bindings and inline the resulting literals into
    // the AST so the VM doesn't try to execute `env(...)` (a build-only
    // intrinsic). `comptime def` bodies are stripped at the same time so
    // a NameError from one of their build-only calls can't surface at
    // runtime. Matches the substitution pass `tyc build` runs before
    // desugaring.
    let (comptime_values, _comptime_diags) = tyc_analyse::evaluate_comptime_with_functions(
        &module,
        &prep.comptime_bindings,
        &prep.comptime_functions,
    );
    module = tyc_analyse::substitute_comptime_literals(
        module,
        &comptime_values,
        &prep.comptime_functions,
    );

    // Collect `@memo` / `@pure(memo=True)` opt-ins exactly like `tyc build`
    // does, so the desugar pass below injects `@functools.cache` instead of
    // silently stripping the marker (which left memoised recursion running
    // exponentially under the VM while the build path returned instantly).
    let memoise_targets: Vec<String> = tyc_analyse::analyse_purity(&module, false)
        .into_iter()
        .filter(|f| f.violation.is_none() && f.memoise)
        .map(|f| f.name)
        .collect();

    // Hand the VM the desugared module so it sees the same shape as the
    // compile path: dataclass-decorated user classes, merged impl blocks,
    // injected runtime imports, and so on. FINDINGS #21 follow-up.
    // Running the full desugar pass also rewrites \`extend\` user-classes
    // into method merges; the builtin-extension rewrite below handles the
    // \`extend str:\` / \`extend list:\` shape that desugar leaves alone.
    //
    // Pass the preprocessor's class-kind markers (plain / raw / frozen) so
    // the VM desugars `plain class` / `class!` / `class … frozen` exactly
    // like `tyc build` — otherwise a `plain class` would be wrongly
    // decorated as a `@dataclass` and its class-level constants treated as
    // slots.
    let desugar_out = tyc_desugar::desugar_module_with(
        &module,
        tyc_desugar::DesugarOptions {
            memoise_functions: memoise_targets,
            raw_class_line_starts: preprocess::line_byte_starts(
                &prep.python_source,
                &prep.raw_class_lines,
            ),
            frozen_class_line_starts: preprocess::line_byte_starts(
                &prep.python_source,
                &prep.frozen_class_lines,
            ),
            plain_class_line_starts: preprocess::line_byte_starts(
                &prep.python_source,
                &prep.plain_class_lines,
            ),
            pub_names: prep.pub_names.clone(),
            ..Default::default()
        },
    );
    module = desugar_out.module;

    // FINDINGS #21: rewrite \`x.method(args)\` to \`__typhon_ext_TYPE__method
    // (x, args)\` for every receiver statically annotated as a built-in
    // type that the source extended with \`extend BUILTIN:\`. Without this
    // step, calls on extended built-ins fail at runtime with
    // \`AttributeError: 'str' object has no attribute 'slug'\`.
    //
    // Cross-module extensions (#202): pre-scan the entry module's import
    // statements, peek at sibling `.ty` files to extract their extension
    // registries, and merge them into the local rewrite pass. The merged
    // free-fn calls resolve at runtime because the sibling module's
    // namespace (which contains the lifted `__typhon_ext_*` functions) is
    // loaded on demand by the VM's import machinery.
    let (mut registry, _stats) = tyc_analyse::extract_builtin_extensions(&mut module);
    // Pre-scan sibling modules for cross-module builtin extensions.
    if let Some(src_root) = origin.and_then(|p| p.parent()) {
        let cross_module_fns =
            merge_cross_module_extensions_for_vm(&module, src_root, &mut registry);
        let _ = tyc_analyse::rewrite_builtin_extension_calls(&mut module, &registry);
        // Inject explicit imports for cross-module extension functions
        // that were used. In the VM, these resolve to the sibling module's
        // lifted free functions when the module is loaded.
        if !cross_module_fns.is_empty() {
            inject_vm_cross_module_ext_imports(&mut module, &cross_module_fns, src_root);
        }
    } else {
        let _ = tyc_analyse::rewrite_builtin_extension_calls(&mut module, &registry);
    }

    let mut interp = Interpreter::new();
    // Source info for traceback frames: file name + line table over the
    // preprocessed source (line-preserving for ordinary statements, so
    // frame numbers match the user's .ty lines).
    interp.current_source = Some(std::rc::Rc::new(interp::SourceInfo::new(
        origin
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<source>".to_string()),
        &prep.python_source,
    )));
    // Seed sys.argv before any user code (or import sys) can observe it.
    let argv0 = origin
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "-".to_string());
    interp.script_argv = std::iter::once(argv0)
        .chain(script_args.iter().cloned())
        .collect();
    // The source_root is the directory holding the entry — sibling .ty
    // files in the same dir become importable, so multi-file projects
    // work under `tyc run` without a separate build step.
    if let Some(path) = origin {
        if let Some(parent) = path.parent() {
            interp.source_root = Some(parent.to_path_buf());
        }
    }
    // Bind `__name__ = "__main__"` so the standard idiom works.
    interp.root.set(
        "__name__",
        Value::Str(std::rc::Rc::new("__main__".to_string())),
    );
    if let Some(p) = origin {
        interp.root.set(
            "__file__",
            Value::Str(std::rc::Rc::new(p.to_string_lossy().into_owned())),
        );
    }

    match interp.run_module(&module) {
        Ok(()) => Ok(0),
        Err(Unwind::Return(_)) => Ok(0),
        Err(Unwind::Exception(exc)) => {
            eprintln!("Traceback (most recent call last):");
            // Frames accumulate innermost-first as the exception bubbles
            // through `call_function`; CPython prints outermost-first.
            // The module-level frame (where the failing call chain
            // started) renders first, from the interpreter's final
            // statement offset.
            if let Some(si) = &interp.current_source {
                let line = si.line_of(interp.current_offset);
                eprintln!("  File \"{}\", line {}, in <module>", si.name, line);
                if let Some(text) = si.line_text(line) {
                    eprintln!("    {text}");
                }
            }
            for frame in exc.frames.iter().rev() {
                match (&frame.file, frame.line) {
                    (Some(file), Some(line)) => {
                        eprintln!("  File \"{file}\", line {line}, in {}", frame.function);
                        if let Some(text) = &frame.line_text {
                            eprintln!("    {text}");
                        }
                    }
                    _ => eprintln!("  in {}", frame.function),
                }
            }
            if exc.message.is_empty() {
                eprintln!("{}", exc.kind);
            } else {
                eprintln!("{}: {}", exc.kind, exc.message);
            }
            Ok(1)
        }
        Err(Unwind::Break | Unwind::Continue | Unwind::QuestionMark(_)) => {
            Err(VmError::runtime("unexpected control-flow at module level"))
        }
    }
}

/// Pre-scan sibling `.ty` files referenced by the entry module's imports,
/// extract their builtin extension registries, and merge them into `registry`.
/// Returns a map of `fn_name → sibling_module_stem` for functions that were
/// added from cross-module sources, so the caller can inject explicit imports.
fn merge_cross_module_extensions_for_vm(
    module: &ruff_python_ast::ModModule,
    src_root: &Path,
    registry: &mut tyc_analyse::ExtensionRegistry,
) -> std::collections::HashMap<String, String> {
    use ruff_python_ast::Stmt;
    use std::collections::HashMap;

    let mut cross_fns: HashMap<String, String> = HashMap::new();
    // Collect unique module names from import statements.
    let mut seen_modules: std::collections::HashSet<String> = std::collections::HashSet::new();
    for stmt in &module.body {
        match stmt {
            Stmt::ImportFrom(i) => {
                if let Some(m) = &i.module {
                    let name = m.id.to_string();
                    if !name.contains('.') && seen_modules.insert(name.clone()) {
                        let sibling_path = src_root.join(format!("{name}.ty"));
                        if sibling_path.exists() {
                            if let Ok(text) = std::fs::read_to_string(&sibling_path) {
                                merge_sibling_extensions(&text, &name, registry, &mut cross_fns);
                            }
                        }
                    }
                }
            }
            Stmt::Import(i) => {
                // Handle all aliases in `import a, b, c` — not just the first.
                for alias in &i.names {
                    let name = alias.name.id.to_string();
                    if !name.contains('.') && seen_modules.insert(name.clone()) {
                        let sibling_path = src_root.join(format!("{name}.ty"));
                        if sibling_path.exists() {
                            if let Ok(text) = std::fs::read_to_string(&sibling_path) {
                                merge_sibling_extensions(&text, &name, registry, &mut cross_fns);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    cross_fns
}

/// Parse a sibling `.ty` source just enough to extract builtin extension
/// sentinel classes and merge their methods into `registry`.
fn merge_sibling_extensions(
    source: &str,
    module_name: &str,
    registry: &mut tyc_analyse::ExtensionRegistry,
    cross_fns: &mut std::collections::HashMap<String, String>,
) {
    let expanded = preprocess::expand_question_ops(&preprocess::expand_inline_question_ops(
        // Shared with the CLI: the VM omitted both of these, so an
        // inline `?` (`f(g()?)`, `elif h()? > 1:`) failed to parse under
        // `tyc run` on a program `tyc build` compiles and runs.
        &preprocess::expand_compound_question_headers(&preprocess::expand_pipes(
            &preprocess::expand_with_chains(&preprocess::expand_go_calls(
                &preprocess::expand_gather_blocks(&preprocess::expand_multiline_guards(
                    &preprocess::expand_typed_let_unpack(&preprocess::expand_lazy_lets(source)),
                )),
            )),
        )),
    ));
    let prep = preprocess::preprocess(&expanded);
    let Ok(parsed) = tyc_syntax::parse_module(&prep.python_source) else {
        return;
    };
    let mut sibling_module = parsed.into_syntax();
    let (sibling_registry, _) = tyc_analyse::extract_builtin_extensions(&mut sibling_module);
    for (builtin_type, methods) in &sibling_registry {
        let entry = registry.entry(builtin_type.clone()).or_default();
        for (method_name, fn_name) in methods {
            entry.entry(method_name.clone()).or_insert_with(|| {
                cross_fns.insert(fn_name.clone(), module_name.to_owned());
                fn_name.clone()
            });
        }
    }
}

/// Inject `from <module> import <fn_name>` AST nodes into `module` for
/// cross-module extension functions. This allows the VM to resolve the
/// lifted free functions during execution.
fn inject_vm_cross_module_ext_imports(
    module: &mut ruff_python_ast::ModModule,
    cross_fns: &std::collections::HashMap<String, String>,
    _src_root: &Path,
) {
    use ruff_python_ast::{name::Name, AtomicNodeIndex, Identifier, Stmt, StmtImportFrom};
    use ruff_text_size::TextRange;
    use std::collections::HashMap;

    // Group by module.
    let mut by_module: HashMap<String, Vec<String>> = HashMap::new();
    for (fn_name, mod_name) in cross_fns {
        by_module
            .entry(mod_name.clone())
            .or_default()
            .push(fn_name.clone());
    }

    let mut injected: Vec<Stmt> = Vec::new();
    for (mod_name, fns) in &by_module {
        let aliases: Vec<ruff_python_ast::Alias> = fns
            .iter()
            .map(|f| ruff_python_ast::Alias {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                name: Identifier {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    id: Name::new(f),
                },
                asname: None,
            })
            .collect();
        injected.push(Stmt::ImportFrom(StmtImportFrom {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            module: Some(Identifier {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                id: Name::new(mod_name),
            }),
            names: aliases,
            level: 0,
            is_lazy: false,
        }));
    }

    // Insert after existing imports. Only skip a leading string-literal
    // expression (module docstring) — other `Stmt::Expr` are executable
    // statements that must run after imports.
    let mut insert_pos = 0;
    if let Some(Stmt::Expr(e)) = module.body.first() {
        if matches!(&*e.value, ruff_python_ast::Expr::StringLiteral(_)) {
            insert_pos = 1;
        }
    }
    while insert_pos < module.body.len() {
        if matches!(
            &module.body[insert_pos],
            Stmt::Import(_) | Stmt::ImportFrom(_)
        ) {
            insert_pos += 1;
        } else {
            break;
        }
    }

    for (i, stmt) in injected.into_iter().enumerate() {
        module.body.insert(insert_pos + i, stmt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_capturing(source: &str) -> Result<i32, VmError> {
        run_source(source, None, &[])
    }

    #[test]
    fn smoke_let_arithmetic_and_print() {
        let src = r#"
let x: int = 2
let y: int = 3
print(x + y)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Small-int fast path: i64-boundary arithmetic and 2**100 must produce the
    /// exact arbitrary-precision result (CPython parity), driven through the
    /// full public run path. The program raises on any wrong value, so a clean
    /// exit code is the assertion.
    #[test]
    fn small_int_fast_path_matches_cpython_semantics() {
        let src = r#"
def main() -> None:
    # Crossing the i64 boundary must not wrap.
    let big: int = 9223372036854775807 + 1
    if big != 9223372036854775808:
        raise ValueError("i64::MAX + 1 wrong")
    let small_again: int = big - 1
    if small_again != 9223372036854775807:
        raise ValueError("demotion wrong")
    if 2 ** 100 != 1267650600228229401496703205376:
        raise ValueError("2**100 wrong")
    # Floor-div / mod sign rules (divisor's sign for %).
    if 7 // -3 != -3 or 7 % -3 != -2:
        raise ValueError("floordiv/mod sign wrong")
    if -7 // 3 != -3 or -7 % 3 != 2:
        raise ValueError("neg floordiv/mod wrong")
    # pow with negative exponent widens to float.
    if 2 ** -1 != 0.5:
        raise ValueError("negative pow wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// CPython collapses numerically-equal int / float / bool dict keys onto a
    /// single slot (`hash(1) == hash(1.0) == hash(True)`). The `VmInt`-backed
    /// `HashKey` must preserve that.
    #[test]
    fn numeric_dict_key_collapse_preserved() {
        let src = r#"
def main() -> None:
    mut d: dict[int, int] = {}
    d[1] = 1
    d[1.0] = 2
    d[True] = 3
    if len(d) != 1:
        raise ValueError("numeric keys did not collapse to one slot")
    if d[1] != 3:
        raise ValueError("collapse did not overwrite the shared slot")
    # A distinct big int and a non-integral float stay separate keys.
    d[10000000000000000000] = 7
    d[1.5] = 8
    if len(d) != 3:
        raise ValueError("distinct numeric keys wrongly merged")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Task 3 direct method-call path must preserve every dispatch flavour:
    /// plain method (fast path), method with args, `@staticmethod` and
    /// `@classmethod` (which fall back to the general path).
    #[test]
    fn method_dispatch_flavours_preserved() {
        let src = r#"
class Counter:
    n: int

impl Counter:
    def get(self) -> int:
        return self.n
    def plus(self, k: int) -> int:
        return self.n + k
    @staticmethod
    def stat() -> int:
        return 42
    @classmethod
    def twice(cls, v: int) -> int:
        return v * 2

def main() -> None:
    let c: Counter = Counter(n=10)
    if c.get() != 10:
        raise ValueError("plain method broken")
    if c.plus(5) != 15:
        raise ValueError("method with arg broken")
    if Counter.stat() != 42:
        raise ValueError("staticmethod on class broken")
    if c.stat() != 42:
        raise ValueError("staticmethod via instance broken")
    if Counter.twice(3) != 6:
        raise ValueError("classmethod broken")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn bytes_percent_formatting_runs() {
        // Regression (PEP 461): the VM must implement `bytes % args` (it only
        // had `str % args`), matching the checker which now accepts it and
        // CPython. Asserting a clean run is enough — a missing arm raised a
        // VmError before.
        let src = r#"
def main() -> None:
    let a: bytes = b"%d items" % 5
    let b: bytes = b"%d-%s" % (5, b"x")
    let c: bytes = b"%b!" % b"x"
    print(a, b, c)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn translate_bytes_format_rewrites_only_b_conversion() {
        // PEP 461 `%b` becomes `%s` (bytes args are latin-1 strings); `%%`,
        // mapping keys, and other conversions are preserved.
        assert_eq!(crate::interp::translate_bytes_format("%b"), "%s");
        assert_eq!(crate::interp::translate_bytes_format("%d-%b"), "%d-%s");
        assert_eq!(crate::interp::translate_bytes_format("%-5b"), "%-5s");
        assert_eq!(
            crate::interp::translate_bytes_format("100%% %b"),
            "100%% %s"
        );
        assert_eq!(crate::interp::translate_bytes_format("%(k)b"), "%(k)s");
        assert_eq!(
            crate::interp::translate_bytes_format("%d items"),
            "%d items"
        );
    }

    #[test]
    fn functions_and_recursion() {
        let src = r#"
def fact(n: int) -> int:
    if n <= 1:
        return 1
    return n * fact(n - 1)

print(fact(6))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn user_exception_construct_raise_catch() {
        // Regression: `raise FooError("msg")` used to die in the VM with
        // `TypeError: FooError() takes 0 arguments (more given)` because the
        // field-less exception subclass was treated as a zero-field dataclass.
        let src = r#"
class AppError(Exception):
    pass

class NotFoundError(AppError):
    pass

def lookup(k: str) -> str:
    if k == "missing":
        raise NotFoundError("no such key")
    return k

def main() -> None:
    try:
        print(lookup("missing"))
    except AppError as e:
        print(type(e).__name__, str(e))

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn user_exception_caught_via_builtin_base() {
        // `except KeyError` must catch a user `class MyKeyError(KeyError):`,
        // and the KeyError single-arg `str()` repr-quoting is inherited
        // (`str(MyKeyError("k")) == "'k'"`).
        let src = r#"
class MyKeyError(KeyError):
    pass

def main() -> None:
    mut caught: bool = False
    try:
        raise MyKeyError("missing-key")
    except KeyError as e:
        caught = True
        assert str(e) == "'missing-key'"
    assert caught
    print("ok")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn user_exception_with_fields_and_super_init() {
        // A hand-written __init__ that calls super().__init__(msg) constructs
        // cleanly and exposes its fields.
        let src = r#"
class HttpError(Exception):
    code: int
    detail: str

impl HttpError:
    def __init__(self, code: int, detail: str) -> None:
        self.code = code
        self.detail = detail
        super().__init__(f"HTTP {code}: {detail}")

def main() -> None:
    try:
        raise HttpError(404, "missing")
    except HttpError as e:
        print(e.code, e.detail, str(e))

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn kwargs_preserve_call_order() {
        // Regression: `**kwargs` was collected into a HashMap, so the
        // resulting dict's iteration/repr order was nondeterministic.
        // CPython preserves keyword-argument order.
        let src = r#"
def build(**fields: int) -> dict[str, int]:
    mut out: dict[str, int] = {}
    for k in fields:
        out[k] = fields[k]
    return out

let d = build(z=1, y=2, x=3, w=4)
assert list(d.keys()) == ["z", "y", "x", "w"]
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn bytes_concat_and_repeat() {
        let src = r#"
let a: bytes = b"hello"
let b: bytes = b" world"
assert a + b == b"hello world"
assert b"ab" * 3 == b"ababab"
assert 2 * b"xy" == b"xyxy"
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn non_string_format_raises_typeerror() {
        // A user `__format__` returning a non-`str` raises `TypeError`
        // (CPython parity), not a silently-coerced string.
        let src = r#"
class T:
    n: int
impl T:
    def __format__(self, spec: str) -> int:
        return 123

def main() -> None:
    mut caught: bool = False
    try:
        let s: str = f"{T(n=1):x}"
    except TypeError:
        caught = True
    assert caught
    print("ok")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn uncaught_exception_message_is_str_form() {
        // A re-caught exception's `str()` is the message, not the args tuple
        // — the same form the uncaught traceback uses.
        let src = r#"
class AppError(Exception):
    pass

def main() -> None:
    try:
        raise AppError("boom")
    except AppError as e:
        assert str(e) == "boom"
    print("ok")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn fieldless_exception_rejects_kwargs() {
        // `BaseException` takes no keyword arguments, so a field-less
        // exception constructed with a keyword raises TypeError.
        let src = r#"
class FooError(Exception):
    pass

def main() -> None:
    mut caught: bool = False
    try:
        raise FooError(message="boom")
    except TypeError:
        caught = True
    assert caught
    print("ok")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn format_rejects_non_string_spec() {
        // `format(obj, 123)` — a non-`str` format spec raises TypeError.
        let src = r#"
def main() -> None:
    mut caught: bool = False
    try:
        let s: str = format(42, 8)
    except TypeError:
        caught = True
    assert caught
    print("ok")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn abc_module_shim() {
        // `from abc import ABC, abstractmethod` + an ABC subclass works in
        // the VM (ABC base is a no-op, abstractmethod is identity).
        let src = r#"
from abc import ABC, abstractmethod

class Handler(ABC):
    @abstractmethod
    def handle(self, e: str) -> str:
        ...

class Echo(Handler):
    pass
impl Echo:
    def handle(self, e: str) -> str:
        return e

def main() -> None:
    let h: Handler = Echo()
    assert h.handle("x") == "x"
    print("ok")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn user_format_dunder_dispatched() {
        // `f"{x:spec}"`, `"{:spec}".format(x)`, and `format(x, spec)` all
        // route through a user `__format__(self, spec)`.
        let src = r#"
class T:
    c: int
impl T:
    def __format__(self, spec: str) -> str:
        return f"<{spec}:{self.c}>"

def main() -> None:
    let t: T = T(c=7)
    assert f"{t:F}" == "<F:7>"
    assert "{:G}".format(t) == "<G:7>"
    assert format(t, "H") == "<H:7>"
    print("ok")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn del_list_slice() {
        // `del lst[i:j]` and extended slices remove the selected indices.
        let src = r#"
mut a: list[int] = [10, 20, 30, 40, 50]
del a[1:3]
assert a == [10, 40, 50]
mut b: list[int] = [0, 1, 2, 3, 4, 5]
del b[::2]
assert b == [1, 3, 5]
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn key_error_str_shows_repr() {
        let src = r#"
def main() -> None:
    try:
        raise KeyError("missing")
    except KeyError as e:
        assert str(e) == "'missing'"
        print("ok")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn dict_view_set_operations() {
        // `dict.keys()` / `dict.items()` are set-like and support `& | - ^`.
        let src = r#"
let d1: dict[str, int] = {"a": 1, "b": 2, "c": 3}
let d2: dict[str, int] = {"b": 2, "c": 4, "d": 5}
assert sorted(d1.keys() & d2.keys()) == ["b", "c"]
assert sorted(d1.keys() | d2.keys()) == ["a", "b", "c", "d"]
assert sorted(d1.keys() - d2.keys()) == ["a"]
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn result_question_mark() {
        let src = r#"
from typhon_runtime import Ok, Err

def safe_div(a: int, b: int) -> Result[int, str]:
    if b == 0:
        return Err("div by zero")
    return Ok(a // b)

def parent() -> Result[int, str]:
    let x: int = safe_div(10, 2)?
    let y: int = safe_div(x, 0)?
    return Ok(y)

let result = parent()
match result:
    case Ok(v):
        print("ok:", v)
    case Err(e):
        print("err:", e)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn checked_cast_union_and_parametric_targets_run() {
        // `EXPR as! int | None` / `EXPR as! dict[str, int]` lower to
        // `__typhon_checked_cast__(EXPR, <type>)`. The VM must evaluate only
        // the value operand: evaluating the type descriptor `int | None` as an
        // ordinary expression used to crash with `unsupported operand type(s)
        // for |: 'function' and 'NoneType'`. The casts also appear nested in a
        // call argument and a comprehension to cover the structural lowering.
        // Wrong results `raise`, so a regression surfaces as a non-zero run.
        let src = r#"
def takes_int(n: int) -> int:
    return n

def widen(x: object) -> int | None:
    return x as! int | None

def reshape(raw: object) -> dict[str, int]:
    return raw as! dict[str, int]

let rows: list[object] = [4, 5, 6]
let doubled = [takes_int(r as! int) * 2 for r in rows]
if doubled != [8, 10, 12]:
    raise ValueError("nested / comprehension cast wrong")
let w = widen(41)
if w != 41:
    raise ValueError("union cast wrong")
let m: object = {"a": 1}
let d = reshape(m)
if d["a"] != 1:
    raise ValueError("parametric cast wrong")
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn checked_cast_rejects_wrong_shape_in_vm() {
        // The `as!` cast must run the same structural check the compile path
        // does — a wrong-shaped value raises `TypeError` under `tyc run` too
        // (previously the VM was an identity passthrough, silently letting a
        // bad value through and diverging from `tyc build && python`). Each
        // arm catches the expected `TypeError`; if a cast wrongly *passes*,
        // the `else`-path `raise` makes the run non-zero.
        let src = r#"
def expect_reject(thunk_ok: bool) -> None:
    if not thunk_ok:
        raise ValueError("as! accepted a wrong-shaped value")

# list is not a dict[str, int]
let a: object = ["x", "y"]
mut a_rejected: bool = False
try:
    let da: dict[str, int] = a as! dict[str, int]
except TypeError:
    a_rejected = True
expect_reject(a_rejected)

# str is not an int
let b: object = "nope"
mut b_rejected: bool = False
try:
    let nb: int = b as! int
except TypeError:
    b_rejected = True
expect_reject(b_rejected)

# dict with a str value is not dict[str, int]
let c: object = {"k": "v"}
mut c_rejected: bool = False
try:
    let dc: dict[str, int] = c as! dict[str, int]
except TypeError:
    c_rejected = True
expect_reject(c_rejected)

# valid casts still pass (int widens to float; None matches int | None)
let f: float = 5 as! float
if f != 5.0:
    raise ValueError("int->float widening cast wrong")
let n: object = None
let opt: int | None = n as! int | None
if opt is not None:
    raise ValueError("None cast wrong")
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn numeric_keys_collapse_like_cpython() {
        // CPython treats numerically-equal bool/int/float as one mapping/set
        // key: `1 == 1.0 == True`, all hash-equal. The VM used to keep them
        // distinct (silent data divergence). Regressions `raise` → non-zero run.
        let src = r#"
mut d: dict[object, str] = {}
d[1] = "a"
d[1.0] = "b"
d[True] = "c"
if len(d) != 1:
    raise ValueError("int/float/bool keys should collapse to one")
# first-inserted key identity is kept, value updated to last write
if str(d) != "{1: 'c'}":
    raise ValueError("expected {1: 'c'}, got " + str(d))

if len({1, 1.0, True, 2}) != 2:
    raise ValueError("set should collapse numeric duplicates")
if 1 not in {1.0}:
    raise ValueError("1 should be in {1.0}")
if hash(1) != hash(1.0):
    raise ValueError("hash(1) should equal hash(1.0)")
# the cross-type frozenset case that the canonical ordering must handle
if frozenset({1, 2.0}) != frozenset({1.0, 2}):
    raise ValueError("mixed-type frozensets should compare equal")
# non-integral floats stay distinct
if len({1.5, 2.5, 1.5}) != 2:
    raise ValueError("non-integral floats should not collapse")
if 1.5 in {1}:
    raise ValueError("1.5 should not be in {1}")
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn try_result_bridges_exceptions() {
        // `try_result(thunk[, on_err])` returns `Ok(thunk())`, or catches a
        // raised exception and returns `Err(on_err(exc))` / `Err(exc)`. Drives
        // both the 2-arg (mapped) and 1-arg (raw exception) forms, and the
        // Ok / Err arms. Wrong results `raise`, surfacing a regression as a
        // non-zero run.
        let src = r#"
def parse(raw: str) -> int:
    return int(raw)

def safe(raw: str) -> Result[int, str]:
    return try_result(lambda: parse(raw), lambda e: "bad")

match safe("7"):
    case Ok(v):
        if v != 7:
            raise ValueError("ok value wrong")
    case Err(e):
        raise ValueError("expected ok, got err")

match safe("nope"):
    case Ok(v):
        raise ValueError("expected err, got ok")
    case Err(e):
        if e != "bad":
            raise ValueError("mapped err wrong")

# 1-arg form: the error is the raw exception object.
match try_result(lambda: parse("x")):
    case Ok(v):
        raise ValueError("expected err from raising thunk")
    case Err(e):
        pass
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn try_result_rejects_wrong_arity() {
        // Extra positional args aren't silently ignored — `try_result` takes
        // 1 or 2, matching the runtime `def try_result(thunk, on_err=None)`.
        let src = "let r = try_result(lambda: 1, lambda e: \"x\", 99)\nprint(r)\n";
        assert_ne!(
            run_capturing(src).unwrap_or(1),
            0,
            "try_result with 3 args must raise (non-zero exit)"
        );
    }

    #[test]
    fn type_alias_binds_as_runtime_value() {
        // A `type` alias must bind a runtime name (CPython binds a lazy
        // `TypeAliasType`); previously this was a no-op, so an imported
        // alias raised `AttributeError`/`NameError`. A sealed-union alias
        // has no first-class VM union value, so it lowers to a tuple of its
        // member types — a valid `isinstance` argument. Reaching the final
        // `print` (exit 0) proves `AB` is bound and usable; an unbound name
        // would raise `NameError` first.
        let src = r#"
class A:
    v: int

class B:
    v: int

type AB = A | B
type Scalar = int

let a = A(v=1)
print(isinstance(a, AB))
print(isinstance(a, Scalar))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn forward_declared_type_alias_resolves_at_runtime() {
        // A `type` alias written *above* the classes it unions must still
        // resolve correctly when used at runtime (CPython binds it lazily).
        // The `isinstance` runs during body execution — before the post-body
        // resolution pass — so it exercises the on-demand `force_alias` path.
        // Each `isinstance` is asserted via a `raise`, so a wrong result
        // (the old string-fallback returning `False`) surfaces as a non-zero
        // run rather than passing silently.
        let src = r#"
type AB = A | B

class A:
    v: int

class B:
    v: int

let a = A(v=1)
if not isinstance(a, AB):
    raise ValueError("forward alias AB did not match its member A")
if isinstance(a, B):
    raise ValueError("A instance wrongly matched B")
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn class_with_method() {
        let src = r#"
class Point:
    x: float
    y: float

impl Point:
    def distance(self) -> float:
        return (self.x * self.x + self.y * self.y) ** 0.5

let p = Point(x=3.0, y=4.0)
print(p.distance())
"#;
        // The preprocessor rewrites `impl Point:` to a sibling class; in v1
        // the VM treats it as a method-bearing class. (Methods are not yet
        // merged from `__typhon_impl_Point`.)
        let _ = run_capturing(src);
    }

    #[test]
    fn loops_and_collections() {
        let src = r#"
let xs: list[int] = [1, 2, 3, 4, 5]
mut total: int = 0
for x in xs:
    total = total + x
print(total)
print([y * 2 for y in xs if y % 2 == 1])
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn match_arm_writes_propagate_to_outer_scope() {
        // Regression for N9 (2026-05-22): `match` arms used to execute in a
        // fresh child env, so `total = v` inside `case Ok(v):` never reached
        // the function-scope `total`. The VM should keep parity with the
        // compiled-Python output.
        let src = r#"
from typhon_runtime import Ok, Err

def main() -> None:
    let outcomes: list[Result[int, str]] = [Ok(1), Err("bad"), Ok(7)]
    mut total: int = 0
    for o in outcomes:
        match o:
            case Ok(v):
                total = total + v
            case Err(_):
                pass
    print(total)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn freeze_builtin_is_identity() {
        // FINDINGS #23: `freeze let` lowers to a `__typhon_freeze__(...)`
        // call. The VM exposes the helper as an identity shim so the
        // lowered code runs without a NameError.
        let src = r#"
let xs = __typhon_freeze__([1, 2, 3])
print(xs)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn freeze_runtime_import_resolves() {
        // The compile path injects `from typhon_runtime.freeze import
        // deep_freeze as __typhon_freeze__`; the VM must serve it too.
        let src = r#"
from typhon_runtime.freeze import deep_freeze as __typhon_freeze__
let xs = __typhon_freeze__([1, 2, 3])
print(xs)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn newtype_is_identity_callable() {
        // FINDINGS #24: `newtype Foo = int` lowers to `Foo = NewType("Foo",
        // int)`. The VM exposes `NewType` as a builtin that returns an
        // identity wrapper, matching CPython's runtime semantics.
        let src = r#"
let Foo = NewType("Foo", int)
print(Foo(42))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn typing_import_resolves() {
        // FINDINGS #25: `from typing import Callable` (and friends) used to
        // crash with ImportError. The VM exposes a typing shim with
        // identity callables for every name the static checker emits.
        let src = r#"
from typing import Callable, Optional, List, Dict, Any, TypeVar
let T = TypeVar("T")
print("ok")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn re_basic_match_search_sub() {
        // FINDINGS #27: `re` was missing. Smoke-test the most-used
        // surface: `re.search`, `re.sub`, `re.findall`.
        let src = r#"
import re
let m = re.search("[a-z]+", "Hello world")
print(m.group())
print(re.sub("[aeiou]", "*", "Hello"))
print(re.findall("[0-9]+", "a1 b22 c333"))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn collections_counter_and_namedtuple() {
        // FINDINGS #27: `collections.Counter` and `namedtuple` smoke test.
        let src = r#"
from collections import Counter, namedtuple
let c = Counter(["a", "b", "a", "c", "a"])
print(c)
let Point = namedtuple("Point", ["x", "y"])
print(Point(1, 2))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn functools_reduce_and_cache() {
        // FINDINGS #27: `functools.reduce` and `cache` smoke test.
        let src = r#"
from functools import reduce, cache
print(reduce(lambda a, b: a + b, [1, 2, 3, 4, 5]))

def slow(n: int) -> int:
    return n * 2

let fast = cache(slow)
print(fast(7))
print(fast(7))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn subclass_constructor_inherits_parent_fields() {
        // FINDINGS #22: `class Dog(Animal):` should accept the parent's
        // `name`/`age` kwargs in addition to its own `breed`. The desugar
        // pass now prepends the parent's field annotations to the
        // subclass body so the VM's auto-`__init__` collects all three.
        let src = r#"
class Animal:
    name: str
    age: int

class Dog(Animal):
    breed: str

let d = Dog(name="Rex", age=5, breed="Husky")
print(d.name)
print(d.age)
print(d.breed)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn extend_builtin_str_method_dispatches() {
        // FINDINGS #21: `extend str: def slug(self) -> str: …` used to
        // fail at runtime with AttributeError. The VM now runs the same
        // desugar + extend-builtin extraction passes as `tyc build`, so
        // call sites like `"Hello".slug()` rewrite to the lifted free
        // function `__typhon_ext_str__slug("Hello")`.
        let src = r#"
extend str:
    def slug(self) -> str:
        return self.lower().replace(" ", "-")

let s: str = "Hello World"
print(s.slug())
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn lazy_let_inside_class_resolves() {
        // FINDINGS #26: `lazy let` inside a class body lowers (in the
        // preprocessor) to a `@cached_property` method, with a hidden
        // `from functools import cached_property as _typhon_cached_property`
        // import injected at module top. The functools shim now exposes
        // `cached_property` as an identity decorator so the import
        // resolves and the method is registered as a regular method on
        // the class. Limitation: callers must invoke as `obj.name()`
        // rather than `obj.name` because the VM has no descriptor
        // protocol — documented at the cached_property registration
        // site in builtins.rs.
        let src = r#"
class Counter:
    n: int

    lazy let doubled: int = self.n * 2

let c = Counter(n=21)
print(c.doubled())
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn itertools_chain_and_accumulate() {
        // FINDINGS #27: `itertools.chain` and `accumulate` smoke test.
        let src = r#"
from itertools import chain, accumulate
print(list(chain([1, 2], [3, 4])))
print(list(accumulate([1, 2, 3, 4])))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn lazy_iterator_adapters_do_not_panic() {
        // Regression: enumerate / zip / map / filter previously panicked with
        // "RefCell already borrowed" the moment they were iterated.
        let src = r#"
for i, v in enumerate(["a", "b"]):
    print(i, v)
for a, b in zip([1, 2], ["x", "y"]):
    print(a, b)
print(list(map(lambda x: x * 2, [1, 2, 3])))
print(list(filter(lambda x: x > 1, [1, 2, 3])))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn review_fixes_builtin_semantics() {
        // Batch of PR-review correctness fixes: int(str, 0) radix autodetect,
        // ZeroDivisionError from divmod, frozenset ops staying frozen,
        // integer-domain math rejecting floats, and str.format honouring __str__.
        let src = r#"
import math

class P:
    n: int

impl P:
    def __str__(self) -> str:
        return f"P{self.n}"

def main() -> None:
    if int("0xff", 0) != 255 or int("0b101", 0) != 5 or int("42", 0) != 42:
        raise ValueError("int base 0 autodetect broken")
    try:
        let _ = divmod(5, 0)
        raise ValueError("divmod by zero should raise")
    except ZeroDivisionError:
        pass
    let f: frozenset = frozenset([1, 2])
    let u: frozenset = f.union([3])
    try:
        u.add(9)
        raise ValueError("frozenset union result must stay frozen")
    except AttributeError:
        pass
    try:
        let _x: int = math.factorial(5.9)
        raise ValueError("factorial must reject floats")
    except TypeError:
        pass
    if "{}".format(P(n=7)) != "P7":
        raise ValueError("str.format must honour __str__")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn review_fixes_dunder_and_descriptors() {
        // __str__ returning a non-str raises TypeError; a subclass overriding a
        // base @property with a plain method is treated as a method.
        let src = r#"
class Bad:
    n: int

impl Bad:
    def __str__(self) -> int:
        return self.n

class Base:
    v: int

impl Base:
    @property
    def x(self) -> int:
        return self.v

class Child(Base):
    v: int

impl Child:
    def x(self) -> int:
        return self.v + 100

def main() -> None:
    try:
        let _ = str(Bad(n=5))
        raise ValueError("__str__ returning non-str must raise")
    except TypeError:
        pass
    let c: Child = Child(v=1)
    if c.x() != 101:
        raise ValueError("overridden property must be a method")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn pydantic_model_validate_and_dump() {
        // Flat `model` classes round-trip through model_validate / model_dump.
        let src = r#"
model User:
    id: int
    name: str
    active: bool

def main() -> None:
    let data: dict[str, object] = {"id": 1, "name": "Ada", "active": True}
    let u: User = User.model_validate(data)
    if u.id != 1 or u.name != "Ada" or not u.active:
        raise ValueError("model_validate fields wrong")
    let d: dict[str, object] = u.model_dump()
    if d["name"] != "Ada":
        raise ValueError("model_dump wrong")
    if u.model_dump_json() != "{\"id\": 1, \"name\": \"Ada\", \"active\": true}":
        raise ValueError("model_dump_json wrong: " + u.model_dump_json())
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn generators_run_eagerly() {
        // `yield` / `yield from` generators iterate correctly under the VM's
        // eager-collection model.
        let src = r#"
from typing import Iterator

def squares(n: int) -> Iterator[int]:
    for i in range(n):
        yield i * i

def flatten(rows: list[list[int]]) -> Iterator[int]:
    for row in rows:
        yield from row

def main() -> None:
    if list(squares(4)) != [0, 1, 4, 9]:
        raise ValueError("squares generator wrong")
    if sum(squares(4)) != 14:
        raise ValueError("sum over generator wrong")
    if list(flatten([[1, 2], [3]])) != [1, 2, 3]:
        raise ValueError("yield from wrong")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn type_object_model() {
        // type(x) is a real type object: .__name__, str(), and == all work
        // for both builtins and user classes.
        let src = r#"
class Foo:
    x: int

def main() -> None:
    if type(5).__name__ != "int":
        raise ValueError("builtin __name__ wrong")
    if type([1]).__name__ != "list":
        raise ValueError("list __name__ wrong")
    if not (type(5) == int):
        raise ValueError("type(5) == int failed")
    if type(5) == str:
        raise ValueError("type(5) == str should be False")
    if str(type(5)) != "<class 'int'>":
        raise ValueError("str(type) wrong")
    let f: Foo = Foo(x=1)
    if type(f).__name__ != "Foo":
        raise ValueError("user class __name__ wrong")
    if not (type(f) == Foo):
        raise ValueError("type(inst) == Class failed")
    if type(5) != type(6):
        raise ValueError("type(5) == type(6) failed")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn str_strip_honours_chars_argument() {
        let src = r#"
def main() -> None:
    let a: str = "...hi...".strip(".")
    if a != "hi":
        raise ValueError("strip(chars) ignored its argument")
    let b: str = "42".zfill(5)
    if b != "00042":
        raise ValueError("zfill broken")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn dunder_methods_dispatch() {
        // Regression: __str__ / __eq__ / __add__ were silently ignored.
        let src = r#"
class V:
    n: int

impl V:
    def __str__(self) -> str:
        return f"V({self.n})"
    def __eq__(self, o: object) -> bool:
        match o:
            case V(x): return self.n == x
            case _: return False
    def __add__(self, o: V) -> V:
        return V(n=self.n + o.n)

def main() -> None:
    let a: V = V(n=2)
    let b: V = V(n=3)
    if str(a) != "V(2)":
        raise ValueError("__str__ ignored")
    if not (a == V(n=2)):
        raise ValueError("__eq__ ignored")
    if (a + b).n != 5:
        raise ValueError("__add__ ignored")
    if a not in [V(n=2), b]:
        raise ValueError("in-operator ignored __eq__")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn property_classmethod_staticmethod() {
        let src = r#"
class C:
    r: float

impl C:
    @property
    def area(self) -> float:
        return 3.14 * self.r * self.r
    @staticmethod
    def unit() -> C:
        return C(r=1.0)
    @classmethod
    def of(cls, r: float) -> C:
        return C(r=r)

def main() -> None:
    let c: C = C.of(2.0)
    if c.area < 12.0:
        raise ValueError("property not invoked")
    if C.unit().r != 1.0:
        raise ValueError("staticmethod broken")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn numeric_builtins_pow_divmod_intbase() {
        let src = r#"
def main() -> None:
    let q: tuple[int, int] = divmod(17, 5)
    if q[0] != 3 or q[1] != 2:
        raise ValueError("divmod broken")
    if pow(2, 10, 100) != 24:
        raise ValueError("modular pow broken")
    if int("ff", 16) != 255:
        raise ValueError("int(str, base) broken")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn unbound_builtin_method_via_pipe() {
        // Regression: `x |> str.lower()` lowers to `str.lower(x)`, which
        // requires unbound builtin-type method access to work.
        let src = r#"
def norm(raw: str) -> str:
    return raw |> str.strip() |> str.lower() |> str.replace(",", "")

def main() -> None:
    if norm("  A,B  ") != "ab":
        raise ValueError("unbound str method / pipe broken")
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    // ── Regression tests for VM stdlib/builtin gaps (N16 gap-fill) ────────

    #[test]
    fn math_integer_domain_functions() {
        // gap 1: gcd, lcm, factorial, isqrt, comb, perm must return correct ints.
        let src = r#"
import math
if math.gcd(12, 18) != 6:
    raise ValueError("gcd wrong")
if math.lcm(4, 6) != 12:
    raise ValueError("lcm wrong")
if math.factorial(5) != 120:
    raise ValueError("factorial wrong")
if math.isqrt(17) != 4:
    raise ValueError("isqrt wrong")
if math.comb(5, 2) != 10:
    raise ValueError("comb wrong")
if math.perm(5, 2) != 20:
    raise ValueError("perm wrong")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_exception_args() {
        let src = r#"
def main() -> None:
    let e: ValueError = ValueError("a", "b", 3)
    if str(e.args) != "('a', 'b', 3)":
        raise ValueError("multi args wrong")
    if str(e) != "('a', 'b', 3)":
        raise ValueError("multi str wrong")
    let e2: ValueError = ValueError("solo")
    if str(e2.args) != "('solo',)" or str(e2) != "solo":
        raise ValueError("single arg wrong")
    try:
        raise TypeError("x", "y")
    except TypeError as ex:
        if str(ex.args) != "('x', 'y')" or ex.__cause__ is not None:
            raise ValueError("handler args wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_property_setter_and_dir() {
        let src = r#"
class Temp:
    _c: float
impl Temp:
    @property
    def celsius(self) -> float:
        return self._c
    @celsius.setter
    def celsius(self, v: float) -> None:
        self._c = v

class Foo:
    a: int
impl Foo:
    def m(self) -> int:
        return self.a

def main() -> None:
    let t: Temp = Temp(_c=0.0)
    t.celsius = 100.0
    if t.celsius != 100.0:
        raise ValueError("property setter broken")
    let d: list[str] = dir(Foo(a=1))
    if not ("a" in d) or not ("m" in d):
        raise ValueError("dir broken")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_getattr_and_comprehension_walrus() {
        // VM gaps that paired with the checker false-reject fixes: a user
        // __getattr__ resolves missing attributes, and a walrus binding in a
        // comprehension leaks its last value to the enclosing scope.
        let src = r#"
plain class Proxy:
    def __getattr__(self, name: str) -> str:
        return f"attr:{name}"

def main() -> None:
    let p: Proxy = Proxy()
    if p.foo != "attr:foo" or p.bar != "attr:bar":
        raise ValueError("__getattr__ broken")
    let ys: list[int] = [y for x in [1, 2, 3] if (y := x * x) > 1]
    if ys != [4, 9] or y != 9:
        raise ValueError("walrus leak broken")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_data_model_round_two() {
        // Sixth round, part 2: slice assignment, __index__, __delitem__,
        // del obj.attr, cached_property, __iter__ returning self, function
        // __name__, set star-unpack, bare-class raise.
        let src = r#"
from functools import cached_property

class Idx:
    v: int
impl Idx:
    def __index__(self) -> int:
        return self.v

class Circle:
    r: float
impl Circle:
    @cached_property
    def area(self) -> float:
        return self.r * self.r

class Counter:
    n: int
impl Counter:
    def __iter__(self) -> Counter:
        return self
    def __next__(self) -> int:
        if self.n <= 0:
            raise StopIteration
        self.n = self.n - 1
        return self.n

class Bag:
    x: int
    y: int

def f(z: int) -> int:
    return z

def main() -> None:
    mut xs: list[int] = [1, 2, 3, 4, 5]
    xs[1:3] = [99]
    if xs != [1, 99, 4, 5]:
        raise ValueError("slice assign")
    if [10, 20, 30][Idx(v=2)] != 30:
        raise ValueError("__index__")
    if Circle(r=3.0).area != 9.0:
        raise ValueError("cached_property")
    if list(Counter(n=3)) != [2, 1, 0]:
        raise ValueError("__iter__ self")
    let b: Bag = Bag(x=1, y=2)
    del b.x
    if hasattr(b, "x") or b.y != 2:
        raise ValueError("del attr")
    if f.__name__ != "f":
        raise ValueError("__name__")
    let ys: list[int] = [1, 2]
    if sorted({*ys, 9}) != [1, 2, 9]:
        raise ValueError("set star")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn exception_model_fixes() {
        // Sixth stress round: builtin exception hierarchy catching, bare
        // re-raise, finally-replaces-exception, type(exc).__name__, and
        // __exit__ exception-info + suppression.
        let src = r#"
class Sup:
    n: int
impl Sup:
    def __enter__(self) -> int:
        return self.n
    def __exit__(self, et: object, ev: object, tb: object) -> bool:
        return True

mut log: list[str] = []

def main() -> None:
    try:
        raise ZeroDivisionError("d")
    except ArithmeticError:
        log.append("arith")
    try:
        let d: dict[str, int] = {}
        let _ = d["x"]
    except LookupError:
        log.append("lookup")
    try:
        try:
            raise ValueError("v")
        except ValueError:
            raise
    except ValueError:
        log.append("reraise")
    try:
        try:
            raise ValueError("a")
        finally:
            raise TypeError("b")
    except Exception as e:
        log.append(type(e).__name__)
    with Sup(n=1):
        raise ValueError("hidden")
    log.append("survived")
    if log != ["arith", "lookup", "reraise", "TypeError", "survived"]:
        raise ValueError(f"wrong: {log}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn truthiness_attrs_and_classvars() {
        // Fifth stress round: __bool__/__len__ truthiness, dynamic-attribute
        // builtins, ClassVar / plain-class class-level attribute access.
        let src = r#"
from typing import ClassVar

class Bag:
    items: list[int]
impl Bag:
    def __len__(self) -> int:
        return len(self.items)

class Flag:
    on: bool
impl Flag:
    def __bool__(self) -> bool:
        return self.on

class C:
    K: ClassVar[int] = 42
    v: int

plain class Reg:
    VERSION: str = "1.0"

def main() -> None:
    if bool(Bag(items=[])) or not bool(Bag(items=[1])):
        raise ValueError("__len__ truthiness wrong")
    if bool(Flag(on=False)) or not bool(Flag(on=True)):
        raise ValueError("__bool__ truthiness wrong")
    let b: Bag = Bag(items=[1])
    if getattr(b, "items") != [1] or not hasattr(b, "items") or hasattr(b, "nope"):
        raise ValueError("getattr/hasattr wrong")
    if C.K != 42 or C(v=1).K != 42:
        raise ValueError("ClassVar access wrong")
    if Reg.VERSION != "1.0":
        raise ValueError("plain class const access wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn power_complex_results() {
        // Negative base to a fractional power is complex (not nan); a complex
        // base to an integer power is exact.
        let src = r#"
def main() -> None:
    let z: complex = (-8) ** (1.0 / 3.0)
    if abs(z.real - 1.0) > 1e-9 or abs(z.imag - 1.7320508075688772) > 1e-9:
        raise ValueError("neg base frac pow wrong")
    if (1j) ** 2 != complex(-1, 0):
        raise ValueError("complex int pow wrong")
    if complex(2, 0) ** 3 != complex(8, 0):
        raise ValueError("complex cube wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn bytes_int_complex_parity_fixes() {
        // Stress-round stdlib gaps: bytes methods, int.to_bytes/.bit_count,
        // complex-from-string.
        let src = r#"
def main() -> None:
    if b"a,b,c".split(b",") != [b"a", b"b", b"c"]:
        raise ValueError("bytes split wrong")
    if b"  x  ".strip() != b"x":
        raise ValueError("bytes strip wrong")
    if not b"hi".startswith(b"h") or not b"hi".endswith(b"i"):
        raise ValueError("bytes starts/ends wrong")
    if b"hello".replace(b"l", b"L") != b"heLLo":
        raise ValueError("bytes replace wrong")
    if b",".join([b"a", b"b"]) != b"a,b":
        raise ValueError("bytes join wrong")
    if (255).to_bytes(4, "big") != b"\x00\x00\x00\xff":
        raise ValueError("to_bytes wrong")
    if (7).bit_count() != 3:
        raise ValueError("bit_count wrong")
    if complex("1+2j") != complex(1, 2) or complex("3j") != complex(0, 3):
        raise ValueError("complex str wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn re_module_parity_fixes() {
        // Stress-round `re` findings: callable sub, Python backref templates,
        // capturing-group split, named group access, finditer, subn.
        let src = r#"
import re

def dbl(mo: re.Match[str]) -> str:
    return str(int(mo.group(0)) * 2)

def main() -> None:
    if re.sub(r"\d", dbl, "1 2 3") != "2 4 6":
        raise ValueError("callable sub wrong")
    if re.split(r"(\d)", "a1b2") != ["a", "1", "b", "2", ""]:
        raise ValueError("capturing split wrong")
    if re.sub(r"(?P<w>\w+)@(?P<d>\w+)", r"\g<d>:\g<w>", "u@h") != "h:u":
        raise ValueError("named backref wrong")
    if re.sub(r"(\w+) (\w+)", r"\2 \1", "hello world") != "world hello":
        raise ValueError("numbered backref wrong")
    let m: re.Match[str]? = re.search(r"(?P<y>\d{4})-(?P<m>\d{2})", "2024-06")
    if m is None:
        raise ValueError("search failed")
    if m.group("y") != "2024" or m.group("m") != "06":
        raise ValueError("named group wrong")
    if re.subn(r"o", "0", "foo")[1] != 2:
        raise ValueError("subn count wrong")
    if [mo.group(0) for mo in re.finditer(r"\d+", "a12b3")] != ["12", "3"]:
        raise ValueError("finditer wrong")
    if re.sub(r"a", "X", "aaaa", 2) != "XXaa":
        raise ValueError("sub count wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn stress_round_two_vm_parity_fixes() {
        // Second adversarial stress round (sub-agent findings):
        //   * enum repr `<Name.MEMBER: value>` + value/name lookup
        //   * round(int, negative ndigits) rounds to tens/hundreds (half-even)
        //   * f-string `:c` int→char
        //   * `!=` derives from a user `__eq__` when `__ne__` is absent
        //   * math.prod
        //   * hash() dispatches a user `__hash__`
        //   * range indexing, bytes iteration, enumerate(start=), zip(strict=)
        let src = r#"
import math

enum Color:
    RED
    GREEN
    BLUE

class CI:
    s: str

impl CI:
    def __eq__(self, other: object) -> bool:
        return isinstance(other, CI) and self.s == other.s
    def __hash__(self) -> int:
        return 7

def main() -> None:
    if repr(Color.RED) != "<Color.RED: 1>":
        raise ValueError("enum repr wrong")
    if repr([Color.GREEN]) != "[<Color.GREEN: 2>]":
        raise ValueError("enum container repr wrong")
    if Color(2) != Color.GREEN or Color["BLUE"] != Color.BLUE:
        raise ValueError("enum lookup wrong")

    if round(123456, -2) != 123500 or round(15, -1) != 20 or round(25, -1) != 20:
        raise ValueError("round negative ndigits wrong")

    if f"{65:c}" != "A":
        raise ValueError("format :c wrong")

    if (CI(s="a") != CI(s="a")) or not (CI(s="a") != CI(s="b")):
        raise ValueError("__ne__ from __eq__ wrong")

    if hash(CI(s="x")) != 7:
        raise ValueError("hash __hash__ ignored")

    if math.prod([1, 2, 3, 4]) != 24:
        raise ValueError("math.prod wrong")

    if range(0, 20, 3)[2] != 6 or range(10)[-1] != 9:
        raise ValueError("range index wrong")
    if list(b"\x01\x02\x03") != [1, 2, 3]:
        raise ValueError("bytes iteration wrong")
    if list(enumerate(["a", "b"], start=10)) != [(10, "a"), (11, "b")]:
        raise ValueError("enumerate start wrong")
    if list(zip([1, 2], ["a", "b"], strict=True)) != [(1, "a"), (2, "b")]:
        raise ValueError("zip strict wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn stress_round_vm_parity_fixes() {
        // Adversarial stress-round findings (VM diverged from CPython):
        //   * str.replace honours the third `count` argument
        //   * sorted/list.sort with reverse=True stay stable for equal keys
        //   * json.dumps honours sort_keys=
        //   * math.isnan/isinf/isfinite exist
        //   * float-presentation format types (f/e/g) coerce int operands
        let src = r#"
import json
import math

def main() -> None:
    if "aaaa".replace("a", "b", 2) != "bbaa":
        raise ValueError("str.replace count ignored")
    if "xy".replace("x", "z", 0) != "xy":
        raise ValueError("str.replace count=0 should be a no-op")

    let data: list[tuple[int, str]] = [(1, "a"), (1, "b"), (2, "c"), (1, "d")]
    if sorted(data, key=lambda p: p[0], reverse=True) != [(2, "c"), (1, "a"), (1, "b"), (1, "d")]:
        raise ValueError("sorted(reverse=True) not stable")
    mut m: list[int] = [3, 1, 2, 1, 3]
    m.sort(reverse=True)
    if m != [3, 3, 2, 1, 1]:
        raise ValueError("list.sort(reverse=True) wrong")

    if json.dumps({"b": 1, "a": 2}, sort_keys=True) != '{"a": 2, "b": 1}':
        raise ValueError("json.dumps sort_keys ignored")

    if not math.isnan(math.nan) or not math.isinf(math.inf) or not math.isfinite(1.0):
        raise ValueError("math predicates broken")

    if f"{42:.2f}" != "42.00" or f"{1000000:e}" != "1.000000e+06":
        raise ValueError("int operand float-format ignored")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn container_elements_dispatch_user_repr() {
        // Regression: an object with a custom `__repr__` that is an ELEMENT of
        // a list / tuple / set / dict (key or value) must be rendered via the
        // user dunder, not the default dataclass repr. Enum members in a
        // container keep the CPython `<Class.NAME: value>` form, and Result
        // Ok/Err values keep their `Ok(value=...)` shape.
        let src = r#"
from typhon_runtime import Ok, Err

class Pt:
    x: int

impl Pt:
    def __repr__(self) -> str:
        return f"<{self.x}>"

class FP frozen:
    n: int

impl FP:
    def __repr__(self) -> str:
        return f"FP[{self.n}]"

class Plain:
    a: int

enum Color:
    RED
    GREEN

def main() -> None:
    if repr([Pt(1), Pt(2)]) != "[<1>, <2>]":
        raise ValueError("list element repr")
    if repr((Pt(1),)) != "(<1>,)":
        raise ValueError("single-tuple element repr")
    if repr((Pt(1), Pt(2))) != "(<1>, <2>)":
        raise ValueError("tuple element repr")
    if repr({"k": Pt(1)}) != "{'k': <1>}":
        raise ValueError("dict value repr")
    if repr({FP(1), FP(2)}) not in ("{FP[1], FP[2]}", "{FP[2], FP[1]}"):
        raise ValueError("set element repr")
    if repr({FP(9): Pt(1)}) != "{FP[9]: <1>}":
        raise ValueError("instance dict-key repr")
    # str() of a container renders elements via repr()
    if str([Pt(1)]) != "[<1>]":
        raise ValueError("str of list uses element repr")
    # nested
    if repr([[Pt(1)], [Pt(2)]]) != "[[<1>], [<2>]]":
        raise ValueError("nested list repr")
    # enum members keep the CPython <Class.NAME: value> form
    if repr([Color.RED, Color.GREEN]) != "[<Color.RED: 1>, <Color.GREEN: 2>]":
        raise ValueError("enum member in list repr")
    # Result values keep the dataclass shape
    if repr([Ok(1), Err("x")]) != "[Ok(value=1), Err(error='x')]":
        raise ValueError("Result in list repr")
    # plain dataclass without custom repr matches CPython default
    if repr([Plain(a=1)]) != "[Plain(a=1)]":
        raise ValueError("plain dataclass in list repr")
    # empty containers
    if repr([]) != "[]" or repr(()) != "()" or repr({}) != "{}":
        raise ValueError("empty container repr")
    if repr(set()) != "set()":
        raise ValueError("empty set repr")
    # strings as elements are quoted via repr
    if repr(["a"]) != "['a']":
        raise ValueError("string element quoting")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn math_float_functions_and_constants() {
        // gap 1: tau, trunc, copysign, hypot, degrees, radians, expm1, log1p, atan2.
        let src = r#"
import math
if abs(math.tau - 6.283185307179586) > 1e-10:
    raise ValueError("tau wrong")
if math.trunc(3.7) != 3:
    raise ValueError("trunc wrong")
if math.copysign(3.0, -1.0) != -3.0:
    raise ValueError("copysign wrong")
if abs(math.hypot(3.0, 4.0) - 5.0) > 1e-10:
    raise ValueError("hypot wrong")
if abs(math.degrees(math.pi) - 180.0) > 1e-10:
    raise ValueError("degrees wrong")
if abs(math.radians(180.0) - math.pi) > 1e-10:
    raise ValueError("radians wrong")
if abs(math.expm1(1.0) - 1.718281828459045) > 1e-10:
    raise ValueError("expm1 wrong")
if abs(math.log1p(1.0) - 0.6931471805599453) > 1e-10:
    raise ValueError("log1p wrong")
if abs(math.atan2(1.0, 1.0) - 0.7853981633974483) > 1e-10:
    raise ValueError("atan2 wrong")
if abs(math.fmod(10.0, 3.0) - 1.0) > 1e-10:
    raise ValueError("fmod wrong")
if abs(math.dist([0,0],[3,4]) - 5.0) > 1e-10:
    raise ValueError("dist wrong")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn counter_most_common_and_elements() {
        // gap 2: Counter.most_common() and Counter.elements() must work.
        let src = r#"
from collections import Counter
c = Counter(["a", "b", "a", "c", "a", "b"])
mc = c.most_common(2)
if len(mc) != 2:
    raise ValueError("most_common(2) length wrong")
if mc[0][0] != "a" or mc[0][1] != 3:
    raise ValueError("most_common top entry wrong")
if mc[1][0] != "b" or mc[1][1] != 2:
    raise ValueError("most_common second entry wrong")
elems = c.elements()
if len(elems) != 6:
    raise ValueError("elements() total count wrong")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn str_format_positional_and_spec() {
        // gap 3: str.format() with positional args and format specs.
        let src = r#"
r1 = "{0}-{1}-{0}".format("a", "b")
if r1 != "a-b-a":
    raise ValueError("positional format wrong: " + r1)
r2 = "{}-{}".format(1, 2)
if r2 != "1-2":
    raise ValueError("auto-index format wrong: " + r2)
r3 = "{:.2f}".format(3.14159)
if r3 != "3.14":
    raise ValueError("float spec wrong: " + r3)
r4 = "{:05d}".format(42)
if r4 != "00042":
    raise ValueError("int spec wrong: " + r4)
r5 = "{{literal}}".format()
if r5 != "{literal}":
    raise ValueError("escaped braces wrong: " + r5)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn str_format_named_kwargs() {
        // gap 3: str.format() with keyword arguments.
        let src = r#"
r = "{name} says {greeting}".format(name="Alice", greeting="hello")
if r != "Alice says hello":
    raise ValueError("named format wrong: " + r)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn fstring_exponent_notation_matches_cpython() {
        // gap 4: f"{x:e}" must produce CPython-style `e+NN` not Rust `eN`.
        let src = r#"
r1 = f"{3.14159:e}"
if r1 != "3.141590e+00":
    raise ValueError("e format wrong: " + r1)
r2 = f"{12345.678:.2e}"
if r2 != "1.23e+04":
    raise ValueError("e with precision wrong: " + r2)
r3 = f"{0.0001:e}"
if r3 != "1.000000e-04":
    raise ValueError("negative exp wrong: " + r3)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn frozenset_repr_matches_cpython() {
        // gap 5: repr(frozenset([...])) must be "frozenset({...})" not "{...}".
        let src = r#"
r = repr(frozenset())
if r != "frozenset()":
    raise ValueError("empty frozenset repr wrong: " + r)
fs = frozenset([1])
r2 = repr(fs)
if not r2.startswith("frozenset("):
    raise ValueError("frozenset repr wrong: " + r2)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    // ── Slot-resolved locals (VM performance Tier 1b) ────────────────────────

    /// Slot-eligible recursion: the classic fib exercises per-call slot frames
    /// (parameter `n` in slot 0) plus the recursion depth counter.
    #[test]
    fn slot_recursion_fib() {
        let src = r#"
def fib(n: int) -> int:
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

def main() -> None:
    if fib(10) != 55:
        raise ValueError("fib(10) wrong")
    if fib(20) != 6765:
        raise ValueError("fib(20) wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A local binding may shadow a module-level name of the same identifier;
    /// the slot must win over the outer name once assigned.
    #[test]
    fn slot_shadows_module_name() {
        let src = r#"
value: int = 100

def f() -> int:
    let value: int = 7
    return value

def main() -> None:
    if f() != 7:
        raise ValueError("local slot did not shadow module name")
    if value != 100:
        raise ValueError("module name was clobbered")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Parameter defaults, `*args`, and `**kwargs` all bind into slots.
    #[test]
    fn slot_param_defaults_and_variadics() {
        let src = r#"
def g(a: int, b: int = 10, *args: int, **kwargs: int) -> int:
    mut total: int = a + b
    for v in args:
        total = total + v
    for k in kwargs:
        total = total + kwargs[k]
    return total

def main() -> None:
    if g(1) != 11:
        raise ValueError("default b wrong")
    if g(1, 2) != 3:
        raise ValueError("explicit b wrong")
    if g(1, 2, 3, 4) != 10:
        raise ValueError("args wrong")
    if g(1, 2, 3, x=5, y=6) != 17:
        raise ValueError("kwargs wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Augmented assignment rebinds the same slot in place.
    #[test]
    fn slot_augmented_assignment() {
        let src = r#"
def f() -> int:
    mut a: int = 1
    a += 4
    a *= 3
    a -= 2
    return a

def main() -> None:
    if f() != 13:
        raise ValueError("augmented slot wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `for` / `with` / `except` / `match`-capture targets are all slot binds.
    #[test]
    fn slot_for_with_except_match_captures() {
        let src = r#"
def classify(v: object) -> str:
    match v:
        case [a, *rest]:
            return "seq:" + str(a) + ":" + str(len(rest))
        case {"k": val, **others}:
            return "map:" + str(val) + ":" + str(len(others))
        case int() as num:
            return "int:" + str(num)
        case _:
            return "other"

def f() -> int:
    mut total: int = 0
    for k in range(4):
        total = total + k
    try:
        raise ValueError("boom")
    except ValueError as e:
        total = total + len(str(e))
    return total

def main() -> None:
    if f() != 10:
        raise ValueError("for/except slots wrong")
    if classify([10, 20, 30]) != "seq:10:2":
        raise ValueError("seq capture wrong")
    if classify({"k": 1, "a": 2, "b": 3}) != "map:1:2":
        raise ValueError("map capture wrong")
    if classify(42) != "int:42":
        raise ValueError("int capture wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Tuple / starred unpacking binds each element into its own slot.
    #[test]
    fn slot_tuple_unpacking() {
        let src = r#"
def f() -> int:
    let (a, b) = (1, 2)
    let (c, *mid, d) = (10, 20, 30, 40)
    mut s: int = a + b + c + d
    for x in mid:
        s = s + x
    return s

def main() -> None:
    if f() != 103:
        raise ValueError("tuple unpack slots wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Walrus assignment inside a slot frame binds the target into a slot and
    /// remains visible after the enclosing statement.
    #[test]
    fn slot_walrus_binding() {
        let src = r#"
def f(xs: list[int]) -> int:
    mut acc: int = 0
    if (n := len(xs)) > 0:
        acc = acc + n
    return acc + n

def main() -> None:
    if f([1, 2, 3]) != 6:
        raise ValueError("walrus slot wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Read-before-assign: with a module-level `x` present, reading a
    /// function-local `x` *before* its first assignment falls through to the
    /// module binding (the VM does NOT raise `UnboundLocalError` — CPython
    /// would). This test pins the pre-existing VM behaviour so the slot path
    /// reproduces it exactly.
    #[test]
    fn slot_read_before_assign_reads_outer() {
        let src = r#"
x: int = 100

def f() -> int:
    let before: int = x
    let x: int = 1
    return before + x

def main() -> None:
    if f() != 101:
        raise ValueError("read-before-assign did not read the outer x")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `del` on a slot local unbinds it; a later read falls through to the
    /// module scope, matching the pre-slot behaviour.
    #[test]
    fn slot_del_unbinds() {
        let src = r#"
x: int = 7

def f() -> int:
    let x: int = 1
    del x
    return x

def main() -> None:
    if f() != 7:
        raise ValueError("del did not unbind the slot")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Ineligible fallbacks: functions using `global`, `nonlocal`, a nested
    /// `def`, or a comprehension keep the classic Env path and behave
    /// identically.
    #[test]
    fn slot_ineligible_fallbacks() {
        let src = r#"
counter: int = 0

def bump() -> None:
    global counter
    counter = counter + 1

def make_adder(base: int) -> object:
    def add(x: int) -> int:
        return base + x
    return add

def with_comprehension(n: int) -> int:
    let squares: list[int] = [i * i for i in range(n)]
    mut total: int = 0
    for s in squares:
        total = total + s
    return total

def outer() -> int:
    mut acc: int = 0
    def inner() -> None:
        nonlocal acc
        acc = acc + 5
    inner()
    inner()
    return acc

def main() -> None:
    bump()
    bump()
    if counter != 2:
        raise ValueError("global fallback wrong")
    let add3 = make_adder(3)
    if add3(4) != 7:
        raise ValueError("closure fallback wrong")
    if with_comprehension(4) != 14:
        raise ValueError("comprehension fallback wrong")
    if outer() != 10:
        raise ValueError("nonlocal fallback wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Closures still capture the enclosing frame correctly even though the
    /// capturing function itself is ineligible (nested def).
    #[test]
    fn slot_closures_capture_correctly() {
        let src = r#"
def make_counter() -> object:
    mut n: int = 0
    def inc() -> int:
        nonlocal n
        n = n + 1
        return n
    return inc

def main() -> None:
    let c = make_counter()
    if c() != 1 or c() != 2 or c() != 3:
        raise ValueError("closure capture wrong")
    let d = make_counter()
    if d() != 1:
        raise ValueError("second closure shares state")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A slot-eligible method (its `self` parameter is slot 0) dispatches and
    /// reads instance fields correctly across many calls.
    #[test]
    fn slot_eligible_method() {
        let src = r#"
class Point:
    x: float
    y: float

impl Point:
    def dist2(self) -> float:
        return self.x * self.x + self.y * self.y

def main() -> None:
    mut total: float = 0.0
    mut i: int = 0
    while i < 5:
        let p: Point = Point(x=1.5, y=2.0)
        total = total + p.dist2()
        i = i + 1
    if total != 31.25:
        raise ValueError("method slot dispatch wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    // ── PEP 654 exception groups / `except*` ──────────────────────────────
    //
    // Every expected value below was produced by running the equivalent
    // Python under CPython 3.13 first; the VM is asserted against it.

    /// A bare (non-group) exception caught by `except*` binds an implicit
    /// `ExceptionGroup('', (exc,))` wrapper, not the exception itself — the
    /// F7 divergence. CPython 3.13:
    ///   type(e).__name__ == 'ExceptionGroup'
    ///   repr(e)          == "ExceptionGroup('', (ValueError('v'),))"
    ///   e.exceptions     == (ValueError('v'),)
    ///   e.message        == ''
    #[test]
    fn except_star_wraps_a_bare_exception_in_a_group() {
        let src = r#"
def main() -> None:
    mut seen: int = 0
    try:
        raise ValueError("v")
    except* ValueError as e:
        seen = 1
        if type(e).__name__ != "ExceptionGroup":
            raise AssertionError("handler must bind an ExceptionGroup")
        if repr(e) != "ExceptionGroup('', (ValueError('v'),))":
            raise AssertionError("wrapper repr wrong: " + repr(e))
        if len(e.exceptions) != 1 or str(e.exceptions[0]) != "v":
            raise AssertionError("wrapper members wrong")
        if e.message != "":
            raise AssertionError("wrapper message wrong")
    if seen != 1:
        raise AssertionError("handler did not run")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Every matching handler runs — once each — and receives only its own
    /// members, keeping the group's message. CPython 3.13 prints
    /// `hV g (ValueError('v'), ValueError('v2'))` then `hT g (TypeError('t'),)`.
    #[test]
    fn except_star_runs_each_matching_handler_once_with_its_own_subgroup() {
        let src = r#"
def main() -> None:
    mut v_runs: int = 0
    mut t_runs: int = 0
    mut v_count: int = 0
    try:
        raise ExceptionGroup("g", [ValueError("v"), TypeError("t"), ValueError("v2")])
    except* ValueError as e:
        v_runs = v_runs + 1
        v_count = len(e.exceptions)
        if e.message != "g":
            raise AssertionError("subgroup lost the group message")
    except* TypeError as e2:
        t_runs = t_runs + 1
        if len(e2.exceptions) != 1:
            raise AssertionError("TypeError subgroup wrong")
    if v_runs != 1 or t_runs != 1:
        raise AssertionError("each handler must run exactly once")
    if v_count != 2:
        raise AssertionError("ValueError subgroup should hold both members")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// The unmatched remainder is re-raised as a group derived from the
    /// original (same message), and a plain `except Exception` catches it
    /// (`ExceptionGroup` derives from `Exception`).
    #[test]
    fn except_star_reraises_the_unhandled_remainder() {
        let src = r#"
def main() -> None:
    mut caught: str = ""
    try:
        try:
            raise ExceptionGroup("g", [ValueError("v"), KeyError("k")])
        except* ValueError as e:
            if len(e.exceptions) != 1:
                raise AssertionError("wrong matched subgroup")
    except Exception as outer:
        caught = repr(outer)
    if caught != "ExceptionGroup('g', [KeyError('k')])":
        raise AssertionError("remainder wrong: " + caught)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A bare exception that no `except*` handler matches propagates as
    /// itself — CPython does not leave the implicit wrapper behind.
    #[test]
    fn unmatched_bare_exception_propagates_unwrapped() {
        let src = r#"
def main() -> None:
    mut name: str = ""
    try:
        try:
            raise ValueError("v")
        except* TypeError as e:
            raise AssertionError("must not match")
    except ValueError as outer:
        name = type(outer).__name__ + ":" + str(outer)
    if name != "ValueError:v":
        raise AssertionError("bare exception was rewrapped: " + name)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Splitting recurses through nested groups and preserves their nesting.
    /// CPython 3.13: the `except* TypeError` handler sees
    /// `ExceptionGroup('outer', [ExceptionGroup('inner', [TypeError('t')])])`.
    #[test]
    fn except_star_split_preserves_nesting() {
        let src = r#"
def main() -> None:
    mut got: str = ""
    try:
        raise ExceptionGroup("outer", [ValueError("v"), ExceptionGroup("inner", [TypeError("t")])])
    except* TypeError as e:
        got = repr(e)
    except* ValueError as e2:
        if repr(e2) != "ExceptionGroup('outer', [ValueError('v')])":
            raise AssertionError("ValueError side wrong: " + repr(e2))
    if got != "ExceptionGroup('outer', [ExceptionGroup('inner', [TypeError('t')])])":
        raise AssertionError("nesting not preserved: " + got)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Group construction, `str`, `repr`, `.args`, `.exceptions`, `.message`,
    /// and CPython's `BaseExceptionGroup` -> `ExceptionGroup` auto-downcast.
    #[test]
    fn exception_group_value_protocol_matches_cpython() {
        let src = r#"
def main() -> None:
    let one = ExceptionGroup("g", [ValueError("v")])
    if str(one) != "g (1 sub-exception)":
        raise AssertionError("str wrong: " + str(one))
    if repr(one) != "ExceptionGroup('g', [ValueError('v')])":
        raise AssertionError("repr wrong: " + repr(one))
    if one.message != "g":
        raise AssertionError("message wrong")
    if len(one.args) != 2 or one.args[0] != "g":
        raise AssertionError("args wrong")
    let two = ExceptionGroup("g", [ValueError("v"), TypeError("t")])
    if str(two) != "g (2 sub-exceptions)":
        raise AssertionError("plural str wrong: " + str(two))
    # BaseExceptionGroup downcasts when every member is an Exception.
    let down = BaseExceptionGroup("g", [ValueError("v")])
    if type(down).__name__ != "ExceptionGroup":
        raise AssertionError("BaseExceptionGroup did not downcast")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `except* ExceptionGroup` is a runtime `TypeError` in CPython, with
    /// this exact message.
    #[test]
    fn catching_a_group_type_with_except_star_is_a_type_error() {
        let src = r#"
def main() -> None:
    mut msg: str = ""
    try:
        try:
            raise ExceptionGroup("g", [ValueError("v")])
        except* ExceptionGroup as e:
            raise AssertionError("must not run")
    except TypeError as te:
        msg = str(te)
    if msg != "catching ExceptionGroup with except* is not allowed. Use except instead.":
        raise AssertionError("wrong message: " + msg)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// F5 — a failing `gather:` task surfaces as
    /// `ExceptionGroup('unhandled errors in a TaskGroup', [...])`, exactly as
    /// `asyncio.TaskGroup` does under CPython. Before this, the VM raised the
    /// bare `ValueError` out of `create_task`, so a plain `except ValueError`
    /// caught it under `tyc run` and the compiled build died uncaught.
    #[test]
    fn gather_task_failure_surfaces_as_an_exception_group() {
        let src = r#"
import asyncio

@gatherable
async def fine() -> int:
    return 1

@gatherable
async def boom() -> int:
    raise ValueError("bad")

async def run() -> None:
    mut caught: str = ""
    try:
        gather:
            a = fine()
            b = boom()
    except* ValueError as e:
        caught = type(e).__name__ + "|" + e.message + "|" + str(len(e.exceptions))
    if caught != "ExceptionGroup|unhandled errors in a TaskGroup|1":
        raise AssertionError("gather failure shape wrong: " + caught)

def main() -> None:
    asyncio.run(run())

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// The companion half of F5: a *plain* `except ValueError` around a
    /// `gather:` must NOT catch the failure — the group does not match it —
    /// which is exactly what the compiled CPython build does.
    #[test]
    fn plain_except_does_not_catch_a_gather_failure() {
        let src = r#"
import asyncio

@gatherable
async def boom() -> int:
    raise ValueError("bad")

@gatherable
async def fine() -> int:
    return 1

async def run() -> None:
    mut wrong: int = 0
    mut group: str = ""
    try:
        try:
            gather:
                a = fine()
                b = boom()
        except ValueError as v:
            wrong = 1
    except Exception as outer:
        group = type(outer).__name__
    if wrong != 0:
        raise AssertionError("plain except must not catch a TaskGroup failure")
    if group != "ExceptionGroup":
        raise AssertionError("expected an ExceptionGroup, got " + group)

def main() -> None:
    asyncio.run(run())

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `else:` and `finally:` still work on a `try*`, and neither is part of
    /// the `except*` block.
    #[test]
    fn except_star_honours_else_and_finally() {
        let src = r#"
def main() -> None:
    mut trace: str = ""
    try:
        trace = trace + "b"
    except* ValueError as e:
        trace = trace + "h"
    else:
        trace = trace + "e"
    finally:
        trace = trace + "f"
    if trace != "bef":
        raise AssertionError("else/finally order wrong: " + trace)
    mut trace2: str = ""
    try:
        raise ValueError("v")
    except* ValueError as e2:
        trace2 = trace2 + "h"
    finally:
        trace2 = trace2 + "f"
    if trace2 != "hf":
        raise AssertionError("finally after handler wrong: " + trace2)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A naked `raise` inside an `except*` handler is a PEP 654 *re-raise*:
    /// CPython merges the re-raised subgroup back with the unhandled
    /// remainder, reconstituting the original group. `raise e` of the bound
    /// subgroup is a *new* raise and keeps the `''`-wrapped shape. Every
    /// expected repr below is CPython 3.13 output:
    ///   naked:    ExceptionGroup('g', [ValueError('v'), TypeError('t')])
    ///   raise e:  ExceptionGroup('', [ExceptionGroup('g', [ValueError('v')]),
    ///                                 ExceptionGroup('g', [TypeError('t')])])
    ///   mixed:    ExceptionGroup('', [OSError('new'),
    ///                                 ExceptionGroup('g', [ValueError('v'), KeyError('k')])])
    ///   handled+naked: ExceptionGroup('g', [ValueError('v')])
    #[test]
    fn except_star_naked_reraise_reconstitutes_the_original_group() {
        let src = r#"
def main() -> None:
    mut got: str = ""
    try:
        try:
            raise ExceptionGroup("g", [ValueError("v"), TypeError("t")])
        except* ValueError:
            raise
    except BaseException as e:
        got = repr(e)
    if got != "ExceptionGroup('g', [ValueError('v'), TypeError('t')])":
        raise AssertionError("naked reraise wrong: " + got)

    mut explicit: str = ""
    try:
        try:
            raise ExceptionGroup("g", [ValueError("v"), TypeError("t")])
        except* ValueError as ex:
            raise ex
    except BaseException as e2:
        explicit = repr(e2)
    if explicit != "ExceptionGroup('', [ExceptionGroup('g', [ValueError('v')]), ExceptionGroup('g', [TypeError('t')])])":
        raise AssertionError("explicit raise-e wrong: " + explicit)

    mut mixed: str = ""
    try:
        try:
            raise ExceptionGroup("g", [ValueError("v"), TypeError("t"), KeyError("k")])
        except* ValueError:
            raise
        except* TypeError:
            raise OSError("new")
    except BaseException as e3:
        mixed = repr(e3)
    if mixed != "ExceptionGroup('', [OSError('new'), ExceptionGroup('g', [ValueError('v'), KeyError('k')])])":
        raise AssertionError("mixed reraise/raise wrong: " + mixed)

    mut partial: str = ""
    try:
        try:
            raise ExceptionGroup("g", [ValueError("v"), TypeError("t")])
        except* ValueError:
            raise
        except* TypeError:
            pass
    except BaseException as e4:
        partial = repr(e4)
    if partial != "ExceptionGroup('g', [ValueError('v')])":
        raise AssertionError("handled+naked wrong: " + partial)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `await` on a TaskGroup task that failed must re-raise the task's
    /// exception — not silently bind `None` and keep executing the body.
    /// CPython 3.13 terminates the body at that await (via cancellation) and
    /// `__aexit__` raises
    /// `ExceptionGroup('unhandled errors in a TaskGroup', [ValueError('boom')])`
    /// with the error exactly once; the statements after the await never run.
    #[test]
    fn await_on_failed_taskgroup_task_raises_instead_of_yielding_none() {
        let src = r#"
import asyncio

async def bad() -> int:
    raise ValueError("boom")
    return 0

async def body() -> None:
    async with asyncio.TaskGroup() as tg:
        let t = tg.create_task(bad())
        let r: int = await t
        raise AssertionError("body continued past an await on a failed task")

def main() -> None:
    mut caught: str = ""
    try:
        asyncio.run(body())
    except Exception as e:
        caught = repr(e)
    if caught != "ExceptionGroup('unhandled errors in a TaskGroup', [ValueError('boom')])":
        raise AssertionError("wrong group: " + caught)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// CPython's `TaskGroup._is_base_error` singles out `KeyboardInterrupt`
    /// and `SystemExit`: the first one observed is re-raised BARE from
    /// `__aexit__` (other collected failures dropped), while every other
    /// failure — a bare `BaseException` included — is wrapped in the group.
    /// All four shapes verified against CPython 3.13.
    #[test]
    fn taskgroup_reraises_keyboard_interrupt_and_system_exit_bare() {
        let src = r#"
import asyncio

async def ok() -> int:
    return 1

async def bad() -> int:
    raise ValueError("boom")
    return 0

async def badki() -> int:
    raise KeyboardInterrupt("k")
    return 0

async def body_ki() -> None:
    async with asyncio.TaskGroup() as tg:
        let t = tg.create_task(ok())
        raise KeyboardInterrupt("body")

async def child_ki() -> None:
    async with asyncio.TaskGroup() as tg:
        let t = tg.create_task(badki())

async def body_exit() -> None:
    async with asyncio.TaskGroup() as tg:
        let t = tg.create_task(ok())
        raise SystemExit(3)

async def base_stays_grouped() -> None:
    async with asyncio.TaskGroup() as tg:
        let t = tg.create_task(ok())
        raise BaseException("b")

async def child_error_then_ki() -> None:
    async with asyncio.TaskGroup() as tg:
        let t1 = tg.create_task(bad())
        let t2 = tg.create_task(badki())

def main() -> None:
    mut got: str = ""
    try:
        asyncio.run(body_ki())
    except KeyboardInterrupt as e:
        got = repr(e)
    if got != "KeyboardInterrupt('body')":
        raise AssertionError("body KI not re-raised bare: " + got)

    mut child: str = ""
    try:
        asyncio.run(child_ki())
    except KeyboardInterrupt as e2:
        child = repr(e2)
    if child != "KeyboardInterrupt('k')":
        raise AssertionError("child KI not re-raised bare: " + child)

    mut se: str = ""
    try:
        asyncio.run(body_exit())
    except SystemExit as e3:
        se = repr(e3)
    if se != "SystemExit(3)":
        raise AssertionError("SystemExit not re-raised bare: " + se)

    mut base: str = ""
    try:
        asyncio.run(base_stays_grouped())
    except BaseException as e4:
        base = repr(e4)
    if base != "BaseExceptionGroup('unhandled errors in a TaskGroup', [BaseException('b')])":
        raise AssertionError("BaseException must stay in the group: " + base)

    mut first: str = ""
    try:
        asyncio.run(child_error_then_ki())
    except KeyboardInterrupt as e5:
        first = repr(e5)
    if first != "KeyboardInterrupt('k')":
        raise AssertionError("KI must win over the group: " + first)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Splitting a mixed `BaseExceptionGroup` re-derives each side through
    /// `BaseExceptionGroup.__new__`, so the matched side downcasts to
    /// `ExceptionGroup` when its members are all ordinary `Exception`s —
    /// making `isinstance(e, Exception)` true inside the handler. The
    /// constructor enforces the same member rule: an `ExceptionGroup` cannot
    /// hold a nested `BaseExceptionGroup`, and a `BaseExceptionGroup` with
    /// one stays base. All verified against CPython 3.13.
    #[test]
    fn except_star_split_downcasts_matched_side_to_exception_group() {
        let src = r#"
def main() -> None:
    mut matched: str = ""
    mut rest: str = ""
    try:
        try:
            raise BaseExceptionGroup("g", [ValueError("v"), KeyboardInterrupt("k")])
        except* ValueError as e:
            matched = type(e).__name__ + "|" + repr(e) + "|" + str(isinstance(e, Exception))
    except BaseException as r:
        rest = type(r).__name__ + "|" + repr(r)
    if matched != "ExceptionGroup|ExceptionGroup('g', [ValueError('v')])|True":
        raise AssertionError("matched side wrong: " + matched)
    if rest != "BaseExceptionGroup|BaseExceptionGroup('g', [KeyboardInterrupt('k')])":
        raise AssertionError("rest side wrong: " + rest)

    let inner = BaseExceptionGroup("i", [KeyboardInterrupt("k")])
    mut nested: str = ""
    try:
        let outer = ExceptionGroup("o", [inner])
        nested = "constructed"
    except TypeError as te:
        nested = str(te)
    if nested != "Cannot nest BaseExceptions in an ExceptionGroup":
        raise AssertionError("nesting rule wrong: " + nested)
    let outer2 = BaseExceptionGroup("o", [inner])
    if type(outer2).__name__ != "BaseExceptionGroup":
        raise AssertionError("group holding a BaseExceptionGroup must stay base")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// CPython's `ADJUST_INDICES` never clamps a positive `start` down to
    /// `len`, so a start beyond the string (or an inverted range) is
    /// no-match territory even for an empty needle: `"abc".find("", 4)` is
    /// `-1`, `"abc".startswith("", 5)` is `False`, `"abc".count("", 2, 1)`
    /// is `0`, `"abc".index("", 4)` raises. All verified against 3.13.
    #[test]
    fn str_search_start_beyond_len_matches_cpython() {
        let src = r#"
def main() -> None:
    let s: str = "abc"
    if s.find("", 4) != -1 or s.find("", 3) != 3 or s.find("", 5) != -1:
        raise AssertionError("find empty needle beyond len wrong")
    if s.rfind("", 4) != -1 or s.rfind("", 3) != 3:
        raise AssertionError("rfind empty needle beyond len wrong")
    if s.startswith("", 5) or not s.startswith("", 3):
        raise AssertionError("startswith empty needle beyond len wrong")
    if s.endswith("", 4) or not s.endswith("", 3):
        raise AssertionError("endswith empty needle beyond len wrong")
    if s.count("", 4) != 0 or s.count("", 3) != 1 or s.count("", 0) != 4:
        raise AssertionError("count empty needle beyond len wrong")
    if s.find("", 2, 1) != -1 or s.count("", 2, 1) != 0:
        raise AssertionError("inverted range must not match an empty needle")
    if s.startswith("", 2, 1) or s.endswith("", 2, 1):
        raise AssertionError("inverted range startswith/endswith wrong")
    if s.find("", -10) != 0 or s.rfind("", -10) != 3:
        raise AssertionError("negative start clamps to 0")
    let e: str = ""
    if e.find("", 0) != 0 or e.find("", 1) != -1 or e.startswith("", 1) or e.count("", 1) != 0:
        raise AssertionError("empty string beyond-len searches wrong")
    mut raised: str = ""
    try:
        let x: int = s.index("", 4)
    except ValueError as ve:
        raised = str(ve)
    if raised != "substring not found":
        raise AssertionError("index beyond len must raise: " + raised)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }
}
