//! A name-keyed table (`function_arity_info`, `function_signatures`,
//! `function_type_bounds`, `contextmanager_yields`, the cross-module
//! source-name seeds) is consulted only when the call or annotation actually
//! resolves to that entry — a method, a callable field, a builtin method, a
//! parameter or a local of the same name is checked against its own type.
//! Also covers the class-scope visibility rule those lookups depend on, and
//! the positional-only `dict.get`.

use super::*;
use tyc_resolve::{resolve_module_with, ResolveOptions};
use tyc_syntax::preprocess::{line_byte_starts, preprocess};

/// The checker as the CLI runs it, with `class!` bindings tagged raw.
fn check(src: &str) -> Diagnostics {
    let prep = preprocess(src);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .unwrap()
        .into_syntax();
    let options = ResolveOptions {
        raw_class_byte_starts: line_byte_starts(&prep.python_source, &prep.raw_class_lines),
        original_source: Some(src.to_owned()),
        ..ResolveOptions::default()
    };
    let (resolved, _) =
        resolve_module_with("<test>".to_owned(), &prep.python_source, &module, options);
    check_module_with(
        "<test>",
        &prep.python_source,
        &resolved,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
        &prep.impl_distributed_lines,
    )
}

fn messages(d: &Diagnostics) -> Vec<String> {
    d.errors().iter().map(|e| e.to_string()).collect()
}

fn assert_clean(src: &str) {
    let d = check(src);
    assert!(
        d.errors().is_empty(),
        "expected no errors, got {:?}",
        messages(&d)
    );
}

/// The errors `src` produces, which must all match `want`; returns them.
fn only_errors(src: &str, want: impl Fn(&TycError) -> bool) -> Vec<String> {
    let d = check(src);
    assert!(
        d.errors().iter().all(want),
        "unexpected errors: {:?}",
        messages(&d)
    );
    messages(&d)
}

fn is_missing(e: &TycError, name: &str, param: &str) -> bool {
    matches!(e, TycError::MissingArgument { name: n, missing, .. }
        if n == name && missing.iter().any(|m| m == param))
}

// ── Attribute callees never key the module-level function table ─────────

#[test]
fn builtin_methods_ignore_same_named_module_functions() {
    // r01: `table.get(key)` inside `def get(table, key, default=0)`.
    assert_clean(
        r#"
def get(table: dict[str, int], key: str, default: int = 0) -> int:
    let found: int? = table.get(key)
    if found is not None:
        return found
    return default

def main() -> None:
    print(get({"x": 1}, "x"))

main()
"#,
    );
    // r02 / r03 / r16 / r23: list.append, str.split, int.is_integer and
    // literal receivers beside module functions of the same names.
    assert_clean(
        r#"
def append(xs: list[int], x: int, y: int) -> None:
    xs.append(x)
    xs.append(y)

def split(text: str, sep: str, limit: int) -> list[str]:
    return text.split(sep)

def is_integer(text: str) -> bool:
    return text.isdigit()

def join(parts: list[str], sep: str) -> str:
    return sep.join(parts)

def count(xs: list[int], x: int, start: int) -> int:
    return [1, 2].count(x)

def main() -> None:
    mut xs: list[int] = []
    append(xs, 1, 2)
    let s: str = "x y"
    let n: int = 4
    print(xs, split("a,b", ",", 1), s.split(), n.is_integer(), is_integer("12"))
    print(", ".join(["a", "b"]), join(["c"], "-"), count([1], 1, 0))

main()
"#,
    );
}

#[test]
fn builtin_method_keywords_ignore_module_function_parameters() {
    // r04, and the sibling report's `xs.sort(reverse=True)` / `d.pop("a")`.
    assert_clean(
        r#"
def split(text: str, delim: str) -> list[str]:
    return text.split(sep=delim)

def sort(items: list[int]) -> list[int]:
    return sorted(items)

def pop(key: str, *, strict: bool) -> int:
    return 0

def main() -> None:
    mut xs: list[int] = [3, 1]
    mut d: dict[str, int] = {"a": 1}
    xs.sort(reverse=True)
    print(split("a,b", ","), sort([2, 1]), d.pop("a"), pop("a", strict=True))

main()
"#,
    );
}

#[test]
fn builtin_method_arguments_are_not_typed_by_a_module_vararg() {
    // r17: `xs.append(1)` was checked against `def append(*items: str)`.
    assert_clean(
        r#"
def append(*items: str) -> str:
    return ",".join(items)

def split(*parts: int) -> int:
    return len(parts)

def main() -> None:
    mut xs: list[int] = []
    xs.append(1)
    print(xs, append("a", "b"), "a,b".split(","), split(1, 2))

main()
"#,
    );
}

#[test]
fn builtin_method_arity_is_not_relaxed_by_a_module_function() {
    // r05: CPython rejects `xs.append(1, 2)`; a module `def append(a, b)`
    // used to make it pass. Same verdict as without the module function.
    let with_module = r#"
def append(a: int, b: int) -> int:
    return a + b

def main() -> None:
    mut xs: list[int] = []
    xs.append(1, 2)
    print(xs, append(1, 2))

main()
"#;
    let without = r#"
def main() -> None:
    mut xs: list[int] = []
    xs.append(1, 2)
    print(xs)

main()
"#;
    for src in [with_module, without] {
        let errors = only_errors(src, |e| matches!(e, TycError::WrongArgCount { .. }));
        assert_eq!(errors.len(), 1, "{errors:?}");
    }
}

