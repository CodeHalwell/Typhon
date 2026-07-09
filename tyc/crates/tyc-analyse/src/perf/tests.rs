//! Unit tests for the `tyc::perf_*` / `tyc::lazy_import_opportunity` family.
//!
//! Each lint gets a positive case, the negative/guard cases that keep it
//! conservative, and (for lint 7) the stdlib / re-export / main-guard
//! exclusions. Detectors run over the *preprocessed* Python, exactly as the
//! CLI / LSP feed them.

use super::*;
use miette::Diagnostic as _;

/// Preprocess + parse `src`, run the whole perf family with `ctx`, and
/// return the `tyc::…` codes of every advice produced.
fn codes_with(src: &str, ctx: &PerfLintContext) -> Vec<String> {
    let prep = tyc_syntax::preprocess::preprocess(src);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .expect("parse failed")
        .into_syntax();
    let diags = perf_diagnostics(&module, "x.ty", &prep.python_source, ctx);
    diags
        .warnings()
        .iter()
        .filter_map(|w| w.code().map(|c| c.to_string()))
        .collect()
}

fn codes(src: &str) -> Vec<String> {
    codes_with(src, &PerfLintContext::default())
}

fn count(codes: &[String], needle: &str) -> usize {
    codes.iter().filter(|c| c.contains(needle)).count()
}

// ── lint 1: perf_membership_in_loop ───────────────────────────────────────────

#[test]
fn membership_in_loop_fires_on_invariant_list() {
    let src = "\
def f(items: list[int], xs: list[int]) -> int:
    mut hits: int = 0
    for x in xs:
        if x in items:
            hits += 1
    return hits
";
    assert_eq!(count(&codes(src), "perf_membership_in_loop"), 1);
}

#[test]
fn membership_in_loop_fires_on_not_in() {
    let src = "\
def f(items: list[int], xs: list[int]) -> int:
    mut hits: int = 0
    for x in xs:
        if x not in items:
            hits += 1
    return hits
";
    assert_eq!(count(&codes(src), "perf_membership_in_loop"), 1);
}

#[test]
fn membership_in_loop_silent_when_list_mutated() {
    // `items.append(...)` inside the loop makes it non-invariant.
    let src = "\
def f(items: list[int], xs: list[int]) -> None:
    for x in xs:
        items.append(x)
        if x in items:
            pass
";
    assert_eq!(count(&codes(src), "perf_membership_in_loop"), 0);
}

#[test]
fn membership_in_loop_silent_for_dict_and_set() {
    // `in` over a dict / set is already O(1); only `list` fires.
    let src = "\
def f(d: dict[str, int], s: set[str], xs: list[str]) -> None:
    for x in xs:
        if x in d:
            pass
        if x in s:
            pass
";
    assert_eq!(count(&codes(src), "perf_membership_in_loop"), 0);
}

#[test]
fn membership_outside_loop_silent() {
    let src = "\
def f(items: list[int], x: int) -> bool:
    return x in items
";
    assert_eq!(count(&codes(src), "perf_membership_in_loop"), 0);
}

#[test]
fn membership_unannotated_param_silent() {
    // `items` has no `list` evidence, so we can't prove the container type.
    let src = "\
def f(items, xs: list[int]) -> None:
    for x in xs:
        if x in items:
            pass
";
    assert_eq!(count(&codes(src), "perf_membership_in_loop"), 0);
}

#[test]
fn membership_list_literal_binding_fires() {
    let src = "\
def f(xs: list[int]) -> int:
    let allow = [1, 2, 3]
    mut hits: int = 0
    for x in xs:
        if x in allow:
            hits += 1
    return hits
";
    assert_eq!(count(&codes(src), "perf_membership_in_loop"), 1);
}

// ── lint 2: perf_list_shift_in_loop ───────────────────────────────────────────

#[test]
fn list_shift_pop_front_in_loop_fires() {
    let src = "\
def f(buf: list[int], n: int) -> None:
    for i in range(n):
        buf.pop(0)
";
    assert_eq!(count(&codes(src), "perf_list_shift_in_loop"), 1);
}

#[test]
fn list_shift_insert_front_in_loop_fires() {
    let src = "\
def f(buf: list[int], n: int) -> None:
    for i in range(n):
        buf.insert(0, i)
";
    assert_eq!(count(&codes(src), "perf_list_shift_in_loop"), 1);
}

#[test]
fn list_shift_attribute_receiver_silent() {
    // `self.items` isn't a bare list-annotated name.
    let src = "\
def f(n: int) -> None:
    for i in range(n):
        self.items.pop(0)
";
    assert_eq!(count(&codes(src), "perf_list_shift_in_loop"), 0);
}

#[test]
fn list_shift_pop_last_silent() {
    // `pop()` / `pop(-1)` is O(1) — only `pop(0)` shifts.
    let src = "\
def f(buf: list[int], n: int) -> None:
    for i in range(n):
        buf.pop()
";
    assert_eq!(count(&codes(src), "perf_list_shift_in_loop"), 0);
}

