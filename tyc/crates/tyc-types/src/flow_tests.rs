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
