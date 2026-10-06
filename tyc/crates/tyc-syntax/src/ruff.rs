//! Ruff parser back-end — the migration target for Typhon's consumer crates.
//!
//! This module is the entry point to the Typhon fork of `ruff_python_parser`
//! that lives under [`vendor/ruff_python_parser`](../../../vendor/ruff_python_parser).
//! It exposes a small surface deliberately:
//!
//! * [`parse_module`] — parse Typhon source directly (no preprocessor needed
//!   for `let` / `mut`; sugar passes like `?`, `|>`, `with`-chains, and
//!   `gather:` still require preprocessing because they are surface syntax
//!   we don't extend the parser for).
//! * Re-exports of [`ast`] and the upstream [`Mutability`] enum so callers
//!   can pattern-match without taking a direct dep on the vendored crate.
//!
//! ## When to use which back-end
//!
//! New code should prefer [`parse_module`] in this module. Existing crates
//! still using [`crate::parser::parse_module`] will be ported one at a time
//! per the plan in `vendor/README.md`; until then the two back-ends coexist
//! and produce independent ASTs.

pub use ruff_python_ast as ast;
pub use ruff_python_ast::Mutability;
pub use ruff_python_parser::{ParseError, Parsed};

use ruff_python_ast::visitor::{walk_expr, Visitor};
use ruff_python_ast::{Expr, ModModule};
use ruff_python_parser::ParseErrorType;
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::lexmask::{ByteKind, LexMask};

/// CPython's tokenizer refuses to open a bracket while 200 are already open
/// (`MAXLEVEL` in `Parser/lexer/lexer.c`): `SyntaxError: too many nested
/// parentheses`. The vendored parser has no such limit.
const CPYTHON_MAX_BRACKET_DEPTH: usize = 200;

/// Parse a Typhon source file with the vendored Ruff parser.
///
/// `source` should contain Typhon source. `let` / `mut` are recognised as
/// soft keywords directly — no preprocessing pass is required for them.
/// Other Typhon-specific sugar (`?`, `|>`, `gather:`, `with`-chains, `go`,
/// etc.) is *not* yet known to this parser; callers that need to accept
/// that sugar should still preprocess it with the helpers in
/// [`crate::preprocess`] before calling this function.
///
/// A module whose emitted Python would open more than 200 brackets at once
/// is rejected the way CPython rejects that `.py` at compile time —
/// otherwise `tyc check` and `tyc run` accepted a program `tyc build` turned
/// into a `.py` that does not compile.
pub fn parse_module(source: &str) -> Result<Parsed<ModModule>, ParseError> {
    let parsed = ruff_python_parser::parse_module(source)?;
    if source_nests_past_cpython_limit(source) {
        if let Some(at) = emitted_bracket_past_cpython_limit(parsed.syntax()) {
            return Err(ParseError {
                error: ParseErrorType::OtherError(format!(
                    "too many nested parentheses: CPython allows at most \
                     {CPYTHON_MAX_BRACKET_DEPTH} open brackets at once"
                )),
                location: TextRange::at(at, 1.into()),
            });
        }
    }
    Ok(parsed)
}

/// Cheap pre-filter: whether the source's own code brackets (not those in
/// strings, comments or f-string fields) ever nest past the limit. The
/// emitted Python can only nest that deep if the source does.
fn source_nests_past_cpython_limit(source: &str) -> bool {
    let opens = source
        .bytes()
        .filter(|b| matches!(b, b'(' | b'[' | b'{'))
        .count();
    if opens <= CPYTHON_MAX_BRACKET_DEPTH {
        return false;
    }
    let mask = LexMask::new(source);
    let mut depth = 0usize;
    for (i, b) in source.bytes().enumerate() {
        if !matches!(b, b'(' | b'[' | b'{' | b')' | b']' | b'}')
            || !matches!(mask.kind(i), ByteKind::Code)
        {
            continue;
        }
        if matches!(b, b'(' | b'[' | b'{') {
            if depth == CPYTHON_MAX_BRACKET_DEPTH {
                return true;
            }
            depth += 1;
        } else {
            depth = depth.saturating_sub(1);
        }
    }
    false
}

/// Where the emitted Python opens a bracket past the limit, if it does.
///
/// Grouping parentheses are not in the AST, and the emitter prints only the
/// ones precedence needs, so `((((1))))` nested 3000 deep is fine. Counted
/// here are the brackets every printing of the AST must keep: list, set and
/// dict displays and comprehensions, call argument lists and subscripts.
/// Tuples, generator expressions and precedence parentheses are not counted
/// — so a miss is possible, a false report is not.
fn emitted_bracket_past_cpython_limit(module: &ModModule) -> Option<TextSize> {
    let mut probe = NestingProbe {
        depth: 0,
        too_deep: None,
    };
    probe.visit_body(&module.body);
    probe.too_deep
}

