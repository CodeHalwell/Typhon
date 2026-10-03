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
