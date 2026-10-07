//! Annotated bindings enforce their declared type.
//!
//! An annotated assignment used to re-type itself to the annotation's frozen
//! shape whenever the value fit that shape but not the annotation, so
//! `let l: list[int] = T` with `T` a tuple bound a `tuple[int, ...]` with no
//! diagnostic — while everything that reads the annotation syntactically (a
//! `@contextmanager` yield type, an attribute's declared field) still saw a
//! list. Only `freeze let` deep-freezes its value; every other annotated
//! binding is now checked like a call argument. The exceptions are the
//! dataclass field defaults the desugar copies per instance (W7-07), and the
//! container narrowings that the old rule had been standing in for.

use super::*;
use tyc_resolve::resolve_module;
use tyc_syntax::preprocess::{expand_and_preprocess_mapped, PreprocessResult};

/// The CLI's front end: the sugar chain (typed tuple unpack, `lazy let`, …)
/// and then the keyword pass.
fn preprocess(src: &str) -> PreprocessResult {
    expand_and_preprocess_mapped(src, false)
}

fn check(src: &str) -> Diagnostics {
    check_with(src, CheckOptions::default())
}

fn check_with(src: &str, options: CheckOptions) -> Diagnostics {
    let prep = preprocess(src);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .unwrap()
        .into_syntax();
    let (resolved, _) = resolve_module("<test>".to_owned(), &prep.python_source, &module);
    check_module_with_options(
        "<test>",
        &prep.python_source,
        &resolved,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
        &prep.impl_distributed_lines,
        None,
        options,
    )
    .diagnostics
}

/// A 3.15 target, with or without `[emit] freeze-dict = "frozendict"`.
fn check_315(src: &str, frozendict: bool) -> Diagnostics {
    check_with(
        src,
        CheckOptions {
            freeze_to_frozendict: frozendict,
            ..CheckOptions::for_target(15)
        },
    )
}