struct NestingProbe {
    depth: usize,
    too_deep: Option<TextSize>,
}

impl NestingProbe {
    fn nested(&mut self, at: TextSize, inner: impl FnOnce(&mut Self)) {
        if self.depth == CPYTHON_MAX_BRACKET_DEPTH {
            self.too_deep.get_or_insert(at);
            return;
        }
        self.depth += 1;
        inner(self);
        self.depth -= 1;
    }
}

impl<'a> Visitor<'a> for NestingProbe {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if self.too_deep.is_some() {
            return;
        }
        match expr {
            Expr::List(_)
            | Expr::Set(_)
            | Expr::Dict(_)
            | Expr::ListComp(_)
            | Expr::SetComp(_)
            | Expr::DictComp(_) => self.nested(expr.start(), |p| walk_expr(p, expr)),
            Expr::Call(call) => {
                self.visit_expr(&call.func);
                self.nested(call.arguments.start(), |p| {
                    p.visit_arguments(&call.arguments)
                });
            }
            Expr::Subscript(sub) => {
                self.visit_expr(&sub.value);
                self.nested(sub.value.end(), |p| p.visit_expr(&sub.slice));
            }
            _ => walk_expr(self, expr),
        }
    }
}

/// Parse a single expression — used by the type checker to resolve quoted
/// forward-reference annotations (`"Node"`, `"Tree[T]"`, `"list[Node]"`)
/// whose content escaped the surface-sugar preprocessor.
pub fn parse_expression(
    source: &str,
) -> Result<Parsed<ruff_python_ast::ModExpression>, ParseError> {
    ruff_python_parser::parse_expression(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ast::Stmt;

    #[test]
    fn let_binding_carries_mutability() {
        let parsed = parse_module("let x: int = 1\n").expect("parse");
        let Stmt::AnnAssign(a) = parsed.into_syntax().body.into_iter().next().unwrap() else {
            panic!("expected StmtAnnAssign");
        };
        assert_eq!(a.mutability, Some(Mutability::Let));
    }

    #[test]
    fn mut_binding_carries_mutability() {
        let parsed = parse_module("mut counter = 0\n").expect("parse");
        let Stmt::Assign(a) = parsed.into_syntax().body.into_iter().next().unwrap() else {
            panic!("expected StmtAssign");
        };
        assert_eq!(a.mutability, Some(Mutability::Mut));
    }

    #[test]
    fn python_315_syntax_parses() {
        // PEP 798, PEP 810 and unary-plus literal patterns: the emitter
        // lowers them for older targets, so the parser accepts them always.
        for src in [
            "x = [*g for g in groups]\n",
            "x = {**m for m in maps}\n",
            "lazy from json import dumps\n",
            "match x:\n    case +1:\n        pass\n    case 1 - +2j:\n        pass\n",
        ] {
            assert!(parse_module(src).is_ok(), "{src:?} must parse");
        }
        // CPython 3.15 still rejects a `-`-signed imaginary part.
        assert!(parse_module("match x:\n    case 1 + -2j:\n        pass\n").is_err());
    }

    #[test]
    fn plain_python_still_parses() {
        let parsed = parse_module("def f(x: int) -> int:\n    return x * 2\n").expect("parse");
        assert_eq!(parsed.into_syntax().body.len(), 1);
    }

    #[test]
    fn bracket_nesting_stops_where_cpython_stops() {
        let nest = |d: usize| format!("x = {}{}\n", "[".repeat(d), "]".repeat(d));
        assert!(parse_module(&nest(200)).is_ok());
        let err = parse_module(&nest(201)).expect_err("201 levels");
        assert!(
            err.to_string().contains("too many nested parentheses"),
            "{err}"
        );
        assert_eq!(usize::from(err.location.start()), "x = ".len() + 200);
        // Grouping parentheses vanish from the emitted Python (the stress
        // corpus nests 3000 of them; 400 keeps the debug test stack small);
        // brackets in strings and comments, and many sequential groups, are
        // not nesting either.
        let grouped = format!("x = {}1{}\n", "(".repeat(400), ")".repeat(400));
        assert!(parse_module(&grouped).is_ok());
        let calls = format!("x = {}1{}\n", "f(".repeat(201), ")".repeat(201));
        assert!(parse_module(&calls).is_err());
        let flat = format!(
            "x = [{}]  # {}\ns = \"{}\"\n",
            "(1), ".repeat(300),
            "(".repeat(300),
            "[".repeat(300)
        );
        assert!(parse_module(&flat).is_ok());
    }

    #[test]
    fn let_identifier_outside_statement_start() {
        // `let` mid-expression is still a regular identifier.
        let parsed = parse_module("y = let + 1\n").expect("parse");
        let Stmt::Assign(a) = parsed.into_syntax().body.into_iter().next().unwrap() else {
            panic!("expected StmtAssign");
        };
        assert_eq!(a.mutability, None);
    }
}
