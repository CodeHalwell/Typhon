//! Typhon AST → Python AST lowering.
//!
//! Phase 2 adds class-to-dataclass desugaring.  Later phases will add sealed
//! unions, the `?` operator, `with`-chains, and other Typhon-specific
//! constructs.
//!
//! The desugarer does not clone or mutate the AST.  Instead it analyses the
//! parsed module and returns a [`DesugarInfo`] value that the emitter uses to
//! inject the appropriate imports and decorators during code generation.

use rustpython_ast::{text_size::TextRange, Expr, Mod, Stmt};

/// Which Python class emission strategy to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClassDefault {
    /// Emit plain classes as `@dataclass(slots=True)` (default).
    #[default]
    Dataclass,
    /// Emit classes as Pydantic `BaseModel` subclasses (opt-in).
    Pydantic,
    /// Pass classes through unchanged.
    None,
}

impl ClassDefault {
    pub fn from_str(s: &str) -> Self {
        match s {
            "pydantic" => ClassDefault::Pydantic,
            "none" => ClassDefault::None,
            _ => ClassDefault::Dataclass,
        }
    }
}

/// Desugaring requirements gathered from a parsed module.
///
/// Passed to `tyc-emit` so the emitter can inject necessary imports and
/// decorators without needing to clone or mutate the AST.
#[derive(Debug, Default)]
pub struct DesugarInfo {
    /// Emitter must inject `from dataclasses import dataclass` at the top and
    /// `@dataclass(slots=True)` before each bare class definition.
    pub needs_dataclass_import: bool,
    /// Emitter must inject `from pydantic import BaseModel` at the top.
    pub needs_pydantic_import: bool,
}

/// Analyse a parsed module and record what desugaring the emitter must apply.
pub fn analyse_module(module: &Mod, class_default: ClassDefault) -> DesugarInfo {
    let mut info = DesugarInfo::default();
    let stmts = match module {
        Mod::Module(m) => &m.body,
        _ => return info,
    };
    for stmt in stmts {
        collect_class_info(stmt, class_default, &mut info);
    }
    info
}

fn has_dataclass_decorator(decorators: &[Expr<TextRange>]) -> bool {
    decorators.iter().any(|d| match d {
        Expr::Name(n) => n.id.as_str() == "dataclass",
        Expr::Call(call) => {
            matches!(call.func.as_ref(), Expr::Name(n) if n.id.as_str() == "dataclass")
        }
        Expr::Attribute(a) => a.attr.as_str() == "dataclass",
        _ => false,
    })
}

fn collect_class_info(
    stmt: &Stmt<TextRange>,
    class_default: ClassDefault,
    info: &mut DesugarInfo,
) {
    match stmt {
        Stmt::ClassDef(c) => {
            if !has_dataclass_decorator(&c.decorator_list) {
                match class_default {
                    ClassDefault::Dataclass => info.needs_dataclass_import = true,
                    ClassDefault::Pydantic => info.needs_pydantic_import = true,
                    ClassDefault::None => {}
                }
            }
            for s in &c.body {
                collect_class_info(s, class_default, info);
            }
        }
        Stmt::FunctionDef(f) => {
            for s in &f.body {
                collect_class_info(s, class_default, info);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{parse, Mode};

    fn analyse(src: &str, cd: ClassDefault) -> DesugarInfo {
        let module = parse(src, Mode::Module, "<test>").expect("parse failed");
        analyse_module(&module, cd)
    }

    #[test]
    fn plain_class_needs_dataclass() {
        let info = analyse("class Foo:\n    x: int\n", ClassDefault::Dataclass);
        assert!(info.needs_dataclass_import);
        assert!(!info.needs_pydantic_import);
    }

    #[test]
    fn already_decorated_class_skipped() {
        let info = analyse(
            "@dataclass\nclass Foo:\n    x: int\n",
            ClassDefault::Dataclass,
        );
        assert!(!info.needs_dataclass_import);
    }

    #[test]
    fn no_classes_no_import() {
        let info = analyse("def foo() -> None:\n    pass\n", ClassDefault::Dataclass);
        assert!(!info.needs_dataclass_import);
    }

    #[test]
    fn class_default_none_skips() {
        let info = analyse("class Foo:\n    x: int\n", ClassDefault::None);
        assert!(!info.needs_dataclass_import);
        assert!(!info.needs_pydantic_import);
    }

    #[test]
    fn nested_class_in_function_detected() {
        let src = "def outer() -> None:\n    class Inner:\n        x: int\n";
        let info = analyse(src, ClassDefault::Dataclass);
        assert!(info.needs_dataclass_import);
    }
}
