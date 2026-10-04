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
/// Generated Unicode case-folding tables for `str.casefold()` — see the module
/// header and `scripts/gen-casefold.py`.
mod casefold_data;
pub mod codecs;
pub mod env;
pub mod error;
pub mod ffi;
pub mod hashes;
pub mod interp;
pub mod pyhash;
pub mod slots;
mod unicode_data;
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
    let (mut module, prep) = front_end(source, FrontEnd::Program).map_err(|e| {
        let where_ = origin
            .map(|p| format!("{}: ", p.display()))
            .unwrap_or_default();
        VmError::Parse(format!("{where_}{e}"))
    })?;

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
        let (cross_module_fns, external_facts) =
            merge_cross_module_extensions_for_vm(&module, src_root, src_root, &mut registry);
        let _ = tyc_analyse::rewrite_builtin_extension_calls_with_facts(
            &mut module,
            &registry,
            &external_facts,
        );
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
    interp.lazy_import_aliases = prep
        .lazy_imports
        .iter()
        .map(|li| (li.module.clone(), li.alias.clone()))
        .collect();
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
    // …and `__doc__`, which CPython binds on every module: the leading
    // string literal, or `None`.
    interp.root.set(
        "__doc__",
        match crate::interp::module_docstring(&module) {
            Some(doc) => Value::Str(std::rc::Rc::new(doc)),
            None => Value::None,
        },
    );
    if let Some(p) = origin {
        interp.root.set(
            "__file__",
            Value::Str(std::rc::Rc::new(p.to_string_lossy().into_owned())),
        );
    }

    let run_result = interp.run_module(&module);
    // CPython flushes every file object still open when the interpreter
    // finalises them; the `io` shim keeps that list.
    flush_open_files(&mut interp);
    match run_result {
        Ok(()) => Ok(0),
        Err(Unwind::Return(_)) => Ok(0),
        Err(Unwind::Exception(exc)) if exc.kind == "SystemExit" => {
            // CPython: `SystemExit(None)` / no argument → 0; an int → that
            // status; anything else is printed to stderr and the status is 1.
            let code = match &exc.value {
                Some(Value::Exception { args, .. }) => match args.first() {
                    None | Some(Value::None) => 0,
                    Some(Value::Int(i)) => {
                        i.to_i64()
                            .unwrap_or(1)
                            .clamp(i32::MIN as i64, i32::MAX as i64) as i32
                    }
                    Some(Value::Bool(b)) => *b as i32,
                    Some(other) => {
                        eprintln!("{}", other.py_str());
                        1
                    }
                },
                _ => {
                    if !exc.message.is_empty() {
                        eprintln!("{}", exc.message);
                        1
                    } else {
                        0
                    }
                }
            };
            Ok(code)
        }
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
        Err(Unwind::Break | Unwind::Continue | Unwind::QuestionMark(_) | Unwind::Yield(_)) => {
            Err(VmError::runtime("unexpected control-flow at module level"))
        }
    }
}

/// Which caller a [`front_end`] run serves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrontEnd {
    /// A `.ty` program file: the entry module or an imported sibling.
    Program,
    /// An embedded stdlib shim: plain Python validated against CPython, so
    /// every class is emitted exactly as written (no `@dataclass`
    /// decoration, no synthesised `__init__`) and there is no `@memo` or
    /// `comptime` to evaluate.
    Shim,
}

/// The one front end every VM entry point shares — the entry program, an
/// imported sibling module, and the embedded stdlib shims. It runs the
/// canonical sugar chain plus the preprocessor
/// ([`preprocess::expand_and_preprocess_mapped`], the exact chain
/// `tyc check` / `tyc build` run), parses, inlines `comptime` values and
/// desugars, so the VM can never drift from the compiled path's pass order
/// again (each caller used to assemble the ten-pass chain by hand, and the
/// copies ran `expand_lazy_lets` before `expand_typed_let_unpack`).
///
/// `lazy import` stays an eager import (`rewrite_lazy_imports = false`):
/// the full lowering produces a descriptor-and-`__getattr__` proxy class,
/// and there is no point deferring an import the VM is about to evaluate.
///
/// The returned `PreprocessResult::line_map` maps a `python_source` line to
/// the line the user wrote (both 0-based); tracebacks use it.
pub(crate) fn front_end(
    source: &str,
    kind: FrontEnd,
) -> Result<(ruff_python_ast::ModModule, preprocess::PreprocessResult), String> {
    let prep = preprocess::expand_and_preprocess_mapped(source, false);
    let parsed = tyc_syntax::parse_module(&prep.python_source).map_err(|e| e.to_string())?;
    let mut module = parsed.into_syntax();
    if kind == FrontEnd::Shim {
        let plain_class_lines: Vec<usize> = prep
            .python_source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.trim_start().starts_with("class "))
            .map(|(i, _)| i)
            .collect();
        let desugar_out = tyc_desugar::desugar_module_with(
            &module,
            tyc_desugar::DesugarOptions {
                plain_class_line_starts: preprocess::line_byte_starts(
                    &prep.python_source,
                    &plain_class_lines,
                ),
                ..Default::default()
            },
        );
        return Ok((desugar_out.module, prep));
    }

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
    // injected runtime imports, and so on. The preprocessor's class-kind
    // markers (plain / raw / frozen) are threaded through so `plain class`
    // / `class!` / `class … frozen` desugar exactly like `tyc build` —
    // otherwise a `plain class` would be wrongly decorated as a
    // `@dataclass` and its class-level constants treated as slots.
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
    Ok((desugar_out.module, prep))
}

/// Pre-scan sibling `.ty` files referenced by the entry module's imports,
/// extract their builtin extension registries, and merge them into `registry`.
/// Returns a map of `fn_name → sibling_module_stem` for functions that were
/// added from cross-module sources, so the caller can inject explicit
/// imports, plus the declared field / return types of the names those
/// imports bind, so the call-site rewrite types `post.title` on an
/// imported `Post`, `make()` on an imported function, or
/// `textutil.make()` on an imported module.
pub(crate) fn merge_cross_module_extensions_for_vm(
    module: &ruff_python_ast::ModModule,
    src_root: &Path,
    importer_dir: &Path,
    registry: &mut tyc_analyse::ExtensionRegistry,
) -> (
    std::collections::HashMap<String, String>,
    tyc_analyse::TypeFacts,
) {
    use ruff_python_ast::Stmt;
    use std::collections::HashMap;

    let mut cross_fns: HashMap<String, String> = HashMap::new();
    let mut external = tyc_analyse::TypeFacts::default();
    // Each sibling is read once; its facts are re-keyed per import.
    let mut loaded: HashMap<String, Option<tyc_analyse::TypeFacts>> = HashMap::new();
    // Resolve an import to the sibling file it names and that file's
    // absolute dotted module name (the name the injected
    // `from <module> import __typhon_ext_…` must use). `a.b.c` is
    // `a/b/c.ty` or the package `a/b/c/__init__.ty`; a relative
    // `from .text import …` / `from ..kinds import …` climbs from the
    // importing module's directory. (Dotted and relative modules were
    // skipped outright before the 2026-09-30 review, so an extension
    // declared in `catalogue/text.ty` was lowered by `tyc build` but
    // raised `AttributeError` under `tyc run`.)
    let resolve = |name: &str, level: u32| -> Option<(std::path::PathBuf, String)> {
        let rel = name.replace('.', "/");
        let base = if level == 0 {
            src_root.to_path_buf()
        } else {
            let mut dir = importer_dir.to_path_buf();
            for _ in 1..level {
                dir = dir.parent()?.to_path_buf();
            }
            dir
        };
        let candidates: Vec<std::path::PathBuf> = if rel.is_empty() {
            vec![base.join("__init__.ty")]
        } else {
            vec![
                base.join(format!("{rel}.ty")),
                base.join(&rel).join("__init__.ty"),
            ]
        };
        let path = candidates.into_iter().find(|p| p.is_file())?;
        let dotted = path
            .strip_prefix(src_root)
            .ok()?
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let dotted = dotted
            .strip_suffix("/__init__")
            .unwrap_or(&dotted)
            .replace('/', ".");
        if dotted.is_empty() {
            return None;
        }
        Some((path, dotted))
    };
    let mut load = |name: &str,
                    level: u32,
                    registry: &mut tyc_analyse::ExtensionRegistry,
                    cross_fns: &mut HashMap<String, String>|
     -> Option<tyc_analyse::TypeFacts> {
        let (path, dotted) = resolve(name, level)?;
        if let Some(cached) = loaded.get(&dotted) {
            return cached.clone();
        }
        let facts = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| merge_sibling_extensions(&text, &dotted, registry, cross_fns));
        loaded.insert(dotted, facts.clone());
        facts
    };
    let publish_lifted = |external: &mut tyc_analyse::TypeFacts, facts: &tyc_analyse::TypeFacts| {
        // The lifted functions' return types, so a chained cross-module
        // call (`t.slug().shout()`) types.
        for (fn_name, ty) in &facts.functions {
            if fn_name.starts_with("__typhon_ext_") {
                external.functions.insert(fn_name.clone(), ty.clone());
            }
        }
    };
    for stmt in &module.body {
        match stmt {
            Stmt::ImportFrom(i) => {
                let name = i.module.as_ref().map(|m| m.id.to_string());
                let name = match (name, i.level) {
                    (Some(n), _) => n,
                    (None, level) if level > 0 => String::new(),
                    (None, _) => continue,
                };
                // `from . import text` / `from .. import kinds`: each alias
                // is first and foremost a *submodule* of that package, and
                // its own extensions and facts are the ones the importing
                // module can reach through it. Only a name the package does
                // not have as a submodule is looked up on the package
                // facade (`from . import Thing` re-exported by a `pub *`).
                if name.is_empty() {
                    for alias in &i.names {
                        let imported = alias.name.as_str();
                        let local = alias
                            .asname
                            .as_ref()
                            .map(|a| a.as_str())
                            .unwrap_or(imported);
                        if let Some(sub) = load(imported, i.level, registry, &mut cross_fns) {
                            publish_lifted(&mut external, &sub);
                            external.modules.insert(local.to_owned(), sub);
                        } else if let Some(pkg) = load("", i.level, registry, &mut cross_fns) {
                            publish_lifted(&mut external, &pkg);
                            external.import_name(&pkg, imported, local);
                        }
                    }
                    continue;
                }
                let Some(facts) = load(&name, i.level, registry, &mut cross_fns) else {
                    continue;
                };
                publish_lifted(&mut external, &facts);
                for alias in &i.names {
                    let imported = alias.name.as_str();
                    let local = alias
                        .asname
                        .as_ref()
                        .map(|a| a.as_str())
                        .unwrap_or(imported);
                    if imported == "*" {
                        external.merge(tyc_analyse::TypeFacts {
                            functions: facts.functions.clone(),
                            async_functions: facts.async_functions.clone(),
                            classes: facts.classes.clone(),
                            modules: HashMap::new(),
                        });
                    } else {
                        external.import_name(&facts, imported, local);
                    }
                }
            }
            Stmt::Import(i) => {
                // Handle all aliases in `import a, b, c` — not just the first.
                for alias in &i.names {
                    let name = alias.name.id.to_string();
                    let Some(facts) = load(&name, 0, registry, &mut cross_fns) else {
                        continue;
                    };
                    publish_lifted(&mut external, &facts);
                    let local = alias
                        .asname
                        .as_ref()
                        .map(|a| a.as_str().to_owned())
                        .unwrap_or_else(|| name.clone());
                    external.modules.insert(local, facts);
                }
            }
            _ => {}
        }
    }
    (cross_fns, external)
}

/// Parse a sibling `.ty` source just enough to extract builtin extension
/// sentinel classes and merge their methods into `registry`. Returns the
/// sibling's declared field / return types (`None` when it does not
/// parse), with the lifted extension functions included.
fn merge_sibling_extensions(
    source: &str,
    module_name: &str,
    registry: &mut tyc_analyse::ExtensionRegistry,
    cross_fns: &mut std::collections::HashMap<String, String>,
) -> Option<tyc_analyse::TypeFacts> {
    let prep = preprocess::expand_and_preprocess_mapped(source, false);
    let parsed = tyc_syntax::parse_module(&prep.python_source).ok()?;
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
    Some(tyc_analyse::collect_module_type_facts(&sibling_module))
}

