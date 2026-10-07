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

/// `check`, with the resolver told which declarations are `plain class` /
/// `class!`, as `tyc-db` tells it.
fn check_class_kinds(src: &str) -> Diagnostics {
    check_class_kinds_with(src, true)
}

/// `markers: false` leaves the resolver only the original source, from which
/// it knows the marked classes by name alone (some LSP paths).
fn check_class_kinds_with(src: &str, markers: bool) -> Diagnostics {
    use tyc_resolve::{resolve_module_with, ResolveOptions};
    use tyc_syntax::preprocess::line_byte_starts;
    let prep = preprocess(src);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .unwrap()
        .into_syntax();
    let options = ResolveOptions {
        raw_class_byte_starts: line_byte_starts(&prep.python_source, &prep.raw_class_lines),
        original_source: Some(src.to_owned()),
        plain_class_byte_starts: markers
            .then(|| line_byte_starts(&prep.python_source, &prep.plain_class_lines)),
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

/// A `plain class` / `class!` marker belongs to one declaration: a
/// same-named class in another scope keeps (or lacks) the copy on its own.
#[test]
fn the_class_gate_reads_each_declarations_own_marker() {
    for src in [
        // Module dataclass, unrelated nested `plain class` / `class!`.
        "freeze let BASE = [1, 2]\nclass Foo:\n    items: list[int] = BASE\n\
def helper() -> int:\n    plain class Foo:\n        n: int\n    return 1\n",
        "freeze let BASE = [1, 2]\nclass Foo:\n    items: list[int] = BASE\n\
def helper() -> int:\n    class! Foo:\n        pass\n    return 1\n",
        // Module `plain class`, local dataclass of the same name.
        "T: tuple[int, ...] = (1, 2)\nplain class Bag:\n    items: list[int]\n\
def make() -> list[int]:\n    class Bag:\n        items: list[int] = T\n\
\x20   let b = Bag()\n    b.items.append(3)\n    return b.items\n",
    ] {
        assert_clean(&check_class_kinds(src), src);
    }
    // The marked class itself still stores its default as written.
    let marked = "T: tuple[int, ...] = (1, 2)\nplain class Bag:\n    items: list[int] = T\n\
def make() -> int:\n    class Bag:\n        n: int = 1\n    return Bag().n\n";
    assert_mismatch(&check_class_kinds(marked), marked);
    // Without per-declaration markers the name decides, as before.
    let by_name = "T: tuple[int, ...] = (1, 2)\nplain class Bag:\n    items: list[int] = T\n";
    assert_mismatch(&check_class_kinds_with(by_name, false), by_name);
}

/// Exceptions and metaclasses lower like `class!`, and a class under an `if`
/// is never decorated: none of them copies its default.
#[test]
fn classes_the_desugar_does_not_decorate_keep_their_default_as_written() {
    for class in [
        "class AppError(Exception):\n    codes: list[int] = T\n",
        "class AppWarning(UserWarning):\n    codes: list[int] = T\n",
        "class Meta(type):\n    items: list[int] = T\n",
        "class Failure(Exception):\n    pass\nclass Timeout(Failure):\n    codes: list[int] = T\n",
        "class Meta(type):\n    pass\nclass Strict(Meta):\n    items: list[int] = T\n",
        "def main() -> None:\n    class AppError(ValueError):\n        codes: list[int] = T\n\
\x20   print(AppError().codes)\n",
        "if True:\n    class Cfg:\n        items: list[int] = T\n",
    ] {
        let src = format!("{FIELD_PRELUDE}{class}");
        assert_mismatch(&check_class_kinds(&src), &src);
    }
    // A `*Error`-named module dataclass is no exception, and a class nested
    // in a function or another class is still reached.
    for class in [
        "class LexError:\n    line: int = 0\nclass Detailed(LexError):\n    codes: list[int] = T\n",
        "class Outer:\n    n: int = 0\n    class Inner:\n        items: list[int] = T\n",
    ] {
        let src = format!("{FIELD_PRELUDE}{class}");
        assert_clean(&check_class_kinds(&src), &src);
    }
}

/// The default's factory reads an enclosing function's local before the
/// module binding; the checker agrees with the desugar on which one.
#[test]
fn a_shadowed_named_default_is_checked_against_the_local_it_reads() {
    // Desugared without a copy: the field holds the local tuple.
    let src = "BASE: list[int] = [1, 2]\n\
def make() -> tuple[int, ...]:\n    let BASE: tuple[int, ...] = (7, 8)\n\
\x20   class Cfg:\n        items: tuple[int, ...] = BASE\n\
\x20   let c = Cfg()\n    return c.items + (9,)\n";
    assert_clean(&check_class_kinds(src), src);
    // `more` is copied by its own `list` annotation; `items` is the tuple,
    // which is a `Sequence` too.
    let copied = "from collections.abc import Sequence\nBASE: list[int] = [1, 2]\n\
def make() -> None:\n    let BASE: tuple[int, ...] = (7, 8)\n\
\x20   class Cfg:\n        items: Sequence[int] = BASE\n        more: list[int] = BASE\n";
    assert_clean(&check_class_kinds(copied), copied);
    // The module list's kind no longer stands in for the local tuple.
    let shadowed = "BASE: list[int] = [1, 2]\n\
def make() -> None:\n    let BASE: tuple[int, ...] = (7, 8)\n\
\x20   class Cfg:\n        items: list[int] | None = BASE\n";
    assert_mismatch(&check_class_kinds(shadowed), shadowed);
    let local_list = "from collections.abc import Sequence\n\
def make() -> None:\n    let BASE: list[int] = [1]\n\
\x20   class Cfg:\n        items: Sequence[int] = BASE\n    print(Cfg().items)\n";
    assert_clean(&check_class_kinds(local_list), local_list);
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

/// An annotation wider than the field: the value is inferred under the field
/// it is stored into, so a literal or a display that fits the field is not
/// widened past it first.
#[test]
fn an_annotated_attribute_target_wider_than_its_field_keeps_the_value_precise() {
    let src = "from collections.abc import Mapping, Sequence\n\
type Mode = \"fast\" | \"slow\"\n\
class Job:\n    mode: Mode = \"fast\"\n    weights: list[float] = []\n    data: dict[str, float] = {}\n\
impl Job:\n    def reset(self) -> None:\n\
\x20       self.mode: str = \"slow\"\n\
\x20       self.weights: Sequence[float] = [1, 2]\n\
\x20       self.data: Mapping[str, float] = {\"a\": 1}\n\
def main() -> None:\n    let j: Job = Job()\n    j.reset()\n    j.mode: str = \"fast\"\n    print(j)\n";
    assert_clean(&check(src), src);
    // A value that fits the annotation but not the field is still reported,
    // once.
    for body in [
        "        self.mode: str = \"medium\"\n",
        "        self.weights: Sequence[float] = (1.0,)\n",
    ] {
        let bad = format!(
            "from collections.abc import Sequence\n\
             type Mode = \"fast\" | \"slow\"\n\
             class Job:\n    mode: Mode = \"fast\"\n    weights: list[float] = []\n\
             impl Job:\n    def reset(self) -> None:\n{body}"
        );
        let d = check(&bad);
        assert_mismatch(&d, &bad);
        assert_eq!(d.errors().len(), 1, "{bad}: {:?}", messages(&d));
    }
}

/// The annotation and the field each read the value under their own type: a
/// field that is not a subtype of the annotation (a TypedDict under a
/// `Mapping` or `dict` annotation) or an alias under `?` still sees the value
/// at its full precision, and the annotation is checked exactly as before.
#[test]
fn an_annotated_attribute_target_reads_the_value_under_each_type_separately() {
    let src = "from collections.abc import Mapping\nfrom typing import TypedDict\n\
type Mode = \"fast\" | \"slow\"\n\
class Cfg(TypedDict):\n    a: int\n\
class Job:\n    cfg: Cfg\n    mode: Mode? = None\n    pair: tuple[float, Mode] = (0.0, \"fast\")\n\
impl Job:\n    def reset(self, fast: bool) -> None:\n\
\x20       self.cfg: Mapping[str, object] = {\"a\": 1}\n\
\x20       self.cfg: dict[str, int] = {\"a\": 2}\n\
\x20       self.mode: str? = \"slow\"\n\
\x20       self.mode: object = \"fast\" if fast else None\n\
\x20       self.pair: tuple[float, str] = (1, \"slow\")\n\
def main() -> None:\n    let j: Job = Job({\"a\": 0})\n    j.reset(True)\n    print(j)\n";
    assert_clean(&check(src), src);
    // A value the field rejects under its own reading is reported, once.
    for body in [
        "        self.mode: str? = \"medium\"\n",
        "        self.mode: object = \"fast\" if fast else \"medium\"\n",
        "        self.pair: tuple[float, str] = (1, \"medium\")\n",
    ] {
        let bad = format!(
            "type Mode = \"fast\" | \"slow\"\n\
             class Job:\n    mode: Mode? = None\n    pair: tuple[float, Mode] = (0.0, \"fast\")\n\
             impl Job:\n    def reset(self, fast: bool) -> None:\n{body}"
        );
        let d = check(&bad);
        assert_mismatch(&d, &bad);
        assert_eq!(d.errors().len(), 1, "{bad}: {:?}", messages(&d));
    }
    // An effectful value is read once: under a narrower field, or under the
    // annotation — where a display, literal or generic call whose type
    // depends on that reading is not held against the field.
    let effectful = "from collections.abc import Mapping, Sequence\nfrom typing import TypedDict\n\
type Mode = \"fast\" | \"slow\"\n\
class Cfg(TypedDict):\n    a: int\n\
def one() -> int:\n    return 1\n\
def flag() -> bool:\n    return True\n\
def empty[T]() -> list[T]:\n    return []\n\
class Job:\n    cfg: Cfg\n    mode: Mode = \"fast\"\n    items: list[float] = []\n\
impl Job:\n    def reset(self) -> None:\n\
\x20       self.cfg: Mapping[str, object] = {\"a\": one()}\n\
\x20       self.mode: str = \"slow\" if flag() else \"fast\"\n\
\x20       self.items: Sequence[object] = empty()\n\
def main() -> None:\n    let j: Job = Job({\"a\": 0})\n    j.reset()\n    print(j)\n";
    assert_clean(&check(effectful), effectful);
    // A call is still checked inside, and reported once; its result is
    // checked against the field whichever type annotates it.
    let call = "class Job:\n    n: int = 0\n\
def make(x: int) -> int:\n    return x\n\
impl Job:\n    def reset(self) -> None:\n        self.n: int = make(\"x\")\n";
    let d = check(call);
    assert_eq!(d.errors().len(), 1, "{:?}", messages(&d));
    for ann in ["int", "object"] {
        let bad = format!(
            "class Job:\n    name: str = \"\"\n\
             def make() -> int:\n    return 1\n\
             impl Job:\n    def reset(self) -> None:\n        self.name: {ann} = make()\n"
        );
        let d = check(&bad);
        assert_mismatch(&d, &bad);
        assert_eq!(d.errors().len(), 1, "{bad}: {:?}", messages(&d));
    }
}

/// A string literal under `Alias?` (`Alias | None`, an alias of a literal
/// union) is that literal, as it is under the bare alias — and a parameter
/// default reads as its annotation asks, as an annotated binding does.
#[test]
fn a_string_literal_fits_an_optional_literal_alias() {
    let src = "type Mode = \"fast\" | \"slow\"\n\
type MaybeMode = Mode | None\n\
class Job:\n    mode: Mode? = None\n\
def run(m: Mode? = \"fast\") -> Mode?:\n    return m\n\
def main() -> None:\n\
\x20   let m: Mode? = \"slow\"\n    let n: MaybeMode = \"fast\"\n    let o: Mode | int | None = \"slow\"\n\
\x20   let j: Job = Job(\"slow\")\n    let k: Job = Job(mode=\"fast\")\n\
\x20   print(m, n, o, j, k, run(\"slow\"), run())\n";
    assert_clean(&check(src), src);
    let defaults = "type Mode = \"fast\" | \"slow\"\n\
def run(m: Mode = \"fast\", w: tuple[float, ...] = (1, 2), d: dict[str, Mode] = {}) -> None:\n    print(m, w, d)\n";
    assert_clean(&check(defaults), defaults);
    for bad in [
        "type Mode = \"fast\" | \"slow\"\nlet m: Mode? = \"medium\"\n",
        "type Mode = \"fast\" | \"slow\"\ndef run(m: Mode? = \"medium\") -> None:\n    print(m)\n",
        "type Mode = \"fast\" | \"slow\"\nclass Job:\n    mode: Mode? = None\nlet j: Job = Job(\"medium\")\n",
    ] {
        assert!(!check(bad).errors().is_empty(), "accepted:\n{bad}");
    }
}

/// The named-default table the checker shares with the desugar follows
/// `global` and `nonlocal`: a nested `global BASE` reads the module's list
/// (copied), and a `nonlocal` rebind to a tuple stops a local list from
/// standing in for the default.
#[test]
fn a_named_default_follows_global_and_nonlocal_declarations() {
    let global = "mut BASE: list[int] = [1, 2]\n\
def outer() -> None:\n    let BASE: list[int] | tuple[int, ...] = (7, 8)\n\
\x20   def inner() -> None:\n        global BASE\n\
\x20       class Cfg:\n            items: list[int] | None = BASE\n\
\x20       print(Cfg().items)\n    inner()\n";
    assert_clean(&check_class_kinds(global), global);
    let nonlocal = "from collections.abc import Sequence\n\
def outer() -> None:\n    mut LOC: Sequence[int] = [1]\n\
\x20   def inner() -> None:\n        nonlocal LOC\n        LOC = (2, 3)\n\
\x20   inner()\n\
\x20   class C:\n        items: list[int] | None = LOC\n    print(C().items)\n";
    assert_mismatch(&check_class_kinds(nonlocal), nonlocal);
    // A rebind that keeps the list keeps the copy.
    let same = nonlocal.replace("LOC = (2, 3)", "LOC = [2, 3]");
    assert_clean(&check_class_kinds(&same), &same);
}

/// A function-local subclass of a function-local exception is emitted
/// without `@dataclass`, as at module level, so its named default is
/// checked as written there too.
#[test]
fn a_local_exception_subclass_keeps_its_default_as_written() {
    let src = format!(
        "{FIELD_PRELUDE}def main() -> None:\n    class Failure(Exception):\n        pass\n\
\x20   class Timeout(Failure):\n        codes: list[int] = T\n    print(Timeout().codes)\n"
    );
    assert_mismatch(&check_class_kinds(&src), &src);
    // Raising one with a message, and a local metaclass chain, check clean.
    let ok = "def make() -> None:\n    class Failure(Exception):\n        pass\n\
\x20   class Timeout(Failure):\n        pass\n\
\x20   try:\n        raise Timeout(\"slow\")\n    except Failure as e:\n        print(\"caught\", e)\n\
\x20   class LMeta(type):\n        pass\n    class LMeta2(LMeta):\n        pass\n\
\x20   class Uses(metaclass=LMeta2):\n        x: int = 0\n    print(Uses().x)\n";
    assert_clean(&check_class_kinds(ok), ok);
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

/// The narrowed class has to fit the current type, as a `match` class
/// pattern's does: a `T` (bounded or not), a newtype and a literal union are
/// still that type when their runtime class is `bool` / `int` / `str` / `Dog`.
#[test]
fn type_identity_keeps_a_typevar_newtype_or_literal_type() {
    let src = "newtype UserId = int\ntype Mode = \"fast\" | \"slow\"\n\
def label(uid: UserId) -> str:\n    return f\"user-{uid}\"\n\
def run(m: Mode) -> str:\n    return m\n\
def passthrough[T](x: T) -> T:\n    if type(x) is bool:\n        return x\n    return x\n\
def same[T](x: T) -> T:\n    if type(x) == float:\n        return x\n    return x\n\
def show(uid: UserId) -> str:\n    if type(uid) is int:\n        return label(uid)\n    return \"?\"\n\
def pick(m: Mode) -> str:\n    if m.__class__ is str:\n        return run(m)\n    return \"?\"\n\
class Animal:\n    name: str\nclass Dog(Animal):\n    pass\n\
def keep[T: Animal](a: T) -> T:\n    if type(a) is Dog:\n        return a\n    return a\n\
def main() -> None:\n    print(passthrough(True), same(1.5), show(UserId(7)), pick(\"slow\"), keep(Dog(\"d\")).name)\n";
    assert_clean(&check(src), src);
    // Class-hierarchy and union narrowings still apply.
    let narrows = "class Animal:\n    name: str\nclass Dog(Animal):\n    def bark(self) -> str:\n        return \"w\"\n\
def f(a: Animal, x: int | str) -> int:\n    if type(a) is Dog:\n        print(a.bark())\n\
\x20   if type(x) is int:\n        return x + 1\n    return 0\n";
    assert_clean(&check(narrows), narrows);
}

/// A class type parameter read through `self` infers as `Unknown`, which
/// accepts any replacement: `type(self.item) is bool` leaves it as it was
/// rather than turning a `T` into a `bool`.
#[test]
fn type_identity_leaves_an_unknown_subject_alone() {
    let src = "class Animal:\n    name: str\nclass Dog(Animal):\n    pass\n\
class Box[T]:\n    item: T\n\
impl[T] Box[T]:\n    def get(self) -> T:\n        if type(self.item) is bool:\n            return self.item\n        return self.item\n\
\x20   def get2(self) -> T:\n        let it = self.item\n        if type(it) is bool:\n            return it\n        return it\n\
\x20   def get3(self) -> T:\n        if self.item.__class__ == int:\n            return self.item\n        return self.item\n\
class Pen[T: Animal]:\n    pet: T\n\
impl[T: Animal] Pen[T]:\n    def pet_of(self) -> T:\n        if type(self.pet) is Dog:\n            return self.pet\n        return self.pet\n\
def main() -> None:\n    print(Box(True).get(), Box(3).get2(), Box(1).get3(), Pen(Dog(\"rex\")).pet_of().name)\n";
    assert_clean(&check(src), src);
    // The class-body method form.
    let body = "class Box[T]:\n    item: T\n\n    def get(self) -> T:\n\
\x20       if type(self.item) is bool:\n            return self.item\n        return self.item\n";
    assert_clean(&check(body), body);
    // A known subject still narrows, and a wrong use is still caught.
    let bad = "def f(x: int | str) -> None:\n    if type(x) is int:\n        print(x.upper())\n";
    assert!(!check(bad).errors().is_empty(), "accepted:\n{bad}");
}

/// A name in `C`'s place that is bound to a value — a `let`, a parameter, a
/// loop target — says nothing about the runtime class, even when the name is
/// `int` or a class's; neither does a call to a rebound `type`.
#[test]
fn type_identity_does_not_trust_a_shadowed_class_name() {
    for src in [
        "def f(x: int | str, cls: type[object]) -> None:\n    let int = cls\n    if type(x) is int:\n        print(x + 1)\n",
        "def f(x: int | str, int: type[object]) -> None:\n    if type(x) is int:\n        print(x + 1)\n",
        "def f(x: int | str, int: type[object]) -> None:\n    if type(x) == int:\n        print(x + 1)\n",
        "def f(x: int | str, int: type[object]) -> None:\n    if x.__class__ is int:\n        print(x + 1)\n",
        "class Foo:\n    v: int = 1\nclass Bar:\n    w: int = 2\n\
def f(x: Foo | Bar) -> None:\n    for Foo in (Bar,):\n        if type(x) is Foo:\n            print(x.v)\n",
        "from collections.abc import Callable\n\
def f(x: int | str, type: Callable[[object], object]) -> None:\n    if type(x) is int:\n        print(x + 1)\n",
    ] {
        assert!(!check(src).errors().is_empty(), "accepted:\n{src}");
    }
    // A class declared in the function itself is still a class.
    let local =
        "def f() -> int:\n    class Foo:\n        v: int = 1\n    class Bar:\n        w: int = 2\n\
\x20   let x: Foo | Bar = Foo()\n    if type(x) is Foo:\n        return x.v\n    return 0\n";
    assert_clean(&check(local), local);
}

/// `collections.abc.Set` is the read-only set ABC — a `frozenset` is one —
/// not the deprecated `typing.Set` alias of `set`. `typing.AbstractSet` is
/// the same ABC.
#[test]
fn an_abstract_set_annotation_accepts_frozen_and_mutable_sets() {
    for src in [
        "from collections.abc import Set\n\
freeze let TAGS = {\"a\", \"b\"}\n\
def main() -> None:\n    let tags: Set[str] = TAGS\n    let more: Set[str] = frozenset({\"c\"})\n\
\x20   mut m: Set[str] = more\n    m = {\"d\"}\n\
\x20   print(sorted(tags | more), \"a\" in tags, len(m))\n",
        "import collections.abc\nS = frozenset({1, 2})\n\
def main() -> None:\n    let s: collections.abc.Set[int] = S\n    print(len(s))\n",
        "from collections.abc import Set\n\
def size(s: Set[int]) -> int:\n    return len(s)\n\
def main() -> None:\n    print(size(frozenset({1})), size({1, 2}), size({3: 4}.keys()))\n",
        "from typing import AbstractSet\n\
def size(s: AbstractSet[int]) -> int:\n    return len(s)\n\
def main() -> None:\n    let a: AbstractSet[int] = frozenset({1})\n    print(size(a), size({1, 2}))\n",
        "from collections.abc import Set\n\
def total(s: Set[int]) -> int:\n    mut n = 0\n    for x in s:\n        n += x\n    return n\n",
    ] {
        assert_clean(&check(src), src);
    }
    for src in [
        // `typing.Set` is still `set`.
        "from typing import Set\ndef main() -> None:\n    let s: Set[int] = frozenset({1})\n",
        "import typing\ndef main() -> None:\n    let s: typing.Set[int] = frozenset({1})\n",
        // A string is not a set, and the element type still has to fit.
        "from collections.abc import Set\ndef main() -> None:\n    let s: Set[str] = \"abc\"\n",
        "from collections.abc import Set\ndef main() -> None:\n    let s: Set[int] = frozenset({\"a\"})\n",
        "from collections.abc import Set\ndef main() -> None:\n    let s: Set[int] = [1, 2]\n",
    ] {
        assert_mismatch(&check(src), src);
    }
    let mutable = "from typing import Set\ndef main() -> None:\n    mut s: Set[int] = set()\n    s.add(1)\n    print(s)\n";
    assert_clean(&check(mutable), mutable);
}

/// Rebinding the root of a narrowed attribute path — by a `for` target, a
/// walrus (in the arm, a `case` guard, a later `and` operand or a
/// comprehension), `with … as`, a pattern capture, `+=`, `del`, or a callee's
/// `global` — stales the narrowing, as `h = …` already did. A property is
/// re-read every time, so it is never narrowed by `match` / `type(…) is`.
#[test]
fn rebinding_the_root_of_a_narrowed_attribute_path_drops_the_narrowing() {
    let decls = "from collections.abc import Mapping\n\
class Holder:\n    items: Mapping[str, int]\n\
def other() -> Holder:\n    return Holder(items={\"a\": 1})\n";
    for body in [
        "    match h.items:\n        case dict():\n            for h in others:\n                h.items[\"k\"] = 1\n        case _:\n            pass\n",
        "    if type(h.items) is dict:\n        for h in others:\n            h.items[\"k\"] = 1\n",
        "    if isinstance(h.items, dict):\n        for h in others:\n            h.items[\"k\"] = 1\n",
        "    if type(h.items) is dict:\n        if (h := other()) is not None:\n            h.items[\"k\"] = 1\n",
        "    match h.items:\n        case dict() if (h := other()) is not None:\n            h.items[\"k\"] = 1\n        case _:\n            pass\n",
        "    if type(h.items) is dict and (h := other()) is not None:\n        h.items[\"k\"] = 1\n",
        "    if isinstance(h.items, dict) and (h := other()) is not None:\n        h.items[\"k\"] = 1\n",
        "    if type(h.items) is dict:\n        let hs = [h := o for o in others]\n        print(hs)\n        h.items[\"k\"] = 1\n",
        "    match h.items:\n        case dict():\n            match others:\n                case [h, *_]:\n                    h.items[\"k\"] = 1\n                case _:\n                    pass\n        case _:\n            pass\n",
        "    if type(h.items) is dict:\n        del h\n        h = other()\n        h.items[\"k\"] = 1\n",
    ] {
        let src = format!(
            "{decls}def f(first: Holder, others: list[Holder]) -> None:\n    mut h: Holder = first\n{body}"
        );
        assert!(!check(&src).errors().is_empty(), "accepted:\n{src}");
    }
    let with_as = format!(
        "{decls}class Ctx:\n    held: Holder\n\
impl Ctx:\n    def __enter__(self) -> Holder:\n        return self.held\n\
\x20   def __exit__(self, *args: object) -> None:\n        pass\n\
def f(h: Holder) -> None:\n    if type(h.items) is dict:\n        with Ctx(held=other()) as h:\n            h.items[\"k\"] = 1\n"
    );
    assert!(!check(&with_as).errors().is_empty(), "accepted:\n{with_as}");
    let global = format!(
        "{decls}mut G: Holder = other()\n\
def swap() -> None:\n    global G\n    G = other()\n\
def f() -> None:\n    if type(G.items) is dict:\n        swap()\n        G.items[\"k\"] = 1\n"
    );
    assert!(!check(&global).errors().is_empty(), "accepted:\n{global}");
    let property = "from collections.abc import Mapping\nclass Holder:\n    n: int\n\
impl Holder:\n    @property\n    def items(self) -> Mapping[str, int]:\n        return {\"b\": 2}\n\
def f(h: Holder) -> None:\n    match h.items:\n        case dict():\n            h.items[\"k\"] = 3\n        case _:\n            pass\n\
\x20   if type(h.items) is dict:\n        h.items[\"k\"] = 3\n";
    assert_eq!(
        check(property).errors().len(),
        2,
        "{property}: {:?}",
        messages(&check(property))
    );
    // Untouched roots keep the narrowing, and a comprehension's own `h` is
    // not the function's.
    let kept = format!(
        "{decls}def f(h: Holder, others: list[Holder]) -> None:\n\
\x20   if type(h.items) is dict:\n        let names = [h.items for h in others]\n        print(names)\n        h.items[\"k\"] = 1\n\
\x20   match h.items:\n        case dict() as d:\n            d[\"j\"] = 2\n            h.items[\"k\"] = 1\n        case _:\n            pass\n"
    );
    assert_clean(&check(&kept), &kept);
}
