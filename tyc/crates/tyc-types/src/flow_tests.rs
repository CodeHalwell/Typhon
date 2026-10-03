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

fn assert_clean(src: &str) {
    let d = check(src);
    assert!(
        d.errors().is_empty(),
        "expected no errors, got {:?}",
        messages(&d)
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

fn assert_rejected(src: &str) {
    let d = check(src);
    assert!(
        !d.errors().is_empty(),
        "expected the stale narrowing to be rejected, got no errors"
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
