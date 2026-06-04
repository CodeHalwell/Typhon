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
}
