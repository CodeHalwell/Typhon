//! Typhon syntax — lexer, parser, and AST extensions.
//!
//! The canonical AST is the vendored fork of `ruff_python_ast` in
//! [`vendor/ruff_python_ast`](../../vendor/ruff_python_ast). The parser is
//! the vendored fork of `ruff_python_parser`, which recognises `let` and
//! `mut` as first-class soft keywords — the resulting `StmtAssign` and
//! `StmtAnnAssign` AST nodes carry a `mutability: Option<Mutability>`
//! field directly, so no preprocessor pass is required for the binding
//! prefixes.
//!
//! The [`preprocess`] module still rewrites surface sugar (`?`
//! nullability, `model:`, `interface:`, `unsafe:`, `comptime`,
//! `gather:`, `go`, `lazy`, `with`-chains, the `?` error-propagation
//! operator) into plain Python before parsing.

pub mod lexer;
pub mod lexmask;
pub mod mro;
pub mod preprocess;
pub mod ruff;

pub use ruff::{parse_expression, parse_module, ParseError, Parsed};
pub use ruff_python_ast as ast;

/// Names among `pure` / `memo` / `gatherable` that a module *defines or
/// imports for itself*.
///
/// The three Typhon marker decorators are recognised purely by their text, so
/// a user (or a library) that legitimately owns one of those names would have
/// their decorator silently deleted — and `@memo` worse than deleted: silently
/// *replaced* by `@functools.cache`. Evidence that the name is bound in the
/// module is enough to know it is not Typhon's marker.
///
/// This is the **single shared derivation** — the same rule-of-one home as
/// [`mro::field_collection_order`]. Both `tyc-analyse` (which decides whether
/// a decorator *is* Typhon's purity/memo/gather marker) and `tyc-desugar`
/// (which decides whether to strip or replace it) consume this function; they
/// make correlated decisions about the same three names, so a private copy in
/// either crate is a drift hazard that would make the pipeline half-strip or
/// half-honour a marker.
pub fn user_bound_marker_names(
    body: &[ruff_python_ast::Stmt],
) -> std::collections::HashSet<String> {
    use ruff_python_ast::{Expr, Stmt};
    const MARKERS: [&str; 3] = ["pure", "memo", "gatherable"];
    let mut out = std::collections::HashSet::new();
    let mut note = |name: &str| {
        if MARKERS.contains(&name) {
            out.insert(name.to_owned());
        }
    };
    for stmt in body {
        match stmt {
            Stmt::Import(i) => {
                for a in &i.names {
                    note(
                        a.asname
                            .as_ref()
                            .map(|n| n.as_str())
                            .unwrap_or_else(|| a.name.as_str()),
                    );
                }
            }
            Stmt::ImportFrom(i) => {
                for a in &i.names {
                    note(
                        a.asname
                            .as_ref()
                            .map(|n| n.as_str())
                            .unwrap_or_else(|| a.name.as_str()),
                    );
                }
            }
            Stmt::FunctionDef(f) => note(f.name.as_str()),
            Stmt::ClassDef(c) => note(c.name.as_str()),
            Stmt::Assign(a) => {
                for t in &a.targets {
                    if let Expr::Name(n) = t {
                        note(n.id.as_str());
                    }
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    note(n.id.as_str());
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {

    #[test]
    fn soft_keywords_are_valid_binding_names() {
        // A soft keyword is a valid identifier everywhere Python allows one,
        // and Typhon is a superset — `let match = re.match(...)` is ordinary
        // code (and the conventional name for a regex result). The parser
        // only accepted `TokenKind::Name` after `let` / `mut`, so `let` fell
        // through as an identifier and the line failed to parse.
        for src in [
            "let match = 1\n",
            "mut match = 1\n",
            "let type = 2\n",
            "let case = 3\n",
            "mut type: int = 4\n",
        ] {
            assert!(
                parse_module(src).is_ok(),
                "should parse as a binding: {src:?}"
            );
        }
    }

    #[test]
    fn user_bound_marker_names_sees_every_binding_form() {
        // The shared derivation must notice a marker name bound via import,
        // from-import (with and without alias), def, class, plain assignment,
        // and annotated assignment — and ignore non-marker names entirely.
        let src = "\
import memo
from lib import pure
from lib import gather as gatherable
def unrelated():
    pass
";
        let parsed = parse_module(src).expect("test source parses");
        let names = user_bound_marker_names(&parsed.into_syntax().body);
        assert!(names.contains("memo"));
        assert!(names.contains("pure"));
        assert!(names.contains("gatherable"));
        assert!(!names.contains("unrelated"));

        let src2 = "memo = object()\npure: int = 1\nclass gatherable:\n    pass\n";
        let parsed2 = parse_module(src2).expect("test source parses");
        let names2 = user_bound_marker_names(&parsed2.into_syntax().body);
        assert_eq!(names2.len(), 3);
    }
    use super::*;
}
