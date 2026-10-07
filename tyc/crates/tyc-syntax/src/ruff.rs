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

/// One construct in `source` that the target CPython cannot parse, such as
/// a 3.15-only comprehension unpacking on a 3.13 target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedSyntax {
    /// Byte range of the construct in `source`.
    pub range: TextRange,
    /// Human-readable reason, naming the construct and the versions, e.g.
    /// "Cannot use iterable unpacking in a list comprehension on Python 3.13
    /// (syntax was added in Python 3.15)".
    pub message: String,
}

/// Every construct in `source` that needs a newer Python than
/// `major.minor`, in source order.
///
/// [`parse_module`] parses against the newest grammar, so a program using
/// 3.15-only syntax type-checks on a 3.13 target and only fails when
/// CPython 3.13 compiles the emitted `.py`. This re-parses with the target
/// version and returns what the vendored parser flags as unsupported there.
/// A source that does not parse at all yields nothing: [`parse_module`]
/// already reports that.
///
/// Only syntax newer than the 3.13 floor is reported (3.14's t-strings and
/// unparenthesised `except A, B:`, 3.15's comprehension unpacking). A
/// `lazy` import is never reported: Typhon's own `lazy import` predates
/// PEP 810 and lowers per target, and the forms it does not lower have
/// their own diagnostic.
///
/// The re-parse runs only when the text could hold one of those forms, so
/// most files pay a byte scan rather than a second parse.
pub fn unsupported_syntax(source: &str, major: u8, minor: u8) -> Vec<UnsupportedSyntax> {
    if (major, minor) >= (3, 15) || !may_hold_newer_syntax(source) {
        return Vec::new();
    }
    let options = ruff_python_parser::ParseOptions::from(ruff_python_parser::Mode::Module)
        .with_target_version(ruff_python_ast::PythonVersion { major, minor });
    let parsed = ruff_python_parser::parse_unchecked(source, options);
    if !parsed.errors().is_empty() {
        return Vec::new();
    }
    parsed
        .unsupported_syntax_errors()
        .iter()
        .filter(|e| {
            use ruff_python_parser::UnsupportedSyntaxErrorKind as K;
            matches!(
                e.kind,
                K::UnpackingInComprehension(_)
                    | K::UnparenthesizedExceptionTypes
                    | K::TemplateStrings
            )
        })
        .map(|e| UnsupportedSyntax {
            range: e.range,
            message: e.to_string(),
        })
        .collect()
}

/// Cheap pre-filter for [`unsupported_syntax`]: whether `source` could hold
/// one of the forms it reports. Errs towards `true`; only a `false` skips
/// the parse.
///
/// - comprehension unpacking starts with `[*`, `{*` or `(*` (spaces
///   allowed after the bracket);
/// - an unparenthesised `except A, B:` has a comma outside brackets
///   between `except` and its colon;
/// - a t-string has a `t` / `T` prefix (optionally with `r`) that starts a
///   token, right before a quote.
pub fn may_hold_newer_syntax(source: &str) -> bool {
    let bytes = source.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' | b'{' | b'(' => {
                let rest = &bytes[i + 1..];
                let skip = rest.iter().take_while(|c| **c == b' ').count();
                if rest.get(skip) == Some(&b'*') {
                    return true;
                }
            }
            b't' | b'T' if i == 0 || !ident(bytes[i - 1]) => {
                let mut j = i + 1;
                if matches!(bytes.get(j), Some(b'r' | b'R')) {
                    j += 1;
                }
                if matches!(bytes.get(j), Some(b'"' | b'\'')) {
                    return true;
                }
            }
            _ => {}
        }
    }
    source.lines().any(|line| {
        let Some(clause) = line.trim_start().strip_prefix("except") else {
            return false;
        };
        let mut depth = 0i32;
        for c in clause.bytes() {
            match c {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => return true,
                b':' if depth == 0 => return false,
                _ => {}
            }
        }
        false
    })
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