#[test]
fn list_shift_outside_loop_silent() {
    let src = "\
def f(buf: list[int]) -> None:
    buf.pop(0)
";
    assert_eq!(count(&codes(src), "perf_list_shift_in_loop"), 0);
}

// ── lint 3: perf_str_concat_in_loop ───────────────────────────────────────────

#[test]
fn str_concat_in_loop_fires() {
    let src = "\
def f(parts: list[str]) -> str:
    mut acc: str = \"\"
    for p in parts:
        acc += p
    return acc
";
    assert_eq!(count(&codes(src), "perf_str_concat_in_loop"), 1);
}

#[test]
fn str_concat_attribute_target_silent() {
    // Only a plain-name target fires; attribute/subscript targets are exempt.
    let src = "\
def f(self, parts: list[str]) -> None:
    for p in parts:
        self.acc += p
";
    assert_eq!(count(&codes(src), "perf_str_concat_in_loop"), 0);
}

#[test]
fn int_accumulator_in_loop_silent() {
    // `total: int += ...` is fine — only `str` accumulation is quadratic.
    let src = "\
def f(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total
";
    assert_eq!(count(&codes(src), "perf_str_concat_in_loop"), 0);
}

#[test]
fn str_concat_outside_loop_silent() {
    let src = "\
def f(a: str, b: str) -> str:
    mut acc: str = a
    acc += b
    return acc
";
    assert_eq!(count(&codes(src), "perf_str_concat_in_loop"), 0);
}

// ── lint 4: perf_sort_in_loop ─────────────────────────────────────────────────

#[test]
fn sorted_of_invariant_in_loop_fires() {
    let src = "\
def f(data: list[int], n: int) -> None:
    for i in range(n):
        let top: list[int] = sorted(data)
";
    assert_eq!(count(&codes(src), "perf_sort_in_loop"), 1);
}

#[test]
fn sort_method_of_invariant_in_loop_fires() {
    let src = "\
def f(data: list[int], n: int) -> None:
    for i in range(n):
        data.sort()
";
    assert_eq!(count(&codes(src), "perf_sort_in_loop"), 1);
}

#[test]
fn sorted_of_mutated_in_loop_silent() {
    // `data` grows each iteration, so re-sorting is genuinely needed.
    let src = "\
def f(data: list[int], n: int) -> None:
    for i in range(n):
        data.append(i)
        let top: list[int] = sorted(data)
";
    assert_eq!(count(&codes(src), "perf_sort_in_loop"), 0);
}

#[test]
fn sorted_in_for_header_silent() {
    // Sorting the thing you iterate is idiomatic and runs once — not flagged.
    let src = "\
def f(data: list[int]) -> None:
    for x in sorted(data):
        pass
";
    assert_eq!(count(&codes(src), "perf_sort_in_loop"), 0);
}

#[test]
fn sort_outside_loop_silent() {
    let src = "\
def f(data: list[int]) -> None:
    data.sort()
";
    assert_eq!(count(&codes(src), "perf_sort_in_loop"), 0);
}

#[test]
fn sort_of_loop_variable_silent() {
    // The loop target `group` is re-bound each iteration, so `sorted(group)`
    // is not re-sorting invariant data — must stay silent.
    let src = "\
def f(groups: list[list[int]]) -> None:
    for group in groups:
        let ordered: list[int] = sorted(group)
";
    assert_eq!(count(&codes(src), "perf_sort_in_loop"), 0);
}

// ── lint 5: perf_sorted_first ─────────────────────────────────────────────────

#[test]
fn sorted_first_fires() {
    let src = "\
def f(xs: list[int]) -> int:
    return sorted(xs)[0]
";
    let c = codes(src);
    assert_eq!(count(&c, "perf_sorted_first"), 1);
}

#[test]
fn sorted_last_fires() {
    let src = "\
def f(xs: list[int]) -> int:
    return sorted(xs)[-1]
";
    assert_eq!(count(&codes(src), "perf_sorted_first"), 1);
}

#[test]
fn sorted_with_key_silent() {
    // Keeping it simple: only the bare form fires.
    let src = "\
def f(xs: list[int]) -> int:
    return sorted(xs, key=abs)[0]
";
    assert_eq!(count(&codes(src), "perf_sorted_first"), 0);
}

#[test]
fn sorted_middle_index_silent() {
    let src = "\
def f(xs: list[int]) -> int:
    return sorted(xs)[1]
";
    assert_eq!(count(&codes(src), "perf_sorted_first"), 0);
}

#[test]
fn sorted_slice_silent() {
    let src = "\
def f(xs: list[int]) -> list[int]:
    return sorted(xs)[:3]
";
    assert_eq!(count(&codes(src), "perf_sorted_first"), 0);
}

// ── lint 6: perf_keys_membership ──────────────────────────────────────────────

#[test]
fn keys_membership_fires() {
    let src = "\
def f(d: dict[str, int], k: str) -> bool:
    return k in d.keys()
";
    assert_eq!(count(&codes(src), "perf_keys_membership"), 1);
}

