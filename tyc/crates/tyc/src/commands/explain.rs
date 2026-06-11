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
            println!("tyc::{code}");
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
            "unknown diagnostic code `{}`. Run `tyc explain --list` to see every code, or browse https://typhon.dev/lang/diagnostics for the catalog.",
            raw
        )),
    }
}

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
        "extend_builtin",
        "freeze",
        "frozen_assign",
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
        "loop_closure_capture",
        "missing_initialiser",
        "missing_return",
        "mutable_default_param",
        "newtype_violation",
        "no_block_shadow",
        "non_exhaustive_match",
        "not_callable",
        "nullable_use",
        "operator_type_mismatch",
        "orphan_py_import",
        "parse",
        "pattern_shadows_outer",
        "pub",
        "python_semantic_drift",
        "resource_not_managed",
        "result_error_mismatch",
        "self_outside_impl",
        "stdlib_module_shadow",
        "stub_mismatch",
        "tuple_index_out_of_range",
        "type_mismatch",
        "typevar_bound",
        "typevar_import_rejected",
        "typing_alias_deprecated",
        "unknown_kwarg",
        "unknown_module",
        "unknown_name",
        "unsafe_value_leak",
        "unused_import",
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
        "extend_builtin" => include_str!("../../../../../docs/diagnostics/extend_builtin.md"),
        "freeze" => include_str!("../../../../../docs/diagnostics/freeze.md"),
        "frozen_assign" => include_str!("../../../../../docs/diagnostics/frozen_assign.md"),
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
        "loop_closure_capture" => {
            include_str!("../../../../../docs/diagnostics/loop_closure_capture.md")
        }
        "missing_initialiser" => {
            include_str!("../../../../../docs/diagnostics/missing_initialiser.md")
        }
        "missing_return" => include_str!("../../../../../docs/diagnostics/missing_return.md"),
        "mutable_default_param" => {
            include_str!("../../../../../docs/diagnostics/mutable_default_param.md")
        }
        "newtype_violation" => {
            include_str!("../../../../../docs/diagnostics/newtype_violation.md")
        }
        "no_block_shadow" => include_str!("../../../../../docs/diagnostics/no_block_shadow.md"),
        "non_exhaustive_match" => {
            include_str!("../../../../../docs/diagnostics/non_exhaustive_match.md")
        }
        "not_callable" => include_str!("../../../../../docs/diagnostics/not_callable.md"),
        "nullable_use" => include_str!("../../../../../docs/diagnostics/nullable_use.md"),
        "operator_type_mismatch" => {
            include_str!("../../../../../docs/diagnostics/operator_type_mismatch.md")
        }
        "orphan_py_import" => include_str!("../../../../../docs/diagnostics/orphan_py_import.md"),
        "parse" => include_str!("../../../../../docs/diagnostics/parse.md"),
        "pattern_shadows_outer" => {
            include_str!("../../../../../docs/diagnostics/pattern_shadows_outer.md")
        }
        "pub" => include_str!("../../../../../docs/diagnostics/pub.md"),
        "python_semantic_drift" => {
            include_str!("../../../../../docs/diagnostics/python_semantic_drift.md")
        }
        "resource_not_managed" => {
            include_str!("../../../../../docs/diagnostics/resource_not_managed.md")
        }
        "result_error_mismatch" => {
            include_str!("../../../../../docs/diagnostics/result_error_mismatch.md")
        }
        "self_outside_impl" => include_str!("../../../../../docs/diagnostics/self_outside_impl.md"),
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
        "unknown_kwarg" => include_str!("../../../../../docs/diagnostics/unknown_kwarg.md"),
        "unknown_module" => include_str!("../../../../../docs/diagnostics/unknown_module.md"),
        "unknown_name" => include_str!("../../../../../docs/diagnostics/unknown_name.md"),
        "unsafe_value_leak" => {
            include_str!("../../../../../docs/diagnostics/unsafe_value_leak.md")
        }
        "unused_import" => include_str!("../../../../../docs/diagnostics/unused_import.md"),
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
}
