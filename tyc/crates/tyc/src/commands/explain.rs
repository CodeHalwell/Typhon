//! `tyc explain <code>` — print the catalog entry for a diagnostic code.
//!
//! Every Markdown file under `docs/diagnostics/` is embedded into the
//! binary at build time and dispatched by short code. Adding a new
//! diagnostic doc means adding a single arm to [`catalog_entry`] —
//! `tyc explain <new_code>` then works without any other plumbing.

use clap::Args;
use miette::{miette, Result};

#[derive(Args, Debug)]
pub struct ExplainArgs {
    /// Diagnostic code to explain (e.g. `tyc::immutable_assign` or `immutable_assign`).
    /// Optional when `--list` is given.
    #[arg(value_name = "CODE", required_unless_present = "list")]
    pub code: Option<String>,

    /// Print every diagnostic code the explainer knows about, one per line.
    #[arg(long)]
    pub list: bool,
}

pub fn run(args: ExplainArgs) -> Result<()> {
    if args.list {
        for code in catalog_codes() {
            if LANGUAGE_TOPICS.contains(code) {
                continue;
            }
            println!("tyc::{code}");
        }
        // `freeze` and `pub` have catalog pages but are not diagnostics —
        // nothing ever reports `tyc::freeze`. Listing them under the code
        // prefix sent readers looking for a diagnostic that does not exist.
        println!();
        println!("language topics (no tyc:: prefix — `tyc explain <topic>`):");
        for topic in LANGUAGE_TOPICS {
            println!("  {topic}");
        }
        return Ok(());
    }
    let raw = args.code.as_deref().unwrap_or("");
    let needle = raw.strip_prefix("tyc::").unwrap_or(raw);
    match catalog_entry(needle) {
        Some(entry) => {
            println!("{}", entry);
            Ok(())
        }
        None => Err(miette!(
            "unknown diagnostic code `{}`. Run `tyc explain --list` to see every code, or browse https://github.com/CodeHalwell/Typhon/tree/main/docs/diagnostics for the catalog.",
            raw
        )),
    }
}

/// Catalog pages that explain a *language feature* rather than a
/// diagnostic. They resolve through `tyc explain` like any other page but
/// are never reported by the compiler, so `--list` shows them separately
/// instead of under the `tyc::` prefix.
const LANGUAGE_TOPICS: &[&str] = &["freeze", "pub"];

/// Every short diagnostic code the explainer knows about, in
/// alphabetical order. Backs `tyc explain --list` (FINDINGS O25).
/// Keep this list in sync with the match in [`catalog_entry`] — the
/// `catalog_listing_matches_entries` test asserts that every listed
/// code resolves to a non-empty entry.
fn catalog_codes() -> &'static [&'static str] {
    &[
        "arg_count",
        "async_without_await",
        "attribute_not_found",
        "auto_gather_missed",
        "blocking_in_async",
        "class_attr_shadows_slot",
        "comptime",
        "contains_secret_literal",
        "cyclic_type_alias",
        "div_by_zero_literal",
        "duplicate_class",
        "duplicate_method",
        "empty_collection_no_annotation",
        "extend_builtin",
        "field_default_ordering",
        "freeze",
        "freeze_not_freezable",
        "frozen_assign",
        "frozen_inheritance_conflict",
        "gather_opportunity",
        "generator_return_type",
        "generic",
        "immutable_assign",
        "impl_unknown_class",
        "implicit_any",
        "impure_pure_fn",
        "interface_isinstance",
        "interface_not_conforming",
        "invalid_config_value",
        "invalid_question_op",
        "io",
        "lazy_import_opportunity",
        "lazy_usage",
        "main_not_called",
        "manual_init",
        "method_in_class_body",
        "missing_annotation",
        "missing_argument",
        "missing_await",
        "missing_binding_kind",
        "incompatible_override",
        "is_literal_comparison",
        "kind_mismatch",
        "loop_closure_capture",
        "missing_field_init",
        "missing_initialiser",
        "missing_return",
        "mutable_default_param",
        "possibly_unbound",
        "go_outside_async",
        "newtype_invalid_base",
        "newtype_violation",
        "no_block_shadow",
        "non_exhaustive_match",
        "not_a_context_manager",
        "not_callable",
        "nullable_use",
        "operator_type_mismatch",
        "orphan_py_import",
        "parallel_opportunity",
        "parse",
        "pattern_shadows_outer",
        "perf_keys_membership",
        "perf_list_shift_in_loop",
        "perf_membership_in_loop",
        "perf_sort_in_loop",
        "perf_sorted_first",
        "perf_str_concat_in_loop",
        "pub",
        "pub_name_collision",
        "pub_star_outside_init",
        "python_semantic_drift",
        "raise_non_exception",
        "resource_not_managed",
        "result_error_mismatch",
        "return_in_except_star",
        "self_outside_impl",
        "shared_mut_across_tasks",
        "stdlib_module_shadow",
        "stub_mismatch",
        "tuple_index_out_of_range",
        "type_mismatch",
        "typevar_bound",
        "typevar_import_rejected",
        "typing_alias_deprecated",
        "typing_alias_in_annotation",
        "unknown_kwarg",
        "unknown_module",
        "unknown_name",
        "unsafe_value_leak",
        "unused_import",
        "use_of_uninitialised",
    ]
}

