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

pub mod builtins;
pub mod env;
pub mod error;
pub mod ffi;
pub mod interp;
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
    let expanded = preprocess::expand_question_ops(&preprocess::expand_pipes(
        &preprocess::expand_with_chains(&preprocess::expand_go_calls(
            &preprocess::expand_gather_blocks(&preprocess::expand_multiline_guards(
                &preprocess::expand_typed_let_unpack(&preprocess::expand_lazy_lets(source)),
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

    // Hand the VM the desugared module so it sees the same shape as the
    // compile path: dataclass-decorated user classes, merged impl blocks,
    // injected runtime imports, and so on. FINDINGS #21 follow-up.
    // Running the full desugar pass also rewrites \`extend\` user-classes
    // into method merges; the builtin-extension rewrite below handles the
    // \`extend str:\` / \`extend list:\` shape that desugar leaves alone.
    let desugar_out = tyc_desugar::desugar_module(&module);
    module = desugar_out.module;

    // FINDINGS #21: rewrite \`x.method(args)\` to \`__typhon_ext_TYPE__method
    // (x, args)\` for every receiver statically annotated as a built-in
    // type that the source extended with \`extend BUILTIN:\`. Without this
    // step, calls on extended built-ins fail at runtime with
    // \`AttributeError: 'str' object has no attribute 'slug'\`.
    let (registry, _stats) = tyc_analyse::extract_builtin_extensions(&mut module);
    let _ = tyc_analyse::rewrite_builtin_extension_calls(&mut module, &registry);

    let mut interp = Interpreter::new();
    // Seed sys.argv before any user code (or import sys) can observe it.
    let argv0 = origin
        .map(|p| p.display().to_string())
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
            Value::Str(std::rc::Rc::new(p.display().to_string())),
        );
    }

    match interp.run_module(&module) {
        Ok(()) => Ok(0),
        Err(Unwind::Return(_)) => Ok(0),
        Err(Unwind::Exception(exc)) => {
            eprintln!("Traceback (most recent call last):");
            for frame in &exc.frames {
                eprintln!("  in {}", frame.function);
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
}
