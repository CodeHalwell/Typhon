//! `tyc explain <code>` — print the catalog entry for a diagnostic code.

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
            "unknown diagnostic code `{}`. See https://typhon.dev/lang/diagnostics for the catalog.",
            args.code
        )),
    }
}

fn catalog_entry(short_code: &str) -> Option<&'static str> {
    Some(match short_code {
        "immutable_assign" => include_str!("../../../../../docs/diagnostics/immutable_assign.md"),
        "method_in_class_body" => {
            include_str!("../../../../../docs/diagnostics/method_in_class_body.md")
        }
        "nullable_use" => include_str!("../../../../../docs/diagnostics/nullable_use.md"),
        "type_mismatch" => include_str!("../../../../../docs/diagnostics/type_mismatch.md"),
        "missing_return" => include_str!("../../../../../docs/diagnostics/missing_return.md"),
        "missing_binding_kind" => {
            include_str!("../../../../../docs/diagnostics/missing_binding_kind.md")
        }
        "unknown_name" => include_str!("../../../../../docs/diagnostics/unknown_name.md"),
        "non_exhaustive_match" => {
            include_str!("../../../../../docs/diagnostics/non_exhaustive_match.md")
        }
        "unused_import" => include_str!("../../../../../docs/diagnostics/unused_import.md"),
        "missing_annotation" => {
            include_str!("../../../../../docs/diagnostics/missing_annotation.md")
        }
        "impure_pure_fn" => include_str!("../../../../../docs/diagnostics/impure_pure_fn.md"),
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
        let entry = catalog_entry("immutable_assign")
            .expect("immutable_assign should be in the catalog");
        assert!(!entry.trim().is_empty());
        assert!(
            entry.contains("immutable_assign"),
            "catalog entry should reference the code name"
        );
    }
}