/// Match a short diagnostic code (the part after `tyc::`) to its catalog
/// page. Every page under `docs/diagnostics/` is embedded into the binary.
fn catalog_entry(short_code: &str) -> Option<&'static str> {
    Some(match short_code {
        "arg_count" => include_str!("../../../../../docs/diagnostics/arg_count.md"),
        "async_without_await" => {
            include_str!("../../../../../docs/diagnostics/async_without_await.md")
        }
        "attribute_not_found" => {
            include_str!("../../../../../docs/diagnostics/attribute_not_found.md")
        }
        "auto_gather_missed" => {
            include_str!("../../../../../docs/diagnostics/auto_gather_missed.md")
        }
        "blocking_in_async" => {
            include_str!("../../../../../docs/diagnostics/blocking_in_async.md")
        }
        "class_attr_shadows_slot" => {
            include_str!("../../../../../docs/diagnostics/class_attr_shadows_slot.md")
        }
        "comptime" => include_str!("../../../../../docs/diagnostics/comptime.md"),
        "contains_secret_literal" => {
            include_str!("../../../../../docs/diagnostics/contains_secret_literal.md")
        }
        "cyclic_type_alias" => include_str!("../../../../../docs/diagnostics/cyclic_type_alias.md"),
        "div_by_zero_literal" => {
            include_str!("../../../../../docs/diagnostics/div_by_zero_literal.md")
        }
        "duplicate_class" => include_str!("../../../../../docs/diagnostics/duplicate_class.md"),
        "duplicate_method" => include_str!("../../../../../docs/diagnostics/duplicate_method.md"),
        "empty_collection_no_annotation" => {
            include_str!("../../../../../docs/diagnostics/empty_collection_no_annotation.md")
        }
        "extend_builtin" => include_str!("../../../../../docs/diagnostics/extend_builtin.md"),
        "field_default_ordering" => {
            include_str!("../../../../../docs/diagnostics/field_default_ordering.md")
        }
        "freeze" => include_str!("../../../../../docs/diagnostics/freeze.md"),
        "freeze_not_freezable" => {
            include_str!("../../../../../docs/diagnostics/freeze_not_freezable.md")
        }
        "frozen_assign" => include_str!("../../../../../docs/diagnostics/frozen_assign.md"),
        "frozen_inheritance_conflict" => {
            include_str!("../../../../../docs/diagnostics/frozen_inheritance_conflict.md")
        }
        "gather_opportunity" => {
            include_str!("../../../../../docs/diagnostics/gather_opportunity.md")
        }
        "generator_return_type" => {
            include_str!("../../../../../docs/diagnostics/generator_return_type.md")
        }
        "generic" => include_str!("../../../../../docs/diagnostics/generic.md"),
        "immutable_assign" => include_str!("../../../../../docs/diagnostics/immutable_assign.md"),
        "impl_unknown_class" => {
            include_str!("../../../../../docs/diagnostics/impl_unknown_class.md")
        }
        "implicit_any" => include_str!("../../../../../docs/diagnostics/implicit_any.md"),
        "impure_pure_fn" => include_str!("../../../../../docs/diagnostics/impure_pure_fn.md"),
        "interface_isinstance" => {
            include_str!("../../../../../docs/diagnostics/interface_isinstance.md")
        }
        "interface_not_conforming" => {
            include_str!("../../../../../docs/diagnostics/interface_not_conforming.md")
        }
        "invalid_config_value" => {
            include_str!("../../../../../docs/diagnostics/invalid_config_value.md")
        }
        "invalid_question_op" => {
            include_str!("../../../../../docs/diagnostics/invalid_question_op.md")
        }
        "io" => include_str!("../../../../../docs/diagnostics/io.md"),
        "lazy_import_opportunity" => {
            include_str!("../../../../../docs/diagnostics/lazy_import_opportunity.md")
        }
        "lazy_usage" => include_str!("../../../../../docs/diagnostics/lazy_usage.md"),
        "main_not_called" => include_str!("../../../../../docs/diagnostics/main_not_called.md"),
        "manual_init" => include_str!("../../../../../docs/diagnostics/manual_init.md"),
        "method_in_class_body" => {
            include_str!("../../../../../docs/diagnostics/method_in_class_body.md")
        }
        "missing_annotation" => {
            include_str!("../../../../../docs/diagnostics/missing_annotation.md")
        }
        "missing_argument" => {
            include_str!("../../../../../docs/diagnostics/missing_argument.md")
        }
        "missing_await" => include_str!("../../../../../docs/diagnostics/missing_await.md"),
        "missing_binding_kind" => {
            include_str!("../../../../../docs/diagnostics/missing_binding_kind.md")
        }
        "incompatible_override" => {
            include_str!("../../../../../docs/diagnostics/incompatible_override.md")
        }
        "is_literal_comparison" => {
            include_str!("../../../../../docs/diagnostics/is_literal_comparison.md")
        }
        "kind_mismatch" => include_str!("../../../../../docs/diagnostics/kind_mismatch.md"),
        "loop_closure_capture" => {
            include_str!("../../../../../docs/diagnostics/loop_closure_capture.md")
        }
        "missing_field_init" => {
            include_str!("../../../../../docs/diagnostics/missing_field_init.md")
        }
        "possibly_unbound" => include_str!("../../../../../docs/diagnostics/possibly_unbound.md"),
        "go_outside_async" => include_str!("../../../../../docs/diagnostics/go_outside_async.md"),
        "missing_initialiser" => {
            include_str!("../../../../../docs/diagnostics/missing_initialiser.md")
        }
        "missing_return" => include_str!("../../../../../docs/diagnostics/missing_return.md"),
        "mutable_default_param" => {
            include_str!("../../../../../docs/diagnostics/mutable_default_param.md")
        }
        "newtype_invalid_base" => {
            include_str!("../../../../../docs/diagnostics/newtype_invalid_base.md")
        }
        "newtype_violation" => {
            include_str!("../../../../../docs/diagnostics/newtype_violation.md")
        }
        "no_block_shadow" => include_str!("../../../../../docs/diagnostics/no_block_shadow.md"),
        "non_exhaustive_match" => {
            include_str!("../../../../../docs/diagnostics/non_exhaustive_match.md")
        }
        "not_a_context_manager" => {
            include_str!("../../../../../docs/diagnostics/not_a_context_manager.md")
        }
        "not_callable" => include_str!("../../../../../docs/diagnostics/not_callable.md"),
        "nullable_use" => include_str!("../../../../../docs/diagnostics/nullable_use.md"),
        "operator_type_mismatch" => {
            include_str!("../../../../../docs/diagnostics/operator_type_mismatch.md")
        }
        "orphan_py_import" => include_str!("../../../../../docs/diagnostics/orphan_py_import.md"),
        "parallel_opportunity" => {
            include_str!("../../../../../docs/diagnostics/parallel_opportunity.md")
        }
        "parse" => include_str!("../../../../../docs/diagnostics/parse.md"),
        "pattern_shadows_outer" => {
            include_str!("../../../../../docs/diagnostics/pattern_shadows_outer.md")
        }
        "perf_keys_membership" => {
            include_str!("../../../../../docs/diagnostics/perf_keys_membership.md")
        }
        "perf_list_shift_in_loop" => {
            include_str!("../../../../../docs/diagnostics/perf_list_shift_in_loop.md")
        }
        "perf_membership_in_loop" => {
            include_str!("../../../../../docs/diagnostics/perf_membership_in_loop.md")
        }
        "perf_sort_in_loop" => {
            include_str!("../../../../../docs/diagnostics/perf_sort_in_loop.md")
        }
        "perf_sorted_first" => {
            include_str!("../../../../../docs/diagnostics/perf_sorted_first.md")
        }
        "perf_str_concat_in_loop" => {
            include_str!("../../../../../docs/diagnostics/perf_str_concat_in_loop.md")
        }
        "pub" => include_str!("../../../../../docs/diagnostics/pub.md"),
        "pub_name_collision" => {
            include_str!("../../../../../docs/diagnostics/pub_name_collision.md")
        }
        "pub_star_outside_init" => {
            include_str!("../../../../../docs/diagnostics/pub_star_outside_init.md")
        }
        "python_semantic_drift" => {
            include_str!("../../../../../docs/diagnostics/python_semantic_drift.md")
        }
        "raise_non_exception" => {
            include_str!("../../../../../docs/diagnostics/raise_non_exception.md")
        }
        "resource_not_managed" => {
            include_str!("../../../../../docs/diagnostics/resource_not_managed.md")
        }
        "result_error_mismatch" => {
            include_str!("../../../../../docs/diagnostics/result_error_mismatch.md")
        }
        "return_in_except_star" => {
            include_str!("../../../../../docs/diagnostics/return_in_except_star.md")
        }
        "self_outside_impl" => include_str!("../../../../../docs/diagnostics/self_outside_impl.md"),
        "shared_mut_across_tasks" => {
            include_str!("../../../../../docs/diagnostics/shared_mut_across_tasks.md")
        }
        "stdlib_module_shadow" => {
            include_str!("../../../../../docs/diagnostics/stdlib_module_shadow.md")
        }
        "stub_mismatch" => include_str!("../../../../../docs/diagnostics/stub_mismatch.md"),
        "tuple_index_out_of_range" => {
            include_str!("../../../../../docs/diagnostics/tuple_index_out_of_range.md")
        }
        "type_mismatch" => include_str!("../../../../../docs/diagnostics/type_mismatch.md"),
        "typevar_bound" => include_str!("../../../../../docs/diagnostics/typevar_bound.md"),
        "typevar_import_rejected" => {
            include_str!("../../../../../docs/diagnostics/typevar_import_rejected.md")
        }
        "typing_alias_deprecated" => {
            include_str!("../../../../../docs/diagnostics/typing_alias_deprecated.md")
        }
        "typing_alias_in_annotation" => {
            include_str!("../../../../../docs/diagnostics/typing_alias_in_annotation.md")
        }
        "unknown_kwarg" => include_str!("../../../../../docs/diagnostics/unknown_kwarg.md"),
        "unknown_module" => include_str!("../../../../../docs/diagnostics/unknown_module.md"),
        "unknown_name" => include_str!("../../../../../docs/diagnostics/unknown_name.md"),
        "unsafe_value_leak" => {
            include_str!("../../../../../docs/diagnostics/unsafe_value_leak.md")
        }
        "unused_import" => include_str!("../../../../../docs/diagnostics/unused_import.md"),
        "use_of_uninitialised" => {
            include_str!("../../../../../docs/diagnostics/use_of_uninitialised.md")
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_unknown_code_returns_error() {
        let result = run(ExplainArgs {
            code: Some("tyc::no_such_thing".into()),
            list: false,
        });
        assert!(result.is_err(), "expected unknown code to return Err");
    }

    #[test]
    fn catalog_listing_matches_entries() {
        // Every code returned by `catalog_codes` must resolve to a
        // non-empty entry — otherwise `tyc explain --list` advertises
        // codes the user can't actually look up (FINDINGS O25).
        for code in catalog_codes() {
            let entry =
                catalog_entry(code).unwrap_or_else(|| panic!("missing catalog entry for `{code}`"));
            assert!(
                !entry.trim().is_empty(),
                "catalog entry for `{code}` should not be empty",
            );
        }
    }

    #[test]
    fn catalog_list_runs() {
        // The `--list` flag must succeed without requiring CODE.
        let result = run(ExplainArgs {
            code: None,
            list: true,
        });
        assert!(result.is_ok(), "tyc explain --list should succeed");
    }

    #[test]
    fn catalog_entry_returns_non_empty_for_immutable_assign() {
        let entry =
            catalog_entry("immutable_assign").expect("immutable_assign should be in the catalog");
        assert!(!entry.trim().is_empty());
        assert!(
            entry.contains("immutable_assign"),
            "catalog entry should reference the code name"
        );
    }

    #[test]
    fn catalog_entry_covers_phase_5_new_codes() {
        for code in [
            "invalid_config_value",
            "orphan_py_import",
            "python_semantic_drift",
            "contains_secret_literal",
        ] {
            assert!(
                catalog_entry(code).is_some(),
                "Phase 5 diagnostic `{code}` should be explainable"
            );
        }
    }

    #[test]
    fn language_topics_are_listed_apart_from_diagnostic_codes() {
        // `--list` printed `tyc::freeze` / `tyc::pub`, which no diagnostic
        // ever reports — readers went looking for a code that cannot fire.
        for topic in LANGUAGE_TOPICS {
            assert!(
                catalog_codes().contains(topic),
                "`{topic}` must still resolve through `tyc explain`"
            );
            assert!(
                catalog_entry(topic).is_some_and(|e| !e.trim().is_empty()),
                "`{topic}` must have a catalog page"
            );
            assert!(
                !catalog_entry(topic).unwrap().starts_with("# tyc::"),
                "`{topic}` is listed as a language topic, so its page must not \
                 present itself as a diagnostic code"
            );
        }
        // Everything else in the catalog IS a diagnostic page.
        for code in catalog_codes() {
            if LANGUAGE_TOPICS.contains(code) {
                continue;
            }
            assert!(
                catalog_entry(code)
                    .unwrap()
                    .starts_with(&format!("# tyc::{code}")),
                "`{code}` is listed as a diagnostic, so its page must open with \
                 `# tyc::{code}`"
            );
        }
    }
}
