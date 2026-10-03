//! Flow-narrowing, invalidation and exhaustiveness regressions from the
//! 2026-10-03 reviews (workstream W1). Kept in their own file so the W1
//! fixes and the expression-typing fixes landing in `lib.rs`'s own test
//! module at the same time do not collide.

use super::*;
use tyc_resolve::resolve_module;
use tyc_syntax::preprocess::preprocess;

fn check(src: &str) -> Diagnostics {
    let prep = preprocess(src);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .unwrap()
        .into_syntax();
    let (resolved, _) = resolve_module("<test>".to_owned(), &prep.python_source, &module);
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

/// No errors, and no `NullableUse` warning (see [`assert_rejected`] for why
/// that check is a warning in this harness).
fn assert_clean(src: &str) {
    let d = check(src);
    let nullable: Vec<String> = d
        .warnings()
        .iter()
        .filter(|w| matches!(w, TycError::NullableUse { .. }))
        .map(|w| w.to_string())
        .collect();
    assert!(
        d.errors().is_empty() && nullable.is_empty(),
        "expected no errors, got {:?} / {:?}",
        messages(&d),
        nullable
    );
}

fn assert_attr_error(src: &str, attr: &str, on: &str) {
    let d = check(src);
    assert!(
        d.errors().iter().any(|e| matches!(
            e,
            TycError::AttributeNotFound { attr: a, recv_type, .. } if a == attr && recv_type == on
        )),
        "expected `{attr}` not defined on `{on}`, got {:?}",
        messages(&d)
    );
}

const PETS: &str = r#"
class Dog frozen:
    bark: str
class Cat frozen:
    meow: str
class Fish frozen:
    fins: int
type Pet = Dog | Cat | Fish
"#;

fn pets(body: &str) -> String {
    format!("{PETS}\n{body}")
}

// ── W1-01: negative branches over a sealed-union alias ────────────────────

#[test]
fn w1_01_else_branch_of_isinstance_strips_the_variant() {
    assert_clean(&pets(
        r#"
def f(p: Pet) -> str:
    if isinstance(p, Dog):
        return p.bark
    else:
        if isinstance(p, Cat):
            return p.meow
        return str(p.fins)
"#,
    ));
}

#[test]
fn w1_01_early_return_chain_leaves_the_last_variant() {
    assert_clean(&pets(
        r#"
def f(p: Pet) -> int:
    if isinstance(p, Dog):
        return 0
    if isinstance(p, Cat):
        return 1
    return p.fins
"#,
    ));
}

#[test]
fn w1_01_not_isinstance_of_a_tuple() {
    assert_clean(&pets(
        r#"
def f(p: Pet) -> int:
    if not isinstance(p, (Dog, Cat)):
        return p.fins
    return 0
"#,
    ));
}

#[test]
fn w1_01_or_condition_with_early_exit() {
    // `isinstance(p, Dog) or isinstance(p, Cat)` exits; afterwards both
    // negations hold together. The second strip must start from the first.
    assert_clean(&pets(
        r#"
def f(p: Pet) -> int:
    if isinstance(p, Dog) or isinstance(p, Cat):
        return 0
    return p.fins
"#,
    ));
    assert_clean(&pets(
        r#"
def f(p: Dog | Cat | Fish) -> int:
    if isinstance(p, Dog) or isinstance(p, Cat):
        return 0
    return p.fins
"#,
    ));
}

#[test]
fn w1_01_nullable_alias_after_none_guard() {
    assert_clean(&pets(
        r#"
def f(p: Pet?) -> int:
    if p is None:
        return -1
    if isinstance(p, Dog) or isinstance(p, Cat):
        return 0
    return p.fins
"#,
    ));
}

#[test]
fn w1_01_ternary_and_comprehension() {
    assert_clean(&pets(
        r#"
def f(p: Pet) -> str:
    return "d" if isinstance(p, Dog) else ("c" if isinstance(p, Cat) else str(p.fins))

def g(ps: list[Pet]) -> list[int]:
    return [p.fins for p in ps if not isinstance(p, Dog) and not isinstance(p, Cat)]
"#,
    ));
}

#[test]
fn w1_01_wildcard_arm_sees_the_remaining_variants() {
    assert_clean(&pets(
        r#"
def f(p: Pet) -> str:
    match p:
        case Fish():
            return "fish"
        case _:
            if isinstance(p, Dog):
                return p.bark
            return p.meow
"#,
    ));
}

#[test]
fn w1_01_or_pattern_capture_is_the_matched_variants() {
    assert_clean(&pets(
        r#"
def f(p: Pet) -> str:
    match p:
        case Dog() | Cat() as an:
            if isinstance(an, Dog):
                return an.bark
            return an.meow
        case Fish():
            return "fish"
"#,
    ));
}

#[test]
fn w1_01_residual_after_none_and_int_arms() {
    assert_clean(
        r#"
def f(p: int | str | None) -> str:
    match p:
        case None:
            return ""
        case int():
            return "i"
        case s:
            return s.upper()
"#,
    );
}

// The fix must not hide real mistakes.

#[test]
fn w1_01_still_rejects_a_variant_that_can_reach_the_access() {
    assert_attr_error(
        &pets(
            r#"
def f(p: Pet) -> int:
    if isinstance(p, Dog):
        return 0
    return p.fins
"#,
        ),
        "fins",
        "Cat",
    );
    assert_attr_error(
        &pets(
            r#"
def f(p: Pet) -> str:
    if isinstance(p, Dog):
        return "d"
    else:
        return p.bark
"#,
        ),
        "bark",
        "Cat",
    );
    assert_attr_error(
        &pets(
            r#"
def f(p: Pet) -> int:
    if not isinstance(p, (Dog, Cat)):
        return 0
    return p.fins
"#,
        ),
        "fins",
        "Dog",
    );
}

#[test]
fn w1_01_refutable_or_guarded_arms_do_not_exclude() {
    for arm in ["case Fish(fins=1):", "case Fish() if x:"] {
        assert_attr_error(
            &pets(&format!(
                r#"
def f(p: Pet, x: bool) -> int:
    match p:
        {arm}
            return 1
        case _:
            return p.fins
"#
            )),
            "fins",
            "Dog",
        );
    }
    assert_attr_error(
        &pets(
            r#"
def f(p: Pet) -> int:
    match p:
        case Dog() | Cat() as an:
            return an.fins
        case Fish():
            return 0
"#,
        ),
        "fins",
        "Dog",
    );
}

#[test]
fn w1_01_recursive_alias_over_builtins_is_not_expanded() {
    // `int` here is the builtin, not a nominal class named `int`.
    assert_clean(
        r#"
type IntTree = int | list["IntTree"]

def total(t: IntTree) -> int:
    match t:
        case int() as n:
            return n
        case list() as xs:
            mut s: int = 0
            for x in xs:
                s = s + total(x)
            return s
"#,
    );
}

// ── W1-02: passing an object to a call ────────────────────────────────────

/// The stale narrowing is reported. `NullableUse` counts at either level:
/// this harness runs without a `typhon.toml`, so it carries the checker's
/// built-in severity rather than the CLI's `nullable-use = "error"` default.
fn assert_rejected(src: &str) {
    let d = check(src);
    let nullable_warning = d
        .warnings()
        .iter()
        .any(|w| matches!(w, TycError::NullableUse { .. }));
    assert!(
        !d.errors().is_empty() || nullable_warning,
        "expected the stale narrowing to be reported, got nothing"
    );
}

const ITEMS: &str = r#"
import logging
class Item:
    name: str
    price: float?
class Box:
    value: int?
class Card frozen:
    name: str?
"#;

fn items(body: &str) -> String {
    format!("{ITEMS}\n{body}")
}

#[test]
fn w1_02_builtin_stdlib_and_container_calls_keep_narrowings() {
    for call in [
        "print(it)",
        "out.append(it)",
        "logging.info(\"%s\", it)",
        "seen.add(it.name)",
    ] {
        assert_clean(&items(&format!(
            r#"
def f(it: Item, out: list[Item], seen: set[str]) -> float:
    if it.price is not None:
        {call}
        return it.price
    return 0.0
"#
        )));
    }
}

#[test]
fn w1_02_local_callee_that_cannot_write_the_field_keeps_narrowings() {
    assert_clean(&items(
        r#"
def show(it: Item) -> None:
    print(it.name)

def rename(it: Item) -> None:
    it.name = "x"

def f(it: Item) -> float:
    if it.price is not None:
        show(it)
        rename(it)
        return it.price
    return 0.0
"#,
    ));
    assert_clean(&items(
        r#"
class Mgr:
    n: int

impl Mgr:
    def show(self, b: Box) -> None:
        print(b)

def f(m: Mgr, b: Box) -> int:
    if b.value is not None:
        m.show(b)
        return b.value
    return 0
"#,
    ));
}

#[test]
fn w1_02_frozen_argument_keeps_its_own_field_narrowings() {
    assert_clean(&items(
        r#"
def logc(c: Card) -> None:
    print(c)

def f(c: Card) -> str:
    if c.name is not None:
        logc(c)
        return c.name.upper()
    return ""
"#,
    ));
}

#[test]
fn w1_02_a_callee_that_can_write_the_field_still_invalidates() {
    // Direct, transitive, through a method, through `setattr`, through a
    // constructor that writes its argument, and through an opaque callable.
    let cases = [
        "def clear(b: Box) -> None:\n    b.value = None\n",
        "def wipe(b: Box) -> None:\n    b.value = None\n\ndef clear(b: Box) -> None:\n    wipe(b)\n",
        "def clear(b: Box) -> None:\n    setattr(b, \"value\", None)\n",
    ];
    for helper in cases {
        assert_rejected(&items(&format!(
            r#"
{helper}
def f(b: Box) -> int:
    if b.value is not None:
        clear(b)
        return b.value
    return 0
"#
        )));
    }
    assert_rejected(&items(
        r#"
class Mgr:
    n: int

impl Mgr:
    def reset(self, b: Box) -> None:
        b.value = None

def f(m: Mgr, b: Box) -> int:
    if b.value is not None:
        m.reset(b)
        return b.value
    return 0
"#,
    ));
    assert_rejected(&items(
        r#"
plain class Taker:
    def __init__(self, b: Box) -> None:
        b.value = None

def f(b: Box) -> int:
    if b.value is not None:
        Taker(b)
        return b.value
    return 0
"#,
    ));
    assert_rejected(&items(
        r#"
from typing import Callable
def f(b: Box, cb: Callable[[Box], None]) -> int:
    if b.value is not None:
        cb(b)
        return b.value
    return 0
"#,
    ));
    assert_rejected(&items(
        r#"
def f(b: Box) -> int:
    if b.value is not None:
        setattr(b, "value", None)
        return b.value
    return 0
"#,
    ));
}

// ── W1-03: method calls and attribute truthiness under `nullable-use = "error"` ──

#[test]
fn w1_03_a_method_call_keeps_the_receiver_slot_narrowed() {
    assert_clean(
        r#"
import sqlite3
class Conn:
    n: int
impl Conn:
    def execute(self, q: str) -> None:
        print(q)
    def commit(self) -> None:
        print("c")
class Repo:
    conn: Conn?
    db: sqlite3.Connection?
impl Repo:
    def log(self, m: str) -> None:
        print(m)
    def save(self) -> int:
        if self.conn is None:
            return 0
        self.conn.execute("x")
        self.conn.commit()
        self.log("x")
        self.conn.commit()
        return 1
    def store(self) -> int:
        if self.db is None:
            return 0
        self.db.execute("x")
        self.db.commit()
        return 1
"#,
    );
}

#[test]
fn w1_03_a_method_that_writes_the_field_still_invalidates() {
    assert_rejected(
        r#"
class Conn:
    n: int
class Repo:
    conn: Conn?
impl Repo:
    def close(self) -> None:
        self.conn = None
    def save(self) -> int:
        if self.conn is None:
            return 0
        self.close()
        return self.conn.n
"#,
    );
    assert_rejected(
        r#"
class Inner:
    x: int?
impl Inner:
    def wipe(self) -> None:
        self.x = None
class Outer:
    inner: Inner
def f(o: Outer) -> int:
    if o.inner.x is None:
        return 0
    o.inner.wipe()
    return o.inner.x
"#,
    );
}

#[test]
fn w1_03_attribute_paths_narrow_on_truthiness() {
    assert_clean(
        r#"
class Node:
    v: int
    nxt: Node?
class Buf:
    data: list[int]?
impl Buf:
    def first(self) -> int:
        if not self.data:
            return 0
        return self.data[0]
def f(head: Node) -> int:
    if head.nxt:
        return head.nxt.v
    return 0
def g(head: Node) -> int:
    return head.nxt.v if head.nxt else 0
def h(head: Node) -> bool:
    if head.nxt and head.nxt.v > 5:
        return True
    return False
"#,
    );
    // Falsy does not imply `None`: the else branch stays nullable.
    assert_rejected(
        r#"
class Node:
    v: int
    nxt: Node?
def f(head: Node) -> int:
    if head.nxt:
        return 0
    return head.nxt.v
"#,
    );
}

// ── W1-04 / W1-05: exhaustiveness ─────────────────────────────────────────

fn non_exhaustive(src: &str) -> Vec<String> {
    check(src)
        .errors()
        .iter()
        .filter_map(|e| match e {
            TycError::NonExhaustiveMatch { missing, .. } => Some(missing.clone()),
            _ => None,
        })
        .collect()
}

const LOAD: &str = r#"
class NotFound frozen:
    path: str
class Timeout frozen:
    secs: int
class Denied frozen:
    who: str
type LoadError = NotFound | Timeout | Denied
def load(p: str) -> Result[str, LoadError]:
    return Err(Denied(who="me"))
"#;

#[test]
fn w1_04_result_with_a_missing_err_variant_is_not_exhaustive() {
    let src = format!(
        r#"{LOAD}
def show(p: str) -> None:
    match load(p):
        case Ok(body):
            print(body)
        case Err(NotFound(path=q)):
            print(q)
        case Err(Timeout(secs=s)):
            print(s)
"#
    );
    assert_eq!(non_exhaustive(&src), vec!["Err(Denied)".to_owned()]);
    let ok_only = format!(
        r#"{LOAD}
def show(p: str) -> None:
    match load(p):
        case Ok(body):
            print(body)
"#
    );
    assert_eq!(non_exhaustive(&ok_only), vec!["Err".to_owned()]);
}

#[test]
fn w1_04_result_value_position_reports_missing_return_too() {
    let d = check(&format!(
        r#"{LOAD}
def show(p: str) -> str:
    match load(p):
        case Ok(body):
            return body
        case Err(NotFound(path=q)):
            return q
        case Err(Timeout(secs=s)):
            return str(s)
"#
    ));
    assert!(
        d.errors()
            .iter()
            .any(|e| matches!(e, TycError::MissingReturn { .. })),
        "got {:?}",
        messages(&d)
    );
}

#[test]
fn w1_04_nullable_bool_and_literal_subjects() {
    assert_eq!(
        non_exhaustive(
            "def f(o: int?) -> None:\n    match o:\n        case int():\n            print(o)\n"
        ),
        vec!["None".to_owned()]
    );
    assert_eq!(
        non_exhaustive(
            "def f(b: bool) -> None:\n    match b:\n        case True:\n            print(1)\n"
        ),
        vec!["False".to_owned()]
    );
    assert_eq!(
        non_exhaustive(
            "type Color = \"red\" | \"green\" | \"blue\"\ndef f(c: Color) -> None:\n    match c:\n        case \"red\":\n            print(1)\n        case \"green\":\n            print(2)\n"
        ),
        vec!["\"blue\"".to_owned()]
    );
}

#[test]
fn w1_04_covering_and_open_subjects_stay_clean() {
    assert_clean(&format!(
        r#"{LOAD}
def a(p: str) -> str:
    match load(p):
        case Ok(body):
            return body
        case Err(NotFound(path=q)):
            return q
        case Err(Timeout(secs=s)):
            return str(s)
        case Err(Denied()):
            return "d"
def b(p: str) -> str:
    match load(p):
        case Ok(v):
            return v
        case Err(e):
            return "e"
def c(o: int?) -> int:
    match o:
        case int():
            return o
        case None:
            return 0
def d(x: bool) -> int:
    match x:
        case True:
            return 1
        case False:
            return 0
def e(s: str?) -> None:
    # `str` is open: literal arms never claim to cover it.
    match s:
        case "a":
            print(1)
        case None:
            print(0)
def f(r: Result[int, str]) -> None:
    match r:
        case Ok(0):
            print(0)
        case Ok(n):
            print(n)
        case Err(m):
            print(m)
"#
    ));
}

const SHAPES: &str = r#"
class Circle frozen:
    r: float
class Rect frozen:
    w: float
class Tri frozen:
    b: float
type Poly = Rect | Tri
type Shape = Circle | Poly
"#;

#[test]
fn w1_05_nested_sealed_unions_flatten_to_their_leaves() {
    assert_clean(&format!(
        r#"{SHAPES}
def area(s: Shape) -> float:
    match s:
        case Circle(r=r):
            return r
        case Rect(w=w):
            return w
        case Tri(b=b):
            return b
"#
    ));
    assert_eq!(
        non_exhaustive(&format!(
            r#"{SHAPES}
def area(s: Shape) -> None:
    match s:
        case Circle(r=r):
            print(r)
        case Rect(w=w):
            print(w)
"#
        )),
        vec!["Tri".to_owned()]
    );
}

#[test]
fn w1_05_an_alias_is_not_a_class_pattern_or_isinstance_target() {
    let d = check(&format!(
        r#"{SHAPES}
def area(s: Shape) -> float:
    match s:
        case Circle(r=r):
            return r
        case Poly():
            return 1.0
        case _:
            return 0.0

def g(s: Shape) -> bool:
    return isinstance(s, Poly) or isinstance(s, (Circle, Shape))
"#
    ));
    let aliases: Vec<&str> = d
        .errors()
        .iter()
        .filter_map(|e| match e {
            TycError::AliasNotAClass { alias, .. } => Some(alias.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        aliases,
        vec!["Poly", "Poly", "Shape"],
        "got {:?}",
        messages(&d)
    );
    // A real class with the same shape of use is fine.
    assert_clean(&format!(
        r#"{SHAPES}
def g(s: Shape) -> bool:
    return isinstance(s, (Rect, Tri))
"#
    ));
}

// ── W1-06 / W1-07: loop exits ─────────────────────────────────────────────

#[test]
fn w1_06_the_for_else_suite_is_checked() {
    let d = check(
        "def f() -> None:\n    for i in range(0):\n        pass\n    else:\n        let bad: str = 1\n",
    );
    assert!(
        d.errors()
            .iter()
            .any(|e| matches!(e, TycError::TypeMismatch { .. })),
        "got {:?}",
        messages(&d)
    );
}

#[test]
fn w1_06_a_for_body_narrowing_does_not_survive_a_zero_iteration_loop() {
    assert_rejected(
        r#"
def f(xs: list[int]) -> int:
    mut last: int? = None
    for v in xs:
        last = v
    return last + 1
"#,
    );
}

#[test]
fn w1_07_a_narrowing_the_while_body_invalidates_does_not_survive_the_loop() {
    assert_rejected(
        r#"
def f() -> None:
    mut x: int? = 1
    mut running: bool = True
    if x is not None:
        while running:
            x = None
            running = False
        print(x + 1)
"#,
    );
    assert_rejected(
        r#"
class B:
    v: int?
def f(b: B) -> None:
    mut running: bool = True
    if b.v is not None:
        while running:
            b.v = None
            running = False
        print(b.v + 1)
"#,
    );
}

#[test]
fn w1_06_07_loop_exits_keep_what_every_path_agrees_on() {
    assert_clean(
        r#"
def load() -> int?:
    return 3
def f() -> int:
    mut y: int? = None
    while y is None:
        y = load()
    return y + 1
def g(xs: list[int]) -> int:
    mut x: int? = 5
    for v in xs:
        x = v
    return x + 1
def h() -> int:
    mut x: int? = None
    while True:
        x = load()
        if x is not None:
            break
    return x + 1
def k(xs: list[int]) -> int:
    for v in xs:
        if v > 2:
            break
    else:
        return 0
    return 1
def m(xs: list[int?]) -> int:
    mut t: int = 0
    for v in xs:
        if v is None:
            continue
        t = t + v
    return t
"#,
    );
}

// ── W1-08: invalidation gaps ──────────────────────────────────────────────

const CLEAR: &str = r#"
class Box:
    value: int?
def clear(b: Box) -> int:
    b.value = None
    return 0
"#;

#[test]
fn w1_08_a_call_in_value_position_invalidates_in_evaluation_order() {
    for body in [
        "        let ignored: int = clear(b)\n        let n: int = b.value + 1\n",
        "        print(clear(b), b.value + 1)\n",
        "        match n:\n            case 1 if clear(b) == 0:\n                let k: int = b.value + 1\n            case _:\n                pass\n",
    ] {
        assert_rejected(&format!(
            "{CLEAR}\ndef f(b: Box, n: int) -> None:\n    if b.value is not None:\n{body}"
        ));
    }
}

#[test]
fn w1_08_writes_through_unnamed_objects_and_aliases_invalidate_the_field() {
    assert_rejected(
        r#"
class Box:
    v: int?
def f(reg: dict[str, Box], b: Box) -> None:
    if b.v is not None:
        reg["k"].v = None
        let n: int = b.v + 1
"#,
    );
    assert_rejected(
        r#"
class H:
    name: str?
def f(hs: list[H], h: H) -> None:
    if h.name is not None:
        hs[0].name = None
        print(h.name.upper())
"#,
    );
    assert_rejected(
        r#"
class Box:
    value: int?
class H:
    b: Box
impl H:
    def reset(self) -> None:
        self.b.value = None
def f(h: H, b: Box) -> None:
    if b.value is not None:
        h.reset()
        let n: int = b.value + 1
"#,
    );
}

#[test]
fn w1_08_yield_hands_control_to_the_caller() {
    assert_rejected(
        r#"
class Box:
    value: int?
def gen(b: Box) -> Iterator[int]:
    if b.value is not None:
        yield 1
        let z: int = b.value + 1
        yield z
"#,
    );
}

#[test]
fn w1_08_a_closure_does_not_keep_a_narrowing_reassigned_after_it() {
    assert_rejected(
        r#"
from typing import Callable
def f() -> None:
    mut v: str? = "x"
    if v is not None:
        let g: Callable[[], str] = lambda: v.upper()
        v = None
        print(g())
"#,
    );
    assert_rejected(
        r#"
def f() -> None:
    mut v: str? = "x"
    if v is not None:
        def g() -> str:
            return v.upper()
        v = None
        print(g())
"#,
    );
    // Not reassigned afterwards: the narrowing holds.
    assert_clean(
        r#"
from typing import Callable
def f(v: str?, xs: list[int], b: Box) -> None:
    if v is not None:
        let g: Callable[[], str] = lambda: v.upper()
        print(g())
    if b.value is not None:
        let n: int = len(xs) + b.value
        print(str(b), b.value + 1)
class Box:
    value: int?
"#,
    );
}
