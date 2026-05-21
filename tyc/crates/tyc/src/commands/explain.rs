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
    #[arg(value_name = "CODE")]
    pub code: String,
}

pub fn run(args: ExplainArgs) -> Result<()> {
    let needle = args.code.strip_prefix("tyc::").unwrap_or(&args.code);
    match catalog_entry(needle) {
        Some(entry) => {
            println!("{}", entry);
            Ok(())
        }
        None => Err(miette!(
            "unknown diagnostic code `{}`. Run `tyc explain --list` (not yet implemented) or see https://typhon.dev/lang/diagnostics for the catalog.",
            args.code
        )),
    }
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
        "extend_builtin" => include_str!("../../../../../docs/diagnostics/extend_builtin.md"),
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
        "missing_initialiser" => {
            include_str!("../../../../../docs/diagnostics/missing_initialiser.md")
        }
        "missing_return" => include_str!("../../../../../docs/diagnostics/missing_return.md"),
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
            code: "tyc::no_such_thing".into(),
        });
        assert!(result.is_err(), "expected unknown code to return Err");
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