#[test]
fn method_kwargs_value_type_comes_from_the_method() {
    // r07 / k10: the method's `**opts: str`, not the module's `**opts: int`.
    let src = |arg: &str| {
        format!(
            r#"
class Settings:
    data: dict[str, str]

impl Settings:
    def configure(self, **opts: str) -> None:
        for k, v in opts.items():
            self.data[k] = v

def configure(**opts: int) -> int:
    return len(opts)

def main() -> None:
    let s: Settings = Settings(data={{}})
    s.configure(mode={arg})
    print(s.data, configure(n=1))

main()
"#
        )
    };
    assert_clean(&src("\"fast\""));
    let errors = only_errors(
        &src("1"),
        |e| matches!(e, TycError::TypeMismatch { expected, .. } if expected == "str"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    // k11: the reverse — a permissive module `**opts: object` no longer
    // masks the method's `**opts: int`.
    let errors = only_errors(
        r#"
class Q:
    n: int

impl Q:
    def build(self, **opts: int) -> int:
        return len(opts)

def build(**opts: object) -> int:
    return len(opts)

def main() -> None:
    print(Q(1).build(a="x"), build(b=2))

main()
"#,
        |e| matches!(e, TycError::TypeMismatch { expected, .. } if expected == "int"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn unbound_method_calls_use_the_method_with_its_receiver_slot() {
    // r08 / r21 / k01 / k02: `User.greet(u, …)`, positionally and by keyword,
    // with and without a colliding module function.
    for module_fn in [
        "",
        "def greet(name: str) -> str:\n    return \"hi \" + name\n",
        "def greet(u: User, prefix: str = \"hi\") -> str:\n    return prefix + u.name\n",
    ] {
        assert_clean(&format!(
            r#"
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

{module_fn}
def main() -> None:
    let u: User = User(name="ann")
    print(User.greet(u, "hello "), User.greet(u, prefix="yo "))

main()
"#
        ));
    }
    let errors = only_errors(
        r#"
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def greet(name: str) -> str:
    return name

def main() -> None:
    let u: User = User(name="ann")
    print(User.greet(u), greet("b"))

main()
"#,
        |e| is_missing(e, "greet", "prefix"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    // The receiver must still be of the class.
    let errors = only_errors(
        r#"
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def main() -> None:
    print(User.greet(3, "x"))

main()
"#,
        |e| matches!(e, TycError::TypeMismatch { expected, .. } if expected == "User"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn implicit_class_methods_take_no_explicit_receiver() {
    // `__init_subclass__` / `__class_getitem__` are class methods without
    // the decorator; `__new__` takes `cls` explicitly.
    assert_clean(
        r#"
class Template:
    pattern: str

impl Template:
    def __init_subclass__(cls) -> None:
        pass
    def __class_getitem__(cls, item: object) -> str:
        return "alias"
    def __new__(cls, pattern: str) -> Template:
        return object.__new__(cls)

Template.__init_subclass__()
print(Template.__class_getitem__(int), Template.__new__(Template, "x"))
"#,
    );
}

#[test]
fn unbound_base_init_with_keywords_is_accepted() {
    // u01: the canonical `Base.__init__(self, name=name)` super-call.
    let src = |call: &str| {
        format!(
            r#"
class! Base:
    name: str

    def __init__(self, name: str) -> None:
        self.name = name

class! Child(Base):
    tag: str

    def __init__(self, name: str, tag: str) -> None:
        {call}
        self.tag = tag

def main() -> None:
    let c: Child = Child("a", "b")
    print(c.name, c.tag)

main()
"#
        )
    };
    assert_clean(&src("Base.__init__(self, name=name)"));
    assert_clean(&src("Base.__init__(self, name)"));
    let errors = only_errors(&src("Base.__init__(self)"), |e| {
        is_missing(e, "__init__", "name")
    });
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn class_qualified_static_and_unbound_generic_calls_stay_clean() {
    // u02 / r29: static methods, interface-qualified and generic-class
    // unbound calls, `self.method`, and module functions by bare name.
    assert_clean(
        r#"
interface Drawable:
    def draw(self, scale: int) -> str

class Box[T]:
    value: T

impl[T] Box[T]:
    def put(self, v: T, note: str) -> None:
        self.value = v

class Sq:
    side: int

impl Sq:
    def draw(self, scale: int) -> str:
        return "sq" + self.redraw(scale)
    def redraw(self, scale: int) -> str:
        return str(self.side * scale)
    @staticmethod
    def unit(scale: int) -> Sq:
        return Sq(side=scale)

class Big(Sq):
    pass

def draw() -> str:
    return "none"

def put(a: int, b: int, c: int) -> None:
    pass

def redraw(a: int, b: int, c: int) -> str:
    return ""

def unit() -> int:
    return 1

def render(d: Drawable) -> str:
    return d.draw(2)

def main() -> None:
    let b: Box[int] = Box(value=1)
    b.put(2, "n")
    Box.put(b, 3, "m")
    let g: Big = Big(side=2)
    print(render(Sq(side=3)), draw(), Sq.unit(4).side, unit(), Sq.draw(g, 3))
    put(1, 2, 3)

main()
"#,
    );
}

#[test]
fn callable_fields_and_properties_ignore_module_functions() {
    // r09 / r12 / r15: a `Callable` field, a property returning a
    // callable and a narrowed optional callback field.
    assert_clean(
        r#"
from typing import Callable

class Handler:
    cb: Callable[[int], int]

class Calc:
    factor: int

impl Calc:
    @property
    def op(self) -> Callable[[int], int]:
        return lambda x: x * self.factor

class Job:
    on_done: Callable[[int], None]?

impl Job:
    def finish(self) -> None:
        if self.on_done is not None:
            self.on_done(42)

def cb(a: int, b: int) -> int:
    return a + b

def op(a: int, b: int, c: int) -> int:
    return a + b + c

def on_done(code: int, msg: str) -> None:
    print(code, msg)

def double(x: int) -> int:
    return x * 2

def report(code: int) -> None:
    print("done", code)

def main() -> None:
    let h: Handler = Handler(cb=double)
    let c: Calc = Calc(factor=3)
    let j: Job = Job(on_done=report)
    j.finish()
    print(h.cb(21), cb(1, 2), c.op(2), op(1, 2, 3))
    on_done(0, "x")

main()
"#,
    );
    // The field's own shape still binds: two arguments to a one-argument
    // callable.
    let errors = only_errors(
        r#"
from typing import Callable

class Handler:
    cb: Callable[[int], int]

def cb(a: int, b: int) -> int:
    return a + b

def double(x: int) -> int:
    return x * 2

def main() -> None:
    let h: Handler = Handler(cb=double)
    print(h.cb(1, 2), cb(1, 2))

main()
"#,
        |e| matches!(e, TycError::WrongArgCount { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn typevar_bound_receivers_use_the_bound_method() {
    // r10 / r20: positionally and by keyword.
    let src = |call: &str| {
        format!(
            r#"
interface Greeter:
    def greet(self, prefix: str) -> str

class English:
    name: str

impl English:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def greet() -> str:
    return "hi"

def run[T: Greeter](x: T) -> str:
    return {call}

def main() -> None:
    print(run(English(name="ann")), greet())

main()
"#
        )
    };
    assert_clean(&src("x.greet(\"hello \")"));
    assert_clean(&src("x.greet(prefix=\"hello \")"));
    let errors = only_errors(&src("x.greet()"), |e| is_missing(e, "greet", "prefix"));
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn receivers_of_every_shape_resolve_their_method() {
    // r24 / r27 / r01 (adversarial): conditional, awaited, walrus,
    // subscripted and call receivers, by keyword, with and without a
    // module-level `greet` that used to make some of them pass.
    for module_fn in ["", "def greet(prefix: str) -> str:\n    return prefix\n"] {
        assert_clean(&format!(
            r#"
import asyncio

class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def make() -> User:
    return User(name="m")

async def fetch() -> User:
    await asyncio.sleep(0)
    return User(name="f")

{module_fn}
async def run(flag: bool) -> None:
    let u: User = User(name="ann")
    let v: User = User(name="bob")
    let users: list[User] = [u]
    let d: dict[str, User] = {{"k": v}}
    print((u if flag else v).greet("0 "), (u if flag else v).greet(prefix="1 "))
    print((u or v).greet(prefix="9 "))
    print(users[0].greet(prefix="2 "), d["k"].greet(prefix="3 "), make().greet(prefix="4 "))
    print((await fetch()).greet(prefix="5 "))
    print((w := make()).greet(prefix="6 "), w.name)
    let t: tuple[User, int] = (User(name="t"), 1)
    print(t[0].greet(prefix="7 "))
    print([y.greet(prefix="8 ") for y in users])

asyncio.run(run(True))
"#
        ));
    }
    let errors = only_errors(
        r#"
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def greet(a: str, b: str, c: str) -> str:
    return a + b + c

def main(flag: bool) -> None:
    let u: User = User(name="ann")
    let v: User = User(name="bob")
    print((u if flag else v).greet(), greet("x", "y", "z"))

main(True)
"#,
        |e| is_missing(e, "greet", "prefix"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn an_alias_of_a_bound_method_calls_the_method() {
    // q02: `let g = u.greet; g(prefix=…)`, with and without a module `g`.
    for module_fn in ["", "def g(prefix: str) -> str:\n    return prefix\n"] {
        assert_clean(&format!(
            r#"
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def run(u: User) -> str:
    let g = u.greet
    return g(prefix="hi ")

{module_fn}
def main() -> None:
    print(run(User(name="b")))

main()
"#
        ));
    }
    let errors = only_errors(
        r#"
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def run(u: User) -> str:
    let g = u.greet
    return g()

def main() -> None:
    print(run(User(name="b")))

main()
"#,
        |e| is_missing(e, "g", "prefix"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn unrelated_names_stay_unaffected() {
    // r18: an `async def get` does not make `d.get(...)` a coroutine; s06: a
    // module-level caller of `get` beside an `impl` method `get`.
    assert_clean(
        r#"
async def get(key: str) -> int:
    return 1

class Store:
    data: dict[str, int]

impl Store:
    def get(self) -> int:
        return 0

def lookup(d: dict[str, int]) -> int:
    let v: int? = d.get("a")
    return v or 0

def main() -> None:
    print(lookup({"a": 1}), Store(data={}).get())

main()
"#,
    );
}

#[test]
fn method_calls_still_check_their_own_arity() {
    // Controls: the fix must not lose a real missing argument or unknown
    // keyword on an instance method.
    let errors = only_errors(
        r#"
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def main() -> None:
    let u: User = User(name="ann")
    print(u.greet(), u.greet(prefx="x"))

main()
"#,
        |e| is_missing(e, "greet", "prefix") || matches!(e, TycError::UnknownKwarg { .. }),
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
}

// ── Name callees: parameters and locals shadowing a module function ──────

#[test]
fn a_parameter_or_local_shadowing_a_module_function_uses_its_own_type() {
    // r25 / r30 and the sibling report's kw-only shadow.
    assert_clean(
        r#"
from typing import Callable

def step(a: int, b: int) -> int:
    return a + b

def fmt(x: int, *, width: int) -> str:
    return str(x)

def apply(step: Callable[[int], int], x: int) -> int:
    return step(x)

def show(fmt: Callable[[int], str]) -> str:
    return fmt(1)

def inc(x: int) -> int:
    return x + 1

def pick() -> Callable[[int], int]:
    return inc

def run() -> int:
    let step: Callable[[int], int] = pick()
    return step(1)

def main() -> None:
    print(apply(inc, 1), run(), show(str), step(1, 2), fmt(1, width=2))

main()
"#,
    );
    let errors = only_errors(
        r#"
from typing import Callable

def step(a: int, b: int) -> int:
    return a + b

def apply(step: Callable[[int], int], x: int) -> int:
    return step(x, x)

def main() -> None:
    print(step(1, 2))

main()
"#,
        |e| matches!(e, TycError::WrongArgCount { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn comprehension_and_walrus_targets_shadowing_a_module_function() {
    // b03 / b04 of the adversarial review.
    assert_clean(
        r#"
from collections.abc import Callable

def g(a: int, b: int) -> int:
    return a + b

def h(a: int, b: int) -> int:
    return a + b

def inc(x: int) -> int:
    return x + 1

def pick() -> Callable[[int], int]?:
    return inc

def main() -> None:
    let fns: list[Callable[[int], int]] = [inc]
    print([g(1) for g in fns], g(1, 2), h(1, 2))
    if (h := pick()) is not None:
        print(h(1))

main()
"#,
    );
}

#[test]
fn module_functions_by_bare_name_keep_their_checks() {
    // Controls: the name gate leaves a real module-level call alone,
    // including `**kwargs` value types and unknown keywords.
    let errors = only_errors(
        r#"
def f(**kwargs: int) -> None:
    print(len(kwargs))

def add(a: int, b: int) -> int:
    return a + b

def main() -> None:
    f(a=1, b="oops")
    print(add(1), add(1, c=2))

main()
"#,
        |e| {
            matches!(e, TycError::TypeMismatch { expected, .. } if expected == "int")
                || is_missing(e, "add", "b")
                || matches!(e, TycError::UnknownKwarg { .. })
        },
    );
    assert_eq!(errors.len(), 3, "{errors:?}");
}

#[test]
fn a_type_guard_shadowed_by_a_parameter_does_not_narrow() {
    // n01 (verify) / b23 (sweep).
    assert_clean(
        r#"
from typing import Callable, TypeGuard

def is_small(v: object) -> TypeGuard[str]:
    return isinstance(v, str)

def check(is_small: Callable[[int], bool], v: int) -> int:
    if is_small(v):
        return v + 1
    return 0

def under_ten(n: int) -> bool:
    return n < 10

def main() -> None:
    print(check(under_ten, 3), is_small("x"))

main()
"#,
    );
    let d = check(
        r#"
from collections.abc import Callable
from typing import TypeGuard

def is_str(x: object) -> TypeGuard[str]:
    return isinstance(x, str)

def yes(x: object) -> bool:
    return True

def f(v: int | str, is_str: Callable[[object], bool]) -> str:
    if is_str(v):
        return v.upper()
    return "no"

print(f(1, yes))
"#,
    );
    assert!(
        d.errors()
            .iter()
            .any(|e| matches!(e, TycError::AttributeNotFound { attr, .. } if attr == "upper")),
        "{:?}",
        messages(&d)
    );
}

#[test]
fn a_parameter_typed_as_a_type_guard_or_noreturn_callable_keeps_its_effect() {
    // Judged by the parameter's own type — with or without a module
    // function of the same name.
    for module_fns in [
        "",
        "def is_str(x: object) -> TypeGuard[str]:\n    return isinstance(x, str)\n\n\
         def fail(msg: str) -> NoReturn:\n    raise ValueError(msg)\n",
    ] {
        assert_clean(&format!(
            r#"
from typing import Callable, NoReturn, TypeGuard

{module_fns}
def f(v: int | str, is_str: Callable[[object], TypeGuard[str]]) -> str:
    if is_str(v):
        return v.upper()
    return "no"

def g(x: int, fail: Callable[[str], NoReturn]) -> int:
    if x > 0:
        return x
    fail("neg")
"#
        ));
    }
}

#[test]
fn the_module_type_guard_still_narrows() {
    assert_clean(
        r#"
from typing import TypeGuard

def is_str(x: object) -> TypeGuard[str]:
    return isinstance(x, str)

def f(v: int | str) -> str:
    if is_str(v):
        return v.upper()
    return "no"

print(f("a"))
"#,
    );
}

#[test]
fn a_noreturn_function_shadowed_by_a_parameter_does_not_end_the_path() {
    // n04 (verify) / b120 (sweep), plus a shadowed `exit` and a parameter
    // named `os` (adversarial n01).
    for src in [
        r#"
from typing import Callable, NoReturn

def fail(msg: str) -> NoReturn:
    raise ValueError(msg)

def run(fail: Callable[[str], None], x: int) -> int:
    if x > 0:
        return x
    fail("neg")

def log(msg: str) -> None:
    print(msg)

print(run(log, -3))
"#,
        r#"
from typing import Callable

def run(exit: Callable[[], None], x: int) -> int:
    if x > 0:
        return x
    exit()

def nop() -> None:
    pass

print(run(nop, -3))
"#,
        r#"
class Job:
    name: str

impl Job:
    def abort(self) -> None:
        print("abort", self.name)

def run(os: Job, x: int) -> int:
    if x > 0:
        return x
    os.abort()

print(run(Job("j"), -1))
"#,
    ] {
        let errors = only_errors(
            src,
            |e| matches!(e, TycError::MissingReturn { fn_name, .. } if fn_name == "run"),
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
    }
    // Controls: the real `NoReturn` function, `os.abort()`, `exit()` and an
    // imported `exit`.
    assert_clean(
        r#"
import os
from sys import exit as leave
from typing import NoReturn

def fail(msg: str) -> NoReturn:
    raise ValueError(msg)

def a(x: int) -> int:
    if x > 0:
        return x
    fail("neg")

def b(x: int) -> int:
    if x > 0:
        return x
    os.abort()

def c(x: int) -> int:
    if x > 0:
        return x
    exit()

print(a(1), b(1), c(1))
"#,
    );
    assert_clean(
        r#"
from sys import exit

def run(x: int) -> int:
    if x > 0:
        return x
    exit(1)

print(run(1))
"#,
    );
}

#[test]
fn a_field_write_summary_is_not_read_through_a_shadowing_parameter() {
    // f01: `reset(node)` through a parameter may null `node.next`.
    let d = check(
        r#"
from typing import Callable

class Node:
    value: int
    next: Node?

def reset(n: Node) -> None:
    print("noop", n.value)

def clear(n: Node) -> None:
    n.next = None

def walk(reset: Callable[[Node], None], node: Node) -> int:
    if node.next is not None:
        reset(node)
        return node.next.value
    return 0

def main() -> None:
    let n: Node = Node(value=1, next=Node(value=2, next=None))
    reset(n)
    print(walk(clear, n))

main()
"#,
    );
    let nullable = d
        .errors()
        .iter()
        .chain(d.warnings())
        .filter(|e| matches!(e, TycError::NullableUse { .. }))
        .count();
    assert_eq!(nullable, 1, "{:?}", messages(&d));
    // Control: the module function itself writes nothing, and a `let`
    // alias of it is just as harmless.
    let d = check(
        r#"
class Node:
    value: int
    next: Node?

def reset(n: Node) -> None:
    print("noop", n.value)

def walk(node: Node) -> int:
    let r = reset
    if node.next is not None:
        reset(node)
        r(node)
        return node.next.value
    return 0

print(walk(Node(value=1, next=None)))
"#,
    );
    assert!(
        d.errors().is_empty()
            && d.warnings()
                .iter()
                .all(|w| !matches!(w, TycError::NullableUse { .. })),
        "{:?} / {:?}",
        messages(&d),
        d.warnings()
    );
}

#[test]
fn a_spawner_shadowed_by_a_comprehension_target_is_not_the_spawner() {
    // A module-level call of a sync function that spawns a task has no
    // event loop to spawn on — but `kick` here is the comprehension's.
    let src = |call: &str| {
        let src = format!(
            r#"
async def work() -> None:
    pass

def kick() -> None:
    go work()

def nop() -> None:
    pass

{call}
"#
        );
        let d = check(&tyc_syntax::preprocess::expand_sugar(&src, true));
        d.errors()
            .iter()
            .filter(|e| e.to_string().contains("via kick"))
            .count()
    };
    assert_eq!(src("print([kick() for kick in [nop]])"), 0);
    assert_eq!(src("kick()"), 1);
}

// ── Class scope is not an enclosing scope of a method ────────────────────

#[test]
fn a_method_body_does_not_see_sibling_methods() {
    // s09: a sibling sync `fetch` hid the module `async def fetch`.
    assert_clean(
        r#"
import asyncio

async def fetch(key: str) -> int:
    await asyncio.sleep(0)
    return len(key)

class Cache:
    data: dict[str, int]

impl Cache:
    def fetch(self, key: str) -> int:
        return self.data.get(key) or 0

    async def load(self, key: str) -> int:
        let v: int = await fetch(key)
        self.data[key] = v
        return v

async def main() -> None:
    let c: Cache = Cache(data={})
    print(await c.load("abc"), c.fetch("abc"))

asyncio.run(main())
"#,
    );
    // s04: the module function's return type, not the sibling's.
    assert_clean(
        r#"
def load(path: str) -> list[str]:
    return [path]

class Repo:
    root: str

impl Repo:
    def load(self) -> int:
        return 0

    def files(self) -> list[str]:
        let xs: list[str] = load(self.root)
        return xs

def main() -> None:
    print(Repo(root="r").files())

main()
"#,
    );
}

#[test]
fn sibling_method_order_does_not_change_the_module_call() {
    // s03 / s05: the sibling `get` before and after the callers, in `impl`
    // and in the class body.
    let total = "    def total(self) -> int:\n        return get(self.data, \"a\") + self.get()\n";
    let get = "    def get(self) -> int:\n        return get(self.data, \"k\", 1)\n";
    let again = "    def again(self) -> int:\n        return get(self.data, \"a\", 5)\n";
    for body in [[total, get, again], [get, total, again]] {
        let body = body.join("\n");
        assert_clean(&format!(
            r#"
def get(table: dict[str, int], key: str, default: int = 0) -> int:
    if key in table:
        return table[key]
    return default

class Store:
    data: dict[str, int]

impl Store:
{body}
def main() -> None:
    let s: Store = Store(data={{"a": 1}})
    print(s.total(), s.again())

main()
"#
        ));
    }
    let d = check(
        r#"
def get(table: dict[str, int], key: str, default: int = 0) -> int:
    if key in table:
        return table[key]
    return default

class Store:
    data: dict[str, int]

    def get(self) -> int:
        return get(self.data, "k", 1)

    def total(self) -> int:
        return get(self.data, "a", 2) + self.get()

def main() -> None:
    let s: Store = Store(data={"a": 1})
    print(s.total())

main()
"#,
    );
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
}

#[test]
fn a_class_field_does_not_shadow_a_builtin_in_a_method() {
    // s08 / s07.
    let d = check(
        r#"
class Board:
    max: int

    def best(self, ys: list[int]) -> int:
        return max(ys) + self.max

def main() -> None:
    let b: Board = Board(max=1)
    print(b.best([3, 9]))

main()
"#,
    );
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
    // (A hand-written `class!` `__init__` is not yet used for constructor
    // argument types — a separate gap — so only the shadowing is asserted.)
    let d = check(
        r#"
class! Stats:
    total: int
    max: int

    def __init__(self, xs: list[int]) -> None:
        self.total = sum(xs)
        self.max = max(xs)

    def bump(self, ys: list[int]) -> int:
        return max(ys) + self.total

def main() -> None:
    let s: Stats = Stats([1, 2, 3])
    print(s.total, s.max, s.bump([9]))

main()
"#,
    );
    assert!(
        !d.errors()
            .iter()
            .any(|e| matches!(e, TycError::NotCallable { .. })),
        "{:?}",
        messages(&d)
    );
}

#[test]
fn a_class_local_to_a_function_is_visible_to_its_methods() {
    // The function frame holding the class is an enclosing scope of the
    // methods; only the class body itself is not.
    assert_clean(
        r#"
def factory() -> int:
    class Local:
        n: int

    impl Local:
        def bump(self) -> Local:
            return Local(n=self.n + 1)

    return Local(n=1).bump().n

print(factory())
"#,
    );
}

#[test]
fn class_scope_stays_visible_where_python_shows_it() {
    // A parameter default is evaluated in the class body; a nested class
    // body sees its own names; the class body itself sees its fields.
    assert_clean(
        r#"
class Limits:
    LIMIT: int = 3
    DOUBLE: int = LIMIT * 2

    def clamp(self, x: int, cap: int = LIMIT) -> int:
        return min(x, cap)

def main() -> None:
    print(Limits().clamp(9), Limits.DOUBLE)

main()
"#,
    );
}

// ── function_type_bounds: each def's own ─────────────────────────────────

#[test]
fn a_method_does_not_inherit_a_module_functions_typevar_bounds() {
    // r28: unbounded method `describe[T]` beside module `describe[T: Named]`.
    assert_clean(
        r#"
interface Named:
    name: str

class Tag:
    text: str

impl Tag:
    def label(self) -> str:
        return "@" + self.text

class Printer:
    prefix: str

impl Printer:
    def describe[T](self, x: T) -> str:
        return self.prefix + str(x.label())

def describe[T: Named](x: T) -> str:
    return x.name

class Person:
    name: str

def main() -> None:
    let p: Printer = Printer(prefix="> ")
    print(p.describe(Tag(text="a")), describe(Person(name="bob")))

main()
"#,
    );
}

#[test]
fn methods_and_nested_defs_do_not_overwrite_module_bounds() {
    // b01 (method), b02 (nested bounded def), b03 (nested unbounded def).
    assert_clean(
        r#"
interface Named:
    name: str

interface Sized:
    size: int

def describe[T: Named](x: T) -> str:
    return x.name

class Person:
    name: str

class Box:
    size: int

class Printer:
    prefix: str

impl Printer:
    def describe[T: Sized](self, x: T) -> str:
        return self.prefix + str(x.size)

def helper() -> str:
    def describe[T: Sized](x: T) -> str:
        return str(x.size)
    return describe(Box(size=3))

def helper2(v: int) -> str:
    def describe[T](x: T) -> str:
        return str(x)
    return describe(v)

def main() -> None:
    let p: Printer = Printer(prefix="> ")
    print(describe(Person(name="bob")), p.describe(Box(size=3)), helper(), helper2(3))

main()
"#,
    );
    // Control: the module function's own bound still binds its callers.
    let errors = only_errors(
        r#"
interface Named:
    name: str

def describe[T: Named](x: T) -> str:
    return x.name

class Box:
    size: int

print(describe(Box(size=3)))
"#,
        |e| matches!(e, TycError::TypeVarBoundViolation { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── Context-manager factories: module functions vs methods ───────────────

#[test]
fn method_and_module_contextmanagers_do_not_mix() {
    // r13: `with p.session() as s` types `s` from `Conn.__enter__`.
    assert_clean(
        r#"
from contextlib import contextmanager
from typing import Iterator

class Conn:
    url: str

impl Conn:
    def __enter__(self) -> str:
        return self.url
    def __exit__(self, a: object, b: object, c: object) -> None:
        pass

class Pool:
    url: str

impl Pool:
    def session(self) -> Conn:
        return Conn(url=self.url)

@contextmanager
def session() -> Iterator[int]:
    yield 1

def main() -> None:
    let p: Pool = Pool(url="db://x")
    with p.session() as s:
        let u: str = s
        print(u.upper())
    with session() as n:
        print(n + 1)

main()
"#,
    );
    // r14: `with lock() as g` is the module function, `m.lock()` the method.
    assert_clean(
        r#"
from contextlib import contextmanager
from typing import Iterator

class Guard:
    label: str

impl Guard:
    def __enter__(self) -> str:
        return self.label
    def __exit__(self, a: object, b: object, c: object) -> None:
        pass

class Mutex:
    n: int

impl Mutex:
    @contextmanager
    def lock(self) -> Iterator[int]:
        yield 7

def lock() -> Guard:
    return Guard(label="g")

def main() -> None:
    with lock() as g:
        let s: str = g
        print(s.upper())
    let m: Mutex = Mutex(n=3)
    with m.lock() as k:
        print(k + 1)

main()
"#,
    );
}

#[test]
fn a_shadowed_contextmanager_factory_is_not_the_module_one() {
    // A parameter named like a module `@contextmanager` function is some
    // other context-manager factory.
    assert_clean(
        r#"
from contextlib import contextmanager
from typing import Callable, ContextManager, Iterator

@contextmanager
def session() -> Iterator[str]:
    yield "s"

def use(session: Callable[[], ContextManager[int]]) -> None:
    with session() as s:
        print(s + 1)

def main() -> None:
    with session() as t:
        print(t.upper())

main()
"#,
    );
}

#[test]
fn each_class_has_its_own_contextmanager_method_yield() {
    // c01: two classes' `acquire`, and a module `acquire` beside a method.
    let errors = only_errors(
        r#"
from contextlib import contextmanager
from collections.abc import Iterator

class IntPool:
    n: int

class StrPool:
    s: str

impl IntPool:
    @contextmanager
    def acquire(self) -> Iterator[int]:
        yield 3

impl StrPool:
    @contextmanager
    def acquire(self) -> Iterator[str]:
        yield "s"

def main() -> None:
    with IntPool(3).acquire() as c:
        print(c.upper())
    with StrPool("x").acquire() as d:
        print(d.upper())

main()
"#,
        |e| matches!(e, TycError::AttributeNotFound { attr, .. } if attr == "upper"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    let errors = only_errors(
        r#"
from contextlib import contextmanager
from collections.abc import Iterator

@contextmanager
def acquire() -> Iterator[int]:
    yield 1

class Pool:
    name: str

impl Pool:
    @contextmanager
    def acquire(self) -> Iterator[str]:
        yield "p"

def main() -> None:
    with acquire() as c:
        print(c.upper())

main()
"#,
        |e| matches!(e, TycError::AttributeNotFound { attr, .. } if attr == "upper"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn an_inherited_contextmanager_method_is_found_through_the_base() {
    let errors = only_errors(
        r#"
from contextlib import contextmanager
from collections.abc import Iterator

class Base:
    n: int

impl Base:
    @contextmanager
    def acquire(self) -> Iterator[int]:
        yield 5

class Child(Base):
    tag: str

def main() -> None:
    with Child(n=1, tag="t").acquire() as c:
        let ok: int = c
        print(ok, c.upper())

main()
"#,
        |e| matches!(e, TycError::AttributeNotFound { attr, .. } if attr == "upper"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── blocking_in_async: a local head is not the module ────────────────────

#[test]
fn a_local_named_like_a_blocking_module_is_not_blocking() {
    // w01: a `requests` dict parameter.
    let d = check(
        r#"
import asyncio

async def lookup(requests: dict[str, int], key: str) -> int:
    await asyncio.sleep(0)
    let v: int? = requests.get(key)
    if v is None:
        return 0
    return v

async def main() -> None:
    print(await lookup({"a": 1}, "a"))

asyncio.run(main())
"#,
    );
    assert!(
        !d.warnings()
            .iter()
            .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
        "{:?}",
        d.warnings()
    );
    // Control: the real module still is, aliased or not.
    let d = check(
        r#"
import asyncio
import time
import time as t

async def main() -> None:
    time.sleep(1)
    t.sleep(1)

asyncio.run(main())
"#,
    );
    let blocking = d
        .warnings()
        .iter()
        .filter(|w| matches!(w, TycError::BlockingInAsync { .. }))
        .count();
    assert_eq!(blocking, 2, "{:?}", d.warnings());
}

// ── resource_not_managed: only the builtin / stdlib resource callees ─────

fn resource_warnings(src: &str) -> Vec<String> {
    check(src)
        .warnings()
        .iter()
        .filter(|w| matches!(w, TycError::ResourceNotManaged { .. }))
        .map(|w| w.to_string())
        .collect()
}

#[test]
fn a_user_binding_named_like_a_resource_callee_is_not_the_resource() {
    // A module-level `def open` (the review repro), assigned and inline.
    let door = r#"
class Door:
    name: str
    is_open: bool

def open(name: str) -> Door:
    return Door(name=name, is_open=True)

def front() -> bool:
    let d = open("front")
    return d.is_open

def back() -> bool:
    return open("back").is_open

def main() -> None:
    print(front(), back())

if __name__ == "__main__":
    main()
"#;
    assert!(
        resource_warnings(door).is_empty(),
        "{:?}",
        resource_warnings(door)
    );
    // A parameter, a local and a loop target of the same name.
    for src in [
        "from collections.abc import Callable\n\ndef f(open: Callable[[str], int]) -> int:\n    let h = open(\"x\")\n    return h\n",
        "from collections.abc import Callable\n\ndef f(socket: Callable[[], int]) -> int:\n    let s = socket()\n    return s\n",
        "def mk(p: str) -> int:\n    return len(p)\n\ndef f() -> int:\n    let open = mk\n    let h = open(\"x\")\n    return h\n",
        "from collections.abc import Callable\n\ndef f(fs: list[Callable[[str], int]]) -> int:\n    mut t = 0\n    for open in fs:\n        let h = open(\"x\")\n        t += h\n    return t\n",
        "from collections.abc import Callable\n\ndef outer(open: Callable[[str], int]) -> Callable[[], int]:\n    def inner() -> int:\n        let h = open(\"x\")\n        return h\n    return inner\n",
    ] {
        assert!(resource_warnings(src).is_empty(), "{src}\n{:?}", resource_warnings(src));
    }
}

#[test]
fn builtin_and_stdlib_resource_callees_still_warn() {
    let src = r#"
import socket
import tempfile

def g(name: str) -> int:
    return len(name)

def f(path: str, open_: int) -> None:
    let h = open(path)
    let s = socket.socket()
    let t = tempfile.TemporaryFile()
    print(open(path).read(), g(path), open_)
"#;
    assert_eq!(
        resource_warnings(src).len(),
        4,
        "{:?}",
        resource_warnings(src)
    );
    // A same-named binding in a sibling function does not hide the builtin.
    let sibling = r#"
from collections.abc import Callable

def a(open: Callable[[str], int]) -> int:
    return open("x")

def b(path: str) -> None:
    let h = open(path)
    print(h)
"#;
    assert_eq!(
        resource_warnings(sibling).len(),
        1,
        "{:?}",
        resource_warnings(sibling)
    );
}

// ── dict.get is positional-only and takes one or two arguments ───────────

#[test]
fn dict_get_rejects_a_keyword_default_and_extra_arguments() {
    let errors = only_errors(
        r#"
def main() -> None:
    let d: dict[str, int] = {"a": 1}
    print(d.get("a", default=0))

main()
"#,
        |e| matches!(e, TycError::UnknownKwarg { kwarg, .. } if kwarg == "default"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    let errors = only_errors(
        r#"
def main() -> None:
    let d: dict[str, int] = {"a": 1}
    print(d.get("a", 1, 2))

main()
"#,
        // The bound the call broke is get's two positionals.
        |e| {
            matches!(
                e,
                TycError::WrongArgCount {
                    expected: 2,
                    actual: 3,
                    ..
                }
            )
        },
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn dict_get_keeps_its_one_and_two_argument_types() {
    assert_clean(
        r#"
from collections.abc import Mapping

def main(m: Mapping[str, int]) -> None:
    let d: dict[str, int] = {"a": 1}
    let maybe: int? = d.get("a")
    let sure: int = d.get("a", 0)
    let from_map: int? = m.get("a", default=0)
    print(maybe, sure, from_map)

main({"b": 2})
"#,
    );
    let errors = only_errors(
        r#"
def main() -> None:
    let d: dict[str, int] = {"a": 1}
    let n: int = d.get("a")
    print(n)

main()
"#,
        |e| {
            matches!(
                e,
                TycError::TypeMismatch { .. } | TycError::NullableUse { .. }
            )
        },
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── An assignment in a function body binds a local of that body ──────────

#[test]
fn a_local_named_like_a_module_binding_is_a_new_local() {
    // An unannotated `let` / `mut` / plain assignment shadowing a module
    // `def`, import or annotated global declares a local; it is not checked
    // as a reassignment of the module binding.
    assert_clean(
        r#"
from typing import Callable

count: int = 0

def step(a: int, b: int) -> int:
    return a + b

def inc(x: int) -> int:
    return x + 1

def pick() -> Callable[[int], int]:
    return inc

def run() -> int:
    let step = pick()
    return step(1)

def label() -> str:
    let count = "x"
    return count

def lam() -> int:
    let step = lambda x: x + 1
    return step(1)

def rebound() -> int:
    mut step = inc
    step = inc
    return step(1)

def main() -> None:
    print(run(), label(), lam(), rebound(), step(1, 2), count)

main()
"#,
    );
    // The local keeps its own type: misuse is still reported, against the
    // local, and the module binding is unchanged in another body.
    let errors = only_errors(
        r#"
def step(a: int, b: int) -> int:
    return a + b

def inc(x: int) -> int:
    return x + 1

def run() -> int:
    mut step = inc
    step = "no"
    return step(1, 2)

def later() -> int:
    return step(1)

run()
"#,
        |e| {
            matches!(
                e,
                TycError::TypeReassignMismatch { .. }
                    | TycError::NotCallable { .. }
                    | TycError::MissingArgument { .. }
            )
        },
    );
    assert_eq!(errors.len(), 3, "{errors:?}");
}

#[test]
fn a_global_declaration_still_reassigns_the_module_binding() {
    let errors = only_errors(
        r#"
mut count: int = 0

def bump() -> None:
    global count
    count = "x"

bump()
"#,
        |e| matches!(e, TycError::TypeReassignMismatch { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn a_shadowing_local_alias_uses_its_target_field_write_summary() {
    // `let reset = touch` inside the body: `reset(b)` calls `touch`, which
    // never writes `conn`.
    let d = check(
        r#"
class Conn:
    n: int

impl Conn:
    def query(self) -> int:
        return self.n

class Box:
    conn: Conn?
    count: int

def reset(b: Box) -> None:
    b.conn = None

def touch(b: Box) -> None:
    b.count = 1

def use_alias(b: Box) -> int:
    let reset = touch
    if b.conn is not None:
        reset(b)
        return b.conn.query()
    return 0

def main() -> None:
    let b = Box(conn=Conn(n=5), count=0)
    print(use_alias(b))
    reset(b)

main()
"#,
    );
    assert!(
        d.errors().is_empty() && d.warnings().is_empty(),
        "{:?} / {:?}",
        messages(&d),
        d.warnings()
    );
    // The mirror image: a local alias of the writer, `let` or `mut`, drops
    // the narrowing.
    for binding in ["let touch = reset", "mut touch = reset"] {
        let src = format!(
            r#"
class Conn:
    n: int

impl Conn:
    def query(self) -> int:
        return self.n

class Box:
    conn: Conn?
    count: int

def reset(b: Box) -> None:
    b.conn = None

def touch(b: Box) -> None:
    b.count = 1

def use_alias(b: Box) -> int:
    {binding}
    if b.conn is not None:
        touch(b)
        return b.conn.query()
    return 0

print(use_alias(Box(conn=Conn(n=5), count=0)), touch)
"#
        );
        let d = check(&src);
        let nullable = d
            .errors()
            .iter()
            .chain(d.warnings())
            .filter(|e| matches!(e, TycError::NullableUse { .. }))
            .count();
        assert_eq!(nullable, 1, "{binding}: {:?}", messages(&d));
    }
}

#[test]
fn a_local_shadowing_a_blocking_module_import_is_not_blocking() {
    for binding in ["let time = Timer(total=0.0)", "mut time = Timer(total=0.0)"] {
        let src = format!(
            r#"
import asyncio
import time

class Timer:
    total: float

impl Timer:
    def sleep(self, n: float) -> None:
        self.total = self.total + n

async def run() -> float:
    {binding}
    time.sleep(1.5)
    await asyncio.sleep(0)
    return time.total

def stamp() -> float:
    return time.monotonic()

print(asyncio.run(run()), stamp() > 0)
"#
        );
        let d = check(&src);
        assert!(d.errors().is_empty(), "{binding}: {:?}", messages(&d));
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
            "{binding}: {:?}",
            d.warnings()
        );
    }
    // `from time import sleep` shadowed by a local `let sleep = fake`.
    let d = check(
        r#"
import asyncio
from time import sleep

def fake(n: float) -> float:
    return n * 2

async def run() -> float:
    let sleep = fake
    await asyncio.sleep(0)
    return sleep(1.5)

print(asyncio.run(run()), sleep)
"#,
    );
    assert!(
        !d.warnings()
            .iter()
            .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
        "{:?}",
        d.warnings()
    );
}

#[test]
fn a_bound_method_alias_keeps_the_receiver_it_was_defined_with() {
    // `client` names a `Recorder` at the call, but `fetch` was bound to the
    // module-level `HttpClient`'s `get`.
    assert_clean(
        r#"
class HttpClient:
    base: str

impl HttpClient:
    def get(self, path: str, *, timeout: float = 1.0) -> str:
        return self.base + path

class Recorder:
    calls: list[str]

impl Recorder:
    def get(self, key: str, default: str, extra: int) -> str:
        return default

let client: HttpClient = HttpClient(base="http://h")
let fetch = client.get

def report(client: Recorder) -> str:
    client.calls.append("report")
    return fetch("/status") + fetch("/x", timeout=2.0)

def poll(recorders: list[Recorder]) -> list[str]:
    return [fetch("/status") + str(len(client.calls)) for client in recorders]

def main() -> None:
    print(report(Recorder(calls=[])), poll([Recorder(calls=[])]))

main()
"#,
    );
    // Control: the alias still checks the method it was bound to.
    let errors = only_errors(
        r#"
class HttpClient:
    base: str

impl HttpClient:
    def get(self, path: str) -> str:
        return self.base + path

class Recorder:
    calls: list[str]

let client: HttpClient = HttpClient(base="http://h")
let fetch = client.get

def report(client: Recorder) -> str:
    return fetch()

print(report(Recorder(calls=[])))
"#,
        |e| is_missing(e, "fetch", "path"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn parameter_defaults_see_the_enclosing_scope() {
    // A default is evaluated where the `def` is, so the body's own
    // `let parse` does not hide the module function there.
    assert_clean(
        r#"
def parse(s: str, *, base: int = 10) -> int:
    return int(s, base)

def scaled(n: int = parse("11", base=2)) -> int:
    let parse: int = n * 2
    return parse

class Cfg:
    n: int

impl Cfg:
    def size(self, n: int = parse("7", base=8)) -> int:
        let parse: int = n
        return parse

def main() -> None:
    print(scaled(), Cfg(n=1).size())

main()
"#,
    );
    // Control: a real arity error in a default is still reported.
    let errors = only_errors(
        r#"
def parse(s: str, *, base: int = 10) -> int:
    return int(s, base)

def scaled(n: int = parse("11", 2, base=2)) -> int:
    let parse: int = n * 2
    return parse

print(scaled())
"#,
        |e| matches!(e, TycError::WrongArgCount { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── A decorator may change a method's call signature ─────────────────────

#[test]
fn a_callable_typed_decorator_gives_the_method_its_signature() {
    // `inject_db` maps `(Repo, str, Db)` to `(Repo, str)`: every call form
    // takes the decorated signature.
    assert_clean(
        r#"
from typing import Callable

class Db:
    url: str

let DB: Db = Db(url="mem://")

class Repo:
    name: str

def inject_db(f: Callable[[Repo, str, Db], str]) -> Callable[[Repo, str], str]:
    def wrapper(self: Repo, key: str) -> str:
        return f(self, key, DB)
    return wrapper

impl Repo:
    @inject_db
    def find(self, key: str, db: Db) -> str:
        return self.name + ":" + key + "@" + db.url

def lookup[T: Repo](r: T) -> str:
    return r.find("k2")

def main() -> None:
    let r: Repo = Repo(name="r")
    print(Repo.find(r, "k"), r.find("k1"), r.find(key="k4"))
    print(lookup(r))
    let g = r.find
    print(g("k3"))

main()
"#,
    );
    // The decorated signature is checked: it takes no `db`, and needs `key`.
    let errors = only_errors(
        r#"
from typing import Callable

class Repo:
    name: str

def drop_n(f: Callable[[Repo, str, int], str]) -> Callable[[Repo, str], str]:
    def wrapper(self: Repo, key: str) -> str:
        return f(self, key, 7)
    return wrapper

impl Repo:
    @drop_n
    def find(self, key: str, n: int) -> str:
        return self.name + key + str(n)

def main() -> None:
    let r: Repo = Repo(name="r")
    print(Repo.find(r), r.find("k", 3))

main()
"#,
        |e| {
            matches!(
                e,
                TycError::MissingArgument { .. } | TycError::WrongArgCount { .. }
            )
        },
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
}

#[test]
fn a_decorator_of_unknown_effect_keeps_the_structural_check() {
    // `with_default` is untyped: the unbound, TypeVar-receiver and alias
    // forms are not checked against the undecorated `def` (CPython: "rk7").
    assert_clean(
        r#"
import functools
from typing import Callable

class Repo:
    name: str

def with_default(f: Callable[..., str]) -> Callable[..., str]:
    @functools.wraps(f)
    def wrapper(self: Repo, key: str, n: int = 7) -> str:
        return f(self, key, n)
    return wrapper

impl Repo:
    @with_default
    def find(self, key: str, n: int) -> str:
        return self.name + key + str(n)

def lookup[T: Repo](r: T) -> str:
    return r.find("k")

def main() -> None:
    let r: Repo = Repo(name="r")
    let g = r.find
    print(Repo.find(r, "k"), lookup(r), g("k"))

main()
"#,
    );
    // A signature-keeping decorator leaves every form checked against the
    // `def`, and an untyped one still checks a bound call, as before.
    let errors = only_errors(
        r#"
import functools
from typing import Callable

class Repo:
    name: str

def logged(f: Callable[..., str]) -> Callable[..., str]:
    return f

impl Repo:
    @functools.cache
    def find(self, key: str) -> str:
        return self.name + key

    @logged
    def peek(self, key: str) -> str:
        return key

def lookup[T: Repo](r: T) -> str:
    return r.find()

def main() -> None:
    let r: Repo = Repo(name="r")
    print(Repo.find(r), lookup(r), r.peek())

main()
"#,
        |e| matches!(e, TycError::MissingArgument { .. }),
    );
    assert_eq!(errors.len(), 3, "{errors:?}");
}

#[test]
fn an_identity_typed_decorator_keeps_the_signature() {
    let errors = only_errors(
        r#"
class Repo:
    name: str

def traced[F](f: F) -> F:
    return f

impl Repo:
    @traced
    def find(self, key: str) -> str:
        return self.name + key

def main() -> None:
    let r: Repo = Repo(name="r")
    print(Repo.find(r))

main()
"#,
        |e| is_missing(e, "find", "key"),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── A user `extend` method replaces the builtin method it shadows ────────

#[test]
fn a_builtin_extension_method_is_checked_against_its_own_signature() {
    assert_clean(
        r#"
extend dict:
    def get(self, key: str, default: int = 0, strict: bool = False) -> int:
        return 7

extend list:
    def append(self, a: int, b: int) -> None:
        pass

def main() -> None:
    let d: dict[str, int] = {"a": 1}
    let xs: list[int] = []
    xs.append(2, 3)
    print(d.get("a", default=5), d.get("z", 1, True), xs)

main()
"#,
    );
    let errors = only_errors(
        r#"
extend dict:
    def get(self, key: str, default: int = 0) -> int:
        return 7

def main() -> None:
    let d: dict[str, int] = {"a": 1}
    print(d.get("a", 1, 2, 3))

main()
"#,
        |e| matches!(e, TycError::WrongArgCount { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── `dict.get` is positional-only on dict subclasses and frozen dicts ────

#[test]
fn dict_subclasses_and_frozen_dicts_have_a_positional_only_get() {
    let errors = only_errors(
        r#"
from collections import defaultdict, Counter, OrderedDict

class Bag(dict[str, int]):
    pass

freeze let CFG: dict[str, int] = {"a": 1}

def main() -> None:
    let dd: defaultdict[str, int] = defaultdict(int)
    let c: Counter[str] = Counter("ab")
    let od: OrderedDict[str, int] = OrderedDict()
    print(dd.get("z", default=0), c.get("z", default=0), od.get("z", default=0))
    print(Bag().get("z", default=0), CFG.get("z", default=0), dd.get("z", 0, 1))

main()
"#,
        |e| {
            matches!(
                e,
                TycError::UnknownKwarg { .. } | TycError::WrongArgCount { .. }
            )
        },
    );
    assert_eq!(errors.len(), 6, "{errors:?}");
    // Controls: the positional forms, a subclass with its own `get`, and a
    // plain `Mapping` (whose `get` takes `default=`).
    assert_clean(
        r#"
from collections import defaultdict
from collections.abc import Mapping

class Lenient(dict[str, int]):
    pass

impl Lenient:
    def get(self, key: str, default: int = 0) -> int:
        return default

freeze let CFG: dict[str, int] = {"a": 1}

def main(m: Mapping[str, int]) -> None:
    let dd: defaultdict[str, int] = defaultdict(int)
    print(dd.get("z"), dd.get("z", 0), CFG.get("a"), CFG.get("z", 0))
    print(Lenient().get("z", default=1), m.get("z", default=0))

main({"b": 2})
"#,
    );
}

// ── await on a sync call: the decorators of the callee itself ────────────

#[test]
fn an_unrelated_decorated_namesake_does_not_hide_a_sync_await() {
    // A decorated method named like the module function, and a decorated
    // module function named like the method.
    let errors = only_errors(
        r#"
import asyncio
import functools

class Tool:
    n: int

impl Tool:
    @staticmethod
    def compute() -> int:
        return 2

class Worker:
    n: int

impl Worker:
    def run(self) -> int:
        return self.n

def compute() -> int:
    return 1

@functools.cache
def run(x: int) -> int:
    return x

async def go_work(w: Worker) -> int:
    return await compute() + await w.run()

def main() -> None:
    print(compute(), Tool.compute(), run(1))
    asyncio.run(go_work(Worker(n=1)))

main()
"#,
        |e| matches!(e, TycError::TypeMismatch { .. }),
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
    // Control: a decorated callee itself stays permissive.
    assert_clean(
        r#"
import asyncio
from typing import Callable, Any

def make_async(f: Callable[..., Any]) -> Callable[..., Any]:
    return f

class Worker:
    n: int

impl Worker:
    @make_async
    def run(self) -> int:
        return self.n

@make_async
def compute() -> int:
    return 1

async def go_work(w: Worker) -> None:
    await compute()
    await w.run()

asyncio.run(go_work(Worker(n=1)))
"#,
    );
}

// ── A nested generic `def` keeps its TypeVar bounds at its call sites ────

#[test]
fn a_nested_generic_def_checks_its_bounds() {
    let errors = only_errors(
        r#"
interface Greeter:
    def greet(self) -> str

class En:
    name: str

impl En:
    def greet(self) -> str:
        return "hi " + self.name

def top[T: Greeter](x: T) -> str:
    return x.greet()

def outer() -> None:
    def inner[T: Greeter](x: T) -> str:
        return x.greet()
    print(inner(En(name="a")))
    print(inner(5))

def main() -> None:
    print(top(En(name="b")))
    outer()

main()
"#,
        |e| matches!(e, TycError::TypeVarBoundViolation { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    // A nested def shadowing a module def uses its own bounds, and the
    // module def keeps its own elsewhere.
    let errors = only_errors(
        r#"
interface Greeter:
    def greet(self) -> str

class En:
    name: str

impl En:
    def greet(self) -> str:
        return "hi " + self.name

def pick[T: Greeter](x: T) -> str:
    return x.greet()

def outer() -> None:
    def pick[T](x: T) -> str:
        return str(x)
    print(pick(5))

def other() -> None:
    print(pick(5))

outer()
other()
"#,
        |e| matches!(e, TycError::TypeVarBoundViolation { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

// ── A method call is judged by the receiver's own method ─────────────────

fn nullable_uses(d: &Diagnostics) -> usize {
    d.errors()
        .iter()
        .chain(d.warnings())
        .filter(|e| matches!(e, TycError::NullableUse { .. }))
        .count()
}

const CONN: &str = r#"
class Conn:
    n: int

impl Conn:
    def query(self) -> int:
        return self.n
"#;

#[test]
fn a_method_field_write_summary_is_the_receivers_own() {
    // `b.reset()` on a `B` cannot run the unrelated `A.reset`.
    let src = format!(
        r#"{CONN}
class A:
    conn: Conn?

impl A:
    def reset(self) -> None:
        self.conn = None

class B:
    conn: Conn?
    count: int

impl B:
    def reset(self) -> None:
        self.count = 0

def use(b: B) -> int:
    if b.conn is not None:
        b.reset()
        return b.conn.query()
    return 0

def main() -> None:
    let a = A(conn=None)
    a.reset()
    print(use(B(conn=Conn(n=4), count=2)))

main()
"#
    );
    let d = check(&src);
    assert_eq!(nullable_uses(&d), 0, "{:?}", messages(&d));
    // A local subclass overriding `reset` may be what `b` is.
    let src = format!(
        r#"{CONN}
class B:
    conn: Conn?
    count: int

impl B:
    def reset(self) -> None:
        self.count = 0

class C(B):
    tag: str

impl C:
    def reset(self) -> None:
        self.conn = None

def use(b: B) -> int:
    if b.conn is not None:
        b.reset()
        return b.conn.query()
    return 0

print(use(C(conn=Conn(n=4), count=2, tag="t")))
"#
    );
    let d = check(&src);
    assert_eq!(nullable_uses(&d), 1, "{:?}", messages(&d));
    // An inherited writer is the receiver's own method.
    let src = format!(
        r#"{CONN}
class Base:
    conn: Conn?

impl Base:
    def reset(self) -> None:
        self.conn = None

class B(Base):
    count: int

class Other:
    conn: Conn?

impl Other:
    def reset(self) -> None:
        pass

def use(b: B) -> int:
    if b.conn is not None:
        b.reset()
        return b.conn.query()
    return 0

print(use(B(conn=Conn(n=4), count=2)), Other(conn=None).reset())
"#
    );
    let d = check(&src);
    assert_eq!(nullable_uses(&d), 1, "{:?}", messages(&d));
}

// ── A function-local class is what its name means in the body ───────────

#[test]
fn a_function_local_class_shadows_the_module_class() {
    assert_clean(
        r#"
class Point frozen:
    x: int

def bump() -> int:
    class Point:
        x: int
    let p = Point(x=1)
    p.x = 5
    return p.x

def named() -> str:
    class Point:
        name: str
    return Point(name="a").name

def main() -> None:
    print(bump(), named(), Point(x=2).x)

main()
"#,
    );
    // A value typed as the module class keeps its fields in the body, and
    // the module class is unchanged outside it; a write is reported where
    // both classes are frozen.
    let errors = only_errors(
        r#"
class Point frozen:
    x: int

def read(p: Point) -> int:
    class Point:
        name: str
    return p.x + len(Point(name="a").name)

def later() -> None:
    let p = Point(x=2)
    p.x = 3

def both() -> None:
    class Point frozen:
        x: int
    let p = Point(x=1)
    p.x = 4

print(read(Point(x=1)))
later()
both()
"#,
        |e| matches!(e, TycError::FrozenAssign { .. }),
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
}

/// `check`, with the resolver told which declarations are `plain class` /
/// `class!`, as `tyc-db` tells it.
fn check_with_class_markers(src: &str) -> Diagnostics {
    let prep = preprocess(src);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .unwrap()
        .into_syntax();
    let options = ResolveOptions {
        raw_class_byte_starts: line_byte_starts(&prep.python_source, &prep.raw_class_lines),
        original_source: Some(src.to_owned()),
        plain_class_byte_starts: Some(line_byte_starts(
            &prep.python_source,
            &prep.plain_class_lines,
        )),
        ..ResolveOptions::default()
    };
    let (resolved, _) =
        resolve_module_with("<test>".to_owned(), &prep.python_source, &module, options);
    check_module_with(
        "<test>",
        &prep.python_source,
        &resolved,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
        &prep.impl_distributed_lines,
    )
}

#[test]
fn a_method_both_shadowing_classes_declare_is_still_a_member() {
    // The same signature on both: interface conformance and `__call__`
    // read it (review N1, l05 / l06).
    assert_clean(
        r#"
from collections.abc import Callable

interface Shape:
    def area(self) -> float

class Circle:
    r: float

impl Circle:
    def area(self) -> float:
        return 3.0 * self.r

class Res:
    name: str

impl Res:
    def __call__(self, x: int) -> int:
        return x + 1

def total(s: Shape) -> float:
    return s.area()

def apply(g: Callable[[int], int]) -> int:
    return g(1)

def f() -> float:
    class Circle:
        r: float

    impl Circle:
        def area(self) -> float:
            return 2.0 * self.r

    let s: Shape = Circle(r=1.0)
    return total(Circle(r=1.0)) + s.area()

def g() -> int:
    class Res:
        name: str

    impl Res:
        def __call__(self, x: int) -> int:
            return x * 3

    let h: Callable[[int], int] = Res(name="e")
    return apply(Res(name="e")) + h(2)

print(f(), g(), total(Circle(r=1.0)), apply(Res(name="o")))
"#,
    );
    // Different signatures: present, but which one is called is not judged.
    assert_clean(
        r#"
from collections.abc import Callable

interface Shape:
    def area(self) -> float

class Circle:
    r: float

impl Circle:
    def area(self, k: int) -> float:
        return k * self.r

class Res:
    name: str

impl Res:
    def __call__(self, x: int, y: int) -> int:
        return x + y

def total(s: Shape) -> float:
    return s.area()

def apply(g: Callable[[int], int]) -> int:
    return g(1)

def f(outer: Circle) -> float:
    class Circle:
        r: float

    impl Circle:
        def area(self) -> float:
            return 2.0 * self.r

    return total(Circle(r=1.0)) + outer.area(2)

def g() -> int:
    class Res:
        name: str

    impl Res:
        def __call__(self, x: int) -> int:
            return x * 3

    let r = Res(name="e")
    return apply(r) + r(4)
"#,
    );
    // Control: a member neither class declares is still missing.
    let errors = only_errors(
        r#"
interface Shape:
    def area(self) -> float

class Circle:
    r: float

def total(s: Shape) -> float:
    return s.area()

def f() -> float:
    class Circle:
        r: float
    return total(Circle(r=1.0))
"#,
        |e| matches!(e, TycError::InterfaceNotConforming { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn a_shadowing_local_accepts_values_of_the_shadowed_type() {
    // Review N2 (a02): later values fit the module binding's declared type,
    // as they did when the local was taken for a reassignment of it.
    assert_clean(
        r#"
mut cache: int? = None
mut total: float = 0.0
mut best: float = 0.0

def f(xs: list[int]) -> int?:
    mut cache = None
    for x in xs:
        if x > 2:
            cache = x
    return cache

def g(xs: list[int]) -> float:
    mut total = 0
    for x in xs:
        total = total + x * 0.5
    return total

def h() -> float:
    mut total = 0
    total += 2.5
    return total

def find_int(xs: list[int]) -> int:
    mut best = 0
    for x in xs:
        if x > best:
            best = x
    return best

def outer() -> int:
    mut best = 1
    def inner() -> float:
        mut total = 0
        total = total + 0.5
        return total
    return best + int(inner())

print(f([1, 3]), g([1, 2]), h(), find_int([3, 1]), outer())
"#,
    );
    // Controls: a value fitting neither type, and the same body with no
    // module binding to shadow.
    let errors = only_errors(
        r#"
mut cache: str? = None

def f(xs: list[int]) -> int?:
    mut cache = None
    for x in xs:
        if x > 2:
            cache = x
    return cache

def g(xs: list[int]) -> float:
    mut fresh = 0
    for x in xs:
        fresh = fresh + x * 0.5
    return fresh
"#,
        |e| matches!(e, TycError::TypeReassignMismatch { .. }),
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
}

#[test]
fn a_module_decorator_named_like_a_keeping_one_is_judged_by_its_types() {
    // Review R10: a user `def cache` / `def pure` that drops a parameter,
    // and a `Concatenate` decorator, are not signature-keeping.
    for name in ["cache", "pure"] {
        assert_clean(&format!(
            r#"
from typing import Callable

class Repo:
    name: str

def {name}(f: Callable[[Repo, str, dict[str, str]], str]) -> Callable[[Repo, str], str]:
    let store: dict[str, str] = {{}}
    def wrapper(self: Repo, key: str) -> str:
        return f(self, key, store)
    return wrapper

impl Repo:
    @{name}
    def find(self, key: str, store: dict[str, str]) -> str:
        return self.name + key

def lookup[T: Repo](r: T) -> str:
    return r.find("k2")

def main() -> None:
    let r: Repo = Repo(name="r")
    let g = r.find
    print(Repo.find(r, "k"), lookup(r), g("k3"))
"#
        ));
    }
    assert_clean(
        r#"
from typing import Callable, Concatenate, Any

class Db:
    url: str

class Repo:
    name: str

def with_db[**P, R](f: Callable[Concatenate[Repo, Db, P], R]) -> Callable[Concatenate[Repo, P], R]:
    let w: Any = f
    return w

impl Repo:
    @with_db
    def find(self, db: Db, key: str) -> str:
        return self.name + key

def lookup[T: Repo](r: T) -> str:
    return r.find("k2")
"#,
    );
}

#[test]
fn a_freeze_let_dict_calls_the_builtin_extension() {
    // Review R18: the desugar rewrites by the `dict[...]` annotation, so the
    // extension's signature applies, nested values included.
    assert_clean(
        r#"
extend dict:
    def get(self, key: str, default: int = 0, strict: bool = False) -> int:
        return 7

freeze let CONFIG: dict[str, int] = {"a": 1}
freeze let CFG: dict[str, dict[str, int]] = {"db": {"a": 1}}

def main() -> None:
    print(CONFIG.get("z", 1, True), CONFIG.get("a", default=5))
    print(CFG["db"].get("zz", 9), CFG["db"].get("a", default=5))
"#,
    );
    // Control: without the extension it is `dict.get`.
    let errors = only_errors(
        r#"
freeze let CFG: dict[str, dict[str, int]] = {"db": {"a": 1}}

def main() -> None:
    print(CFG["db"].get("a", default=5))
"#,
        |e| matches!(e, TycError::UnknownKwarg { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn a_dict_head_named_get_is_the_collections_one_only() {
    // Review R37: a user generic class named `Counter` keeps its own `get`;
    // `extend Counter:` patches the collections class; a `freeze let` of an
    // `Any` value may be any `Mapping`.
    let errors = only_errors(
        r#"
class Counter[T]:
    n: int = 0

impl[T] Counter[T]:
    def get(self, key: T, default: int = 0) -> int:
        return default + 10

    def total(self, key: T, start: int, step: int) -> int:
        return start + step

def main() -> None:
    let c: Counter[str] = Counter()
    print(c.get("z", default=3), c.get("z", 1))
    let r: str = c.get("z", 1)
    print(r)
"#,
        |e| matches!(e, TycError::TypeMismatch { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_clean(
        r#"
from collections import Counter
from collections.abc import Mapping
from typing import Any

extend Counter:
    def get(self, key: str, default: int = 0) -> int:
        return default + 1000

def make() -> Any:
    return {"a": 1}

freeze let M: Mapping[str, int] = make()

def main() -> None:
    let c: Counter[str] = Counter("ab")
    print(c.get("z", default=3), M.get("z", default=5))
"#,
    );
    // Control: `OrderedDict` is a C type, which `extend` cannot patch.
    let errors = only_errors(
        r#"
from collections import OrderedDict

extend OrderedDict:
    def get(self, key: str, default: int = 0) -> int:
        return default

def main() -> None:
    let o: OrderedDict[str, int] = OrderedDict()
    print(o.get("z", default=3))
"#,
        |e| matches!(e, TycError::UnknownKwarg { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn a_method_effect_follows_each_classs_resolution_order() {
    // Review R19: `D(A, B)` reaches `A.reset` through a `B`-typed receiver,
    // and a diamond resolves `R.reset` before `Base.reset`.
    for (classes, param) in [
        (
            "class A:\n    conn: Conn?\n\nimpl A:\n    def reset(self) -> None:\n        self.conn = None\n\nclass B:\n    conn: Conn?\n\nimpl B:\n    def reset(self) -> None:\n        pass\n\nclass D(A, B):\n    pass\n",
            "B",
        ),
        (
            "class Base:\n    conn: Conn?\n\nimpl Base:\n    def reset(self) -> None:\n        pass\n\nclass L(Base):\n    pass\n\nclass R(Base):\n    pass\n\nimpl R:\n    def reset(self) -> None:\n        self.conn = None\n\nclass D(L, R):\n    pass\n",
            "D",
        ),
        (
            "class Base:\n    conn: Conn?\n\nimpl Base:\n    def reset(self) -> None:\n        pass\n\nclass L(Base):\n    pass\n\nclass R(Base):\n    pass\n\nimpl R:\n    def reset(self) -> None:\n        self.conn = None\n\nclass D(L, R):\n    pass\n",
            "L",
        ),
    ] {
        let src = format!(
            "{CONN}\n{classes}\ndef use(b: {param}) -> int:\n    if b.conn is not None:\n        b.reset()\n        return b.conn.query()\n    return 0\n"
        );
        let d = check(&src);
        assert_eq!(nullable_uses(&d), 1, "{src}\n{:?}", messages(&d));
    }
    // Control: `D(B, A)` resolves `B.reset`, which writes nothing.
    let src = format!(
        "{CONN}\nclass A:\n    conn: Conn?\n\nimpl A:\n    def reset(self) -> None:\n        self.conn = None\n\nclass B:\n    conn: Conn?\n\nimpl B:\n    def reset(self) -> None:\n        pass\n\nclass D(B, A):\n    pass\n\ndef use(b: B) -> int:\n    if b.conn is not None:\n        b.reset()\n        return b.conn.query()\n    return 0\n"
    );
    let d = check(&src);
    assert_eq!(nullable_uses(&d), 0, "{:?}", messages(&d));
}

#[test]
fn await_judges_every_def_the_call_may_resolve_to() {
    // Review R23: a diamond resolves `C.run` (decorated) before `A.run`, and
    // a later `def` in the same body replaces the earlier one.
    let asyncify = r#"
import asyncio
from collections.abc import Callable, Coroutine
from typing import Any

def asyncify[**P, R](f: Callable[P, R]) -> Callable[P, Coroutine[Any, Any, R]]:
    async def inner(*args: P.args, **kwargs: P.kwargs) -> R:
        await asyncio.sleep(0)
        return f(*args, **kwargs)
    return inner
"#;
    assert_clean(&format!(
        r#"{asyncify}
class A:
    n: int

impl A:
    def run(self) -> int:
        return self.n

class B(A):
    pass

class C(A):
    pass

impl C:
    @asyncify
    def run(self) -> int:
        return self.n + 1

class D(B, C):
    pass

class Worker:
    n: int

impl Worker:
    def run(self) -> int:
        return self.n

    @asyncify
    def run(self) -> int:
        return self.n + 1

async def go(w: D, k: Worker) -> int:
    return await w.run() + await k.run()
"#
    ));
    // Control: a plain synchronous method is still not awaitable.
    let errors = only_errors(
        r#"
class A:
    n: int

impl A:
    def run(self) -> int:
        return self.n

class B(A):
    pass

async def go(w: B) -> int:
    return await w.run()
"#,
        |e| matches!(e, TycError::TypeMismatch { .. }),
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn a_local_class_kind_is_the_local_declarations() {
    // Review R26: a local dataclass shadowing a module `plain class`, and a
    // local `plain class` that must not make the module dataclass plain.
    for src in [
        "plain class Point:\n    x: int = 0\n\ndef f() -> int:\n    class Point:\n        x: int\n        y: int\n    let p = Point(x=1, y=2)\n    let q = Point(1, 2)\n    let mk = Point\n    return p.x + q.y + mk(x=1, y=2).x\n\nprint(f(), Point().x)\n",
        "class Point:\n    x: int\n    y: int\n\ndef f() -> int:\n    plain class Point:\n        x: int = 4\n    let p = Point()\n    return p.x\n\ndef g() -> int:\n    return Point(x=1, y=2).y\n\nprint(f(), g(), Point(x=1, y=2).y)\n",
    ] {
        let d = check_with_class_markers(src);
        assert!(d.errors().is_empty(), "{src}\n{:?}", messages(&d));
    }
    // Controls: each `plain class` without `__init__` still takes no
    // arguments where its name means it.
    let src = "plain class Bag:\n    x: int = 0\n\nclass Point:\n    x: int\n\ndef f() -> int:\n    plain class Point:\n        x: int = 4\n    return Point(x=1).x\n\nprint(f(), Bag(x=1).x)\n";
    let d = check_with_class_markers(src);
    let arity = d
        .errors()
        .iter()
        .filter(|e| matches!(e, TycError::WrongArgCount { .. }))
        .count();
    assert_eq!(arity, 2, "{:?}", messages(&d));
}

#[test]
fn a_function_local_stdlib_import_still_names_the_resource() {
    // Review N23: a function-local `import` binds the stdlib module itself,
    // in its own body and in closures over it.
    let src = r#"
def probe() -> None:
    import socket
    let s = socket.socket()
    s.close()

def temp() -> None:
    import tempfile
    let t = tempfile.NamedTemporaryFile()
    t.close()

def db() -> None:
    import sqlite3
    let c = sqlite3.connect(":memory:")
    c.close()

def outer() -> None:
    import socket
    def inner() -> None:
        let s = socket.socket()
        s.close()
    inner()

def text(path: str) -> None:
    from io import open
    let f = open(path)
    f.close()
"#;
    assert_eq!(
        resource_warnings(src).len(),
        5,
        "{:?}",
        resource_warnings(src)
    );
    // Review R25: an `open` imported from anywhere else is not the builtin.
    let src = r#"
from os import open, O_RDONLY
from doors import open as open_door

def fd(path: str) -> int:
    let n = open(path, O_RDONLY)
    return n

def local(path: str) -> None:
    from doors import open
    let d = open(path)
    print(d, open_door(path))
"#;
    assert!(
        resource_warnings(src).is_empty(),
        "{:?}",
        resource_warnings(src)
    );
}

#[test]
fn typing_protocol_bounds_are_not_compared_by_name() {
    // Review N4: `str` is `Sized` and `Hashable` without any nominal
    // relation saying so.
    assert_clean(
        r#"
from typing import Sized, Hashable, SupportsFloat, SupportsInt, SupportsAbs

def size[S: Sized](s: S) -> int:
    return len(s)

def outer() -> int:
    def sz[S: Sized](s: S) -> int:
        return len(s)
    def hs[H: Hashable](h: H) -> H:
        return h
    def fl[F: SupportsFloat](x: F) -> float:
        return float(x)
    def it[I: SupportsInt](x: I) -> int:
        return int(x)
    def ab[A: SupportsAbs[int]](x: A) -> int:
        return abs(x)
    print(hs("k"), hs(3), fl(2), it(2.5), ab(-3))
    return sz([1]) + sz("ab") + size({"a": 1})

print(outer())
"#,
    );
}

/// A per-class `Fields` summary drops those fields' narrowings on every
/// object, but the all-classes answer it refines was `Anything` here (another
/// class's `reset` calls an opaque hook), which touches only the call's own
/// arguments; the unrelated `h.conn` narrowing must survive `a.reset()`, as it
/// did before per-class summaries.
#[test]
fn per_class_method_summary_keeps_an_unrelated_objects_narrowing() {
    assert_clean(
        r#"
from typing import Callable

class Conn:
    n: int

impl Conn:
    def query(self) -> int:
        return self.n

class A:
    conn: Conn?

impl A:
    def reset(self) -> None:
        self.conn = None

class Hooked:
    hook: Callable[[], None]

impl Hooked:
    def reset(self) -> None:
        let h = self.hook
        h()

class Holder:
    conn: Conn?

def noop() -> None:
    pass

def use(a: A, h: Holder) -> int:
    if h.conn is not None:
        a.reset()
        return h.conn.query()
    return 0

def main() -> None:
    let k = Hooked(hook=noop)
    k.reset()
    print(use(A(conn=None), Holder(conn=Conn(n=4))))

main()
"#,
    );
}
