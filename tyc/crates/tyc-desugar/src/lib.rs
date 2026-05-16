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
            "dataclass" => ClassDefault::Dataclass,
            // Unknown values fall back to the default; a separate config
            // validation step should warn the user about typos.
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
    /// Emitter must inject `from pydantic import BaseModel` at the top and
    /// add `BaseModel` as a base class to bare class definitions.
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

/// Returns true if `expr` refers to a `dataclass` symbol, handling:
/// - bare name: `@dataclass`
/// - attribute access: `@dataclasses.dataclass`
/// - call (with or without args): `@dataclass()`, `@dataclasses.dataclass(slots=True)`
fn is_dataclass_expr(expr: &Expr<TextRange>) -> bool {
    match expr {
        Expr::Name(n) => n.id.as_str() == "dataclass",
        Expr::Attribute(a) => a.attr.as_str() == "dataclass",
        Expr::Call(c) => is_dataclass_expr(&c.func),
        _ => false,
    }
}

fn has_dataclass_decorator(decorators: &[Expr<TextRange>]) -> bool {
    decorators.iter().any(is_dataclass_expr)
}

fn collect_class_info(
    stmt: &Stmt<TextRange>,
    class_default: ClassDefault,
    info: &mut DesugarInfo,
) {
    use rustpython_ast::ExceptHandler;

    match stmt {
        Stmt::ClassDef(c) => {
            if !has_dataclass_decorator(&c.decorator_list) {
                match class_default {
                    ClassDefault::Dataclass => info.needs_dataclass_import = true,
                    ClassDefault::Pydantic => info.needs_pydantic_import = true,
                    ClassDefault::None => {}
                }
            }
            recurse_stmts(&c.body, class_default, info);
        }

        // Recurse into all statement forms that can contain a nested body.
        Stmt::FunctionDef(f) => recurse_stmts(&f.body, class_default, info),
        Stmt::AsyncFunctionDef(f) => recurse_stmts(&f.body, class_default, info),

        Stmt::If(i) => {
            recurse_stmts(&i.body, class_default, info);
            recurse_stmts(&i.orelse, class_default, info);
        }
        Stmt::For(f) => {
            recurse_stmts(&f.body, class_default, info);
            recurse_stmts(&f.orelse, class_default, info);
        }
        Stmt::AsyncFor(f) => {
            recurse_stmts(&f.body, class_default, info);
            recurse_stmts(&f.orelse, class_default, info);
        }
        Stmt::While(w) => {
            recurse_stmts(&w.body, class_default, info);
            recurse_stmts(&w.orelse, class_default, info);
        }
        Stmt::With(w) => recurse_stmts(&w.body, class_default, info),
        Stmt::AsyncWith(w) => recurse_stmts(&w.body, class_default, info),

        Stmt::Try(t) => {
            recurse_stmts(&t.body, class_default, info);
            for h in &t.handlers {
                if let ExceptHandler::ExceptHandler(eh) = h {
                    recurse_stmts(&eh.body, class_default, info);
                }
            }
            recurse_stmts(&t.orelse, class_default, info);
            recurse_stmts(&t.finalbody, class_default, info);
        }
        Stmt::TryStar(t) => {
            recurse_stmts(&t.body, class_default, info);
            for h in &t.handlers {
                if let ExceptHandler::ExceptHandler(eh) = h {
                    recurse_stmts(&eh.body, class_default, info);
                }
            }
            recurse_stmts(&t.orelse, class_default, info);
            recurse_stmts(&t.finalbody, class_default, info);
        }
        Stmt::Match(m) => {
            for case in &m.cases {
                recurse_stmts(&case.body, class_default, info);
            }
        }

        // Leaf statements carry no nested bodies.
        _ => {}
    }
}

fn recurse_stmts(stmts: &[Stmt<TextRange>], class_default: ClassDefault, info: &mut DesugarInfo) {
    for s in stmts {
        collect_class_info(s, class_default, info);
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
    fn dotted_decorator_skipped() {
        // @dataclasses.dataclass(slots=True) should not trigger re-injection.
        let info = analyse(
            "import dataclasses\n@dataclasses.dataclass(slots=True)\nclass Foo:\n    x: int\n",
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

    #[test]
    fn nested_class_in_async_function_detected() {
        let src = "async def outer() -> None:\n    class Inner:\n        x: int\n";
        let info = analyse(src, ClassDefault::Dataclass);
        assert!(info.needs_dataclass_import);
    }

    #[test]
    fn nested_class_in_if_detected() {
        let src = "if True:\n    class Cond:\n        x: int\n";
        let info = analyse(src, ClassDefault::Dataclass);
        assert!(info.needs_dataclass_import);
    }

    #[test]
    fn nested_class_in_for_body_detected() {
        let src = "for i in range(1):\n    class Iter:\n        x: int\n";
        let info = analyse(src, ClassDefault::Dataclass);
        assert!(info.needs_dataclass_import);
    }

    #[test]
    fn from_str_dataclass() {
        assert_eq!(ClassDefault::from_str("dataclass"), ClassDefault::Dataclass);
    }

    #[test]
    fn from_str_pydantic() {
        assert_eq!(ClassDefault::from_str("pydantic"), ClassDefault::Pydantic);
    }

    #[test]
    fn from_str_unknown_falls_back_to_dataclass() {
        assert_eq!(ClassDefault::from_str("typo"), ClassDefault::Dataclass);
    }
}