#[test]
fn keys_not_in_membership_fires() {
    let src = "\
def f(d: dict[str, int], k: str) -> bool:
    return k not in d.keys()
";
    assert_eq!(count(&codes(src), "perf_keys_membership"), 1);
}

#[test]
fn plain_dict_membership_silent() {
    let src = "\
def f(d: dict[str, int], k: str) -> bool:
    return k in d
";
    assert_eq!(count(&codes(src), "perf_keys_membership"), 0);
}

#[test]
fn keys_iteration_silent() {
    // `for k in d.keys():` isn't a membership test.
    let src = "\
def f(d: dict[str, int]) -> None:
    for k in d.keys():
        pass
";
    assert_eq!(count(&codes(src), "perf_keys_membership"), 0);
}

#[test]
fn keys_membership_literal_left_silent() {
    // A bare-literal element (`"a" in d.keys()`) is demonstration-shaped;
    // only a variable/expression tested against keys is nudged.
    let src = "\
def f(d: dict[str, int]) -> bool:
    return \"a\" in d.keys()
";
    assert_eq!(count(&codes(src), "perf_keys_membership"), 0);
}

// ── lint 7: lazy_import_opportunity ───────────────────────────────────────────

#[test]
fn lazy_import_fires_for_function_only_use() {
    let src = "\
import numpy as np

def f() -> object:
    return np.array([1])
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 1);
}

#[test]
fn lazy_import_silent_for_stdlib() {
    let src = "\
import os

def f() -> str:
    return os.getcwd()
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_for_module_level_use() {
    let src = "\
import numpy as np

DEFAULT = np.zeros(3)

def f() -> object:
    return np.array([1])
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_for_annotation_use() {
    // A module-level def annotated with the imported type touches it at
    // import time — deferral would break the (eagerly-evaluated) annotation.
    let src = "\
import numpy as np

def f(x: np.ndarray) -> int:
    return 0
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_when_module_defines_main() {
    // A top-level `def main()` marks a script/entrypoint even without a
    // `__main__` guard — its imports load when it runs, so deferral is moot.
    let src = "\
import numpy as np

def main() -> None:
    print(np.array([1]))
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_with_main_guard() {
    // An executable script's imports load when it runs — deferral is moot.
    let src = "\
import numpy as np

def main() -> None:
    print(np.array([1]))

if __name__ == \"__main__\":
    main()
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_when_already_lazy() {
    // `lazy import np = numpy` preprocesses to `import numpy as np`; the ctx
    // records `np` as already-lazy so we don't re-nudge it.
    let src = "\
import numpy as np

def f() -> object:
    return np.array([1])
";
    let ctx = PerfLintContext {
        lazy_import_aliases: vec!["np".to_string()],
        ..PerfLintContext::default()
    };
    assert_eq!(count(&codes_with(src, &ctx), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_when_pub_exported() {
    let src = "\
import numpy as np

def f() -> object:
    return np.array([1])
";
    let ctx = PerfLintContext {
        pub_names: vec!["np".to_string()],
        ..PerfLintContext::default()
    };
    assert_eq!(count(&codes_with(src, &ctx), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_when_in_all() {
    let src = "\
import numpy as np

__all__ = [\"np\"]

def f() -> object:
    return np.array([1])
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_with_pub_star() {
    let src = "\
import numpy as np

def f() -> object:
    return np.array([1])
";
    let ctx = PerfLintContext {
        has_pub_star: true,
        ..PerfLintContext::default()
    };
    assert_eq!(count(&codes_with(src, &ctx), "lazy_import_opportunity"), 0);
}

#[test]
fn lazy_import_silent_in_init_module() {
    let prep = tyc_syntax::preprocess::preprocess(
        "import numpy as np\n\ndef f() -> object:\n    return np.array([1])\n",
    );
    let module = tyc_syntax::parse_module(&prep.python_source)
        .expect("parse failed")
        .into_syntax();
    let mut diags = Diagnostics::new();
    lazy_import_opportunity_diagnostics(
        &module,
        "pkg/__init__.ty",
        &prep.python_source,
        &PerfLintContext::default(),
        &mut diags,
    );
    let hits = diags
        .warnings()
        .iter()
        .filter_map(|w| w.code().map(|c| c.to_string()))
        .filter(|c| c.contains("lazy_import_opportunity"))
        .count();
    assert_eq!(hits, 0, "__init__ modules are a re-export surface");
}

#[test]
fn lazy_import_silent_for_decorator_use() {
    // A decorator runs at def-creation (import) time.
    let src = "\
import click

@click.command()
def cli() -> None:
    return None
";
    assert_eq!(count(&codes(src), "lazy_import_opportunity"), 0);
}

// ── stdlib table sanity ───────────────────────────────────────────────────────

#[test]
fn stdlib_table_matches_expectations() {
    assert!(is_stdlib_top_level("os"));
    assert!(is_stdlib_top_level("json"));
    assert!(is_stdlib_top_level("asyncio"));
    assert!(!is_stdlib_top_level("numpy"));
    assert!(!is_stdlib_top_level("httpx"));
}