/// Inject `from <module> import <fn_name>` AST nodes into `module` for
/// cross-module extension functions. This allows the VM to resolve the
/// lifted free functions during execution.
pub(crate) fn inject_vm_cross_module_ext_imports(
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

/// Flush the `io` shim's still-open file objects (a no-op when the program
/// never opened a file).
fn flush_open_files(interp: &mut Interpreter) {
    let Some(io) = interp.module_cache.get("__builtin__:io").cloned() else {
        return;
    };
    if let Ok(flush) = interp.get_attr(&io, "_flush_all") {
        let _ = interp.call_value(flush, vec![], &[]);
    }
}

/// True when the in-process VM can serve `name` without CPython.
///
/// `tyc run` uses this to stay a drop-in: a program importing anything
/// outside the modelled set takes the compiled path instead of dying with
/// `ModuleNotFoundError` on code `tyc build` runs fine.
pub fn models_module(name: &str) -> bool {
    crate::builtins::models_module(name)
}

/// The names the VM's model of `module` exports — what `dir(module)` lists
/// there — or `None` when the VM does not model it (an unmodelled root, or
/// a dotted name that is not a module: `math.pi` is a float). `module` may
/// be dotted (`os.path`, `typhon_runtime.tasks`).
///
/// `probe` is an interpreter kept across calls so each module is
/// instantiated once per scan; it has no source root, so a project module
/// never resolves through it. `tyc run`'s pre-run scan uses this to send a
/// program that reaches for an attribute the VM lacks down the compiled
/// path, the way an unmodelled *module* already does — so the table is the
/// live module, never a hand-maintained list.
pub fn modelled_module_exports(
    probe: &mut Interpreter,
    module: &str,
) -> Option<std::collections::BTreeSet<String>> {
    if !models_module(module) {
        return None;
    }
    match probe.import_module(module) {
        Ok(Value::Module(m)) => Some(crate::builtins::module_dir_names(&m)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI runs the interpreter on a dedicated 256 MiB worker stack
    /// (`tyc/src/main.rs`); libtest threads get 2 MiB, which a deep but
    /// *legal* recursion overflows in debug builds long before the VM's own
    /// 1000-frame guard fires — `slot_recursion_fib` used to abort the
    /// whole test binary on aarch64. Run every VM invocation the way the
    /// CLI does so the tests stop depending on the host's stack size.
    ///
    /// Measured native stack per VM call frame (aarch64, 2026-10-03,
    /// depth-1000 recursion on the worker thread): **~99,370 B (97 KiB)
    /// debug, ~7,119 B (7 KiB) release** — a debug build needs ~97 MiB for
    /// the 1000-frame guard alone, so anything near a 2–8 MiB default
    /// thread stack is marginal by an order of magnitude.
    const TEST_WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

    fn run_on_worker_stack<F, T>(f: F) -> T
    where
        F: FnOnce() -> T + Send,
        T: Send,
    {
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .stack_size(TEST_WORKER_STACK_SIZE)
                .spawn_scoped(scope, f)
                .expect("failed to spawn the VM test worker thread");
            match handle.join() {
                Ok(v) => v,
                Err(payload) => std::panic::resume_unwind(payload),
            }
        })
    }

    fn run_capturing(source: &str) -> Result<i32, VmError> {
        run_on_worker_stack(|| run_source(source, None, &[]))
    }

    /// A probe that calls `os.chdir` changes the *process* working directory,
    /// and libtest runs tests concurrently, so two such tests make every
    /// relative-path assertion in the suite nondeterministic. Serialise them.
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn review_nan_identity_in_containers() {
        assert_eq!(
            run_capturing(
                r###"let x: float = float("nan")
let y: float = float("nan")
assert not (x == x)
assert [x] == [x]
assert (x,) == (x,)
assert {"n": x} == {"n": x}
assert x in [x]
assert not (y in [x])
assert not ([x] == [y])
let d: dict[float, int] = {x: 1, y: 2}
assert len(d) == 2
assert d[x] == 1
assert d[y] == 2
"###
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn freeze_passes_immutable_values_through_like_the_runtime() {
        // The emitted runtime's `deep_freeze` returns enum members, dates,
        // timedeltas, timezones and paths unchanged; the VM must too.
        assert_eq!(
            run_capturing(
                r###"import datetime
from pathlib import Path
enum Mode:
    FAST
    SLOW
let d = datetime.date(2026, 1, 2)
let p = Path("a")
freeze let CFG = {"mode": Mode.FAST, "d": d, "dt": datetime.datetime(2026, 1, 2), "td": datetime.timedelta(days=1), "tz": datetime.timezone.utc, "p": p}
assert CFG["mode"] is Mode.FAST
assert CFG["d"] is d
assert CFG["p"] is p
assert CFG["td"] == datetime.timedelta(days=1)
"###
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn review_frozen_metadata_and_instance_identity() {
        assert_eq!(
            run_capturing(
                r###"let key: str = "__typhon_frozen__"
let d: dict[str, int] = {key: 1}
d["x"] = 2
freeze let D: dict[str, int] = d
assert len(D) == 2
assert D[key] == 1
freeze let S: set[str] = {key}
assert key in S
assert key in S.union({"x"})
assert len(S.union({"x"})) == 2
class Box frozen:
    values: list[int]
let b: Box = Box(values=[1])
freeze let B: Box = b
assert B is b
B.values.append(2)
assert B.values == [1, 2]
"###
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn review_question_evaluates_prior_arguments_before_success_and_error() {
        assert_eq!(
            run_capturing(
                r###"let calls: list[str] = []
def first() -> int:
    calls.append("first")
    return 1
def second(fail: bool) -> Result[int, str]:
    calls.append("second")
    return Err("bad") if fail else Ok(2)
def combine(a: int, b: int) -> int:
    return a + b
def use(fail: bool) -> Result[int, str]:
    return Ok(combine(
        first(),
        second(fail)?,
    ))
let result = use(False)
assert calls == ["first", "second"]
assert result.value == 3
calls.clear()
let failed = use(True)
assert calls == ["first", "second"]
assert failed.error == "bad"
"###
            )
            .unwrap(),
            0
        );
    }

    /// The declared list and the resolver must agree: a name in the list
    /// that no longer resolves would send `tyc run` into the VM for a
    /// module it cannot serve, and one the resolver serves but the list
    /// omits would send it to CPython for no reason.
    #[test]
    fn vm_modelled_modules_all_resolve() {
        for name in crate::builtins::MODELLED_MODULE_ROOTS {
            let src = format!("import {name}\n");
            assert_eq!(
                run_capturing(&src).unwrap_or(1),
                0,
                "`import {name}` must resolve in the VM — it is in MODELLED_MODULE_ROOTS"
            );
        }
        for absent in [
            "sqlite3",
            "subprocess",
            "threading",
            "decimal",
            "yaml",
            "numpy",
        ] {
            assert!(
                !models_module(absent),
                "`{absent}` is not modelled, so `tyc run` must take the compiled path"
            );
        }
        assert!(models_module("os.path"), "a submodule follows its root");
        assert!(models_module("collections.abc"));
    }

    // ── VM ↔ CPython parity: the 2026-09-30 release-readiness review, §6 ──
    //
    // Each test is a program that raises on the first value that differs
    // from what CPython 3.13 prints for the same source (the expected values
    // were taken from the hosting python3.13), so a green run means the VM
    // and the compiled path agree.

    /// A `.ty` program plus the sibling files it imports, run from a temp dir.
    fn run_project(files: &[(&str, &str)], entry: &str) -> Result<i32, VmError> {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        run_on_worker_stack(|| run_file(&dir.path().join(entry), &[]))
    }

    const CHECK: &str = r#"
def check(label: str, got: object, want: object) -> None:
    if got != want:
        raise ValueError(f"{label}: got {got!r}, want {want!r}")
"#;

    #[test]
    fn dotted_import_binds_each_submodule_on_its_parent() {
        // `import a.b.c` binds `a` and sets `a.b` / `a.b.c`, as CPython's
        // import machinery does; `import a.b as x` and `from a import b`
        // hand back the very same module objects.
        let code = run_project(
            &[
                ("shapes/__init__.ty", "pub *\n"),
                ("shapes/kinds.ty", "pub class Circle:\n    radius: float\n"),
                (
                    "shapes/ops.ty",
                    "from .kinds import Circle\n\npub def area(c: Circle) -> float:\n    return 3.0 * c.radius * c.radius\n",
                ),
                ("ns/inner.ty", "pub let VALUE: int = 7\n"),
                (
                    "main.ty",
                    r#"
import shapes.ops
import shapes.kinds
import shapes.ops as ops_alias
from shapes import ops as ops_from
import os.path
import typhon_runtime.tasks
import ns.inner

c = shapes.kinds.Circle(radius=2.0)
if shapes.ops.area(c) != 12.0:
    raise ValueError("dotted import did not bind the submodule on its package")
if shapes.ops.__name__ != "shapes.ops" or shapes.kinds.__name__ != "shapes.kinds":
    raise ValueError("submodule names")
if ops_alias is not shapes.ops or ops_from is not shapes.ops:
    raise ValueError("one module object per submodule")
if os.path.join("a", "b") != "a/b":
    raise ValueError("os.path through the dotted import")
if typhon_runtime.tasks.spawn is None:
    raise ValueError("typhon_runtime.tasks")
if ns.inner.VALUE != 7 or ns.__name__ != "ns":
    raise ValueError("a namespace package (no __init__.ty) imports too")
"#,
                ),
            ],
            "main.ty",
        );
        assert_eq!(code.unwrap(), 0);
    }

    #[test]
    fn map_over_several_iterables_stops_at_the_shortest() {
        let src = format!(
            "{CHECK}
check(\"two\", list(map(lambda a, b: a + b, [1, 2], [10, 20, 30])), [11, 22])
check(\"pow\", list(map(pow, [2, 3], [3, 2])), [8, 9])
check(\"three\", list(map(max, [1, 9, 3], [4, 5, 6], [7, 0, 8])), [7, 9, 8])
check(\"one\", list(map(lambda x: x * 2, [1, 2, 3])), [2, 4, 6])
check(\"mixed\", list(map(lambda a, b, c: (a, b, c), \"ab\", [1, 2, 3], (True,))), [(\"a\", 1, True)])
check(\"type\", type(map(str, [1])).__name__, \"map\")
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn await_rejects_a_non_awaitable_like_cpython() {
        // `await` on anything but a coroutine, a task or an awaitable native
        // result is CPython's TypeError, spelled with the operand's type; the
        // VM used to hand the value back. Everything the VM does model as
        // awaitable — including a native result stored before it is awaited
        // — keeps working.
        let src = format!(
            "{CHECK}
import asyncio
from contextlib import asynccontextmanager, AsyncExitStack

def sync_function() -> int:
    return 3

class Box:
    v: int

async def main() -> None:
    seen: list[str] = []
    for value in [1, \"x\", None, [1], Box(v=1), sync_function()]:
        try:
            await value
        except TypeError as e:
            seen.append(str(e))
        else:
            raise ValueError(\"await of a non-awaitable must raise\")
    check(\"messages\", seen, [
        \"object int can't be used in 'await' expression\",
        \"object str can't be used in 'await' expression\",
        \"object NoneType can't be used in 'await' expression\",
        \"object list can't be used in 'await' expression\",
        \"object Box can't be used in 'await' expression\",
        \"object int can't be used in 'await' expression\",
    ])
    check(\"sleep\", await asyncio.sleep(0), None)
    stored = asyncio.sleep(0)
    check(\"stored sleep\", await stored, None)
    check(\"gather\", await asyncio.gather(asyncio.sleep(0), asyncio.sleep(0)), [None, None])
    check(\"gather kw\", await asyncio.gather(asyncio.sleep(0), return_exceptions=True), [None])
    check(\"wait_for\", await asyncio.wait_for(asyncio.sleep(0), 1.0), None)
    check(\"to_thread\", await asyncio.to_thread(sync_function), 3)
    t = asyncio.create_task(asyncio.sleep(0))
    check(\"task\", await t, None)
    q: asyncio.Queue[int] = asyncio.Queue()
    await q.put(5)
    check(\"queue\", await q.get(), 5)
    ev = asyncio.Event()
    ev.set()
    check(\"event\", await ev.wait(), True)
    lock = asyncio.Lock()
    check(\"acquire\", await lock.acquire(), True)
    lock.release()
    async with lock:
        check(\"locked\", lock.locked(), True)
    async with asyncio.TaskGroup() as tg:
        tg.create_task(asyncio.sleep(0))
    async with asyncio.timeout(5):
        await asyncio.sleep(0)

    @asynccontextmanager
    async def session():
        yield \"open\"

    async with session() as s:
        check(\"acm\", s, \"open\")
    async with AsyncExitStack() as stack:
        check(\"exit stack\", await stack.enter_async_context(session()), \"open\")
        check(\"lock via stack\", await stack.enter_async_context(lock), None)

asyncio.run(main())
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn go_outside_a_running_loop_raises_like_cpython() {
        // `go f()` lowers to `typhon_runtime.tasks.spawn`, which is
        // `asyncio.create_task` under CPython: outside `asyncio.run` it
        // raises `RuntimeError: no running event loop` and the coroutine
        // never runs. Inside the loop — from a coroutine or from a sync
        // helper it calls — it spawns as before.
        let src = r#"
import asyncio

ran = 0

async def f() -> None:
    global ran
    ran += 1

def kick() -> None:
    go f()

def main() -> None:
    try:
        kick()
    except RuntimeError as e:
        if str(e) != "no running event loop":
            raise ValueError(f"wrong message: {e}")
    else:
        raise ValueError("go outside a running loop must raise")

main()
if ran != 0:
    raise ValueError("the coroutine must not run when spawn fails")

try:
    asyncio.create_task(f())
except RuntimeError as e:
    if str(e) != "no running event loop":
        raise ValueError(f"create_task: {e}")
else:
    raise ValueError("create_task outside a running loop must raise")

async def inner() -> None:
    go f()

def helper() -> None:
    go f()

async def inner2() -> None:
    helper()
    go f() -> task
    await task

async def nested() -> None:
    try:
        asyncio.run(f())
    except RuntimeError as e:
        if str(e) != "asyncio.run() cannot be called from a running event loop":
            raise ValueError(f"nested: {e}")
    else:
        raise ValueError("nested asyncio.run must raise")

asyncio.run(inner())
asyncio.run(inner2())
asyncio.run(nested())
if ran != 3:
    raise ValueError(f"expected 3 spawns inside the loop, got {ran}")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Evaluate each expression with the hosting `python3.13` and return the
    /// `repr` of each result, in order — `None` when no python3.13 is on
    /// PATH (the caller then skips, or panics when TYC_REQUIRE_PYTHON is
    /// set: the same loud-or-skip convention as the pipeline tests).
    fn python313_repr_oracle(exprs: &[&str]) -> Option<Vec<String>> {
        let out = std::process::Command::new("python3.13")
            .arg("-c")
            .arg("import math, sys\nfor e in sys.argv[1:]:\n    print(repr(eval(e)))")
            .args(exprs)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        (lines.len() == exprs.len()).then_some(lines)
    }

    /// Run a probe (Typhon source, ending in an accumulator join) through
    /// the hosting `python3.13` and return its stdout. Only the two surface
    /// rewrites the VM itself applies are needed to make the probe valid
    /// Python: `mut x = …` -> `x = …`, and `unsafe:` -> `if True:`.
    ///
    /// Whole-transcript tests used to bury one platform's output in a
    /// literal, which pinned host facts — errno numbers, the OSError
    /// subclass, the tempdir path, CPython's locale-encoding spelling — as
    /// if they were VM behaviour, and failed on every other host.
    fn python313_transcript(probe: &str, accumulator: &str) -> Option<String> {
        let py = format!(
            "{}\nprint(chr(10).join({}))",
            probe.replace("mut ", "").replace("unsafe:", "if True:"),
            accumulator
        );
        let out = std::process::Command::new("python3.13")
            .arg("-c")
            .arg(&py)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let mut text = String::from_utf8(out.stdout).ok()?;
        if text.ends_with('\n') {
            text.pop();
        }
        Some(text)
    }

    #[test]
    fn math_gains_the_missing_names_value_for_value() {
        // libm-backed results can differ by an ulp between platforms (glibc
        // vs Apple libm) and the VM deliberately calls the *host* libm, so
        // pinning one platform's literals failed on macOS while the VM was
        // bit-correct against the Mac's own CPython. The expectations are
        // computed from the hosting python3.13 at test time instead.
        let cases: &[(&str, &str)] = &[
            ("gamma(0.5)", "math.gamma(0.5)"),
            ("gamma(5)", "math.gamma(5)"),
            ("gamma(-2.5)", "math.gamma(-2.5)"),
            ("gamma(170.5)", "math.gamma(170.5)"),
            ("gamma(1e-5)", "math.gamma(1e-5)"),
            ("gamma(inf)", "math.gamma(math.inf)"),
            ("lgamma(3)", "math.lgamma(3)"),
            ("lgamma(0.5)", "math.lgamma(0.5)"),
            ("lgamma(-2.5)", "math.lgamma(-2.5)"),
            ("lgamma(1e5)", "math.lgamma(1e5)"),
            ("lgamma(1e-5)", "math.lgamma(1e-5)"),
            ("lgamma(-inf)", "math.lgamma(-math.inf)"),
            ("erf(0.5)", "math.erf(0.5)"),
            ("erfc(0.5)", "math.erfc(0.5)"),
            ("erf(-2)", "math.erf(-2)"),
            ("erfc(3.5)", "math.erfc(3.5)"),
            ("sinh", "math.sinh(1.5)"),
            ("cosh", "math.cosh(1.5)"),
            ("tanh", "math.tanh(1.5)"),
            ("asinh", "math.asinh(1.5)"),
            ("acosh", "math.acosh(1.5)"),
            ("atanh", "math.atanh(0.5)"),
            ("cbrt(27)", "math.cbrt(27)"),
            ("cbrt(-8.0)", "math.cbrt(-8.0)"),
            ("exp2(10)", "math.exp2(10)"),
            ("exp2(0.5)", "math.exp2(0.5)"),
            ("fma", "math.fma(2, 3, 4)"),
            ("fma small", "math.fma(0.1, 10, -1)"),
            ("frexp", "[math.frexp(8.0), math.frexp(-3.5), math.frexp(5e-324), math.frexp(1e-310), math.frexp(0.0), math.frexp(math.inf)]"),
            ("ldexp", "[math.ldexp(1.5, -1074), math.ldexp(1e-300, 1100), math.ldexp(3, 4), math.ldexp(True, 1), math.ldexp(1, -2**70), math.ldexp(0.0, 2**70)]"),
            ("modf", "[math.modf(3.75), math.modf(-3.75), math.modf(math.inf), math.modf(-math.inf)]"),
            ("modf(-0.0)", "math.modf(-0.0)"),
            ("nextafter", "[math.nextafter(1.0, 2.0), math.nextafter(1.0, 0.0), math.nextafter(0.0, 1.0), math.nextafter(0.0, -1.0), math.nextafter(1, 2, steps=3), math.nextafter(1, 0, steps=2**80), math.nextafter(-0.0, 0.0), math.nextafter(1, 2, steps=0)]"),
            ("ulp", "[math.ulp(1.0), math.ulp(0.0), math.ulp(-2.5), math.ulp(1.7976931348623157e308), math.ulp(3)]"),
            ("isclose", "[math.isclose(1.0, 1.0000000001), math.isclose(1, 1.1), math.isclose(1, 1.1, rel_tol=0.2), math.isclose(0.0, 1e-10, abs_tol=1e-9), math.isclose(math.inf, math.inf), math.isclose(math.nan, math.nan), math.isclose(math.inf, 1e308), math.isclose(3, 3)]"),
            ("sumprod", "[math.sumprod([1, 2, 3], [4, 5, 6]), math.sumprod([1.5, 2], [2, 3.5]), math.sumprod([0.1] * 10, [0.1] * 10), math.sumprod([2**70, 1], [1, 2.5]), math.sumprod([True], [1.5]), math.sumprod((1, 2), [3.0, 4.0]), math.sumprod([], []), math.sumprod([1e16, 1.0, -1e16], [1.0, 1.0, 1.0])]"),
            ("sumprod overflow", "math.sumprod([1e308, 1e308], [10, -10])"),
        ];
        let exprs: Vec<&str> = cases.iter().map(|(_, e)| *e).collect();
        let Some(wants) = python313_repr_oracle(&exprs) else {
            if std::env::var_os("TYC_REQUIRE_PYTHON").is_some() {
                panic!("python3.13 is required as the math oracle (TYC_REQUIRE_PYTHON is set)");
            }
            eprintln!(
                "skipping math_gains_the_missing_names_value_for_value: no python3.13 on PATH"
            );
            return;
        };
        let mut checks = String::new();
        for ((label, expr), want) in cases.iter().zip(&wants) {
            if label.starts_with("gamma(") || label.starts_with("lgamma(") {
                // `gamma` / `lgamma` are CPython's *own* Lanczos evaluation,
                // not libm calls, and even two CPython builds disagree here:
                // glibc's python3.13 gives gamma(-2.5) = …29417 and
                // lgamma(1e5) = …36566 while Apple libm's gives …29418 and
                // …36569. The VM's port pins the glibc rounding, so a host
                // CPython can legitimately land a few ulp away — compare
                // with a 1e-15 relative tolerance instead of exactly.
                checks.push_str(&format!("check_gamma({label:?}, repr({expr}), {want:?})\n"));
            } else {
                checks.push_str(&format!("check({label:?}, repr({expr}), {want:?})\n"));
            }
        }
        let src = format!(
            "{CHECK}
import math

def check_gamma(label: str, got: str, want: str) -> None:
    a: float = float(got)
    b: float = float(want)
    if a == b or (a != a and b != b):
        return
    let scale: float = max(abs(a), abs(b))
    if scale > 0.0 and abs(a - b) <= 1e-15 * scale:
        return
    raise ValueError(f\"{{label}}: got {{got!r}}, want {{want!r}}\")

{checks}
def expect(label: str, kind: str, message: str, thunk: object) -> None:
    try:
        thunk()
    except Exception as e:
        if type(e).__name__ != kind or str(e) != message:
            raise ValueError(f\"{{label}}: got {{type(e).__name__}}: {{e}}\")
    else:
        raise ValueError(f\"{{label}}: did not raise\")

expect(\"gamma(1000)\", \"OverflowError\", \"math range error\", lambda: math.gamma(1000))
expect(\"gamma(-1)\", \"ValueError\", \"math domain error\", lambda: math.gamma(-1))
expect(\"gamma(0.0)\", \"ValueError\", \"math domain error\", lambda: math.gamma(0.0))
expect(\"gamma(-inf)\", \"ValueError\", \"math domain error\", lambda: math.gamma(-math.inf))
expect(\"gamma(1e-320)\", \"OverflowError\", \"math range error\", lambda: math.gamma(1e-320))
expect(\"lgamma(0)\", \"ValueError\", \"math domain error\", lambda: math.lgamma(0))
expect(\"lgamma(1e308)\", \"OverflowError\", \"math range error\", lambda: math.lgamma(1e308))
expect(\"exp2(2000)\", \"OverflowError\", \"math range error\", lambda: math.exp2(2000))
expect(\"cosh(1000)\", \"OverflowError\", \"math range error\", lambda: math.cosh(1000))
expect(\"acosh(0.5)\", \"ValueError\", \"math domain error\", lambda: math.acosh(0.5))
expect(\"atanh(1)\", \"ValueError\", \"math domain error\", lambda: math.atanh(1))
expect(\"fma overflow\", \"OverflowError\", \"overflow in fma\", lambda: math.fma(1e308, 10, 0))
expect(\"fma invalid\", \"ValueError\", \"invalid operation in fma\", lambda: math.fma(math.inf, 0, 1))
expect(\"ldexp float exp\", \"TypeError\", \"Expected an int as second argument to ldexp.\", lambda: math.ldexp(1, 2.0))
expect(\"ldexp range\", \"OverflowError\", \"math range error\", lambda: math.ldexp(1, 2000))
expect(\"nextafter negative steps\", \"ValueError\", \"steps must be a non-negative integer\", lambda: math.nextafter(1, 2, steps=-1))
expect(\"isclose negative tol\", \"ValueError\", \"tolerances must be non-negative\", lambda: math.isclose(1, 2, rel_tol=-1))
expect(\"isclose positional tol\", \"TypeError\", \"isclose() takes exactly 2 positional arguments (3 given)\", lambda: math.isclose(1, 2, 0.5))
expect(\"sumprod length\", \"ValueError\", \"Inputs are not the same length\", lambda: math.sumprod([1, 2], [1]))
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn re_exports_flag_aliases_classes_and_error() {
        let src = format!(
            "{CHECK}
import re

check(\"aliases\", [re.A, re.I, re.L, re.M, re.S, re.U, re.X, re.NOFLAG, re.DEBUG], [256, 2, 4, 8, 16, 32, 64, 0, 128])
check(\"alias identity\", [re.A == re.ASCII, re.I == re.IGNORECASE, re.L == re.LOCALE, re.M == re.MULTILINE, re.S == re.DOTALL, re.U == re.UNICODE, re.X == re.VERBOSE], [True] * 7)
check(\"RegexFlag\", [re.RegexFlag(2), int(re.I | re.M)], [2, 10])
m = re.match(r\"(\\d+)\", \"12ab\")
p = re.compile(\"x\")
check(\"classes\", [isinstance(m, re.Match), isinstance(p, re.Pattern), type(m) is re.Match, type(p) is re.Pattern, type(m).__name__, type(p).__name__, re.error is re.PatternError], [True, True, True, True, \"Match\", \"Pattern\", True])
try:
    re.compile(\"(\")
except re.error as e:
    check(\"error attrs\", [isinstance(e, Exception), isinstance(e, re.PatternError), e.pattern, e.pos], [True, True, \"(\", None])
else:
    raise ValueError(\"re.compile('(') must raise re.error\")
try:
    re.compile(\"(\")
except Exception as e:
    check(\"error type\", type(e).__name__, \"PatternError\")
check(\"flags\", [re.findall(\"a\", \"AaA\", re.I), re.compile(\"^b\", re.M).findall(\"a\\nb\"), re.compile(\"a.b\", re.S).match(\"a\\nb\") is not None, re.compile(\"a b # c\", re.X).match(\"ab\") is not None, re.sub(\"a\", \"x\", \"AaA\", flags=re.I), re.split(\"A\", \"aAa\", flags=re.I), re.compile(\"A\", re.I).sub(\"x\", \"aAa\", count=1), re.compile(\"a\", flags=re.I).pattern, re.compile(\"a\", flags=re.I).flags], [[\"A\", \"a\", \"A\"], [\"b\"], True, True, \"xxx\", [\"\", \"\", \"\", \"\"], \"xAa\", \"a\", 2])
check(\"pattern methods\", [re.compile(\"b\").fullmatch(\"b\") is not None, re.compile(\"b\").fullmatch(\"bb\") is None, re.compile(\"b\").search(\"abc\", 1).start(), re.compile(\"b\").match(\"abc\", pos=1) is not None, re.compile(\"c\").search(\"abc\", 0, 2) is None, re.compile(r\"(?P<w>\\w)\").groupindex, re.compile(\"(a)(b)\").groups], [True, True, 1, True, True, {{\"w\": 1}}, 2])
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn heapq_gains_heappushpop_heapreplace_and_merge() {
        let src = format!(
            "{CHECK}
import heapq

h = [5, 1, 8]
heapq.heapify(h)
check(\"heappushpop\", [heapq.heappushpop(h, 0), heapq.heappushpop(h, 7), h], [0, 1, [5, 7, 8]])
check(\"heapreplace\", [heapq.heapreplace(h, 2), h], [5, [2, 7, 8]])
check(\"merge\", [list(heapq.merge([1, 4, 7], [2, 3, 9], [0, 8])), list(heapq.merge([9, 4], [7, 3, 1], reverse=True)), list(heapq.merge([\"ab\", \"c\"], [\"b\", \"dd\"], key=len)), list(heapq.merge([(1, \"a\"), (2, \"a\")], [(1, \"b\")], key=lambda t: t[0])), list(heapq.merge())], [[0, 1, 2, 3, 4, 7, 8, 9], [9, 7, 4, 3, 1], [\"b\", \"ab\", \"c\", \"dd\"], [(1, \"a\"), (1, \"b\"), (2, \"a\")], []])
try:
    heapq.heapreplace([], 1)
except IndexError as e:
    check(\"heapreplace empty\", str(e), \"index out of range\")
else:
    raise ValueError(\"heapreplace on an empty heap must raise\")
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn functools_gains_update_wrapper_partialmethod_singledispatchmethod() {
        let src = format!(
            "{CHECK}
import functools
from functools import partialmethod, singledispatchmethod, update_wrapper

check(\"WRAPPER\", [functools.WRAPPER_ASSIGNMENTS, functools.WRAPPER_UPDATES], [(\"__module__\", \"__name__\", \"__qualname__\", \"__doc__\", \"__annotations__\", \"__type_params__\"), (\"__dict__\",)])

def original(x: int) -> int:
    \"\"\"doc of original\"\"\"
    return x + 1

def wrapper(*args: object, **kwargs: object) -> object:
    return original(*args, **kwargs)

update_wrapper(wrapper, original)
check(\"update_wrapper\", [wrapper.__name__, wrapper.__doc__, wrapper.__wrapped__ is original, wrapper(1)], [\"original\", \"doc of original\", True, 2])

plain class Cell:
    def __init__(self, base: int) -> None:
        self.base = base

    def scaled(self, factor: int, offset: int = 0) -> int:
        return self.base * factor + offset

    double = partialmethod(scaled, 2)
    shifted = partialmethod(scaled, 1, offset=10)

c = Cell(5)
check(\"partialmethod\", [c.double(), c.shifted(), c.double(offset=1), Cell.double(c)], [10, 15, 11, 10])

plain class Negotiator:
    @singledispatchmethod
    def handle(self, arg: object) -> str:
        return \"object\"

    @handle.register(int)
    def _(self, arg: int) -> str:
        return f\"int {{arg}}\"

    @handle.register(str)
    def _(self, arg: str) -> str:
        return f\"str {{arg}}\"

n = Negotiator()
check(\"singledispatchmethod\", [n.handle(1), n.handle(\"a\"), n.handle(1.5)], [\"int 1\", \"str a\", \"object\"])
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn contextlib_gains_abstract_bases_decorators_chdir_aclosing_and_async_exit_stack() {
        let src = format!(
            "{CHECK}
import os
import asyncio
from contextlib import AbstractContextManager, AbstractAsyncContextManager, ContextDecorator, AsyncContextDecorator, chdir, aclosing, AsyncExitStack, contextmanager

events: list[str] = []
log: list[str] = []
order: list[str] = []

plain class Tracker(ContextDecorator):
    def __enter__(self) -> object:
        events.append(\"enter\")
        return self

    def __exit__(self, *exc: object) -> bool:
        events.append(\"exit\")
        return False

@Tracker()
def work() -> int:
    events.append(\"work\")
    return 42

check(\"ContextDecorator\", [work(), events], [42, [\"enter\", \"work\", \"exit\"]])

plain class Managed(AbstractContextManager):
    pass

with Managed() as m:
    check(\"AbstractContextManager\", isinstance(m, Managed), True)

here = os.getcwd()
with chdir(\"/\"):
    check(\"chdir inside\", os.getcwd(), \"/\")
check(\"chdir restored\", os.getcwd(), here)

plain class Closer:
    def __init__(self) -> None:
        self.closed = False

    async def aclose(self) -> None:
        self.closed = True

plain class ATracker(AsyncContextDecorator):
    async def __aenter__(self) -> object:
        log.append(\"aenter\")
        return self

    async def __aexit__(self, *exc: object) -> bool:
        log.append(\"aexit\")
        return False

plain class AManaged(AbstractAsyncContextManager):
    pass

@ATracker()
async def awork() -> int:
    log.append(\"awork\")
    return 7

@contextmanager
def sync_cm(tag: str):
    order.append(f\"+{{tag}}\")
    yield tag
    order.append(f\"-{{tag}}\")

async def acallback(tag: str) -> None:
    order.append(f\"cb {{tag}}\")

async def main() -> None:
    closer = Closer()
    async with aclosing(closer) as c:
        check(\"aclosing yields\", c is closer, True)
    check(\"aclosing closed\", closer.closed, True)
    check(\"AsyncContextDecorator\", [await awork(), log], [7, [\"aenter\", \"awork\", \"aexit\"]])
    async with AManaged() as am:
        check(\"AbstractAsyncContextManager\", isinstance(am, AManaged), True)
    log.clear()
    async with AsyncExitStack() as stack:
        a = stack.enter_context(sync_cm(\"a\"))
        b = await stack.enter_async_context(ATracker())
        stack.push_async_callback(acallback, \"z\")
        stack.callback(order.append, \"sync cb\")
        check(\"stack values\", [a, isinstance(b, ATracker)], [\"a\", True])
    check(\"stack order\", order, [\"+a\", \"sync cb\", \"cb z\", \"-a\"])
    check(\"stack async exit\", log, [\"aenter\", \"aexit\"])

asyncio.run(main())
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn operator_gains_in_place_ops_index_inv_call() {
        let src = format!(
            "{CHECK}
import operator

xs = [1]
check(\"iadd list\", [operator.iadd(xs, [2]), xs], [[1, 2], [1, 2]])
check(\"iadd int\", operator.iadd(1, 2), 3)
check(\"in-place\", [operator.isub(5, 2), operator.imul(3, 4), operator.itruediv(7, 2), operator.ifloordiv(7, 2), operator.imod(7, 3), operator.ipow(2, 5), operator.ilshift(1, 3), operator.irshift(16, 2), operator.iand(6, 3), operator.ior(4, 1), operator.ixor(6, 3), operator.iconcat([1], [2])], [3, 12, 3.5, 3, 1, 32, 8, 4, 2, 5, 5, [1, 2]])
check(\"index/inv/call\", [operator.index(5), operator.index(True), operator.inv(5), operator.call(max, 1, 2, 3), operator.call(str.upper, \"a\")], [5, True, -6, 3, \"A\"])
try:
    operator.iconcat(1, 2)
except TypeError as e:
    check(\"iconcat error\", str(e), \"'int' object can't be concatenated\")
else:
    raise ValueError(\"iconcat on ints must raise\")
try:
    operator.index(1.5)
except TypeError as e:
    check(\"index error\", str(e), \"'float' object cannot be interpreted as an integer\")
else:
    raise ValueError(\"index on a float must raise\")
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn collections_gains_user_dict_list_and_string() {
        let src = format!(
            "{CHECK}
from collections import UserDict, UserList, UserString

plain class Registry(UserDict):
    def __missing__(self, key: str) -> str:
        return f\"<{{key}}>\"

r = Registry({{\"a\": 1}}, b=2)
r[\"c\"] = 3
check(\"UserDict\", [len(r), r[\"a\"], r[\"zzz\"], sorted(r.keys()), \"b\" in r, r.get(\"nope\"), dict(r.items()) == {{\"a\": 1, \"b\": 2, \"c\": 3}}, repr(UserDict({{\"x\": 1}}))], [3, 1, \"<zzz>\", [\"a\", \"b\", \"c\"], True, None, True, \"{{'x': 1}}\"])
del r[\"a\"]
check(\"UserDict del\", sorted(r), [\"b\", \"c\"])
check(\"UserDict or\", dict(UserDict({{\"a\": 1}}) | {{\"b\": 2}}), {{\"a\": 1, \"b\": 2}})
check(\"UserDict fromkeys\", dict(UserDict.fromkeys([\"x\", \"y\"], 0)), {{\"x\": 0, \"y\": 0}})

ul = UserList([3, 1, 2])
ul.append(0)
ul.sort()
check(\"UserList\", [list(ul), len(ul), ul[0], list(ul[1:]), isinstance(ul[1:], UserList), ul == [0, 1, 2, 3], list(ul + [9]), list(ul * 2)[:5], 2 in ul, ul.index(3), repr(ul)], [[0, 1, 2, 3], 4, 0, [1, 2, 3], True, True, [0, 1, 2, 3, 9], [0, 1, 2, 3, 0], True, 3, \"[0, 1, 2, 3]\"])
ul += [7]
ul.extend(UserList([8]))
check(\"UserList mutation\", [list(ul), ul.pop(), list(ul)], [[0, 1, 2, 3, 7, 8], 8, [0, 1, 2, 3, 7]])

us = UserString(\"Hello\")
check(\"UserString\", [str(us), len(us), us.upper().data, isinstance(us.upper(), UserString), (us + \"!\").data, us == \"Hello\", us < \"Zebra\", \"ell\" in us, us[1:3].data, us.replace(\"l\", \"L\").data, us.split(\"l\"), us.find(\"l\"), repr(us), int(UserString(\"42\")), us * 2 == \"HelloHello\", us.startswith(\"He\"), us.isalpha()], [\"Hello\", 5, \"HELLO\", True, \"Hello!\", True, True, True, \"el\", \"HeLLo\", [\"He\", \"\", \"o\"], 2, \"'Hello'\", 42, True, True, True])
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn hashlib_gains_sha3_pbkdf2_hmac_and_file_digest() {
        let src = format!(
            "{CHECK}
import hashlib
import io

check(\"sha3\", [hashlib.sha3_224(b\"abc\").hexdigest(), hashlib.sha3_256(b\"\").hexdigest(), hashlib.sha3_384(b\"abc\").hexdigest(), hashlib.sha3_512(b\"abc\").hexdigest(), hashlib.sha3_256(b\"a\" * 200).hexdigest(), hashlib.sha3_256(b\"b\" * 136).hexdigest(), hashlib.sha3_256(b\"b\" * 135).hexdigest(), hashlib.new(\"sha3_256\", b\"\").hexdigest()], [\"e642824c3f8cf24ad09234ee7d3c766fc9a3a5168d0c94ad73b46fdf\", \"a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a\", \"ec01498288516fc926459f58e2c6ad8df9b473cb0fc08c2596da7cf0e49be4b298d88cea927ac7f539f1edf228376d25\", \"b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0\", \"cce34485baf2bf2aca99b94833892a4f52896d3d153f7b840cc4f9fe695f1387\", \"491d43679ebf9eeb191b33432034caebed97df8be9125a6db9b133c7ce660ca7\", \"bbcfe2803f2108ca1d38b9f9f5499cec8535096c9156e895817b60f407e229f4\", \"a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a\"])
check(\"sha3 sizes\", [(h().digest_size, h().block_size, h().name) for h in (hashlib.sha3_224, hashlib.sha3_256, hashlib.sha3_384, hashlib.sha3_512)], [(28, 144, \"sha3_224\"), (32, 136, \"sha3_256\"), (48, 104, \"sha3_384\"), (64, 72, \"sha3_512\")])
check(\"pbkdf2\", [hashlib.pbkdf2_hmac(\"sha256\", b\"password\", b\"salt\", 1000).hex(), hashlib.pbkdf2_hmac(\"sha1\", b\"password\", b\"salt\", 2, 40).hex(), hashlib.pbkdf2_hmac(\"sha512\", b\"pw\", b\"na\", 1).hex(), hashlib.pbkdf2_hmac(\"sha3_256\", b\"pw\", b\"na\", 3, dklen=20).hex()], [\"632c2812e46d4604102ba7618e9d6d7d2f8128f6266b4a03264d2a0460b7dcb3\", \"ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957cae93136266537a8d7bf4b76c51094cc1ae010b1\", \"d1271dbcae53c2502c92308b2f4b75bc5f0cd4ff05ed918cb94fca074dbddeef9b4c1b493bb04f20ee8cce9497701efd5d5d486614a851c61192b516f918fac7\", \"133280d8cb9acf41c441f241844430bd991aac03\"])
check(\"file_digest\", [hashlib.file_digest(io.BytesIO(b\"hello world\"), \"sha256\").hexdigest(), hashlib.file_digest(io.BytesIO(b\"hello world\"), hashlib.sha256).hexdigest()], [\"b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9\", \"b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9\"])

def expect(label: str, kind: str, message: str, thunk: object) -> None:
    try:
        thunk()
    except Exception as e:
        if type(e).__name__ != kind or str(e) != message:
            raise ValueError(f\"{{label}}: got {{type(e).__name__}}: {{e}}\")
    else:
        raise ValueError(f\"{{label}}: did not raise\")

expect(\"0 iterations\", \"ValueError\", \"iteration value must be greater than 0.\", lambda: hashlib.pbkdf2_hmac(\"sha256\", b\"p\", b\"s\", 0))
expect(\"dklen 0\", \"ValueError\", \"key length must be greater than 0.\", lambda: hashlib.pbkdf2_hmac(\"sha256\", b\"p\", b\"s\", 1, 0))
expect(\"str password\", \"TypeError\", \"a bytes-like object is required, not 'str'\", lambda: hashlib.pbkdf2_hmac(\"sha256\", \"p\", b\"s\", 1))
expect(\"float iterations\", \"TypeError\", \"'float' object cannot be interpreted as an integer\", lambda: hashlib.pbkdf2_hmac(\"sha256\", b\"p\", b\"s\", 1.5))
expect(\"unknown digest\", \"ValueError\", \"unsupported hash type nope\", lambda: hashlib.pbkdf2_hmac(\"nope\", b\"p\", b\"s\", 1))
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    #[test]
    fn descriptor_get_is_honoured_on_class_attributes() {
        // A descriptor object stored as a class attribute is read through
        // its `__get__`, on the instance and on the class — the protocol
        // `partialmethod` / `singledispatchmethod` rest on.
        let src = format!(
            "{CHECK}
plain class Desc:
    def __init__(self, tag: str) -> None:
        self.tag = tag

    def __get__(self, obj: object, owner: object = None) -> str:
        return f\"{{self.tag}}:{{obj is None}}:{{owner.__name__}}\"

plain class Host:
    a = Desc(\"assigned\")

check(\"descriptor\", [Host().a, Host.a], [\"assigned:False:Host\", \"assigned:True:Host\"])
"
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
    }

    /// The pre-run scan asks the VM what each modelled module exports; the
    /// answer is the live module's `dir()`, never a hand-written table.
    #[test]
    fn modelled_module_exports_lists_a_modules_live_names() {
        let mut probe = Interpreter::new();
        let math = modelled_module_exports(&mut probe, "math").expect("math is modelled");
        for name in ["isclose", "nextafter", "sumprod", "gamma", "pi"] {
            assert!(math.contains(name), "math.{name}");
        }
        let os_path = modelled_module_exports(&mut probe, "os.path").expect("os.path is modelled");
        assert!(os_path.contains("join"));
        let sys = modelled_module_exports(&mut probe, "sys").expect("sys is modelled");
        assert!(sys.contains("modules") && sys.contains("argv"));
        let tasks = modelled_module_exports(&mut probe, "typhon_runtime.tasks")
            .expect("typhon_runtime.tasks is modelled");
        assert!(tasks.contains("spawn"));
        let re = modelled_module_exports(&mut probe, "re").expect("re is modelled");
        assert!(re.contains("I") && re.contains("Match") && !re.contains("purge"));
        assert_eq!(modelled_module_exports(&mut probe, "sqlite3"), None);
        assert_eq!(
            modelled_module_exports(&mut probe, "math.pi"),
            None,
            "a non-module member is not a module"
        );
        for name in crate::builtins::MODELLED_MODULE_ROOTS {
            assert!(
                modelled_module_exports(&mut probe, name).is_some_and(|names| !names.is_empty()),
                "`{name}` must answer with its names"
            );
        }
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
        // function `__typhon_ext_str__slug__("Hello")`.
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
    fn finditer_with_pos_reports_offsets_in_the_original_string() {
        // PR #488 review: `finditer(s, pos)` searched a slice, so `span()`
        // was relative to the slice. Empty matches still advance.
        let src = r#"
import re
let spans = [m.span() for m in re.compile("a").finditer("aXa", 1)]
if spans != [(2, 3)]:
    raise ValueError(str(spans))
let starts = [m.start() for m in re.compile("").finditer("ab", 1)]
if starts != [1, 2]:
    raise ValueError(str(starts))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn starred_typed_unpack_keeps_pythons_arity_rule() {
        // PR #488 review: the slice-based lowering bound `first` and `last`
        // to the same element of a one-element sequence; the starred
        // assignment it lowers through now raises as Python does.
        let src = r#"
def f(xs: list[int]) -> int:
    let (first: int, *rest, last: int) = xs
    return first + last + len(rest)

if f([1, 2, 3, 4]) != 7:
    raise ValueError("wrong arithmetic")
try:
    f([1])
    raise AssertionError("no error")
except ValueError as e:
    if "not enough values to unpack" not in str(e):
        raise AssertionError(str(e))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn user_dict_pop_rejects_a_second_default() {
        let src = r#"
from collections import UserDict
let d = UserDict({"a": 1})
try:
    d.pop("x", 1, 2)
    raise AssertionError("accepted")
except TypeError as e:
    if "takes from 2 to 3 positional arguments" not in str(e):
        raise AssertionError(str(e))
if d.pop("a") != 1 or d.pop("zz", 5) != 5:
    raise AssertionError("pop")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn exit_stack_push_accepts_a_context_manager() {
        let src = r#"
from contextlib import ExitStack
mut log: list[str] = []
class Res:
    name: str
impl Res:
    def __enter__(self) -> Res:
        return self
    def __exit__(self, et, ev, tb) -> bool:
        log.append("exit " + self.name)
        return False
with ExitStack() as stack:
    stack.push(Res(name="a"))
    stack.callback(lambda: log.append("cb"))
if log != ["cb", "exit a"]:
    raise AssertionError(str(log))
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn extend_builtin_attribute_and_call_receivers_dispatch() {
        // docs/release-readiness-review-2026-09-30.md §5: an extension
        // method called on an attribute (`p.title.slug()`), a call
        // (`make().slug()`), an `impl` method call (`p.url().slug()`) or a
        // chained extension call (`make().slug().slug()`) was never
        // lowered, so the VM raised `AttributeError` like CPython did.
        let src = r#"
extend str:
    def slug(self) -> str:
        return self.lower().replace(" ", "-")

class Post:
    title: str

impl Post:
    def url(self) -> str:
        return self.title.slug()

def make() -> str:
    return "A B"

def main() -> None:
    let p: Post = Post(title="Hello World")
    let out: list[str] = [p.url(), make().slug(), p.title.slug(), p.url().slug(), make().slug().slug()]
    if out != ["hello-world", "a-b", "hello-world", "hello-world", "a-b"]:
        raise ValueError(str(out))

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn lazy_let_inside_class_resolves() {
        // FINDINGS #26: `lazy let` inside a class body lowers (in the
        // preprocessor) to a `@_typhon_cached_property` method, with a
        // hidden `from functools import cached_property as
        // _typhon_cached_property` import injected at module top. The VM
        // models `functools.cached_property` faithfully: the attribute is
        // read *without* a call, the body runs once per instance on first
        // access and the value is cached in the instance dict, and calling
        // the cached value is the ordinary `'int' object is not callable`
        // TypeError. Expectations pinned against CPython 3.13.
        let src = r#"
calls: list[int] = []

def compute(n: int) -> int:
    calls.append(n)
    return n * 2

class Counter:
    n: int

    lazy let doubled: int = compute(self.n)

let c = Counter(n=21)
if len(calls) != 0:
    raise ValueError("lazy let computed eagerly at construction")
if c.doubled != 42:
    raise ValueError("wrong value: " + str(c.doubled))
if c.doubled != 42:
    raise ValueError("wrong value on second access")
if len(calls) != 1:
    raise ValueError("cached_property body ran " + str(len(calls)) + " times")
try:
    c.doubled()
except TypeError as e:
    if str(e) != "'int' object is not callable":
        raise ValueError("unexpected message: " + str(e))
else:
    raise ValueError("calling the cached value did not raise")
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
    # pydantic serialises compactly — `,` and `:` with no spaces.
    if u.model_dump_json() != "{\"id\":1,\"name\":\"Ada\",\"active\":true}":
        raise ValueError("model_dump_json wrong: " + u.model_dump_json())
    # `str()` is pydantic's space-joined field list; `repr()` is the
    # constructor form. Neither leaks the `model_config` class attribute.
    if str(u) != "id=1 name='Ada' active=True":
        raise ValueError("model str wrong: " + str(u))
    if repr(u) != "User(id=1, name='Ada', active=True)":
        raise ValueError("model repr wrong: " + repr(u))
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `json.loads` decodes UTF-8 and `\uXXXX` escapes (surrogate pairs
    /// included), accepts the non-finite constants CPython accepts, and
    /// raises a `JSONDecodeError` carrying CPython's message and character
    /// position — catchable both as `json.JSONDecodeError` and `ValueError`.
    #[test]
    fn json_loads_matches_cpython() {
        let src = r#"
import json

def expect_error(doc: str, msg: str, pos: int, lineno: int, colno: int) -> None:
    try:
        json.loads(doc)
    except json.JSONDecodeError as e:
        if e.msg != msg or e.pos != pos or e.lineno != lineno or e.colno != colno:
            raise ValueError(f"{doc!r}: got {e.msg!r} {e.pos} {e.lineno} {e.colno}")
        if str(e) != f"{msg}: line {lineno} column {colno} (char {pos})":
            raise ValueError("str(e) wrong: " + str(e))
        return
    raise ValueError(f"{doc!r} did not raise")

def main() -> None:
    if json.loads("\"h\u00e9llo \u2603\"") != "héllo ☃":
        raise ValueError("utf-8 passthrough")
    if json.loads("\"caf\\u00e9 \\ud83d\\ude00\"") != "café 😀":
        raise ValueError("unicode escapes")
    if json.loads("[1.0, 2.5e0, -0.0, 10000000000000000000000, 1e999]") != [1.0, 2.5, -0.0, 10000000000000000000000, float("inf")]:
        raise ValueError("numbers")
    let nan: list[float] = json.loads("[NaN, Infinity, -Infinity]")
    if not (nan[0] != nan[0] and nan[1] == float("inf") and nan[2] == float("-inf")):
        raise ValueError("constants")
    if json.loads("{\"a\": [1, {\"b\": null}], \"c\": true}") != {"a": [1, {"b": None}], "c": True}:
        raise ValueError("nested")
    expect_error("x", "Expecting value", 0, 1, 1)
    expect_error("", "Expecting value", 0, 1, 1)
    expect_error("[1,", "Expecting value", 3, 1, 4)
    expect_error("[1,]", "Illegal trailing comma before end of array", 2, 1, 3)
    expect_error("{\"a\" 1}", "Expecting ':' delimiter", 5, 1, 6)
    expect_error("{\"a\": 1,}", "Illegal trailing comma before end of object", 7, 1, 8)
    expect_error("{\"a\":1 \"b\":2}", "Expecting ',' delimiter", 7, 1, 8)
    expect_error("\"abc", "Unterminated string starting at", 0, 1, 1)
    expect_error("[1] x", "Extra data", 4, 1, 5)
    expect_error("{1: 2}", "Expecting property name enclosed in double quotes", 1, 1, 2)
    expect_error("nul", "Expecting value", 0, 1, 1)
    expect_error("\"a\\qb\"", "Invalid \\escape", 2, 1, 3)
    expect_error("\"a\\u12G4\"", "Invalid \\uXXXX escape", 3, 1, 4)
    expect_error("\"a\nb\"", "Invalid control character at", 2, 1, 3)
    expect_error("  \n  [1, \n 2, }", "Expecting value", 14, 3, 5)
    expect_error("01", "Extra data", 1, 1, 2)
    expect_error("-", "Expecting value", 0, 1, 1)
    # A JSONDecodeError is a ValueError.
    try:
        json.loads("{")
    except ValueError:
        pass
    # ... and the qualified form matches by class identity.
    try:
        json.loads("[")
    except (KeyError, json.JSONDecodeError):
        pass
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `json.dumps` honours `ensure_ascii` (on by default), `separators`,
    /// `indent` (int or str), `sort_keys` and `allow_nan`, coerces scalar keys
    /// and raises CPython's `TypeError` for anything else.
    #[test]
    fn json_dumps_matches_cpython() {
        let src = r#"
import json

def main() -> None:
    if json.dumps({"k": 1.0, "s": "é\n\x01☃😀", "n": None, "t": (1, 2), "b": False}) != "{\"k\": 1.0, \"s\": \"\\u00e9\\n\\u0001\\u2603\\ud83d\\ude00\", \"n\": null, \"t\": [1, 2], \"b\": false}":
        raise ValueError("default: " + json.dumps({"k": 1.0, "s": "é\n\x01☃😀", "n": None, "t": (1, 2), "b": False}))
    if json.dumps("é", ensure_ascii=False) != "\"é\"":
        raise ValueError("ensure_ascii=False")
    if json.dumps({"a": 1}, separators=(",", ":")) != "{\"a\":1}":
        raise ValueError("separators")
    if json.dumps([1, {"a": []}], indent=2) != "[\n  1,\n  {\n    \"a\": []\n  }\n]":
        raise ValueError("indent: " + json.dumps([1, {"a": []}], indent=2))
    if json.dumps([[]], indent="\t") != "[\n\t[]\n]":
        raise ValueError("str indent")
    if json.dumps([], indent=2) != "[]" or json.dumps({}, indent=2) != "{}":
        raise ValueError("empty with indent")
    if json.dumps({"b": {"c": 1}, "a": 2}, sort_keys=True) != "{\"a\": 2, \"b\": {\"c\": 1}}":
        raise ValueError("sort_keys")
    if json.dumps({1: "a", 2.5: "b", None: "d"}) != "{\"1\": \"a\", \"2.5\": \"b\", \"null\": \"d\"}":
        raise ValueError("key coercion: " + json.dumps({1: "a", 2.5: "b", None: "d"}))
    if json.dumps(float("inf")) != "Infinity" or json.dumps(1e16) != "1e+16":
        raise ValueError("floats")
    try:
        json.dumps({"a": {1, 2}})
        raise ValueError("set accepted")
    except TypeError as e:
        if str(e) != "Object of type set is not JSON serializable":
            raise ValueError("set message: " + str(e))
    try:
        json.dumps({(1, 2): 3})
        raise ValueError("tuple key accepted")
    except TypeError as e:
        if str(e) != "keys must be str, int, float, bool or None, not tuple":
            raise ValueError("key message: " + str(e))
    try:
        json.dumps(float("nan"), allow_nan=False)
        raise ValueError("nan accepted")
    except ValueError as e:
        if str(e) != "Out of range float values are not JSON compliant: nan":
            raise ValueError("nan message: " + str(e))
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `dataclasses.asdict` / `astuple` recurse in declaration order (the
    /// previous implementation iterated an unordered map, so the output
    /// changed between runs), and `fields` / `is_dataclass` / `replace` exist.
    #[test]
    fn dataclasses_reflection_matches_cpython() {
        let src = r#"
import dataclasses

class Address:
    street: str
    city: str

class Person:
    name: str
    age: int
    address: Address
    tags: list[str] = []

def main() -> None:
    let p: Person = Person(name="Alice", age=30, address=Address(street="1 Main", city="X"), tags=["a"])
    let d: dict[str, object] = dataclasses.asdict(p)
    if str(d) != "{'name': 'Alice', 'age': 30, 'address': {'street': '1 Main', 'city': 'X'}, 'tags': ['a']}":
        raise ValueError("asdict: " + str(d))
    let t: tuple[object, ...] = dataclasses.astuple(p)
    if str(t) != "('Alice', 30, ('1 Main', 'X'), ['a'])":
        raise ValueError("astuple: " + str(t))
    let names: list[str] = [f.name for f in dataclasses.fields(p)]
    if names != ["name", "age", "address", "tags"]:
        raise ValueError("fields")
    let types: list[str] = [str(f.type) for f in dataclasses.fields(Address)]
    if types != ["str", "str"]:
        raise ValueError("field types: " + str(types))
    if not dataclasses.is_dataclass(p) or not dataclasses.is_dataclass(Person) or dataclasses.is_dataclass(42):
        raise ValueError("is_dataclass")
    let q: Person = dataclasses.replace(p, age=31)
    if q.age != 31 or q.name != "Alice" or p.age != 30:
        raise ValueError("replace")
    try:
        dataclasses.replace(p, nope=1)
        raise ValueError("replace accepted unknown field")
    except TypeError as e:
        if str(e) != "Person.__init__() got an unexpected keyword argument 'nope'":
            raise ValueError("replace message: " + str(e))
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// The long tail of builtin behaviour that real programs hit: `sys.exit`
    /// raising a catchable `SystemExit` (with `finally` running), `next`'s
    /// default, `enumerate`'s start, `list.index` bounds, `bool` bitwise
    /// results, `KeyError` payloads, `int()` error text, `%s` dispatching a
    /// user `__repr__`, and format specs without a presentation type.
    #[test]
    fn builtin_long_tail_matches_cpython() {
        let src = r#"
import sys

plain class R:
    n: int = 0

impl R:
    def __init__(self, n: int) -> None:
        self.n = n

    def __repr__(self) -> str:
        return f"R<{self.n}>"

def main() -> None:
    let it = iter([1])
    next(it)
    if next(it, "done") != "done":
        raise ValueError("next default")
    if list(enumerate("ab", 1)) != [(1, "a"), (2, "b")]:
        raise ValueError("enumerate start")
    if [1, 2, 3, 2].index(2, 2) != 3 or [1, 2, 3].index(3, -1) != 2:
        raise ValueError("list.index bounds")
    try:
        [1].index(5)
        raise ValueError("index accepted")
    except ValueError as e:
        if str(e) != "5 is not in list":
            raise ValueError("index message: " + str(e))
    if (True & False) is not False or (True | False) is not True or (True ^ True) is not False:
        raise ValueError("bool bitwise")
    try:
        let d: dict[str, int] = {"a": 1}
        print(d["k"])
    except KeyError as e:
        if e.args[0] != "k" or str(e) != "'k'":
            raise ValueError("KeyError payload: " + str(e) + " " + str(e.args))
    try:
        {"a": 1}.pop("zz")
    except KeyError as e:
        if str(e) != "'zz'":
            raise ValueError("pop KeyError: " + str(e))
    try:
        int("x1")
    except ValueError as e:
        if str(e) != "invalid literal for int() with base 10: 'x1'":
            raise ValueError("int message: " + str(e))
    if "%s %r" % (R(1), R(2)) != "R<1> R<2>":
        raise ValueError("printf dispatch: " + ("%s %r" % (R(1), R(2))))
    if f"{1234567.891:,}|{1.5:10}|{3.14159:.3}|{1e16:.3}|{3.0:.3}|{-0.0:+}" != "1,234,567.891|       1.5|3.14|1e+16|3.0|-0.0":
        raise ValueError("float spec: " + f"{1234567.891:,}|{1.5:10}|{3.14159:.3}|{1e16:.3}|{3.0:.3}|{-0.0:+}")
    if f"{'hello':.3}|{'hi':6.1}|" != "hel|h     |":
        raise ValueError("str precision: " + f"{'hello':.3}|{'hi':6.1}|")
    try:
        try:
            sys.exit("fatal")
        finally:
            print("cleanup")
    except SystemExit as e:
        if str(e) != "fatal":
            raise ValueError("SystemExit payload")
    try:
        exit(3)
    except SystemExit:
        pass
    try:
        try:
            sys.exit(2)
        except Exception:
            raise ValueError("SystemExit must not be an Exception")
    except SystemExit:
        pass
main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// An uncaught `SystemExit` becomes the process exit status: an int is
    /// the code, no argument is 0, anything else prints and exits 1.
    #[test]
    fn uncaught_system_exit_maps_to_exit_status() {
        assert_eq!(run_capturing("import sys\nsys.exit(5)\n").unwrap(), 5);
        assert_eq!(run_capturing("import sys\nsys.exit()\n").unwrap(), 0);
        assert_eq!(run_capturing("raise SystemExit\n").unwrap(), 0);
        assert_eq!(
            run_capturing("import sys\nsys.exit(\"bad config\")\n").unwrap(),
            1
        );
    }

    /// `except` over an attribute-qualified exception class matches — a
    /// module-exported native constructor (`asyncio.CancelledError`) or a
    /// VM-synthesised class (`json.JSONDecodeError`) — and `CancelledError`
    /// escapes `except Exception` as it does in CPython.
    #[test]
    fn qualified_except_clauses_match() {
        let src = r#"
import asyncio

def main() -> None:
    try:
        raise asyncio.TimeoutError()
    except asyncio.TimeoutError:
        pass
    try:
        raise asyncio.TimeoutError()
    except TimeoutError:
        pass
    mut escaped: bool = False
    try:
        try:
            raise asyncio.CancelledError()
        except Exception:
            raise ValueError("CancelledError must not be an Exception")
    except asyncio.CancelledError:
        escaped = True
    if not escaped:
        raise ValueError("not escaped")
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
    fn abandoned_generators_are_finalised_at_loop_exit() {
        // W5-05: CPython finalises a generator when its last reference drops,
        // running `finally` / `with` exits. The VM has no GC, so it does this
        // at the end of the loop that holds the only reference — which covers
        // `break` / `return` out of a `for` over a generator. A generator the
        // program kept a reference to is left live, as CPython leaves it.
        //
        // Residual (docs/vm.md): a generator held only by a function local is
        // finalised when that local's scope ends in CPython, which needs
        // refcounting the VM does not model.
        let src = r#"
from typing import Iterator

out: list[str] = []

def g() -> Iterator[int]:
    try:
        yield 1
        yield 2
    finally:
        out.append("cleanup")

def break_out() -> None:
    for x in g():
        out.append("body " + str(x))
        break

def return_out() -> None:
    for x in g():
        if x == 1:
            return

def still_referenced() -> None:
    let it: Iterator[int] = g()
    out.append(str(next(it)))
    out.append("held")

def main() -> None:
    break_out()
    return_out()
    still_referenced()
    if out != ["body 1", "cleanup", "cleanup", "1", "held"]:
        raise ValueError("abandoned-generator cleanup wrong: " + repr(out))

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn class_attributes_resolve_on_read_not_at_construction() {
        // W5-06: a class attribute read through an instance must go through
        // the class, so a later update to it (a class-level counter) is
        // visible from instances built earlier. Copying it into the instance
        // dict at construction froze the old value.
        let src = r#"
plain class Counter:
    instances: int = 0

impl Counter:
    def __init__(self) -> None:
        Counter.instances += 1

plain class Circle:
    unit: str = "px"

def main() -> None:
    let a: Counter = Counter()
    let b: Counter = Counter()
    if Counter.instances != 2 or a.instances != 2 or b.instances != 2:
        raise ValueError("class attr not shared: " + str([Counter.instances, a.instances, b.instances]))
    # A per-instance assignment still shadows the class attribute, and a
    # sibling instance keeps reading the class value.
    let c: Circle = Circle()
    c.unit = "em"
    let d: Circle = Circle()
    if c.unit != "em" or d.unit != "px" or Circle.unit != "px":
        raise ValueError("instance shadow/read wrong: " + str([c.unit, d.unit, Circle.unit]))

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

    /// PEP 3134 chaining: `raise X from Y` records `__cause__`, an exception
    /// escaping a handler records the one it interrupted as `__context__`,
    /// `from None` suppresses the context, and a fresh exception has neither.
    #[test]
    fn vm_exception_chaining() {
        let src = r#"
class AppError(Exception):
    pass

def main() -> None:
    try:
        try:
            raise ValueError("inner")
        except ValueError as inner:
            raise RuntimeError("wrapper") from inner
    except RuntimeError as e:
        if repr(e.__cause__) != "ValueError('inner')":
            raise ValueError("explicit cause wrong")
        if repr(e.__context__) != "ValueError('inner')":
            raise ValueError("implicit context wrong")
        if not e.__suppress_context__:
            raise ValueError("from-clause should suppress context")

    try:
        try:
            raise KeyError("k")
        except KeyError:
            raise AppError("app")
    except AppError as e:
        if repr(e.__context__) != "KeyError('k')" or e.__cause__ is not None:
            raise ValueError("user exception chaining wrong")
        if e.__suppress_context__:
            raise ValueError("plain raise should not suppress context")

    try:
        try:
            raise ValueError("v")
        except ValueError:
            raise TypeError("t") from None
    except TypeError as e:
        if e.__cause__ is not None or not e.__suppress_context__:
            raise ValueError("from None wrong")

    # An error the handler body runs into chains just like an explicit raise.
    try:
        try:
            raise ValueError("first")
        except ValueError:
            let _bad: int = int("nope")
    except ValueError as e:
        if repr(e.__context__) != "ValueError('first')":
            raise ValueError("native error context wrong")

    # `finally` raising over a propagating exception chains it too.
    try:
        try:
            raise ValueError("pending")
        finally:
            raise RuntimeError("from finally")
    except RuntimeError as e:
        if repr(e.__context__) != "ValueError('pending')":
            raise ValueError("finally context wrong")

    let fresh: ValueError = ValueError("fresh")
    if fresh.__cause__ is not None or fresh.__context__ is not None:
        raise ValueError("fresh exception should have no chain")
    if fresh.__suppress_context__:
        raise ValueError("fresh exception should not suppress context")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// The builtin-value dunder table is per-type, as CPython's is: an int
    /// has no `__len__`, a str no `__int__`, a set no `__getitem__`. A
    /// `@runtime_checkable` Protocol reads the same table structurally.
    #[test]
    fn vm_builtin_dunder_table_and_runtime_protocols() {
        let src = r#"
from typing import Protocol, runtime_checkable

@runtime_checkable
class Sized(Protocol):
    def __len__(self) -> int: ...

class Box:
    n: int
impl Box:
    def __len__(self) -> int:
        return self.n

def main() -> None:
    let probes: list[bool] = [
        hasattr(5, "__len__"),
        hasattr("s", "__int__"),
        hasattr([], "__bool__"),
        hasattr(set(), "__getitem__"),
        hasattr(1.5, "__index__"),
    ]
    if any(probes):
        raise ValueError(f"builtin dunder table too permissive: {probes}")
    let present: list[bool] = [
        hasattr({}, "__reversed__"),
        hasattr(5, "__index__"),
        hasattr("s", "__len__"),
        hasattr(5, "__float__"),
    ]
    if not all(present):
        raise ValueError(f"builtin dunder table too strict: {present}")
    if not isinstance([1], Sized) or not isinstance(Box(n=2), Sized):
        raise ValueError("runtime_checkable protocol should match structurally")
    if isinstance(5, Sized):
        raise ValueError("an int is not Sized")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A parameter before `/` never binds by keyword: with `**kwargs` the
    /// keyword lands there, and without one CPython rejects the call.
    #[test]
    fn vm_positional_only_parameters() {
        let src = r#"
def g(a: int, /, **kw: int) -> str:
    return f"{a} {sorted(kw.items())}"

def h(x: int, /, y: int = 5) -> int:
    return x + y

def only(a: int, /, b: int) -> int:
    return a + b

def main() -> None:
    if g(1, a=2) != "1 [('a', 2)]":
        raise ValueError("positional-only name should reach **kwargs")
    if h(1) != 6 or h(1, 2) != 3 or h(1, y=3) != 4:
        raise ValueError("positional-or-keyword binding wrong")
    try:
        let _ = only(a=1, b=2)
        raise ValueError("expected a TypeError")
    except TypeError as e:
        if "positional-only arguments passed as keyword arguments: 'a'" not in str(e):
            raise ValueError(f"wrong message: {e}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A batch of exception messages and conversions the differential
    /// harness caught the VM wording differently from CPython.
    #[test]
    fn vm_cpython_error_message_parity() {
        let src = r#"
def message(thunk: object) -> str:
    try:
        let _ = thunk()
        return "<no error>"
    except Exception as e:
        return f"{type(e).__name__}: {e}"

def main() -> None:
    let cases: list[tuple[object, str]] = [
        (lambda: 1 / 0, "ZeroDivisionError: division by zero"),
        (lambda: 1 // 0, "ZeroDivisionError: integer division or modulo by zero"),
        (lambda: 1 % 0, "ZeroDivisionError: integer modulo by zero"),
        (lambda: 1.0 / 0, "ZeroDivisionError: float division by zero"),
        (lambda: 1.0 // 0, "ZeroDivisionError: float floor division by zero"),
        (lambda: 1.0 % 0, "ZeroDivisionError: float modulo by zero"),
        (lambda: round(float("nan")), "ValueError: cannot convert float NaN to integer"),
        (lambda: round(float("inf")), "OverflowError: cannot convert float infinity to integer"),
        (lambda: float("abc"), "ValueError: could not convert string to float: 'abc'"),
        (lambda: chr(-1), "ValueError: chr() arg not in range(0x110000)"),
        (lambda: ord("ab"), "TypeError: ord() expected a character, but string of length 2 found"),
        (lambda: max([]), "ValueError: max() iterable argument is empty"),
        (lambda: [].pop(), "IndexError: pop from empty list"),
        (lambda: [1].pop(5), "IndexError: pop index out of range"),
    ]
    for thunk, want in cases:
        let got: str = message(thunk)
        if got != want:
            raise ValueError(f"got {got!r}, want {want!r}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Conversions, formatting and containment the same sweep turned up.
    #[test]
    fn vm_stdlib_conversion_parity() {
        let src = r#"
import json
from collections import defaultdict

def main() -> None:
    if float.fromhex("0x1.8p1") != 3.0 or float.fromhex("-0x1p-1") != -0.5:
        raise ValueError("float.fromhex wrong")
    if "a {x} {x}".format_map({"x": 1}) != "a 1 1":
        raise ValueError("str.format_map wrong")
    if "{n:>{w}}".format(n=42, w=8) != "      42":
        raise ValueError("nested format spec wrong")
    if (-1).to_bytes(2, "big", signed=True) != b"\xff\xff":
        raise ValueError("signed to_bytes wrong")
    if (255).to_bytes() != b"\xff" or (255).to_bytes(length=2, byteorder="big") != b"\x00\xff":
        raise ValueError("to_bytes defaults wrong")
    if int.from_bytes(b"\xff", "big", signed=True) != -1:
        raise ValueError("signed from_bytes wrong")
    if not issubclass(bool, int):
        raise ValueError("bool is a subclass of int")
    if str(OSError("a", "b", "c", "d")) != "[Errno a] b: 'c'":
        raise ValueError(f"OSError str wrong: {OSError('a', 'b', 'c', 'd')}")
    if defaultdict(int) != {} or defaultdict(list, {"a": [1]}) != {"a": [1]}:
        raise ValueError("defaultdict equality wrong")
    try:
        let _ = json.loads("{,}")
    except ValueError as e:
        if type(e).__name__ != "JSONDecodeError":
            raise ValueError("JSONDecodeError should be a ValueError")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `freeze let` builds a `mappingproxy`, whose `str()` is the mapping it
    /// wraps and whose `repr()` names the proxy — CPython's split.
    #[test]
    fn vm_frozen_dict_str_and_repr() {
        let src = r#"
freeze let CONFIG = {"limits": {"max": 100}}

def main() -> None:
    if str(CONFIG["limits"]) != "{'max': 100}":
        raise ValueError(f"str wrong: {CONFIG['limits']}")
    if repr(CONFIG["limits"]) != "mappingproxy({'max': 100})":
        raise ValueError(f"repr wrong: {CONFIG['limits']!r}")
    if str(CONFIG) != "{'limits': mappingproxy({'max': 100})}":
        raise ValueError(f"nested str wrong: {CONFIG}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A value-mixin enum member *is* its value: it converts, indexes,
    /// joins, and inherits the mixin's methods.
    #[test]
    fn vm_value_mixin_enum_members_behave_as_their_value() {
        let src = r#"
from enum import IntEnum, StrEnum

class IntE(IntEnum):
    ONE = 1
    TWO = 2

class StrE(StrEnum):
    X = "x"
    Y = "y"

def main() -> None:
    if "%d" % IntE.ONE != "1" or "%.1f" % IntE.ONE != "1.0":
        raise ValueError("printf conversion wrong")
    if abs(IntE.ONE) != 1 or IntE.ONE.bit_length() != 1:
        raise ValueError("numeric surface wrong")
    if [10, 20, 30][IntE.ONE] != 20 or "abc"[IntE.ONE] != "b":
        raise ValueError("indexing wrong")
    if len(StrE.X) != 1 or StrE.X.upper() != "X":
        raise ValueError("str surface wrong")
    if ",".join([StrE.X, StrE.Y]) != "x,y":
        raise ValueError("join wrong")
    if len(IntE) != 2 or IntE.ONE not in IntE:
        raise ValueError("enum class is sized and contains its members")
    try:
        let _ = IntE["NOPE"]
        raise ValueError("expected a KeyError")
    except KeyError as e:
        if str(e) != "'NOPE'":
            raise ValueError(f"KeyError message wrong: {e}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Slices are values: a multi-axis subscript carries them (and
    /// `Ellipsis`) through to `__getitem__`, and they repr as CPython does.
    #[test]
    fn vm_slice_and_ellipsis_values() {
        let src = r#"
plain class Probe:
    pass

impl Probe:
    def __getitem__(self, key: object) -> object:
        return key

def main() -> None:
    let p: Probe = Probe()
    if repr(p[1:2, 3]) != "(slice(1, 2, None), 3)":
        raise ValueError(f"multi-axis slice wrong: {p[1:2, 3]!r}")
    if repr(p[::2]) != "slice(None, None, 2)":
        raise ValueError("bare slice wrong")
    if repr(p[..., 0]) != "(Ellipsis, 0)":
        raise ValueError(f"ellipsis wrong: {p[..., 0]!r}")
    if p[...] is not Ellipsis:
        raise ValueError("Ellipsis should be a singleton")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A type-keyed registry sees one key for `type(5)` and the `int`
    /// constructor — what makes `functools.singledispatch` dispatch.
    #[test]
    fn vm_builtin_type_objects_are_one_registry_key() {
        let src = r#"
from functools import singledispatch

@singledispatch
def describe(x: object) -> str:
    return "object"

@describe.register(int)
def describe_int(x: int) -> str:
    return "int"

@describe.register(str)
def describe_str(x: str) -> str:
    return "str"

def main() -> None:
    if describe(42) != "int" or describe("a") != "str" or describe(1.5) != "object":
        raise ValueError("singledispatch did not dispatch on the builtin type")
    let registry: dict[object, str] = {}
    registry[int] = "hit"
    if registry.get(type(42), "miss") != "hit":
        raise ValueError("type(5) and int must be the same dict key")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `re.findall` returns the group (or a tuple of groups) for a pattern
    /// that has any, and a `range` matches a sequence pattern.
    #[test]
    fn vm_findall_groups_and_range_sequence_pattern() {
        let src = r#"
import re

def shape(v: object) -> str:
    match v:
        case []:
            return "empty"
        case [x]:
            return f"one {x}"
        case [x, *rest]:
            return f"many {x} {rest}"
        case _:
            return "other"

def main() -> None:
    let pairs = re.findall(r"(\w+)=(\d+)", "a=1 b=22")
    if pairs != [("a", "1"), ("b", "22")]:
        raise ValueError(f"two groups should give tuples: {pairs}")
    let ones = re.findall(r"(\d+)", "a=1 b=22")
    if ones != ["1", "22"]:
        raise ValueError(f"one group should give strings: {ones}")
    let whole = re.findall(r"\d+", "a=1 b=22")
    if whole != ["1", "22"]:
        raise ValueError(f"no groups should give matches: {whole}")
    if shape(range(3)) != "many 0 [1, 2]":
        raise ValueError(f"a range is a sequence: {shape(range(3))}")
    if shape(range(0)) != "empty":
        raise ValueError("an empty range matches the empty pattern")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A plain `enum.Flag` combines into composite pseudo-members, and
    /// `auto()` in a flag enum numbers by bit rather than by increment.
    #[test]
    fn vm_flag_enum_composites() {
        let src = r#"
from enum import Flag, IntFlag, auto

class Style(Flag):
    BOLD = auto()
    ITALIC = auto()
    UNDERLINE = auto()

class Permission(IntFlag):
    READ = 1
    WRITE = 2
    EXECUTE = 4

def main() -> None:
    if Style.UNDERLINE.value != 4:
        raise ValueError(f"auto() in a Flag numbers by bit: {Style.UNDERLINE.value}")
    let style: Style = Style.BOLD | Style.UNDERLINE
    if style.value != 5 or style.name != "BOLD|UNDERLINE":
        raise ValueError(f"composite wrong: {style.name} {style.value}")
    if Style.BOLD not in style or Style.ITALIC in style:
        raise ValueError("flag containment wrong")
    if (Style.BOLD & Style.ITALIC).value != 0:
        raise ValueError("empty composite wrong")
    let full: Permission = Permission.READ | Permission.WRITE | Permission.EXECUTE
    if int(full) != 7 or Permission.READ not in full:
        raise ValueError("IntFlag still combines through its int mixin")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `model_validate` builds nested models from nested mappings, and
    /// `model_validate_json` parses first — pydantic's behaviour.
    #[test]
    fn vm_model_validate_builds_nested_models() {
        let src = r#"
model Address:
    city: str

model Person:
    name: str
    address: Address?
    tags: list[str] = []

def main() -> None:
    let p: Person = Person.model_validate(
        {"name": "Ada", "address": {"city": "London"}, "tags": ["x"]}
    )
    if p.address is None or p.address.city != "London":
        raise ValueError("nested mapping should become a nested model")
    let q: Person = Person.model_validate_json('{"name": "Bob", "address": null}')
    if q.name != "Bob" or q.address is not None:
        raise ValueError("model_validate_json wrong")
    # `model_dump` unwraps the nested model again, as pydantic's does.
    let dumped = p.model_dump()
    if str(dumped) != "{'name': 'Ada', 'address': {'city': 'London'}, 'tags': ['x']}":
        raise ValueError(f"model_dump should recurse, got {dumped}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// Numeric conversions and reprs the differential harness caught: the
    /// shortest-repr tie break, Unicode digits, digit-group underscores,
    /// bytes-like conversion, `bool` against `float`, the modular inverse
    /// form of `pow`, and a complex format spec.
    #[test]
    fn vm_numeric_conversion_and_repr_parity() {
        let src = r#"
def probe(fn: object, arg: object) -> str:
    try:
        return str(fn(arg))
    except Exception as e:
        return type(e).__name__

def main() -> None:
    # `1e15 + 0.3` is exactly `1000000000000000.25`: an exact tie, which
    # CPython breaks toward the even last digit.
    if str(1e15 + 0.3) != "1000000000000000.2":
        raise ValueError(f"tie break wrong: {1e15 + 0.3}")
    # ...but `1.1 * 3` is not a tie, and its odd last digit stands.
    if str(1.1 * 3) != "3.3000000000000003":
        raise ValueError(f"non-tie must not be rounded: {1.1 * 3}")
    if str(9999999999999998.0) != "9999999999999998.0" or str(1e16) != "1e+16":
        raise ValueError("scientific-notation threshold wrong")
    if int("１２３") != 123 or int("٣") != 3 or float("１.５") != 1.5:
        raise ValueError("Unicode decimal digits should convert")
    if float("1_000.5") != 1000.5 or int("1_0") != 10:
        raise ValueError("digit-group underscores should convert")
    for bad in ["1__0", "_1", "1_"]:
        if probe(int, bad) != "ValueError":
            raise ValueError(f"misplaced underscore should be rejected: {bad}")
    if int(b"12") != 12 or float(b"1.5") != 1.5 or int(bytearray(b"7")) != 7:
        raise ValueError("a bytes-like should convert like a string")
    if sorted([True, False, 0.5]) != [False, 0.5, True]:
        raise ValueError("bool should order against float")
    if pow(3, -1, 7) != 5 or pow(2, -2, 9) != 7:
        raise ValueError("modular inverse wrong")
    if probe(int, "x") != "ValueError":
        raise ValueError("a non-numeric string is still rejected")
    if f"{1 + 2j:.1f}" != "1.0+2.0j" or f"{1 - 2j:.2f}" != "1.00-2.00j":
        raise ValueError(f"complex format spec wrong: {1 + 2j:.1f}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// A bound method reprs as CPython's does — the class the method is
    /// defined on, and the receiver through its own `__repr__`.
    #[test]
    fn vm_bound_method_repr() {
        let src = r#"
from pathlib import Path

class Base:
    n: int
impl Base:
    def bump(self) -> int:
        return self.n + 1
    def __repr__(self) -> str:
        return f"Base<{self.n}>"

class Derived(Base):
    pass

def main() -> None:
    if str(Derived(n=1).bump) != "<bound method Base.bump of Base<1>>":
        raise ValueError(f"wrong: {Derived(n=1).bump}")
    let p: Path = Path("/tmp")
    if str(p.iterdir) != "<bound method Path.iterdir of PosixPath('/tmp')>":
        raise ValueError(f"path method repr wrong: {p.iterdir}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `os.access` honours the requested mode, `os.truncate` grows a file
    /// with NULs, and a lone UTF-16 surrogate goes through the decode error
    /// handler instead of always raising.
    #[test]
    fn vm_access_truncate_and_utf16_error_handlers() {
        let src = r#"
import base64
import os
import shutil
import tempfile

def main() -> None:
    let d: str = "/tmp/zz_vm_access_probe"
    os.makedirs(d, exist_ok=True)
    let f: str = d + "/plain.txt"
    with open(f, "w") as fh:
        fh.write("abc")
    os.chmod(f, 0o644)
    if not os.access(f, os.F_OK) or not os.access(f, os.R_OK):
        raise ValueError("an existing readable file should answer F_OK / R_OK")
    if os.access(f, os.X_OK):
        raise ValueError("a 0o644 file is not executable, even for root")
    if os.access(d + "/nope", os.F_OK):
        raise ValueError("a missing path answers False")

    os.truncate(f, 6)
    if os.path.getsize(f) != 6 or open(f, "rb").read() != b"abc\x00\x00\x00":
        raise ValueError("truncate should grow with NULs")
    os.truncate(f, 2)
    if open(f, "rb").read() != b"ab":
        raise ValueError("truncate should shrink")

    let bad: bytes = b"\x00\xd8A\x00"
    if bad.decode("utf-16-le", "ignore") != "A":
        raise ValueError("ignore handler wrong")
    if bad.decode("utf-16-le", "replace") != "\ufffdA":
        raise ValueError("replace handler wrong")
    if bad.decode("utf-16-le", "backslashreplace") != "\\x00\\xd8A":
        raise ValueError("backslashreplace handler wrong")

    # `mkstemp` / `NamedTemporaryFile` are owner-only at creation, `base64`
    # honours `validate=`, and `copytree(symlinks=True)` recreates a link
    # rather than copying through it.
    let fd_path = tempfile.mkstemp()
    os.close(fd_path[0])
    if os.stat(fd_path[1]).st_mode & 0o777 != 0o600:
        raise ValueError("mkstemp should be 0600")
    if base64.b64decode(b"YWJj====") != b"abc":
        raise ValueError("lax base64 should ignore excess padding")
    for bad_b64 in [b"YWJj====", b"YW!j"]:
        try:
            let _ = base64.b64decode(bad_b64, validate=True)
            raise ValueError("validate=True should reject it")
        except ValueError as e:
            if type(e).__name__ != "Error":
                raise ValueError(f"expected binascii.Error, got {type(e).__name__}")
    let tree: str = tempfile.mkdtemp()
    os.makedirs(tree + "/src/inner", exist_ok=True)
    os.makedirs(tree + "/outside", exist_ok=True)
    os.symlink(tree + "/outside", tree + "/src/link")
    shutil.copytree(tree + "/src", tree + "/dst", symlinks=True)
    if not os.path.islink(tree + "/dst/link"):
        raise ValueError("copytree(symlinks=True) must recreate the link")
    try:
        let _ = bad.decode("utf-16-le")
        raise ValueError("strict should raise")
    except UnicodeDecodeError as e:
        if "position 0-1" not in str(e):
            raise ValueError(f"strict message should name the span: {e}")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_stdlib_shim_edge_cases_match_cpython() {
        let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let src = r#"
import argparse
import base64
import collections
import contextlib
import csv
import datetime
import hashlib
import io
import itertools
import os
import pathlib
import random
import shutil
import tempfile


def check(label: str, got: str, want: str) -> None:
    if got != want:
        raise ValueError(f"{label}: got {got!r}, want {want!r}")


def write_csv(rows: list[list[object]], quoting: int, escapechar: str | None) -> str:
    let buf: io.StringIO = io.StringIO()
    csv.writer(buf, quoting=quoting, escapechar=escapechar).writerows(rows)
    return buf.getvalue()


def main() -> None:
    os.chdir(tempfile.mkdtemp())

    # QUOTE_NONE escapes rather than quotes, and refuses without an escape.
    try:
        let _ = write_csv([["a,b"]], csv.QUOTE_NONE, None)
        raise ValueError("QUOTE_NONE must refuse an unescapable field")
    except csv.Error as e:
        check("csv-refuse", str(e), "need to escape, but no escapechar set")
    check("csv-escape", write_csv([["a,b"]], csv.QUOTE_NONE, "|"), "a|,b\r\n")
    # A quote only opens a field at its start.
    check("csv-midquote", str(list(csv.reader(['ab"c,d']))), "[['ab\"c', 'd']]")
    check("csv-postquote", str(list(csv.reader(['"ab"cd']))), "[['abcd']]")
    check("csv-escaped-read", str(list(csv.reader(["a\\,b"], escapechar="\\"))), "[['a,b']]")

    # `os.umask` really sets the process mask.
    let old: int = os.umask(0o077)
    with open("u.txt", "w") as f:
        f.write("x")
    os.mkdir("d")
    check("umask-file", oct(os.stat("u.txt").st_mode & 0o777), "0o600")
    check("umask-dir", oct(os.stat("d").st_mode & 0o777), "0o700")
    check("umask-prev", oct(os.umask(old)), "0o77")

    # Seeking past end-of-file leaves a NUL gap; truncate grows a file only.
    let sio: io.StringIO = io.StringIO("abc")
    sio.seek(5)
    sio.write("X")
    check("sparse-str", repr(sio.getvalue()), "'abc\\x00\\x00X'")
    let bio: io.BytesIO = io.BytesIO(b"abc")
    bio.seek(5)
    bio.write(b"X")
    check("sparse-bytes", str(bio.getvalue()), "b'abc\\x00\\x00X'")
    check("bytesio-truncate", str(io.BytesIO(b"abc").truncate(5)), "5")
    check("bytesio-kept", str(io.BytesIO(b"abc").getvalue()), "b'abc'")
    with open("t.bin", "wb") as wb:
        wb.write(b"abc")
    with open("t.bin", "r+b") as fb:
        check("file-truncate", str(fb.truncate(5)), "5")
    check("file-grown", str(open("t.bin", "rb").read()), "b'abc\\x00\\x00'")

    # `follow_symlinks=False` recreates the link; `which` honours the mode.
    with open("target.txt", "w") as t:
        t.write("hi")
    os.symlink("target.txt", "link.txt")
    shutil.copyfile("link.txt", "out.txt", follow_symlinks=False)
    check("copy-link", f"{os.path.islink('out.txt')} {os.readlink('out.txt')}", "True target.txt")
    shutil.copyfile("link.txt", "out2.txt")
    check("copy-follow", str(os.path.islink("out2.txt")), "False")
    with open("noexec", "w") as g:
        g.write("")
    os.chmod("noexec", 0o644)
    check("which-nonexec", str(shutil.which("./noexec")), "None")
    os.chmod("noexec", 0o755)
    check("which-exec", str(shutil.which("./noexec")), "./noexec")

    # The permissive base64 decode discards every non-alphabet character.
    check("b64-noise", str(base64.b64decode(b"Y W!Jj")), "b'abc'")
    check("b64-tab", str(base64.b64decode(b"\tYWJj\n ")), "b'abc'")
    check("b64-pad-ok", str(base64.b64decode(b"YWJ=")), "b'ab'")
    for bad, want in [(b"YW=", "Incorrect padding"), (b"Y", "Invalid base64-encoded string: number of data characters (1) cannot be 1 more than a multiple of 4")]:
        try:
            let _ = base64.b64decode(bad)
            raise ValueError("should have failed")
        except ValueError as e:
            check("b64-" + str(bad), str(e), want)

    # `exit_on_error=False` reaches the unrecognized-arguments path.
    let p = argparse.ArgumentParser(prog="p", exit_on_error=False)
    p.add_argument("--x")
    try:
        let _ = p.parse_args(["--y"])
        raise ValueError("should have raised")
    except argparse.ArgumentError as e:
        check("argparse-exit", str(e), "unrecognized arguments: --y")

    # `ChainMap`'s mutators work on the first mapping.
    let cm = collections.ChainMap({"x": 1}, {"y": 2})
    check("chainmap-pop", f"{cm.pop('x')} {dict(cm.maps[0])}", "1 {}")
    let cm2 = collections.ChainMap({"x": 1}, {"y": 2})
    cm2.update(z=3)
    check("chainmap-update", str(dict(cm2.maps[0])), "{'x': 1, 'z': 3}")
    let cm3 = collections.ChainMap({"x": 1}, {"y": 2})
    check("chainmap-setdefault", f"{cm3.setdefault('y', 9)} {cm3.setdefault('z', 5)}", "2 5")

    # `glob` honours `case_sensitive` and `recurse_symlinks`.
    os.makedirs("g/sub", exist_ok=True)
    with open("g/sub/item.txt", "w") as h:
        h.write("")
    os.symlink("sub", "g/link")
    let g: pathlib.Path = pathlib.Path("g")
    check("glob-nocase", str(sorted([str(x) for x in pathlib.Path("g/sub").glob("*.TXT", case_sensitive=False)])), "['g/sub/item.txt']")
    check("glob-case", str(sorted([str(x) for x in pathlib.Path("g/sub").glob("*.TXT")])), "[]")
    check("rglob-links", str(sorted([str(x) for x in g.rglob("item.txt", recurse_symlinks=True)])), "['g/link/item.txt', 'g/sub/item.txt']")
    check("rglob-nolinks", str(sorted([str(x) for x in g.rglob("item.txt")])), "['g/sub/item.txt']")

    # A byte outside 0-255 is rejected by every mutator.
    mut ba: bytearray = bytearray(b"a")
    for label, thunk in [("extend", lambda: ba.extend([256])), ("insert", lambda: ba.insert(0, 256))]:
        try:
            let _ = thunk()
            raise ValueError(label + " should have raised")
        except ValueError as e:
            check("ba-" + label, str(e), "byte must be in range(0, 256)")
    ba[0:1] = [120]
    check("ba-slice-write", str(ba), "bytearray(b'x')")

    # `ExitStack` keeps unwinding after a callback raises.
    let log: list[str] = []
    try:
        with contextlib.ExitStack() as stack:
            stack.callback(lambda: log.append("outer"))
            stack.callback(lambda: (log.append("inner"), 1 // 0)[0])
        raise ValueError("should have propagated")
    except ZeroDivisionError:
        check("exitstack", str(log), "['inner', 'outer']")

    # The SHA-2 family and unkeyed BLAKE2.
    check("sha224", hashlib.sha224(b"").hexdigest()[:16], "d14a028c2a3a2bc9")
    check("sha384", hashlib.sha384(b"").hexdigest()[:16], "38b060a751ac9638")
    check("blake2b", hashlib.blake2b(b"").hexdigest()[:16], "786a02f742015903")
    check("blake2s", hashlib.blake2s(b"").hexdigest()[:16], "69217a3079908094")
    check("blake2b-block", hashlib.blake2b(b"x" * 128).hexdigest()[:16], hashlib.new("blake2b", b"x" * 128).hexdigest()[:16])

    # `bytearray` is bytes-like everywhere CPython says it is, and the
    # combinatorics iterators refuse a negative selection length.
    check("hash-bytearray", hashlib.sha256(bytearray(b"abc")).hexdigest()[:16], "ba7816bf8f01cfea")
    let seeded = random.Random()
    seeded.seed(bytearray(b"seed"), version=2)
    let plain = random.Random()
    plain.seed(b"seed", version=2)
    check("seed-bytearray", str(seeded.random()), str(plain.random()))
    for label, fn in [("permutations", itertools.permutations),
                      ("combinations", itertools.combinations),
                      ("cwr", itertools.combinations_with_replacement)]:
        try:
            let _ = list(fn("abc", -1))
            raise ValueError(label + " should refuse a negative r")
        except ValueError as e:
            check("negative-" + label, str(e), "r must be non-negative")

    # An append-mode seek *past* the end still writes at EOF, gap and all.
    with open("ap.txt", "w") as aw:
        aw.write("abc")
    with open("ap.txt", "a+") as af:
        af.seek(10)
        af.write("X")
        check("append-past-eof-tell", str(af.tell()), "4")
    check("append-past-eof", str(open("ap.txt", "rb").read()), "b'abcX'")

    # `PurePath.match` / `full_match` honour `case_sensitive` too.
    check("match-nocase", f"{pathlib.PurePosixPath('item.txt').match('*.TXT', case_sensitive=False)}", "True")
    check("match-case", f"{pathlib.PurePosixPath('item.txt').match('*.TXT')}", "False")
    check("fullmatch-nocase", f"{pathlib.PurePosixPath('a/item.txt').full_match('**/*.TXT', case_sensitive=False)}", "True")

    # A fixed-offset `fromutc` only accepts a datetime it owns.
    let tz = datetime.timezone(datetime.timedelta(hours=2))
    try:
        let _ = tz.fromutc(datetime.datetime(2020, 1, 1))
        raise ValueError("fromutc should refuse a naive datetime")
    except ValueError as e:
        check("fromutc-naive", str(e), "fromutc: dt.tzinfo is not self")
    check("fromutc-own", str(tz.fromutc(datetime.datetime(2020, 1, 1, tzinfo=tz))), "2020-01-01 02:00:00+02:00")

    # `os.path` keeps a bytes path in bytes, high bytes included.
    check("bytes-join", str(os.path.join(b"/tmp", b"x")), "b'/tmp/x'")
    check("bytes-split", str(os.path.split(b"/a/b")), "(b'/a', b'b')")
    check("bytes-normpath", str(os.path.normpath(b"/a/../b//c")), "b'/b/c'")
    check("bytes-highbyte", str(os.path.basename(b"/a/\xff\xfe")), "b'\\xff\\xfe'")
    check("bytes-isabs", f"{os.path.isabs(b'/tmp')}", "True")
    try:
        let _ = os.path.join(b"/a", "b")
        raise ValueError("mixing str and bytes should be refused")
    except TypeError as e:
        check("bytes-mix", str(e), "Can't mix strings and bytes in path components")

    # Base32 is written in eight-character quanta with five legal padding
    # shapes; anything else is malformed rather than half-decodable.
    check("b32-ok", str(base64.b32decode(b"MZXW6===")), "b'foo'")
    check("b32-casefold", str(base64.b32decode(b"mzxw6===", casefold=True)), "b'foo'")
    for bad in [b"A", b"MY", b"MZXW6Y==", b"========", b"MZXW6YTBO==="]:
        try:
            let _ = base64.b32decode(bad)
            raise ValueError("b32decode should refuse " + str(bad))
        except ValueError as e:
            check("b32-" + str(bad), str(e), "Incorrect padding")

    # `groupby` compares keys by equality, so a fresh key object per item
    # still collapses into one group.
    check("groupby-eq", str([k for k, _ in itertools.groupby([1, 1], key=lambda x: [x])]), "[[1]]")
    check("groupby-runs", str([(k, list(g)) for k, g in itertools.groupby([1, 1, 2, 1])]), "[(1, [1, 1]), (2, [2]), (1, [1])]")
    try:
        let _ = list(itertools.product("ab", repeat=-1))
        raise ValueError("product should refuse a negative repeat")
    except ValueError as e:
        check("product-negative", str(e), "repeat argument cannot be negative")

    # `Path.touch` applies its mode to a file it creates, and leaves an
    # existing one's permissions alone.
    let touched: pathlib.Path = pathlib.Path("touched.txt")
    touched.touch(mode=0o600)
    check("touch-mode", oct(os.stat("touched.txt").st_mode & 0o777), "0o600")
    touched.touch()
    check("touch-again", oct(os.stat("touched.txt").st_mode & 0o777), "0o600")
    print("ok")


main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_type_parameter_and_class_reflection_match_cpython() {
        let src = r#"

def first[T = int](xs: list[T]) -> T:
    return xs[0]


def pair[K, V](k: K, v: V) -> tuple[K, V]:
    return (k, v)


def variadic[*Ts, **P]() -> None:
    return None


class Box[T = str]:
    value: T


class Bounded[T: int]:
    value: T


class Constrained[T: (int, str)]:
    value: T


class Plain:
    a: int


def plain(a: int) -> int:
    return a


def check(label: str, got: str, want: str) -> None:
    if got != want:
        raise ValueError(f"{label}: got {got!r}, want {want!r}")


def main() -> None:
    # PEP 695 parameters are real objects, and PEP 696 defaults read back.
    check("fn-params", str(first.__type_params__), "(T,)")
    check("cls-params", str(Box.__type_params__), "(T,)")
    check("fn-default", str(first.__type_params__[0].__default__), "<class 'int'>")
    check("cls-default", str(Box.__type_params__[0].__default__), "<class 'str'>")
    check("has-default", f"{first.__type_params__[0].has_default()}", "True")
    check("name", first.__type_params__[0].__name__, "T")
    check("two", str(pair.__type_params__), "(K, V)")
    check("no-default", f"{pair.__type_params__[0].has_default()}", "False")
    check("nodefault-repr", repr(pair.__type_params__[0].__default__), "typing.NoDefault")
    check("bound", str(Bounded.__type_params__[0].__bound__), "<class 'int'>")
    check(
        "constraints",
        str(Constrained.__type_params__[0].__constraints__),
        "(<class 'int'>, <class 'str'>)",
    )
    check("variadic", str(variadic.__type_params__), "(Ts, P)")
    # A non-generic function or class reports the empty tuple, not an error.
    check("plain-fn", str(plain.__type_params__), "()")
    check("plain-cls", str(Plain.__type_params__), "()")

    # A class object reprs with the module its body ran in.
    check("cls-repr", str(Plain), "<class '__main__.Plain'>")
    check("type-of", str(type(Plain(a=1))), "<class '__main__.Plain'>")
    print("ok")


main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_unicode_properties_bytes_and_printf_match_cpython() {
        let src = r#"
def check(label: str, got: str, want: str) -> None:
    if got != want:
        raise ValueError(f"{label}: got {got!r}, want {want!r}")

def main() -> None:
    # `isdigit`, `isdecimal` and `isnumeric` are three different questions.
    check("sup", f"{'²'.isdigit()}{'²'.isdecimal()}{'²'.isnumeric()}", "TrueFalseTrue")
    check("half", f"{'½'.isdigit()}{'½'.isdecimal()}{'½'.isnumeric()}", "FalseFalseTrue")
    check("arabic", f"{'٣'.isdigit()}{'٣'.isdecimal()}", "TrueTrue")
    check("roman", f"{'Ⅷ'.isnumeric()}", "True")

    # `isprintable()` rejects the format and separator characters too, and
    # `repr()` escapes exactly what it rejects.
    check("zwsp", f"{'\u200b'.isprintable()}{'\u200b'.isspace()}", "FalseFalse")
    check("repr-zwsp", repr("\u200b"), "'\\u200b'")
    check("repr-nbsp", repr("\xa0"), "'\\xa0'")
    check("repr-tag", repr("\U000e0001"), "'\\U000e0001'")
    check("repr-acc", repr("é"), "'é'")

    # `\x1c`-`\x1f` are whitespace to Python but not to the White_Space
    # property, so they split and strip.
    check("split-sep", str("a\x1cb\x1dc".split()), "['a', 'b', 'c']")
    check("strip-sep", repr("\x1c a \x1c".strip()), "'a'")
    check("isspace-sep", f"{'\x1c'.isspace()}", "True")

    # Titlecase is not uppercase for the digraphs, and `ß` expands.
    check("digraph", "ǆemal".title(), "ǅemal")
    check("sharp-title", "ß".title(), "Ss")
    check("sharp-cap", "ß".capitalize(), "Ss")
    check("digraph-cap", "ǆ".capitalize(), "ǅ")
    check("sharp-swap", "ß".swapcase(), "SS")
    check("dotted-swap", "İ".swapcase(), "i̇")

    # A whitespace split with a maxsplit leaves the remainder verbatim.
    check("split-max", str(" a b ".split(None, 1)), "['a', 'b ']")
    check("rsplit-max", str(" a b c ".rsplit(None, 1)), "[' a b', 'c']")

    # bytes carry the ASCII-only twin of most of the str surface.
    check("b-isalpha", f"{b'abc'.isalpha()}{b'12'.isdigit()}{b' '.isspace()}", "TrueTrueTrue")
    check("b-title", str(b"a1b".title()), "b'A1B'")
    check("b-partition", str(b"a-b".partition(b"-")), "(b'a', b'-', b'b')")
    check("b-rpartition", str(b"a-b-c".rpartition(b"-")), "(b'a-b', b'-', b'c')")
    check("b-just", str(b"abc".center(7, b"*")) + str(b"abc".zfill(5)), "b'**abc**'b'00abc'")
    check("b-splitlines", str(b"a\nb\rc\x0bd".splitlines()), "[b'a', b'b', b'c\\x0bd']")
    check("b-hex", b"abc".hex(":") + " " + b"abc".hex(":", 2), "61:62:63 61:6263")
    check("b-expandtabs", str(b"a\tb".expandtabs(4)), "b'a   b'")

    # printf: `%u` is a `%d` alias, `%a` escapes, a precision is a digit count.
    check("printf-u", "%u" % 3, "3")
    check("printf-a", "%a" % "é", "'\\xe9'")
    check("printf-prec", "%.3d|%.3x|%#.5x" % (5, 255, 255), "005|0ff|0x000ff")

    # cp1252 is Latin-1 with the C1 block replaced.
    check("cp1252", str("ab€".encode("cp1252")), "b'ab\\x80'")
    check("cp1252-dec", b"\x80\xa0".decode("cp1252"), "€\xa0")
    try:
        let _ = "日".encode("cp1252")
        raise ValueError("cp1252 has no entry for that character")
    except UnicodeEncodeError as e:
        check("cp1252-err", str(e), "'charmap' codec can't encode character '\\u65e5' in position 0: character maps to <undefined>")

    # NaN comparisons answer False rather than raising.
    let n: float = float("nan")
    check("minmax", f"{max(1, n)} {max(n, 1)} {min(1, n)} {min(n, 1)}", "1 nan 1 nan")
    check("nan-cmp", f"{n < 1}{n > 1}{n <= 1}{n >= 1}", "FalseFalseFalseFalse")
    check("divmod-bool", str(divmod(True, 2)), "(0, 1)")
    check("round-bool", f"{round(True)} {round(True, -1)}", "1 0")
    print("ok")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_format_spec_corners_match_cpython() {
        let src = r#"
def check(label: str, got: str, want: str) -> None:
    if got != want:
        raise ValueError(f"{label}: got {got!r}, want {want!r}")

def main() -> None:
    # `_` groups a binary, octal or hex integer in fours, not threes.
    check("hex", f"{1234567:_x}", "12_d687")
    check("bin", f"{1234567:_b}", "1_0010_1101_0110_1000_0111")
    check("oct", f"{1234567:_o}", "455_3207")
    check("HEX", f"{1234567:_X}", "12_D687")
    check("dec", f"{1234567:_d}", "1_234_567")
    check("short", f"{15:_b}", "1111")

    # A float with a precision but no presentation type reaches for an
    # exponent one decade sooner than `g` does, and keeps a `.0`.
    check("none-100", f"{100.0:.3}", "1e+02")
    check("g-100", f"{100.0:.3g}", "100")
    check("none-1e6", f"{1000000.0:.7}", "1e+06")
    check("none-1", f"{1.0:.1}", "1e+00")
    check("none-3", f"{3.0:.3}", "3.0")
    check("none-pi", f"{3.14159:.3}", "3.14")

    # `#` on a float always leaves a decimal point, and `g` keeps the
    # trailing zeros it would otherwise strip.
    check("alt-g", f"{3.0:#g}", "3.00000")
    check("alt-3g", f"{3:#.3g}", "3.00")
    check("alt-0f", f"{3.0:#.0f}", "3.")
    check("alt-0e", f"{3:#.0e}", "3.e+00")
    check("alt-1", f"{3.0:#.1}", "3.e+00")
    check("alt-1e10", f"{1e10:#g}", "1.00000e+10")
    check("alt-pct", f"{0.5:#.0%}", "50.%")
    check("alt-nan", f"{float('nan'):#.0f}", "nan")
    check("printf-g", "%#g" % 3.0, "3.00000")
    check("printf-f", "%#.0f" % 3.0, "3.")
    check("printf-nan", "%g" % float("nan"), "nan")

    # A nested field in a spec draws the *next* argument, not the first.
    check("nested-w", "[{:{}}]".format(3, 5), "[    3]")
    check("nested-kw", "[{:{w}.{p}f}]".format(3.14159, w=8, p=2), "[    3.14]")
    check("nested-2", "[{:{}{}}]".format(3, ">", 6), "[     3]")

    # `!a` escapes non-ASCII, where `!r` does not.
    check("bang-a", f"{'ü'!a}", "'\\xfc'")
    check("bang-r", f"{'ü'!r}", "'ü'")
    check("bang-a2", "{!a}".format("aübሴ"), "'a\\xfcb\\u1234'")
    print("ok")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn vm_directory_modes_append_writes_and_metadata_copies() {
        let src = r#"
import argparse
import os
import shutil
import tempfile

def main() -> None:
    let base: str = tempfile.mkdtemp()

    # `os.mkdir` applies the mode it is given; `tempfile` leans on that for
    # owner-only temporary directories. 0o700 survives any umask.
    let private: str = base + "/private"
    os.mkdir(private, 0o700)
    if os.stat(private).st_mode & 0o777 != 0o700:
        raise ValueError("os.mkdir must honour its mode argument")
    os.makedirs(base + "/deep/leaf", 0o700)
    if os.stat(base + "/deep/leaf").st_mode & 0o777 != 0o700:
        raise ValueError("makedirs applies the mode to the leaf")
    if os.stat(tempfile.mkdtemp()).st_mode & 0o777 != 0o700:
        raise ValueError("mkdtemp is owner-only")

    # An append-mode write lands at end-of-file however the cursor moved.
    let text: str = base + "/t.txt"
    with open(text, "w") as w:
        w.write("abc")
    with open(text, "a+") as f:
        f.seek(0)
        if f.read() != "abc":
            raise ValueError("a+ should read from the start after a seek")
        f.seek(0)
        f.write("X")
        if f.tell() != 4:
            raise ValueError("a text tell should follow the append")
    if open(text).read() != "abcX":
        raise ValueError("a seek must not redirect an append-mode write")

    let raw: str = base + "/b.bin"
    with open(raw, "wb") as wb:
        wb.write(b"abc")
    with open(raw, "ab+") as fb:
        fb.seek(0)
        fb.write(b"X")
    if open(raw, "rb").read() != b"abcX":
        raise ValueError("binary append writes go to the end too")

    # `copy2` keeps the source timestamps; `copy` deliberately does not.
    let s: str = base + "/src.txt"
    with open(s, "w") as sw:
        sw.write("hello")
    os.utime(s, (100000.0, 100000.0))
    shutil.copy2(s, base + "/two.txt")
    if os.stat(base + "/two.txt").st_mtime != 100000.0:
        raise ValueError("copy2 must preserve mtime")
    shutil.copy(s, base + "/three.txt")
    if os.stat(base + "/three.txt").st_mtime == 100000.0:
        raise ValueError("plain copy leaves a fresh mtime")

    # A greedy `nargs="*"` must leave the values a later positional needs.
    let p = argparse.ArgumentParser(prog="p")
    p.add_argument("files", nargs="*")
    p.add_argument("dest")
    let one = p.parse_args(["out"])
    if vars(one) != {"files": [], "dest": "out"}:
        raise ValueError(f"greedy star swallowed dest: {vars(one)}")
    let many = p.parse_args(["a", "b", "out"])
    if vars(many) != {"files": ["a", "b"], "dest": "out"}:
        raise ValueError(f"star should keep the rest: {vars(many)}")

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
    fn class_kinds_hash_eq_frozen_slots_match_cpython() {
        // Dict/set keys honour a user `__hash__` / `__eq__`; a non-frozen
        // dataclass is unhashable; a frozen one hashes like its field tuple;
        // a plain class hashes and compares by identity, takes no
        // constructor arguments and gets `object.__repr__`; `hash()` of the
        // builtin types is CPython's (PYTHONHASHSEED=0 for str/bytes);
        // frozen / slots enforcement and the generated `__init__` argument
        // errors carry CPython 3.13's exact messages. Every expected line
        // was printed by `PYTHONHASHSEED=0 python3.13` on the equivalent
        // program (`@dataclass(slots=True[, frozen=True])` classes).
        let src = r#"
import dataclasses
from dataclasses import FrozenInstanceError

class Pt frozen:
    x: int
    y: int

class Mut:
    x: int

plain class CI:
    s: str = ""

impl CI:
    def __init__(self, s: str) -> None:
        self.s = s
    def __eq__(self, other: object) -> bool:
        return isinstance(other, CI) and other.s.lower() == self.s.lower()
    def __hash__(self) -> int:
        return hash(self.s.lower())
    def __repr__(self) -> str:
        return f"CI({self.s!r})"

plain class Bag:
    items: list[str]
    count: int = 0

plain class Node:
    v: int = 0

impl Node:
    def __init__(self, v: int) -> None:
        self.v = v

class P3:
    x: int
    y: int
    z: int = 0

class Q4:
    a: int
    b: int
    c: int
    d: int

class E0:
    pass

out: list[str] = []

def emit(*parts: object) -> None:
    out.append(" ".join(str(p) for p in parts))

def show(label: str, f: object) -> None:
    try:
        emit(label, f())
    except Exception as e:
        emit(label, type(e).__name__, e)

def main() -> None:
    d = {CI("A"): 1}
    emit("H1", d[CI("a")], CI("a") in d, CI("A") in {CI("a")}, len({CI("a"), CI("A")}), d.get(CI("a")), d.pop(CI("A")), d)
    s = {CI("x")}
    s.add(CI("X"))
    s.discard(CI("x"))
    emit("H2", s, [CI("a")] == [CI("A")], ([CI("a")], 1) == ([CI("A")], 1), {"k": CI("a")} == {"k": CI("A")})
    emit("H3", hash(Pt(1, 2)) == hash((1, 2)), {Pt(1, 2): "v"}[Pt(1, 2)], len({Pt(1, 2), Pt(1, 2)}), Pt(1, 2) == Pt(1, 2), Pt(1, 2) != Pt(1, 3))
    show("H4", lambda: hash(Mut(1)))
    show("H5", lambda: {Mut(1): 1})
    show("H6", lambda: {Mut(1)})
    emit("H7", Mut(1) == Mut(1), Mut(1) in [Mut(1)], [Mut(1)].index(Mut(1)))
    n1 = Node(1)
    n2 = Node(1)
    emit("H8", n1 == n2, n1 == n1, n1 != n2, len({n1, n2}), n1 in {n1}, n2 in {n1}, [n1] == [n1], [n1] == [n2])
    emit("H9", hash(None), hash(True), hash(1), hash(-1), hash(2**61), hash(1.5), hash(-2.0), hash((1, 2)), hash(()), hash("abc"), hash(b"ab"), hash(frozenset({1, 2})), hash(complex(1, 2)), hash(range(5)), hash(2**100))
    p = Pt(1, 2)
    show("F1", lambda: setattr(p, "x", 5))
    show("F2", lambda: setattr(p, "z", 5))
    def delx() -> None:
        del p.x
    show("F3", delx)
    try:
        p.x = 9
    except AttributeError as e:
        emit("F4", type(e).__name__, e, isinstance(e, FrozenInstanceError), isinstance(e, AttributeError))
    emit("F5", p, dataclasses.replace(p, y=7))
    m = Mut(1)
    m.x = 2
    show("S1", lambda: setattr(m, "y", 3))
    emit("S2", m)
    b = Bag()
    emit("P1", type(b).__name__, Bag.count, b.count, hasattr(b, "items"))
    show("P2", lambda: b.items)
    show("P3", lambda: Bag(1))
    show("P4", lambda: Bag(items=[]))
    b.items = ["x"]
    emit("P5", b.items, repr(b).startswith("<__main__.Bag object at 0x"), str(b) == repr(b))
    show("D1", lambda: P3())
    show("D2", lambda: P3(1))
    show("D3", lambda: P3(1, 2, 3, 4))
    show("D4", lambda: P3(1, q=2))
    show("D5", lambda: P3(1, 2, x=1))
    show("D6", lambda: P3(1, 2, 3, 4, q=1))
    show("D7", lambda: P3(y=2, z=3))
    show("D8", lambda: Q4())
    show("D9", lambda: Q4(1))
    show("D10", lambda: E0(1))
    emit("D11", P3(1, 2), P3(1, y=2, z=5), Q4(1, 2, 3, 4))
    def m1(v: object) -> str:
        match v:
            case Pt(a, b, c):
                return "3"
            case Pt(a, b):
                return f"2:{a},{b}"
        return "none"
    show("M1", lambda: m1(Pt(1, 2)))
    def m2(v: object) -> str:
        match v:
            case Bag(a):
                return "1"
        return "none"
    show("M2", lambda: m2(Bag()))
    def m3(v: object) -> str:
        match v:
            case Pt(a, y=bb):
                return f"{a},{bb}"
        return "none"
    emit("M3", m3(Pt(4, 5)), m3(Bag()))

main()

expected = """H1 1 True True 1 1 1 {}
H2 set() True True True
H3 True v 1 True True
H4 TypeError unhashable type: 'Mut'
H5 TypeError unhashable type: 'Mut'
H6 TypeError unhashable type: 'Mut'
H7 True True 0
H8 False True True 2 True False True False
H9 4238894112 1 1 -2 1 1152921504606846977 -2 -3550055125485641917 5740354900026072187 -4594863902769663758 6148830537548944441 -1826646154956904602 2000007 5795932985296280846 549755813888
F1 FrozenInstanceError cannot assign to field 'x'
F2 TypeError super(type, obj): obj (instance of Pt) is not an instance or subtype of type (Pt).
F3 FrozenInstanceError cannot delete field 'x'
F4 FrozenInstanceError cannot assign to field 'x' True True
F5 Pt(x=1, y=2) Pt(x=1, y=7)
S1 AttributeError 'Mut' object has no attribute 'y' and no __dict__ for setting new attributes
S2 Mut(x=2)
P1 Bag 0 0 False
P2 AttributeError 'Bag' object has no attribute 'items'
P3 TypeError Bag() takes no arguments
P4 TypeError Bag() takes no arguments
P5 ['x'] True True
D1 TypeError P3.__init__() missing 2 required positional arguments: 'x' and 'y'
D2 TypeError P3.__init__() missing 1 required positional argument: 'y'
D3 TypeError P3.__init__() takes from 3 to 4 positional arguments but 5 were given
D4 TypeError P3.__init__() got an unexpected keyword argument 'q'
D5 TypeError P3.__init__() got multiple values for argument 'x'
D6 TypeError P3.__init__() got an unexpected keyword argument 'q'
D7 TypeError P3.__init__() missing 1 required positional argument: 'x'
D8 TypeError Q4.__init__() missing 4 required positional arguments: 'a', 'b', 'c', and 'd'
D9 TypeError Q4.__init__() missing 3 required positional arguments: 'b', 'c', and 'd'
D10 TypeError E0.__init__() takes 1 positional argument but 2 were given
D11 P3(x=1, y=2, z=0) P3(x=1, y=2, z=5) Q4(a=1, b=2, c=3, d=4)
M1 TypeError Pt() accepts 2 positional sub-patterns (3 given)
M2 TypeError Bag() accepts 0 positional sub-patterns (1 given)
M3 4,5 none""".split("\n")
for i, (got, want) in enumerate(zip(out, expected)):
    if got != want:
        raise AssertionError(f"line {i}: got {got!r}, want {want!r}")
if len(out) != len(expected):
    raise AssertionError(f"{len(out)} lines, want {len(expected)}")
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn random_module_matches_cpython_sequences_and_errors() {
        // `random`: seeded module-level and `Random`-instance sequences, every
        // distribution, `getstate` / `setstate`, `sample(counts=)`, `choices`,
        // subclassing, and the exact error messages.
        // The expectation is derived from the hosting python3.13 at
        // test time; its output carries host facts (errno numbers, the
        // OSError subclass, the tempdir, the locale-encoding spelling)
        // that differ by platform and are not the VM's to pin.
        let probe = r#"
out: list[str] = []

def emit(*parts: object) -> None:
    out.append(" ".join(str(p) for p in parts))

import random

def show(label: str, f: object) -> None:
    try:
        emit(label, repr(f()))
    except Exception as e:
        emit(label, type(e).__name__, e)

random.seed(7)
emit("R1", random.random(), random.random(), random.randint(1, 6), random.randrange(10), random.randrange(5, 50, 5), random.uniform(1.5, 2.5))
emit("R2", random.choice([1, 2, 3, 4]), random.getrandbits(10), random.getrandbits(70), random.getrandbits(0))
xs = list(range(10))
random.shuffle(xs)
emit("R3", xs, random.sample(range(100), 5), random.sample([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30], 8))
emit("R4", random.gauss(0, 1), random.gauss(0, 1), random.gauss(), random.normalvariate(1, 2), random.expovariate(1.5), random.triangular(1, 3), random.triangular(1, 3, 2.5))
emit("R5", random.choices("abc", k=5), random.choices([1, 2, 3], weights=[1, 1, 8], k=6), random.choices([1, 2], cum_weights=[1, 3], k=4))
emit("R6", random.lognormvariate(0, 1), random.betavariate(2, 3), random.gammavariate(2.5, 1.0), random.gammavariate(1.0, 2.0), random.gammavariate(0.5, 1.0), random.paretovariate(2), random.weibullvariate(1, 2), random.vonmisesvariate(0, 1), random.vonmisesvariate(0, 1e-7))
r = random.Random(7)
emit("R7", r.random(), r.randint(1, 100), r.getrandbits(40), r.choice("xyz"))
r2 = random.Random(7)
emit("R8", r2.random() == 0.32383276483316237, r2.getstate()[0], len(r2.getstate()[1]), r2.getstate()[2])
st = r2.getstate()
a = r2.random(); r2.setstate(st); b = r2.random()
emit("R9", a == b, a)
random.seed("hello"); emit("R10", random.random(), random.randbytes(4))
random.seed(b"hello"); emit("R11", random.random())
random.seed(2.5); emit("R12", random.random())
random.seed(-9); emit("R13", random.random())
random.seed(2 ** 70); emit("R14", random.random())
random.seed(0); emit("R15", random.random(), random.gauss(), random.gauss())
random.seed(0); emit("R16", random.gauss(), random.random(), random.gauss())
random.seed(12345); emit("R17", [random.randint(0, 2 ** 40) for _ in range(3)], random.sample(range(2 ** 40), 2), random.randrange(2 ** 64 + 5))
show("E1", lambda: random.randrange(0))
show("E2", lambda: random.randrange(5, 5))
show("E3", lambda: random.randrange(5, 1, 2))
show("E4", lambda: random.randrange(1, 5, 0))
show("E5", lambda: random.randrange(1.5))
show("E6", lambda: random.randint(5, 1))
show("E7", lambda: random.randrange(5, None, 2))
show("E8", lambda: random.choice([]))
show("E9", lambda: random.sample({1, 2}, 1))
show("E10", lambda: random.sample([1, 2], 3))
show("E11", lambda: random.getrandbits(-1))
show("E12", lambda: random.seed([1]))
show("E13", lambda: random.choices([1, 2], [1, 2, 3]))
show("E14", lambda: random.choices([1, 2], 3))
show("E15", lambda: random.gammavariate(0, 1))
show("E16", lambda: random.sample([1, 2, 3], 2, counts=[1, 1]))
random.seed(3); emit("R18", random.sample([1, 2, 3], 4, counts=[2, 1, 3]), random.binomialvariate(5, 0.3), random.binomialvariate(1, 0.5), random.binomialvariate(7, 0.8))
class Sub(random.Random):
    pass
s = Sub(99)
emit("R19", s.random(), s.randint(1, 10), isinstance(s, random.Random))
"#;
        let Some(expected) = python313_transcript(probe, "out") else {
            if std::env::var_os("TYC_REQUIRE_PYTHON").is_some() {
                panic!("python3.13 is required as the oracle (TYC_REQUIRE_PYTHON is set)");
            }
            eprintln!("skipping random_module_matches_cpython_sequences_and_errors: no python3.13 on PATH");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("transcript.txt");
        let src = format!(
            "{probe}\nwith open({path:?}, \"w\", encoding=\"utf-8\") as _out:\n    _out.write(\"\\n\".join(out))\n",
            path = out_path.display().to_string()
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
        let got = std::fs::read_to_string(&out_path).unwrap();
        // CPython spells the default text encoding per host ("utf-8" under
        // glibc, "UTF-8" on macOS); the VM's `io` shim lowercases it. Fold
        // the case so the test compares file semantics, not locale naming —
        // the spelling difference is documented in docs/vm.md.
        let normalise = |s: String| s.replace("utf-8", "UTF-8");
        let (got, expected) = (normalise(got), normalise(expected));
        assert_eq!(
            got, expected,
            "VM transcript differs from the hosting python3.13"
        );
    }

    #[test]
    fn hashlib_module_matches_cpython() {
        // `hashlib`: md5 / sha1 / sha256 / sha512 digests, incremental `update`,
        // `copy`, sizes, `new`, and the CPython error messages for a str or a
        // non-buffer input — expected text printed by python3.13 on this program.
        let src = r#"
out: list[str] = []

def emit(*parts: object) -> None:
    out.append(" ".join(str(p) for p in parts))

import hashlib

def show(label: str, f: object) -> None:
    try:
        emit(label, repr(f()))
    except Exception as e:
        emit(label, type(e).__name__, e)

emit("H1", hashlib.md5(b"abc").hexdigest(), hashlib.sha1(b"abc").hexdigest())
emit("H2", hashlib.sha256(b"abc").hexdigest(), hashlib.sha512(b"").hexdigest()[:32])
h = hashlib.sha256()
h.update(b"ab")
h.update(b"c")
emit("H3", h.hexdigest() == hashlib.sha256(b"abc").hexdigest(), h.name, h.digest_size, h.block_size, hashlib.sha512().block_size, hashlib.md5().digest_size)
c = h.copy()
c.update(b"d")
emit("H4", h.hexdigest()[:8], c.hexdigest()[:8], hashlib.sha256(b"abcd").hexdigest()[:8], len(h.digest()), hashlib.new("md5", b"x").hexdigest())
emit("H5", hashlib.sha256("typhon".encode("utf-8")).hexdigest(), sorted(hashlib.algorithms_guaranteed & {"md5", "sha256"}))
show("E1", lambda: hashlib.sha256("abc"))
show("E2", lambda: hashlib.sha256(123))
show("E3", lambda: hashlib.new("nope"))

expected = """H1 900150983cd24fb0d6963f7d28e17f72 a9993e364706816aba3e25717850c26c9cd0d89d
H2 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad cf83e1357eefb8bdf1542850d66d8007
H3 True sha256 32 64 128 16
H4 ba7816bf 88d4266f 88d4266f 32 9dd4e461268c8034f5c8564e155c67a6
H5 b1fc1c68a28135561cdb827e85081055da92f15662d9c33466d153b4e8e9a7b4 ['md5', 'sha256']
E1 TypeError Strings must be encoded before hashing
E2 TypeError object supporting the buffer API required
E3 ValueError unsupported hash type nope"""
got = "\n".join(out)
if got != expected:
    raise AssertionError("mismatch:\n" + got + "\n--- want ---\n" + expected)
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    #[test]
    fn filesystem_modules_match_cpython() {
        let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // `pathlib`, `open` / file objects, `io.StringIO` / `BytesIO`, `os` / `os.path`,
        // `glob`, `shutil` and `tempfile`, with CPython's `OSError` messages.
        // The expectation is derived from the hosting python3.13 at
        // test time; its output carries host facts (errno numbers, the
        // OSError subclass, the tempdir, the locale-encoding spelling)
        // that differ by platform and are not the VM's to pin.
        let probe = r#"
__lines: list[str] = []

def emit(*parts: object) -> None:
    __lines.append(" ".join(str(p) for p in parts))

import os
import io
import shutil
import tempfile
import glob
from pathlib import Path, PurePosixPath

def show(label: str, f: object) -> None:
    try:
        emit(label, repr(f()))
    except Exception as e:
        emit(label, type(e).__name__, e)

BASE = tempfile.mkdtemp(prefix="zz_probe_fs_")
os.chdir(BASE)
# Fixtures for the probes whose *kind* of failure must not depend on who
# is running the suite: unlinking a directory, rmdir-ing a file and
# rmdir-ing a non-empty directory all report the type error only when the
# caller may write the parent. Probing `/tmp` and `/etc/passwd` instead
# reported those as root and `PermissionError` as anyone else.
os.makedirs("zz_env/full")
Path("zz_env/full/inner").write_text("x")
Path("zz_env/file").write_text("abc")

# ---- pure paths
for spec in ["a//b/./c/", "", ".", "/", "//x/y", "///x", "a/../b", "a/b/", "./a", "/a/b.tar.gz", ".bashrc", "a.", "a..b", "..", "a/..", "c:/x"]:
    pp = Path(spec)
    emit("PP", repr(spec), str(pp), pp.parts, repr(pp.name), repr(pp.suffix), pp.suffixes, repr(pp.stem), str(pp.parent), repr(pp.anchor), repr(pp.root), pp.is_absolute(), [str(x) for x in pp.parents])
emit("PJ", Path("a", "b", "/c", "d"), Path("a", Path("b")), Path("a") / "b" / "c", "x" / Path("y"), Path("a").joinpath("b", "c"), Path("/a") / "/b", Path("a") / "")
show("W1", lambda: Path("a/b.txt").with_name("c.md"))
show("W2", lambda: Path("a/b.txt").with_suffix(".md"))
show("W3", lambda: Path("a/b.txt").with_suffix(""))
show("W4", lambda: Path("a/b").with_suffix(".x"))
show("W5", lambda: Path("a/b.txt").with_stem("q"))
show("W6", lambda: Path("a/b.txt").with_name(""))
show("W7", lambda: Path("a/b.txt").with_name("x/y"))
show("W8", lambda: Path("/").with_name("x"))
show("W9", lambda: Path("a/b.txt").with_suffix("txt"))
show("W10", lambda: Path("a/b.txt").with_suffix("."))
show("W11", lambda: Path("a").with_suffix(".x/y"))
show("W12", lambda: Path("a/b.tar.gz").with_suffix(".zip"))
show("R1", lambda: Path("/a/b/c").relative_to("/a"))
show("R2", lambda: Path("/a/b/c").relative_to("/x"))
show("R3", lambda: Path("a/b").relative_to("a/b"))
show("R4", lambda: Path("/a/b").relative_to("/a/b/c", walk_up=True))
show("R5", lambda: Path("/a/b").relative_to("c"))
show("R6", lambda: (Path("/a/b").is_relative_to("/a"), Path("/a/b").is_relative_to("/x")))
show("M1", lambda: (Path("a/b.py").match("*.py"), Path("a/b.py").match("a/*.py"), Path("/a/b.py").match("/*.py"), Path("a/b.py").match("b.py"), Path("a/b/c.py").match("a/*.py"), Path("a/b/c.py").match("**/*.py"), Path("a/b/c.py").full_match("**/*.py"), Path("a/b/c.py").full_match("a/**"), Path("a/b/c.py").match("*.PY"), Path("a/b/c.py").match("a/**/c.py"), Path("/a/b.py").match("*.py"), Path("/a/b.py").match("a/b.py"), Path("a/b.py").match("/a/b.py"), Path("a/b/c.py").full_match("*/c.py"), Path("a/b/c.py").full_match("*/*/c.py"), Path("a/b/c.py").match("[ab]/c.py"), Path("a.b").match("a?b"), Path("x.py").match("*.p[xy]")))
show("M2", lambda: Path("a").match(""))
show("M3", lambda: Path("a").glob(""))
show("M4", lambda: list(Path("/nonexistent_dir_zz").glob("*")))
show("O1", lambda: sorted([Path("a-b"), Path("a/b"), Path("a"), Path("a.b"), Path("/z"), Path("b")]))
show("O2", lambda: (Path("a/b") < Path("a-b"), Path("a") == Path("./a"), Path("a") == "a", hash(Path("a")) == hash(Path("./a")), Path("a") != Path("A")))
show("O3", lambda: Path("a") / 5)
show("O4", lambda: 5 / Path("a"))
show("O5", lambda: Path(5))
show("O6", lambda: (repr(Path("it's")), str(Path("a\\b")), Path("a b"), os.fspath(Path("q")), Path("a").as_posix(), Path("/a/b c").as_uri()))
show("O7", lambda: Path("a/b").parents[0:2])
show("O8", lambda: (Path("a/b/c").parents[-1], Path("a/b/c").parents[5]))
show("O9", lambda: (repr(Path("a").parents), len(Path("/a/b").parents), list(Path("/a/b").parents)))
show("O10", lambda: (type(Path("x")).__name__, PurePosixPath("x/y").name, isinstance(Path("x"), PurePosixPath), Path("x") == PurePosixPath("x")))
show("O11", lambda: Path("~/x").expanduser() == Path(os.environ["HOME"]) / "x")
show("O12", lambda: (str(Path.home()) == os.environ["HOME"], str(Path.cwd()) == os.getcwd(), Path("rel").absolute() == Path.cwd() / "rel"))
show("O13", lambda: (Path("a/../b/./c").resolve() == Path.cwd() / "b/c", Path("/nonexistent/../x").resolve()))
show("O14", lambda: Path("/tmp/zz_definitely_missing/q").stat())
show("O15", lambda: Path("/tmp/zz_definitely_missing/q").mkdir())
show("O16", lambda: Path("/tmp").mkdir())
show("O17", lambda: Path("/tmp/zz_definitely_missing/q").read_text())
show("O18", lambda: Path("/tmp").read_text())
show("O19", lambda: Path("/tmp/zz_definitely_missing/q").unlink())
show("O20", lambda: Path("/tmp/zz_definitely_missing/q").rmdir())
show("O22", lambda: list(Path("/tmp/zz_definitely_missing/q").iterdir()))
show("O23", lambda: Path("/tmp/zz_definitely_missing/q").rename("/tmp/zz2"))
show("O24", lambda: Path("zz_env/full").unlink())
show("O25", lambda: Path("zz_env/file").rmdir())
show("O26", lambda: Path("/tmp").write_text(5))
show("O27", lambda: Path("/tmp/zz_definitely_missing/q").touch())
try:
    open("/tmp/zz_definitely_missing/q")
except OSError as e:
    emit("O29", type(e).__name__, e, e.errno, e.strerror, e.filename, e.args)
mut e = OSError(2, "msg", "f"); emit("O30", e, e.args, e.errno, e.strerror, e.filename, type(e).__name__)
e = OSError("one"); emit("O31", e, e.args, e.errno, e.strerror, e.filename)
e = OSError(2, "msg"); emit("O32", e, e.args, e.errno, e.filename)
e = FileNotFoundError(2, "No such file or directory", "x"); emit("O33", e, isinstance(e, OSError))
st = Path("zz_env/file").stat(); emit("O34", type(st).__name__, st.st_size > 0, isinstance(st.st_mtime, float), isinstance(st.st_size, int), st.st_mode & 0o170000 == 0o100000, len(st), st[6] == st.st_size)

# ---- open / files
for f in [lambda: open("/nonexistent_zz/x"), lambda: open("/tmp"), lambda: open("/tmp/x.txt", "q"), lambda: open(5.5), lambda: open("/nonexistent_zz/x", "w"), lambda: open("/etc/passwd", "rb").read(0), lambda: open("/etc/passwd", "rw"), lambda: open("/etc/passwd", "rb", encoding="utf-8")]:
    show("F", f)
mut p = Path("zz_probe.txt"); n = p.write_text("a\nb\n"); emit("T1", n, p.read_text(), p.read_bytes(), p.write_text("x\r\ny\n", newline=""), p.read_bytes(), p.write_text("l1\nl2", newline="\r\n"), p.read_bytes(), p.read_text(), p.read_text(newline=""))
emit("T2", p.stat().st_size, p.exists(), p.is_file(), p.is_dir(), p.is_symlink(), p.samefile("zz_probe.txt"))
q = p.rename("zz_probe2.txt"); emit("T3", q, q.exists(), p.exists()); q.unlink(); show("T4", lambda: q.unlink()); emit("T5", q.unlink(missing_ok=True))
with open("f1.txt", "w") as f:
    emit("T6", f.write("héllo\nwörld"), f.mode, f.name, f.encoding, f.closed, repr(f), f.writable(), f.readable())
emit("T7", f.closed, open("f1.txt").read(), open("f1.txt", encoding="latin-1").read(), open("f1.txt", "rb").read())
with open("f1.txt") as f:
    emit("T8", f.readline(), f.readline(), f.readline(), f.tell(), f.seek(0), f.read(3), list(f))
with open("f1.txt", "a") as f:
    f.write("\nmore")
emit("T9", open("f1.txt").readlines(), open("f1.txt").read().splitlines())
with open("f2.bin", "wb") as f:
    emit("T10", f.write(b"\x00\x01ab"), repr(f)[:30], f.mode)
with open("f2.bin", "rb") as f:
    emit("T11", f.read(2), f.read(), f.tell(), f.seek(1), f.read(1), f.readable(), f.writable())
show("T12", lambda: open("f2.bin", "rb").write(b"x"))
show("T13", lambda: open("f1.txt", "w").read())
mut f = open("f1.txt"); f.close(); show("T14", lambda: f.read()); show("T14b", lambda: f.write("x"))
with open("f3.txt", "w", encoding="ascii") as f:
    show("T15", lambda: f.write("é"))
show("T16", lambda: open("f2.bin", "r", encoding="ascii").read())
emit("T17", repr(open("f2.bin", "r", encoding="latin-1").read()), repr(open("f2.bin", errors="replace").read()))
with open("f4.txt", "x") as f:
    f.write("new")
show("T18", lambda: open("f4.txt", "x"))
with open("f4.txt", "r+") as f:
    f.seek(0, 2); f.write("!"); f.seek(0); emit("T19", f.read())
with open("f4.txt", "w+") as f:
    f.write("abc"); f.seek(1); emit("T20", f.read(), f.tell())
f = open("f5.txt", "w"); f.write("unflushed")
emit("T21", os.path.getsize("f5.txt"))
f.flush(); emit("T22", os.path.getsize("f5.txt"), open("f5.txt").read())
emit("T23", list(open("f1.txt")), sorted(os.listdir(".")))
with open("f6.txt", "w") as f:
    print("hello", "there", sep="-", file=f)
    print(1, 2, end="!", file=f)
emit("T24", repr(open("f6.txt").read()))

unsafe:
    mut sio = io.StringIO("ab\ncd\n")
    emit("I1", sio.read(1), sio.read(), sio.tell(), sio.seek(0), sio.readline(), sio.readlines(), sio.getvalue(), sio.write("X"), sio.getvalue(), sio.tell())
    sio = io.StringIO(); sio.write("hello\nworld"); emit("I2", sio.getvalue(), list(io.StringIO("a\nb\n")), sio.closed, repr(sio)[:20], sio.seek(0, 2), sio.tell(), sio.truncate(3), sio.getvalue())
    show("I3", lambda: io.StringIO(5))
    show("I4", lambda: io.StringIO("x").write(5))
    sio.close(); show("I5", lambda: sio.getvalue()); show("I5b", lambda: sio.read())
    with io.StringIO("q") as sf: emit("I6", sf.read(), sf.closed)
    emit("I7", sf.closed)
    bio = io.BytesIO(b"ab\ncd"); emit("I8", bio.read(1), bio.read(), bio.getvalue(), bio.seek(0), bio.readline(), bio.write(b"Z"), bio.getvalue(), bio.tell(), list(io.BytesIO(b"1\n2")))
    show("I9", lambda: io.BytesIO("x"))
    show("I10", lambda: io.BytesIO(b"x").write("s"))
    sio = io.StringIO("abc"); emit("I11", sio.seek(1), sio.read(), sio.seek(0), sio.write("Z"), sio.getvalue(), sio.read(), io.StringIO("a\r\nb").readlines(), io.StringIO("a\r\nb", newline="").readlines(), io.StringIO("x").readable(), io.StringIO("x").writable(), io.StringIO("x").seekable())
    emit("I12", isinstance(sio, io.IOBase), isinstance(sio, io.TextIOBase), io.StringIO.__name__, type(sio).__name__, io.StringIO("a").encoding)
    out = io.StringIO(); print("x", 1, file=out); emit("I13", repr(out.getvalue()))


# ---- os / os.path
emit("P1", os.path.join("a", "b", "/c", "d"), os.path.join("a/", "b"), os.path.join("a", ""), os.path.split("a/b/c"), os.path.split("a"), os.path.split("/a"), os.path.splitext("f.tar.gz"), os.path.splitext(".bashrc"), os.path.splitext("a/b.c/d"), os.path.basename("a/b/"), os.path.dirname("a/b/"), os.path.normpath("a//b/../c/./d/"), os.path.normpath("/../a"), os.path.normpath(""), os.path.isabs("/a"), os.path.relpath("/a/b/c", "/a/d"), os.path.commonpath(["/a/b/c", "/a/b/d"]), os.path.commonprefix(["abc", "abd"]), os.path.expanduser("~/x") == os.environ["HOME"].rstrip("/") + "/x", os.path.abspath("x") == os.getcwd() + "/x", os.sep, os.pathsep, os.linesep == "\n", os.name, os.curdir, os.pardir, os.extsep, os.altsep, os.devnull)
show("P2", lambda: os.path.commonpath(["/a", "b"]))
show("P3", lambda: os.path.relpath(""))
show("P4", lambda: os.listdir("/nonexistent_zz"))
show("P5", lambda: os.remove("/nonexistent_zz"))
show("P6", lambda: os.mkdir("/tmp"))
show("P7", lambda: os.rmdir("/nonexistent_zz"))
show("P8", lambda: os.rename("/nonexistent_zz", "/tmp/q"))
show("P9", lambda: os.makedirs("/tmp"))
show("P10", lambda: os.makedirs("/tmp", exist_ok=True))
show("P11", lambda: os.path.getsize("/nonexistent_zz"))
show("P12", lambda: os.stat("/nonexistent_zz"))
show("P13", lambda: os.rmdir("zz_env/full"))
show("P14", lambda: os.remove("zz_env/full"))
show("P15", lambda: os.listdir("/etc/passwd"))
show("P16", lambda: os.path.getsize(Path("/nonexistent_zz")))
emit("P17", os.getcwd() == BASE, os.path.isdir("."), os.path.isfile("f1.txt"), os.path.exists("nope"), os.path.getsize("f1.txt"), os.getenv("HOME") == os.environ["HOME"], os.getenv("ZZ_NOPE", "dflt"), isinstance(os.getpid(), int), os.cpu_count() >= 1, os.system("exit 3"), os.path.samefile("f1.txt", "./f1.txt"), os.strerror(2), os.access("f1.txt", os.R_OK), os.access("nope", os.F_OK))

# ---- directory tree: glob, walk, shutil
d = Path("tree")
d.mkdir(); (d / "sub").mkdir(); (d / "sub" / "deep").mkdir(parents=True, exist_ok=True); (d / "a.py").write_text("1"); (d / "sub" / "b.py").write_text("2"); (d / "sub" / "deep" / "c.txt").write_text("3"); (d / ".hidden").write_text("")
rel = lambda xs: sorted(str(x.relative_to(d)) for x in xs)
emit("G1", rel(d.glob("*")), rel(d.glob("*.py")), rel(d.rglob("*.py")), rel(d.glob("**/*.py")), rel(d.glob("**/")), rel(d.rglob("*")), rel(d.glob("sub/*")), rel(d.glob("*/")), rel(d.glob("**")), rel(d.glob("sub/deep/c.txt")), rel(d.glob("nope/*")), rel(d.rglob("deep")))
emit("G2", rel(d.iterdir()), [(str(Path(r).relative_to(d)), sorted(ds), sorted(fs)) for r, ds, fs in os.walk(d)], sorted(os.listdir(d)))
emit("G3", [(str(r.relative_to(d)), sorted(ds), sorted(fs)) for r, ds, fs in d.walk()], [(str(Path(r).relative_to(d)), sorted(ds), sorted(fs)) for r, ds, fs in os.walk(d, topdown=False)])
show("G4", lambda: d.rmdir())
show("G5", lambda: (d / "a.py").mkdir())
show("G6", lambda: (d / "sub" / "deep" / "c.txt").touch())
emit("G7", sorted(glob.glob("tree/*.py")), sorted(glob.glob("tree/**/*.py", recursive=True)), sorted(glob.glob("*.py", root_dir="tree")), sorted(glob.glob("tree/*")), glob.glob("tree/nope*"), sorted(glob.glob("tree/**", recursive=True)), glob.escape("a[b]*"), sorted(glob.glob("tree/.*")), sorted(glob.glob("tree/*", include_hidden=True)), glob.glob("tree"), glob.glob("tree/"))
emit("S1", shutil.copy("tree/a.py", "copy.py"), shutil.copy("tree/a.py", "tree/sub"), shutil.copyfile("tree/a.py", "cf.py"), shutil.copy2("tree/a.py", "c2.py"), open("copy.py").read())
emit("S2", shutil.copytree("tree", "tree2"), sorted(os.listdir("tree2")), shutil.move("tree2", "moved"), os.path.isdir("moved"), shutil.move("cf.py", "moved"), sorted(os.listdir("moved")))
show("S3", lambda: shutil.copyfile("nope", "x"))
show("S4", lambda: shutil.copytree("tree", "moved"))
show("S5", lambda: shutil.copy("tree", "x"))
show("S6", lambda: shutil.rmtree("tree/a.py"))
show("S7", lambda: shutil.copyfile("tree/a.py", "tree/a.py"))
emit("S8", shutil.which("python3.13") is not None, shutil.which("definitely_not_a_cmd_zz"), shutil.rmtree("moved"), os.path.exists("moved"))
show("S9", lambda: shutil.rmtree(Path("nope_dir")))
shutil.rmtree(d); emit("S10", d.exists())

# ---- tempfile
td = tempfile.mkdtemp(); emit("X1", td.startswith(tempfile.gettempdir() + "/tmp"), len(os.path.basename(td)), os.path.isdir(td)); os.rmdir(td)
with tempfile.TemporaryDirectory() as t: emit("X2", os.path.isdir(t), type(t).__name__, t.startswith("/tmp/tmp"))
emit("X3", os.path.exists(t))
fd, fp = tempfile.mkstemp(suffix=".txt"); emit("X4", type(fd).__name__, fp.endswith(".txt"), os.path.isfile(fp)); os.close(fd); os.remove(fp)
with tempfile.NamedTemporaryFile(mode="w+", suffix=".log", delete=False) as f: f.write("hi"); f.seek(0); emit("X5", f.read(), f.name.endswith(".log"), os.path.isfile(f.name), type(f).__name__)
emit("X6", os.path.isfile(f.name)); os.remove(f.name)
with tempfile.NamedTemporaryFile() as f: nm = f.name; emit("X7", os.path.isfile(nm), f.mode)
emit("X8", os.path.isfile(nm), tempfile.gettempdir(), tempfile.tempdir)
os.chdir("/tmp")
shutil.rmtree(BASE)
emit("DONE", os.path.exists(BASE))
"#;
        let Some(expected) = python313_transcript(probe, "__lines") else {
            if std::env::var_os("TYC_REQUIRE_PYTHON").is_some() {
                panic!("python3.13 is required as the oracle (TYC_REQUIRE_PYTHON is set)");
            }
            eprintln!("skipping filesystem_modules_match_cpython: no python3.13 on PATH");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("transcript.txt");
        let src = format!(
            "{probe}\nwith open({path:?}, \"w\", encoding=\"utf-8\") as _out:\n    _out.write(\"\\n\".join(__lines))\n",
            path = out_path.display().to_string()
        );
        assert_eq!(run_capturing(&src).unwrap(), 0);
        let got = std::fs::read_to_string(&out_path).unwrap();
        // CPython spells the default text encoding per host ("utf-8" under
        // glibc, "UTF-8" on macOS); the VM's `io` shim lowercases it. Fold
        // the case so the test compares file semantics, not locale naming —
        // the spelling difference is documented in docs/vm.md.
        let normalise = |s: String| s.replace("utf-8", "UTF-8");
        let (got, expected) = (normalise(got), normalise(expected));
        assert_eq!(
            got, expected,
            "VM transcript differs from the hosting python3.13"
        );
    }

    #[test]
    fn codecs_slices_and_bytes_find_match_cpython() {
        // `str.encode` / `bytes.decode` / `str(bytes, enc)` / `bytes(str, enc)` with
        // the CPython error handlers and messages, `slice` objects, and
        // `bytes.find(sub, start, end)` — expected text printed by python3.13.
        let src = r#"
__lines: list[str] = []

def emit(*parts: object) -> None:
    __lines.append(" ".join(str(p) for p in parts))

def show(label: str, f: object) -> None:
    try:
        emit(label, repr(f()))
    except Exception as e:
        emit(label, type(e).__name__, e)

show("C1", lambda: "héllo".encode("utf-8"))
show("C2", lambda: "héllo".encode("ascii"))
show("C3", lambda: "héllo".encode("ascii", "ignore"))
show("C4", lambda: "héllo".encode("ascii", errors="replace"))
show("C5", lambda: "héllo".encode(encoding="ascii", errors="backslashreplace"))
show("C6", lambda: "héllo".encode("latin-1"))
show("C7", lambda: "日本".encode("latin-1"))
show("C8", lambda: "hi".encode("utf-16"))
show("C12", lambda: "x".encode("nope"))
show("C13", lambda: b"\xff\xfeh\x00i\x00".decode("utf-16"))
show("C14", lambda: b"h\xc3\xa9".decode("utf-8"))
show("C15", lambda: b"h\xff".decode("utf-8"))
show("C16", lambda: b"h\xc3".decode("utf-8"))
show("C17", lambda: b"\xc3\x28".decode("utf-8"))
show("C18", lambda: b"h\xff".decode("utf-8", "ignore"))
show("C19", lambda: b"h\xff".decode("utf-8", errors="replace"))
show("C20", lambda: b"h\xff".decode(encoding="utf-8", errors="backslashreplace"))
show("C21", lambda: b"h\xe9".decode("latin-1"))
show("C22", lambda: b"h\xe9".decode("ascii"))
show("C24", lambda: "x".encode("UTF8") + "y".encode("Utf_8") + "z".encode("latin1") + "w".encode("ISO-8859-1") + "v".encode("us-ascii") + "t".encode("utf-8-sig"))
show("C25", lambda: b"\xef\xbb\xbfhi".decode("utf-8-sig"))
show("C31", lambda: b"ab\xff\xfecd".decode("utf-8", "replace"))
show("C32", lambda: b"\xe9".decode())
show("C33", lambda: "é".encode())
show("C37", lambda: b"h\x00i\x00".decode("utf-16-le") + b"\x00h\x00i".decode("utf-16-be"))
show("C38", lambda: b"h\x00i".decode("utf-16"))
show("C40", lambda: str(b"h\xe9", "latin-1") + str(b"ok", encoding="ascii"))
show("C41", lambda: bytes("é", "utf-8") + bytes("é", "latin-1"))
show("C43", lambda: bytes("é"))
show("C44", lambda: (b"abcabc".find(b"c", 3), b"abcabc".find(b"c", 3, 5), b"abcabc".index(b"a", 1), b"abc".find(b"z", 1)))
show("C45", lambda: b"abcabc".index(b"z", 1))
show("C46", lambda: (repr(slice(1, 5, 2)), slice(3).start, slice(3).stop, slice(1, 5, 2).indices(10), slice(-1).indices(5), slice(None, None, -1).indices(4), type(slice(1, 2)).__name__, isinstance(slice(1, 2), tuple), isinstance(slice(1, 2), slice), slice(None).indices(3), slice(2, 100).indices(5)))
show("C47", lambda: slice(1, 2, 0).indices(5))
show("C48", lambda: (hasattr(5, "__fspath__"), hasattr("s", "__len__"), hasattr(5, "nope")))

expected = """C1 b'h\\xc3\\xa9llo'
C2 UnicodeEncodeError 'ascii' codec can't encode character '\\xe9' in position 1: ordinal not in range(128)
C3 b'hllo'
C4 b'h?llo'
C5 b'h\\\\xe9llo'
C6 b'h\\xe9llo'
C7 UnicodeEncodeError 'latin-1' codec can't encode characters in position 0-1: ordinal not in range(256)
C8 b'\\xff\\xfeh\\x00i\\x00'
C12 LookupError unknown encoding: nope
C13 'hi'
C14 'hé'
C15 UnicodeDecodeError 'utf-8' codec can't decode byte 0xff in position 1: invalid start byte
C16 UnicodeDecodeError 'utf-8' codec can't decode byte 0xc3 in position 1: unexpected end of data
C17 UnicodeDecodeError 'utf-8' codec can't decode byte 0xc3 in position 0: invalid continuation byte
C18 'h'
C19 'h�'
C20 'h\\\\xff'
C21 'hé'
C22 UnicodeDecodeError 'ascii' codec can't decode byte 0xe9 in position 1: ordinal not in range(128)
C24 b'xyzwv\\xef\\xbb\\xbft'
C25 'hi'
C31 'ab��cd'
C32 UnicodeDecodeError 'utf-8' codec can't decode byte 0xe9 in position 0: unexpected end of data
C33 b'\\xc3\\xa9'
C37 'hihi'
C38 UnicodeDecodeError 'utf-16-le' codec can't decode byte 0x69 in position 2: truncated data
C40 'héok'
C41 b'\\xc3\\xa9\\xe9'
C43 TypeError string argument without an encoding
C44 (5, -1, 3, -1)
C45 ValueError subsection not found
C46 ('slice(1, 5, 2)', None, 3, (1, 5, 2), (0, 4, 1), (3, -1, -1), 'slice', False, True, (0, 3, 1), (2, 5, 1))
C47 ValueError slice step cannot be zero
C48 (False, True, False)"""
got = "\n".join(__lines)
if got != expected:
    raise AssertionError("mismatch:\n" + got + "\n--- want ---\n" + expected)
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
elems = list(c.elements())
if len(elems) != 6:
    raise ValueError("elements() total count wrong")
if elems != ["a", "a", "a", "b", "b", "c"]:
    raise ValueError("elements() order wrong: " + str(elems))
try:
    len(c.elements())
except TypeError:
    pass
else:
    raise ValueError("elements() must be an iterator (CPython: itertools.chain), not a sequence")
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

    /// The builtin surface a program reaches for that the VM was missing:
    /// the `numbers` tower on int/float, the string predicates, the unbound
    /// method form of a builtin type, `issubclass`, function attributes, and
    /// class objects as dict keys.
    #[test]
    fn builtin_type_surface_matches_cpython() {
        let src = r#"
def main() -> None:
    if not "hi".isascii() or not "".isascii() or "é".isascii():
        raise AssertionError("isascii wrong")
    if not "a_b1".isidentifier() or "1a".isidentifier() or "".isidentifier():
        raise AssertionError("isidentifier wrong")
    if not "a b".isprintable() or "a\tb".isprintable() or not "".isprintable():
        raise AssertionError("isprintable wrong")
    if (5).real != 5 or (5).imag != 0 or (5).numerator != 5 or (5).denominator != 1:
        raise AssertionError("int numeric tower wrong")
    if (5).conjugate() != 5 or (2.5).conjugate() != 2.5 or (2.5).real != 2.5:
        raise AssertionError("conjugate/real wrong")
    if int.from_bytes(b"\x01\x02", "big") != 258:
        raise AssertionError("int.from_bytes big-endian wrong")
    if int.from_bytes(b"\x01\x02", "little") != 513:
        raise AssertionError("int.from_bytes little-endian wrong")
    # The unbound method form CPython exposes on every builtin type.
    if str.upper("ab") != "AB" or list.count([1, 1, 2], 1) != 2:
        raise AssertionError("unbound builtin method wrong")
    if dict.get({"a": 1}, "a") != 1 or str.__name__ != "str":
        raise AssertionError("unbound dict.get / type __name__ wrong")
    if not issubclass(ValueError, Exception) or issubclass(Exception, ValueError):
        raise AssertionError("issubclass on builtin exceptions wrong")
    if not issubclass(bool, object):
        raise AssertionError("everything subclasses object")

class A:
    pass

class B(A):
    pass

def tagged() -> int:
    return 1

def check_more() -> None:
    if not issubclass(B, A) or issubclass(A, B):
        raise AssertionError("issubclass on user classes wrong")
    # A class object is hashable by identity, which is what makes a
    # type-keyed registry work.
    let registry: dict[object, str] = {A: "a", B: "b", int: "i"}
    if registry[A] != "a" or registry[B] != "b" or registry[int] != "i":
        raise AssertionError("class-keyed dict wrong")
    # A function has a `__dict__` a decorator can publish API on.
    tagged.marker = 7
    if tagged.marker != 7 or tagged.__dict__ != {"marker": 7}:
        raise AssertionError("function attribute wrong")

main()
check_more()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// The stdlib shims added for the differential corpus, checked against
    /// the values CPython 3.13 produces.
    #[test]
    fn added_stdlib_shims_match_cpython() {
        let src = r#"
import base64
import bisect
import csv
import functools
import io
import operator
import string
import sys
from contextlib import ExitStack, nullcontext, redirect_stdout, suppress

def cmp(a: int, b: int) -> int:
    return (a > b) - (a < b)

def main() -> None:
    if base64.b64encode(b"hello") != b"aGVsbG8=" or base64.b64decode(b"aGVsbG8=") != b"hello":
        raise AssertionError("base64 round-trip wrong")
    if base64.urlsafe_b64encode(b"\xfb\xff") != b"-_8=" or base64.b16encode(b"ab") != b"6162":
        raise AssertionError("base64 alphabet variants wrong")
    mut xs: list[int] = [1, 3, 5, 7]
    if bisect.bisect_left(xs, 5) != 2 or bisect.bisect_right(xs, 5) != 3:
        raise AssertionError("bisect wrong")
    bisect.insort(xs, 4)
    if xs != [1, 3, 4, 5, 7]:
        raise AssertionError("insort wrong")
    if string.digits != "0123456789" or string.capwords("a b") != "A B":
        raise AssertionError("string constants wrong")
    if string.Template("$a-${b}").substitute(a="x", b="y") != "x-y":
        raise AssertionError("string.Template wrong")
    if string.Template("$a-$b").safe_substitute(a="x") != "x-$b":
        raise AssertionError("safe_substitute wrong")
    if operator.add(2, 3) != 5 or operator.itemgetter(1)([9, 8]) != 8:
        raise AssertionError("operator wrong")
    if operator.attrgetter("real")(5) != 5 or operator.methodcaller("upper")("ab") != "AB":
        raise AssertionError("operator getters wrong")
    let buf = io.StringIO()
    csv.writer(buf).writerow(["a", 'q"x', "c,d"])
    if buf.getvalue() != 'a,"q""x","c,d"\r\n':
        raise AssertionError("csv writer wrong: " + repr(buf.getvalue()))
    if list(csv.reader(io.StringIO('a,"q""x","c,d"\r\n'))) != [["a", 'q"x', "c,d"]]:
        raise AssertionError("csv reader wrong")
    if sorted([3, 1, 2], key=functools.cmp_to_key(cmp)) != [1, 2, 3]:
        raise AssertionError("cmp_to_key wrong")
    if functools.partial(pow, exp=2)(3) != 9:
        raise AssertionError("partial keyword binding wrong")
    if not isinstance(sys.modules, dict) or "sys" not in sys.modules:
        raise AssertionError("sys.modules wrong")
    with suppress(ValueError):
        raise ValueError("swallowed")
    let cap = io.StringIO()
    with redirect_stdout(cap):
        print("captured")
    if cap.getvalue() != "captured\n":
        raise AssertionError("redirect_stdout wrong: " + repr(cap.getvalue()))
    with nullcontext(7) as v:
        if v != 7:
            raise AssertionError("nullcontext wrong")
    mut log: list[str] = []
    with ExitStack() as stack:
        stack.callback(lambda: log.append("cleanup"))
        log.append("body")
    if log != ["body", "cleanup"]:
        raise AssertionError("ExitStack wrong")
    try:
        raise KeyError("k")
    except KeyError:
        let info = sys.exc_info()
        if info[0].__name__ != "KeyError" or info[2] is not None:
            raise AssertionError("exc_info wrong")
    if sys.exc_info()[0] is not None:
        raise AssertionError("exc_info outside a handler must be all-None")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `lazy let` must defer to first *use*, `%(key)s` must read a mapping,
    /// `sys.modules` must carry `__main__`, and `eval` must exist.
    #[test]
    fn deferred_binding_and_the_remaining_builtins_match_cpython() {
        let src = r#"
import sys

lazy let SLOW: int = expensive()

def expensive() -> int:
    log.append("computed")
    return 42

mut log: list[str] = []

def main() -> None:
    # The initialiser is lowered above `def expensive`, so running it at the
    # binding raised `NameError` on a program `tyc build` runs fine.
    if log != []:
        raise AssertionError("lazy let must not run its factory at the binding")
    if SLOW != 42 or SLOW + 1 != 43 or str(SLOW) != "42":
        raise AssertionError("lazy let value wrong")
    if log != ["computed"]:
        raise AssertionError("lazy let must materialise exactly once: " + str(log))

    if "%(a)s-%(b)d" % {"a": "x", "b": 2} != "x-2":
        raise AssertionError("printf mapping key wrong")
    if "__main__" not in sys.modules:
        raise AssertionError("sys.modules must carry the entry module")
    if eval("1 + 2") != 3 or eval("'a' * 3") != "aaa":
        raise AssertionError("eval wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// The async iteration and context-manager protocols, and the enum /
    /// set / dict operator corners a real program reaches.
    #[test]
    fn async_protocols_and_operator_corners_match_cpython() {
        let src = r#"
import asyncio
import enum
from contextlib import asynccontextmanager
from typing import AsyncIterator

class Colour(enum.IntEnum):
    RED = 1
    BLUE = 2

class Perm(enum.IntFlag):
    READ = 1
    WRITE = 2

plain class Counter:
    def __init__(self, n: int) -> None:
        self.n = n
        self.i = 0

    def __aiter__(self) -> object:
        return self

    async def __anext__(self) -> int:
        if self.i >= self.n:
            raise StopAsyncIteration()
        self.i = self.i + 1
        return self.i

@asynccontextmanager
async def ctx(name: str) -> AsyncIterator[str]:
    log.append("enter:" + name)
    yield name
    log.append("exit:" + name)

mut log: list[str] = []

async def run() -> list[int]:
    mut seen: list[int] = []
    # A hand-written async iterator: `__aiter__` + `__anext__`, which the
    # VM did not recognise at all.
    async for v in Counter(3):
        seen.append(v)
    # `@asynccontextmanager` was an identity decorator, so this raised
    # "does not support the asynchronous context manager protocol".
    async with ctx("db") as c:
        log.append("using:" + c)
    return seen

def main() -> None:
    if asyncio.run(run()) != [1, 2, 3]:
        raise AssertionError("async iteration wrong")
    # The teardown after the `yield` must run AFTER the body, not before.
    if log != ["enter:db", "using:db", "exit:db"]:
        raise AssertionError("async context manager order wrong: " + str(log))

    # Set comparison is subset/superset, not ordering.
    if not ({1, 2} <= {1, 2, 3}) or not ({1} < {1, 2}) or ({1, 2} < {1, 2}):
        raise AssertionError("set subset comparison wrong")
    if not ({1, 2} >= {1}) or ({1} > {1, 2}):
        raise AssertionError("set superset comparison wrong")
    # PEP 584 dict merge.
    if ({"a": 1} | {"b": 2}) != {"a": 1, "b": 2}:
        raise AssertionError("dict merge wrong")
    # A value-mixin enum member IS its value.
    if int(Colour.RED) != 1 or -Colour.RED != -1 or Colour.RED + 1 != 2:
        raise AssertionError("IntEnum arithmetic wrong")
    if Perm.READ not in (Perm.READ | Perm.WRITE):
        raise AssertionError("IntFlag membership wrong")
    # …but a bare int is still not a container, as in CPython.
    mut raised: str = ""
    try:
        if 1 in 3:
            pass
    except TypeError as e:
        raised = str(e)
    if raised != "argument of type 'int' is not iterable":
        raise AssertionError("bare int containment must raise: " + raised)

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `bytearray` was missing entirely — `Value::Bytes` is immutable, so
    /// the mutable sibling is a shim class marked as a `bytearray` for
    /// `isinstance`. Checked against CPython 3.13.
    #[test]
    fn bytearray_matches_cpython() {
        let src = r#"
def main() -> None:
    mut arr = bytearray(b"abc")
    arr.append(100)
    arr[0] = 65
    if repr(arr) != "bytearray(b'Abcd')" or bytes(arr) != b"Abcd":
        raise AssertionError("repr / bytes wrong: " + repr(arr))
    if len(arr) != 4 or arr[1] != 98 or list(arr) != [65, 98, 99, 100]:
        raise AssertionError("sequence protocol wrong")
    if repr(arr[1:3]) != "bytearray(b'bc')" or arr.decode("ascii") != "Abcd":
        raise AssertionError("slice / decode wrong")
    if arr.hex() != "41626364":
        raise AssertionError("hex wrong")
    if repr(bytearray(3)) != "bytearray(b'\\x00\\x00\\x00')":
        raise AssertionError("bytearray(int) wrong: " + repr(bytearray(3)))
    if bytearray([1, 2]) != bytearray(b"\x01\x02"):
        raise AssertionError("bytearray(iterable) wrong")
    if arr != b"Abcd" or b"Abcd" != arr:
        raise AssertionError("comparison with bytes wrong")
    arr.extend(b"ef")
    if repr(arr + b"!") != "bytearray(b'Abcdef!')":
        raise AssertionError("concatenation wrong")
    if repr(arr.upper()) != "bytearray(b'ABCDEF')" or arr.find(b"cd") != 2:
        raise AssertionError("read methods wrong")
    if not arr.startswith(b"Ab") or b"x" in arr or 65 not in arr:
        raise AssertionError("containment wrong")
    if arr.pop() != 102 or repr(arr) != "bytearray(b'Abcde')":
        raise AssertionError("pop wrong")
    if repr(bytearray.fromhex("6162")) != "bytearray(b'ab')":
        raise AssertionError("fromhex wrong")
    if not isinstance(arr, bytearray) or type(arr).__name__ != "bytearray":
        raise AssertionError("isinstance / type name wrong")
    mut raised: str = ""
    try:
        arr[0] = 999
    except ValueError as e:
        raised = str(e)
    if raised != "byte must be in range(0, 256)":
        raise AssertionError("range check wrong: " + raised)
    # `bytes` containment, which the VM rejected outright.
    if 97 not in b"abc" or b"bc" not in b"abc" or b"z" in b"abc":
        raise AssertionError("bytes containment wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// The odd corners a formatting-heavy program hits: `str.format`
    /// conversions and accessors, `str.center`'s padding bias, `float.hex`
    /// and the `numbers` ratio methods.
    #[test]
    fn formatting_and_numeric_corners_match_cpython() {
        let src = r#"
plain class Point:
    def __init__(self) -> None:
        self.x = 7

def main() -> None:
    if "{name!r:>8}".format(name="n") != "     'n'":
        raise AssertionError("!r conversion wrong: " + "{name!r:>8}".format(name="n"))
    if "{0!s}-{0!r}".format("a") != "a-'a'":
        raise AssertionError("!s / !r wrong")
    if "{p.x}".format(p=Point()) != "7":
        raise AssertionError("attribute accessor wrong")
    if "{d[k]}-{xs[1]}".format(d={"k": 1}, xs=[9, 8]) != "1-8":
        raise AssertionError("index accessor wrong")
    # CPython biases the extra pad character right only when both the
    # padding and the width are odd.
    if "ab".center(5) != "  ab " or "ab".center(6) != "  ab  ":
        raise AssertionError("center bias wrong: " + repr("ab".center(5)))
    if "ab".center(5, "*") != "**ab*" or "ab".center(7, "-") != "---ab--":
        raise AssertionError("center with fill wrong")
    if (2.5).hex() != "0x1.4000000000000p+1" or (0.0).hex() != "0x0.0p+0":
        raise AssertionError("float.hex wrong: " + (2.5).hex())
    if (-0.0).hex() != "-0x0.0p+0" or (1.0).hex() != "0x1.0000000000000p+0":
        raise AssertionError("float.hex sign / exponent wrong")
    if (5).as_integer_ratio() != (5, 1) or (2.5).as_integer_ratio() != (5, 2):
        raise AssertionError("as_integer_ratio wrong")
    if bytes.fromhex("6162") != b"ab":
        raise AssertionError("bytes.fromhex wrong")

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }

    /// `class X(NamedTuple)` is a tuple and `class X(TypedDict)` is a dict in
    /// CPython. The VM built a plain instance for both, so `p[0]` and
    /// `u["name"]` raised `'instance' object is not subscriptable` on
    /// programs `tyc build` runs fine.
    #[test]
    fn typing_records_are_tuples_and_dicts_like_cpython() {
        let src = r#"
from typing import NamedTuple, TypedDict

class Point(NamedTuple):
    x: int
    y: int = 5

class User(TypedDict):
    id: int
    name: str

def main() -> None:
    let p: Point = Point(x=1, y=2)
    if (p.x, p.y) != (1, 2) or p[0] != 1 or p[1] != 2:
        raise AssertionError("NamedTuple field / index access wrong")
    if len(p) != 2 or list(p) != [1, 2] or p != (1, 2):
        raise AssertionError("NamedTuple tuple protocol wrong")
    if repr(p) != "Point(x=1, y=2)":
        raise AssertionError("NamedTuple repr wrong: " + repr(p))
    if p._asdict() != {"x": 1, "y": 2} or p._replace(y=9) != (1, 9):
        raise AssertionError("NamedTuple _asdict/_replace wrong")
    if Point(x=3) != (3, 5):
        raise AssertionError("NamedTuple field default wrong")
    if Point._fields != ("x", "y"):
        raise AssertionError("NamedTuple _fields wrong")

    let u: User = User(id=1, name="Alice")
    if u["name"] != "Alice" or u["id"] != 1:
        raise AssertionError("TypedDict subscript wrong")
    if not isinstance(u, dict) or sorted(u.keys()) != ["id", "name"]:
        raise AssertionError("TypedDict must construct a plain dict")
    if repr(u) != "{'id': 1, 'name': 'Alice'}":
        raise AssertionError("TypedDict repr wrong: " + repr(u))

main()
"#;
        assert_eq!(run_capturing(src).unwrap(), 0);
    }
}
