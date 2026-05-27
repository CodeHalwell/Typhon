//! Typhon-specific tokens that extend the Python token set.
//!
//! Phase 0 introduces `let` and `mut` as first-class keywords.
//! Phase 2 adds `model` (Pydantic class emission) and `comptime` (build-time
//! constant evaluation).
//! Phase 3 adds `impl`, `extend`, `interface`, `unsafe`, `gather`, `go`, and
//! `lazy` for extension methods, structural typing, dynamism boundaries,
//! concurrency primitives, and deferred module loading.
//! These are recognised here and stripped (pre-processed) before the
//! underlying Python parser sees them, so the existing Python parser can
//! handle the remainder of the grammar unchanged.

/// A Typhon keyword that is not part of standard Python.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TyphonKeyword {
    /// `let` — declares an immutable binding (like Rust's `let`).
    Let,
    /// `mut` — declares a mutable binding (like Rust's `let mut`).
    Mut,
    /// `model` — declares a Pydantic `BaseModel` class instead of a dataclass.
    Model,
    /// `comptime` — marks a binding whose RHS is evaluated at build time and
    /// inlined as a literal in the emitted Python.
    Comptime,
    /// `impl` — attaches a method block to a previously-declared class, Rust-style.
    /// The preprocessor rewrites `impl ClassName:` to `class __typhon_impl_ClassName(object):`,
    /// and the desugar pass merges the methods back into the target class.
    Impl,
    /// `extend` — like `impl`, but for adding methods to a type.
    /// For user-defined classes it is treated identically to `impl`.
    Extend,
    /// `interface` — declares a structural protocol. The preprocessor rewrites
    /// `interface Name:` to `class Name(Protocol):`.
    Interface,
    /// `unsafe` — opens a lexical region in which `Any` types may flow freely.
    /// The preprocessor rewrites `unsafe:` to `if True:  # __typhon_unsafe__` so
    /// scoping survives the Python round-trip.
    Unsafe,
    /// `gather` — runs the inner bindings concurrently. Defaults to
    /// `asyncio.TaskGroup`; `gather(strategy="best-effort"):` lowers to
    /// `asyncio.gather(..., return_exceptions=True)`.
    Gather,
    /// `go` — spawns a background task via `typhon_runtime.tasks.spawn(...)`.
    Go,
    /// `lazy` — prefix for `lazy import X = module`; the module is loaded on
    /// first attribute access rather than at import time.
    Lazy,
    /// `class!` — declares a raw class that suppresses `@dataclass`
    /// injection at desugar time. Intended for subclasses of bases that
    /// need a non-trivial `__init__` to run before fields are assigned —
    /// `torch.nn.Module`, `enum.Enum`, `unittest.TestCase`, and ORM
    /// declarative bases. When the body has no hand-written `def
    /// __init__` and the class has at least one positional base, the
    /// desugar pass synthesises one that calls `super().__init__()` and
    /// then assigns each annotated field through `self`; otherwise the
    /// author's `__init__` is preserved verbatim.
    RawClass,
    /// `plain class` — the canonical Python-interop escape hatch.
    ///
    /// A multi-word, **line-prefix** keyword: detected by the preprocessor
    /// when a line begins with the literal text `"plain class "`. It is
    /// deliberately not registered in [`Self::keyword_of`], which handles
    /// single-token soft keywords; the multi-word form lives in the
    /// preprocessor's line-prefix branch instead.
    ///
    /// The preprocessor strips the leading `plain ` so the Python parser
    /// sees an ordinary `class NAME(...):` header. The desugar pass treats
    /// the class like a `class!` raw class for the purpose of skipping the
    /// `@dataclass` decorator, but — unlike `class!` — it never synthesises
    /// an `__init__`. The body is emitted exactly as written.
    PlainClass,
    /// `newtype` — declares a nominal alias over a base type.
    ///
    /// `newtype UserId = int` lowers to `UserId = NewType("UserId", int)`
    /// (Python `typing.NewType`) at preprocess time. The type checker
    /// treats `UserId` as nominally distinct from `int`: a bare `int`
    /// flowing into a `UserId`-typed slot is rejected, while a `UserId`
    /// flows into an `int` slot freely. Construction goes through
    /// `UserId(x)`, which type-checks the argument against `int` and
    /// yields a `UserId`-typed value.
    Newtype,
    /// `pub` — marks a module-level declaration as part of the public
    /// API surface. The preprocessor strips the prefix and records the
    /// line so the desugar pass can synthesise `__all__ = [...]` from
    /// every `pub`-marked name. Modules with at least one `pub` symbol
    /// get an `__all__` list emitted at the top; modules with none get
    /// no `__all__` (preserves current `from foo import *` behaviour).
    /// May appear before `def`, `class`, `let`, `mut`, `model`,
    /// `interface`, `class!`, `plain class`, `newtype`, and `type`.
    Pub,
    /// `freeze` — modifier on a `let` binding that deep-freezes the
    /// bound value. `freeze let users = […]` lowers to `users =
    /// __typhon_freeze__(…)`, where the runtime helper recursively
    /// wraps `list → tuple`, `dict → MappingProxyType`, `set →
    /// frozenset`, walks `@dataclass(frozen=True)` instances, and
    /// raises on anything that can't be frozen. Stronger than
    /// `let`'s binding-only immutability — the wrapped value cannot
    /// be mutated through any reference.
    Freeze,
    /// `frozen` — modifier on a `class` declaration. The preprocessor
    /// strips the modifier so the Python parser sees `class X:` (or
    /// `class X(Base):`) and records the line so the desugar pass can
    /// emit `@dataclasses.dataclass(slots=True, frozen=True)`. The
    /// `Frozen` keyword entry is recorded in the `stripped` list
    /// purely so `tyc fmt` can restore the modifier — desugar uses
    /// `frozen_class_lines` directly.
    Frozen,
    /// `pub *` — wildcard re-export marker. The preprocessor strips
    /// the entire line so the Python parser doesn't see it. The
    /// `PubStar` keyword entry is recorded in the `stripped` list
    /// purely so `tyc fmt` can restore the line — desugar / build
    /// orchestration uses `pub_star_lines` directly.
    PubStar,
}

