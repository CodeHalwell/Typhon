//! Typhon AST → Python AST lowering (Phase 2+).
//!
//! Phase 2 implements class desugaring: plain Typhon `class` definitions are
//! rewritten to `@dataclass(slots=True)` Python classes, and the required
//! `from dataclasses import dataclass` import is injected when needed.
//!
//! Future phases will add desugaring for sealed unions, the `?` operator,
//! `with`-chains, and other Typhon-specific constructs.

use rustpython_ast::{
    text_size::TextRange, Alias, Constant, ExprCall, ExprConstant, ExprContext, ExprName,
    Identifier, Int, Keyword, Mod, ModModule, Stmt, StmtImportFrom,
};

/// Desugar a Typhon module AST into a plain Python AST.
///
/// Currently performs one transformation: every `class` definition that does
/// not already carry a `@dataclass` decorator gets `@dataclass(slots=True)`
/// prepended to its decorator list, and `from dataclasses import dataclass` is
/// injected at the top of the module when at least one class was transformed.
pub fn desugar_module(module: &Mod<TextRange>) -> Mod<TextRange> {
    match module {
        Mod::Module(m) => Mod::Module(desugar_mod_module(m)),
        other => other.clone(),
    }
}

fn desugar_mod_module(m: &ModModule<TextRange>) -> ModModule<TextRange> {
    let mut transformed_classes = false;

    let new_body: Vec<Stmt<TextRange>> = m
        .body
        .iter()
        .map(|stmt| match stmt {
            Stmt::ClassDef(c) if !has_dataclass_decorator(&c.decorator_list) => {
                transformed_classes = true;
                let mut new_class = c.clone();
                new_class.decorator_list.insert(0, make_dataclass_call());
                Stmt::ClassDef(new_class)
            }
            other => other.clone(),
        })
        .collect();

    let final_body = if transformed_classes && !has_dataclass_import(&m.body) {
        let mut body = vec![make_dataclass_import()];
        body.extend(new_body);
        body
    } else {
        new_body
    };

    ModModule {
        range: m.range,
        body: final_body,
        type_ignores: m.type_ignores.clone(),
    }
}

// ── AST helpers ──────────────────────────────────────────────────────────────

/// Build the expression `dataclass(slots=True)`.
fn make_dataclass_call() -> rustpython_ast::Expr<TextRange> {
    rustpython_ast::Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(rustpython_ast::Expr::Name(ExprName {
            range: TextRange::default(),
            id: Identifier::new("dataclass"),
            ctx: ExprContext::Load,
        })),
        args: vec![],
        keywords: vec![Keyword {
            range: TextRange::default(),
            arg: Some(Identifier::new("slots")),
            value: rustpython_ast::Expr::Constant(ExprConstant {
                range: TextRange::default(),
                value: Constant::Bool(true),
                kind: None,
            }),
        }],
    })
}

/// Build the statement `from dataclasses import dataclass`.
fn make_dataclass_import() -> Stmt<TextRange> {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        module: Some(Identifier::new("dataclasses")),
        names: vec![Alias {
            range: TextRange::default(),
            name: Identifier::new("dataclass"),
            asname: None,
        }],
        level: Some(Int::new(0)),
    })
}

/// Return `true` if the decorator list already contains `@dataclass` or
/// `@dataclass(...)`.
fn has_dataclass_decorator(decorators: &[rustpython_ast::Expr<TextRange>]) -> bool {
    decorators.iter().any(|d| match d {
        rustpython_ast::Expr::Name(n) => n.id.as_str() == "dataclass",
        rustpython_ast::Expr::Call(c) => matches!(c.func.as_ref(),
            rustpython_ast::Expr::Name(n) if n.id.as_str() == "dataclass"
        ),
        _ => false,
    })
}

/// Return `true` if the body already contains `from dataclasses import dataclass`.
fn has_dataclass_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| {
        if let Stmt::ImportFrom(imp) = stmt {
            if imp.module.as_deref() == Some("dataclasses") {
                return imp.names.iter().any(|a| a.name.as_str() == "dataclass");
            }
        }
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{parse, Mode};
    use tyc_emit::emit;

    fn parse_and_desugar(src: &str) -> String {
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let desugared = desugar_module(&m);
        emit(&desugared)
    }

    #[test]
    fn plain_class_gets_dataclass_decorator() {
        let src = "class Point:\n    x: int\n    y: int\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("@dataclass(slots=True)"), "output:\n{out}");
        assert!(out.contains("from dataclasses import dataclass"), "output:\n{out}");
    }

    #[test]
    fn class_with_existing_dataclass_not_duplicated() {
        let src = "from dataclasses import dataclass\n\n@dataclass\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        // Original @dataclass preserved, no @dataclass(slots=True) added
        assert!(!out.contains("slots=True"), "output:\n{out}");
        // Import not duplicated
        assert_eq!(out.matches("from dataclasses import dataclass").count(), 1, "output:\n{out}");
    }

    #[test]
    fn non_class_statements_pass_through() {
        let src = "x: int = 1\n\ndef f() -> None:\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(!out.contains("dataclass"), "output:\n{out}");
    }

    #[test]
    fn multiple_classes_one_import() {
        let src = "class A:\n    x: int\n\nclass B:\n    y: str\n";
        let out = parse_and_desugar(src);
        assert_eq!(out.matches("from dataclasses import dataclass").count(), 1, "output:\n{out}");
        assert_eq!(out.matches("@dataclass(slots=True)").count(), 2, "output:\n{out}");
    }
}
