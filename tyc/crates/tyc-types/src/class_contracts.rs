//! Class declaration options used by member and operator contracts.
use super::*;
use ruff_python_ast::visitor::{self, Visitor};

pub(super) fn dataclass_option(c: &Checker, name: &str, option: &str) -> Option<bool> {
    struct Scan<'s> {
        name: &'s str,
        option: &'s str,
        result: Option<bool>,
    }
    impl<'a> Visitor<'a> for Scan<'_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::ClassDef(cd) = stmt {
                if cd.name.as_str() == self.name {
                    for decorator in &cd.decorator_list {
                        if let Expr::Call(call) = &decorator.expression {
                            let is_dataclass = match call.func.as_ref() {
                                Expr::Name(n) => n.id.as_str() == "dataclass",
                                Expr::Attribute(a) => a.attr.as_str() == "dataclass",
                                _ => false,
                            };
                            if is_dataclass {
                                self.result = Some(call.arguments.keywords.iter().any(|kw| {
                                    kw.arg.as_ref().is_some_and(|n| n.as_str() == self.option)
                                        && matches!(kw.value,Expr::BooleanLiteral(ref b) if b.value)
                                }));
                            }
                        }
                    }
                }
            }
            visitor::walk_stmt(self, stmt);
        }
    }
    let mut scan = Scan {
        name,
        option,
        result: None,
    };
    for stmt in &c.module?.body {
        scan.visit_stmt(stmt);
    }
    scan.result
}

/// IntEnum/StrEnum keys compare and hash like their scalar values.
pub(super) fn enum_key_compatible(c: &Checker, key: &Type, index: &Type) -> bool {
    let Type::Class(name) = key else { return false };
    c.enums.contains_key(name)
        && match index {
            Type::Int | Type::Bool => c.class_derives_from_builtin(
                name,
                &["int", "IntEnum", "enum.IntEnum", "IntFlag", "enum.IntFlag"],
            ),
            Type::Str | Type::LitStr(_) => {
                c.class_derives_from_builtin(name, &["str", "StrEnum", "enum.StrEnum"])
            }
            _ => false,
        }
}