impl TyphonKeyword {
    /// Return the source spelling of this keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Let => "let",
            Self::Mut => "mut",
            Self::Model => "model",
            Self::Comptime => "comptime",
            Self::Impl => "impl",
            Self::Extend => "extend",
            Self::Interface => "interface",
            Self::Unsafe => "unsafe",
            Self::Gather => "gather",
            Self::Go => "go",
            Self::Lazy => "lazy",
            Self::RawClass => "class!",
            Self::PlainClass => "plain class",
            Self::Newtype => "newtype",
            Self::Pub => "pub",
            Self::Freeze => "freeze",
            Self::Frozen => "frozen",
            Self::PubStar => "pub *",
        }
    }

    /// Try to parse a keyword from a source slice.
    pub fn keyword_of(s: &str) -> Option<Self> {
        match s {
            "let" => Some(Self::Let),
            "mut" => Some(Self::Mut),
            "model" => Some(Self::Model),
            "comptime" => Some(Self::Comptime),
            "impl" => Some(Self::Impl),
            "extend" => Some(Self::Extend),
            "interface" => Some(Self::Interface),
            "unsafe" => Some(Self::Unsafe),
            "gather" => Some(Self::Gather),
            "go" => Some(Self::Go),
            "lazy" => Some(Self::Lazy),
            "class!" => Some(Self::RawClass),
            "newtype" => Some(Self::Newtype),
            "pub" => Some(Self::Pub),
            "freeze" => Some(Self::Freeze),
            _ => None,
        }
    }
}

/// A single token produced by the Typhon lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TyphonToken<'src> {
    /// A Typhon-specific keyword (`let` / `mut`).
    Keyword(TyphonKeyword),
    /// Any other text fragment (passed through unchanged to the Python parser).
    Text(&'src str),
}

/// Lex a Typhon source string into a sequence of [`TyphonToken`]s.
///
/// This is a *word-boundary* lexer: it splits the source on whitespace
/// boundaries, recognises `let` and `mut`, and returns everything else as
/// [`TyphonToken::Text`] slices pointing into the original string.
pub fn lex(source: &str) -> Vec<TyphonToken<'_>> {
    let mut tokens = Vec::new();
    let mut start = 0;

    let bytes = source.as_bytes();
    let len = bytes.len();

    while start < len {
        // Skip to the next word-start (non-whitespace, non-special).
        // We scan for potential identifiers: [a-zA-Z_][a-zA-Z0-9_]*
        // Emit any non-identifier bytes as Text fragments directly.
        let ch = bytes[start] as char;
        if ch.is_ascii_alphabetic() || ch == '_' {
            // Scan to end of identifier.
            let mut end = start + 1;
            while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            let word = &source[start..end];
            if let Some(kw) = TyphonKeyword::keyword_of(word) {
                tokens.push(TyphonToken::Keyword(kw));
            } else {
                tokens.push(TyphonToken::Text(word));
            }
            start = end;
        } else {
            // Emit single non-identifier character as Text.
            // Coalesce consecutive non-identifier characters into one slice.
            let mut end = start + 1;
            while end < len {
                let c = bytes[end] as char;
                if c.is_ascii_alphabetic() || c == '_' {
                    break;
                }
                end += 1;
            }
            tokens.push(TyphonToken::Text(&source[start..end]));
            start = end;
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_let_and_mut() {
        let src = "let x: int = 1";
        let tokens = lex(src);
        assert_eq!(tokens[0], TyphonToken::Keyword(TyphonKeyword::Let));
    }

    #[test]
    fn does_not_match_partial_keyword() {
        // "letter" should NOT be tokenised as `let` + `ter`.
        let src = "letter = 1";
        let tokens = lex(src);
        assert!(
            tokens.iter().all(|t| !matches!(t, TyphonToken::Keyword(_))),
            "partial keyword match should not occur"
        );
    }

    #[test]
    fn mut_keyword() {
        let src = "mut count: int = 0";
        let tokens = lex(src);
        assert_eq!(tokens[0], TyphonToken::Keyword(TyphonKeyword::Mut));
    }
}