/// `check`, with the resolver told which classes are `plain class` /
/// `class!` (the CLI passes the original source for this).
fn check_class_kinds(src: &str) -> Diagnostics {
    use tyc_resolve::{resolve_module_with, ResolveOptions};
    let prep = preprocess(src);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .unwrap()
        .into_syntax();
    let options = ResolveOptions {
        raw_class_byte_starts: tyc_syntax::preprocess::line_byte_starts(
            &prep.python_source,
            &prep.raw_class_lines,
        ),
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

fn mismatches(d: &Diagnostics) -> usize {
    d.errors()
        .iter()
        .filter(|e| matches!(e, TycError::TypeMismatch { .. }))
        .count()
}

fn assert_mismatch(d: &Diagnostics, src: &str) {
    assert!(
        mismatches(d) > 0,
        "expected a type_mismatch for:\n{src}\ngot {:?}",
        messages(d)
    );
}

fn assert_clean(d: &Diagnostics, src: &str) {
    assert!(
        d.errors().is_empty(),
        "expected no errors for:\n{src}\ngot {:?}",
        messages(d)
    );
}

const PRELUDE: &str = "from collections.abc import Mapping\n\
def getm() -> Mapping[str, int]:\n    return {\"a\": 1}\n\
let T: tuple[int, ...] = (1, 2)\n\
let FS: frozenset[int] = frozenset({1})\n";

// ── Bug: the frozen-shape re-type ───────────────────────────────────────

#[test]
fn annotated_bindings_reject_the_frozen_variant_of_their_container() {
    for body in [
        "def f() -> None:\n    let l: list[int] = T\n",
        "def f() -> None:\n    let s: set[int] = FS\n",
        "def f() -> None:\n    let d: dict[str, int] = getm()\n",
        "def f() -> None:\n    let l: list[int] = (1, 2)\n",
        "def f() -> None:\n    let l: list[int] = tuple([1])\n",
        "def f() -> None:\n    let l: list[int] = ()\n",
        "def f() -> None:\n    let s: set[int] = frozenset()\n",
        "def f() -> None:\n    mut m: list[int] = T\n",
        // Optional, union, alias and nested annotations.
        "def f() -> None:\n    let l: list[int]? = T\n",
        "def f() -> None:\n    let l: list[int] | None = T\n",
        "def f() -> None:\n    let u: list[int] | str = (1, 2)\n",
        "def f() -> None:\n    let l: list[list[int]] = ((1,), (2,))\n",
        "def f() -> None:\n    let a: tuple[list[int], ...] = ((1,),)\n",
        "def f() -> None:\n    let n: dict[str, list[int]] = {\"a\": (1,)}\n",
        "type IntList = list[int]\nlet AL: IntList = T\n",
        // Every binding position.
        "let L: list[int] = T\n",
        "pub let PL: list[int] = T\n",
        "X: list[int] = T\n",
        "mut M: set[int] = FS\n",
    ] {
        let src = format!("{PRELUDE}{body}");
        assert_mismatch(&check(&src), &src);
    }
}

#[test]
fn a_typed_tuple_unpack_leg_enforces_its_annotation() {
    let src = "def pair() -> tuple[tuple[int, ...], int]:\n    return ((1, 2), 1)\n\
               def f() -> None:\n    let (a: list[int], b: int) = pair()\n    print(a, b)\n";
    assert_mismatch(&check(src), src);
    let ok = "def pair() -> tuple[tuple[int, ...], int]:\n    return ((1, 2), 1)\n\
              def f() -> None:\n    let (a: tuple[int, ...], b: int) = pair()\n    print(a, b)\n";
    assert_clean(&check(ok), ok);
}

#[test]
fn a_comptime_let_enforces_its_annotation() {
    let src = "comptime let Y: list[int] = (1, 2)\nprint(Y)\n";
    assert_mismatch(&check(src), src);
}

#[test]
fn a_frozendict_does_not_fill_a_dict_annotation_under_either_knob() {
    let src = "def main() -> None:\n\
               \x20   let fd: frozendict[str, int] = frozendict({\"a\": 1})\n\
               \x20   let d: dict[str, int] = fd\n\
               \x20   d[\"b\"] = 2\n";
    for frozendict in [false, true] {
        let d = check_315(src, frozendict);
        assert_eq!(mismatches(&d), 1, "{frozendict}: {:?}", messages(&d));
        assert_eq!(d.errors().len(), 1, "{frozendict}: {:?}", messages(&d));
    }
    let frozen = "freeze let CFG: dict[str, list[int]] = {\"a\": [1]}\n\
                  def f() -> None:\n    let r: dict[str, list[int]] = CFG\n    print(r)\n";
    for frozendict in [false, true] {
        assert_mismatch(&check_315(frozen, frozendict), frozen);
    }
}

/// The rejected binding keeps the type it declared, so its correct later
/// uses are no longer misreported against the tuple it was re-typed to.
#[test]
fn a_rejected_annotated_binding_keeps_its_declared_type() {
    let src = "let T: tuple[int, ...] = (1, 2)\n\
               def f() -> None:\n    mut m: list[int] = T\n    m = [1, 2]\n    m.append(3)\n";
    let d = check(src);
    assert_eq!(d.errors().len(), 1, "{:?}", messages(&d));
    assert_eq!(mismatches(&d), 1, "{:?}", messages(&d));
}

/// The `@contextmanager` yield type comes from the syntactic annotation, so
/// the re-type let `r.append` through and crash.
#[test]
fn a_contextmanager_yield_of_a_misannotated_binding_is_rejected() {
    let src = "from contextlib import contextmanager\n\
               from collections.abc import Iterator, Sequence\n\
               let T: tuple[int, ...] = (1, 2)\n\
               @contextmanager\n\
               def cm() -> Iterator[Sequence[int]]:\n    let s: list[int] = T\n    yield s\n\
               def main() -> None:\n    with cm() as r:\n        r.append(3)\n";
    assert_mismatch(&check(src), src);
}

#[test]
fn freeze_let_and_correctly_annotated_frozen_reads_stay_accepted() {
    let decls = "from collections.abc import Mapping, Sequence\n\
                 freeze let NAMES: list[str] = [\"a\"]\n\
                 freeze let CFG: dict[str, list[int]] = {\"a\": [1]}\n\
                 freeze let TAGS: set[str] = {\"t\"}\n";
    let ok = format!(
        "{decls}def f() -> None:\n\
         \x20   let names: tuple[str, ...] = NAMES\n\
         \x20   let seq: Sequence[str] = NAMES\n\
         \x20   let row: tuple[int, ...] = CFG[\"a\"]\n\
         \x20   let m: Mapping[str, tuple[int, ...]] = CFG\n\
         \x20   let tags: frozenset[str] = TAGS\n\
         \x20   let copy: list[str] = list(NAMES)\n\
         \x20   let alias = NAMES\n\
         \x20   print(names, seq, row, m, tags, copy, alias)\n"
    );
    assert_clean(&check(&ok), &ok);
    for bad in [
        "    NAMES.append(\"b\")\n",
        "    let alias = NAMES\n    alias.append(\"b\")\n",
        "    let row: list[int] = CFG[\"a\"]\n",
        "    let d: dict[str, list[int]] = CFG\n",
        "    let t: set[str] = TAGS\n",
    ] {
        let src = format!("{decls}def f() -> None:\n{bad}");
        assert!(!check(&src).errors().is_empty(), "accepted:\n{src}");
    }
}

#[test]
fn an_annotated_freeze_let_dict_is_a_frozendict_when_chosen() {
    let src = "freeze let CFG: dict[str, int] = {\"port\": 8080}\n\
               let key: set[frozendict[str, int]] = {CFG}\nprint(key)\n";
    assert_clean(&check_315(src, true), src);
}

#[test]
fn conversions_into_annotated_bindings_stay_accepted() {
    let src = "from collections.abc import Mapping, Sequence\n\
               def f(t: tuple[int, ...], fs: frozenset[int], m: Mapping[str, int], \
               xss: list[list[int]]) -> None:\n\
               \x20   let a: list[int] = []\n\
               \x20   let b: list[int] = list(t)\n\
               \x20   let c: set[int] = set(fs)\n\
               \x20   let d: dict[str, int] = dict(m)\n\
               \x20   let e: list[int] = [*t]\n\
               \x20   let g: dict[str, int] = {**m}\n\
               \x20   let h: list[int] = sorted(t)\n\
               \x20   let i: Sequence[int] = t\n\
               \x20   let j: list[int] = [*xs for xs in xss]\n\
               \x20   let k: dict[str, int] = {}\n\
               \x20   let s: set[int] = set()\n\
               \x20   print(a, b, c, d, e, g, h, i, j, k, s)\n";
    assert_clean(&check_with(src, CheckOptions::for_target(15)), src);
}

// ── Class fields: the W7-07 copy ────────────────────────────────────────

const FIELD_PRELUDE: &str = "from collections.abc import Mapping\n\
let T: tuple[int, ...] = (1, 2)\n\
let FS: frozenset[int] = frozenset({1})\n\
let M: Mapping[str, int] = {\"a\": 1}\n";

/// The desugar lowers a named default to `field(default_factory=lambda:
/// list(T))`, so these fields hold a real list / set / dict.
#[test]
fn a_copied_dataclass_field_default_of_frozen_shape_is_accepted() {
    for class in [
        "class Cfg:\n    items: list[int] = T\n    tags: set[int] = FS\n    table: dict[str, int] = M\n",
        "class Cfg frozen:\n    items: list[int] = T\n",
        "class Box[V]:\n    items: list[int] = T\n    v: V? = None\n",
        // A set copies into a list, a mapping's keys into a set.
        "let S: set[int] = {1}\nclass Cfg:\n    items: list[int] = S\n    keys: set[str] = M\n",
        "def main() -> None:\n    let t: tuple[int, ...] = (1, 2)\n    class Box:\n        items: list[int] = t\n    print(Box().items)\n",
        // Copied through the module binding's own `list` annotation.
        "type IntList = list[int]\nfreeze let BASE: list[int] = [1, 2]\nclass Box:\n    items: IntList = BASE\n",
    ] {
        let src = format!("{FIELD_PRELUDE}{class}");
        assert_clean(&check_class_kinds(&src), &src);
    }
}

#[test]
fn a_dataclass_field_default_the_desugar_stores_as_written_is_checked() {
    for class in [
        // The copy is shallow: the inner tuples stay tuples.
        "let TT: tuple[tuple[int, ...], ...] = ((1, 2), (3,))\nclass Box:\n    items: list[list[int]] = TT\n",
        // An alias head is not copied.
        "type IntList = list[int]\nclass Box:\n    items: IntList = T\n",
        // Only a name is copied, never a literal or a call.
        "class Box:\n    items: list[int] = (1, 2)\n",
        "def mk() -> tuple[int, ...]:\n    return (1, 2)\nclass Box:\n    items: list[int] = mk()\n",
        "class Box:\n    tags: set[int] = frozenset({1})\n",
        // Classes the desugar does not decorate keep the default as written.
        "plain class Box:\n    items: list[int] = T\n",
        "class! Box(Exception):\n    items: list[int] = T\n",
        "model Box:\n    items: list[int] = T\n",
        "from dataclasses import dataclass\n@dataclass\nclass Box:\n    items: list[int] = T\n",
        "from typing import ClassVar\nclass Box:\n    items: ClassVar[list[int]] = T\n",
        // A name the class body binds is out of the factory lambda's reach.
        "class Box:\n    T: tuple[int, ...] = (1, 2)\n    items: list[int] = T\n",
        // Copying cannot fix the element type.
        "let TS: tuple[str, ...] = (\"a\",)\nclass Box:\n    items: list[int] = TS\n",
    ] {
        let src = format!("{FIELD_PRELUDE}{class}");
        assert_mismatch(&check_class_kinds(&src), &src);
    }
}

/// The carve-out reads neither `[emit] freeze-dict` nor the annotation's
/// frozen shape: a `Mapping` or `frozendict` name copies into a `dict` field
/// under both settings.
#[test]
fn a_copied_dict_field_default_is_accepted_under_either_knob() {
    let src = "from collections.abc import Mapping\n\
               let M: Mapping[str, int] = {\"a\": 1}\n\
               let FD: frozendict[str, int] = frozendict({\"b\": 2})\n\
               class Cfg:\n    table: dict[str, int] = M\n    other: dict[str, int] = FD\n";
    for frozendict in [false, true] {
        assert_clean(&check_315(src, frozendict), src);
    }
}

// ── Annotated attribute targets ─────────────────────────────────────────

#[test]
fn an_annotated_attribute_target_is_checked_against_annotation_and_field() {
    for (body, why) in [
        (
            "        self.items: list[int] = T\n",
            "tuple into list annotation",
        ),
        ("        self.name: int = 5\n", "int into a str field"),
    ] {
        let src = format!(
            "let T: tuple[int, ...] = (1, 2)\n\
             class C:\n    items: list[int]\n    name: str\n\
             impl C:\n    def reset(self) -> None:\n{body}"
        );
        let d = check(&src);
        assert_mismatch(&d, &src);
        assert_eq!(d.errors().len(), 1, "{why}: {:?}", messages(&d));
    }
    let ok = "class C:\n    items: list[int]\n    name: str\n\
              impl C:\n    def reset(self) -> None:\n\
              \x20       self.items: list[int] = [1]\n        self.name: str = \"n\"\n";
    assert_clean(&check(ok), ok);
}

// ── `lazy let` ───────────────────────────────────────────────────────────

#[test]
fn a_module_lazy_let_enforces_its_annotation() {
    for src in [
        "lazy let N: int = \"x\"\nprint(N)\n",
        "def mk() -> tuple[int, ...]:\n    return (1, 2)\nlazy let ITEMS: list[int] = mk()\nprint(ITEMS)\n",
    ] {
        assert_mismatch(&check(src), src);
    }
    // The binding is the factory's value, annotated or not.
    let unannotated = "lazy let NAME = \"x\"\ndef main() -> None:\n    print(NAME + 1)\n";
    assert!(
        !check(unannotated).errors().is_empty(),
        "accepted: {unannotated}"
    );
    let ok = "def load() -> dict[str, int]:\n    return {\"port\": 1}\n\
              lazy let CFG: dict[str, int] = load()\n\
              lazy let PORT = CFG[\"port\"] + 1\n\
              lazy let LATER: list[str] = names()\n\
              def names() -> list[str]:\n    return [\"a\"]\n\
              def main() -> None:\n    let p: int = CFG[\"port\"] + PORT\n    print(p, LATER[0].upper())\n";
    assert_clean(&check(ok), ok);
}

// ── Narrowings the re-type had been standing in for ─────────────────────

/// These ran correctly and passed only because `Mapping[str, int]` is
/// `dict[str, int]`'s frozen shape: with the re-type gone, the subject has to
/// narrow (review verdict "MUST STAY CLEAN").
#[test]
fn container_patterns_and_type_identity_narrow_an_abstract_subject() {
    let src = "from collections.abc import Mapping\n\
class Holder:\n    m: Mapping[str, int]\n\
def by_match(x: Mapping[str, int]) -> int:\n    match x:\n        case dict():\n            let d: dict[str, int] = x\n            return len(d) + d[\"a\"]\n        case _:\n            return 0\n\
def by_match_as(x: Mapping[str, int]) -> int:\n    match x:\n        case dict() as dd:\n            let d: dict[str, int] = dd\n            return sum(d.values())\n        case _:\n            return -1\n\
def by_guard(x: Mapping[str, int]) -> int:\n    match x:\n        case dict() if len(x) > 0:\n            let d: dict[str, int] = x\n            return d[\"a\"]\n        case _:\n            return 0\n\
def by_type_is(x: Mapping[str, int]) -> int:\n    if type(x) is dict:\n        let d: dict[str, int] = x\n        return d[\"a\"]\n    return -2\n\
def by_type_eq(x: Mapping[str, int]) -> int:\n    if type(x) == dict:\n        let d: dict[str, int] = x\n        return d[\"a\"]\n    return 0\n\
def by_class(x: Mapping[str, int]) -> int:\n    if x.__class__ is dict:\n        let d: dict[str, int] = x\n        return d[\"a\"]\n    return 0\n\
def by_attr(h: Holder) -> int:\n    match h.m:\n        case dict():\n            let d: dict[str, int] = h.m\n            return d[\"a\"]\n        case _:\n            return 0\n\
def by_type_attr(h: Holder) -> int:\n    if type(h.m) is dict:\n        let d: dict[str, int] = h.m\n        return d[\"a\"]\n    return 0\n\
def main() -> None:\n    print(by_match({\"a\": 1}), by_match_as({\"a\": 1}), by_guard({\"a\": 1}), by_type_is({\"a\": 7}), \
by_type_eq({\"a\": 1}), by_class({\"a\": 1}), by_attr(Holder(m={\"a\": 3})), by_type_attr(Holder(m={\"a\": 3})))\n";
    for frozendict in [false, true] {
        assert_clean(&check_315(src, frozendict), src);
    }
    assert_clean(&check(src), src);
    // The same narrowing clears the call-argument and return forms, which
    // never had the re-type to hide behind.
    let calls = "from collections.abc import Mapping\n\
def takes(d: dict[str, int]) -> int:\n    return d[\"a\"]\n\
def by_match(x: Mapping[str, int]) -> int:\n    match x:\n        case dict():\n            return takes(x)\n        case _:\n            return 0\n\
def ret(x: Mapping[str, int]) -> dict[str, int]:\n    if type(x) is dict:\n        return x\n    return {}\n";
    assert_clean(&check(calls), calls);
}

#[test]
fn sequence_and_set_patterns_keep_their_element_types() {
    let src = "from collections.abc import Sequence, Set\n\
def f(xs: Sequence[int], s: Set[str] | None) -> int:\n\
\x20   match xs:\n        case list():\n            let l: list[int] = xs\n            return len(l)\n\
\x20       case tuple() as t:\n            let u: tuple[int, ...] = t\n            return len(u)\n\
\x20       case _:\n            return 0\n";
    assert_clean(&check(src), src);
    // The element type is carried, not erased.
    let bad = "from collections.abc import Sequence\n\
def f(xs: Sequence[int]) -> None:\n    match xs:\n        case list():\n            let l: list[str] = xs\n        case _:\n            pass\n";
    assert_mismatch(&check(bad), bad);
}

/// `type(x) is C` says nothing where it fails (a subclass instance fails it),
/// and a variable in `C`'s place names no class.
#[test]
fn type_identity_narrows_only_the_branch_where_it_holds() {
    let negative = "class A:\n    a: int\nclass B:\n    b: int\n\
def f(v: A | B) -> int:\n    if type(v) is not A:\n        return v.b\n    return v.a\n";
    assert!(!check(negative).errors().is_empty(), "{negative}");
    let positive = "class A:\n    a: int\nclass B:\n    b: int\n\
def f(v: A | B) -> int:\n    if type(v) is A:\n        let x: A = v\n        return x.a\n    return 0\n\
def g(v: A | B) -> int:\n    if type(v) is not A:\n        return 0\n    return v.a\n";
    assert_clean(&check(positive), positive);
    let variable = "def f(x: int, cls: type) -> int:\n    if type(x) is cls:\n        return x + 1\n    return x\n";
    assert_clean(&check(variable), variable);
}
