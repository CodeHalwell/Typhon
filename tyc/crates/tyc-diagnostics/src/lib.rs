//! Diagnostic infrastructure for the Typhon compiler.
//!
//! Every compiler error or warning is represented as a [`TycError`] that
//! implements [`miette::Diagnostic`], giving rich source-span rendering in
//! the terminal.

use miette::{Diagnostic, NamedSource, SourceOffset, SourceSpan};
use thiserror::Error;

/// A Typhon compiler error with source-location information.
#[derive(Debug, Clone, Error, Diagnostic)]
pub enum TycError {
    /// The source file could not be read.
    #[error("could not read file '{path}': {cause}")]
    #[diagnostic(code(tyc::io), help("check that the file exists and is readable"))]
    Io { path: String, cause: String },

    /// The source file could not be parsed as a valid Typhon/Python program.
    #[error("parse error in '{path}'")]
    #[diagnostic(code(tyc::parse))]
    Parse {
        path: String,
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("{message}")]
        span: SourceSpan,
    },

    /// A `val` binding was re-assigned after its declaration.
    #[error("cannot assign to immutable binding '{name}'")]
    #[diagnostic(
        code(tyc::immutable_assign),
        help("change `val` to `var` if you need a mutable binding")
    )]
    ImmutableAssign {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("first declared here")]
        declaration: SourceSpan,
        #[label("illegal re-assignment here")]
        assignment: SourceSpan,
    },

    /// A name is used but never declared in any enclosing scope.
    #[error("cannot find '{name}' in scope")]
    #[diagnostic(
        code(tyc::unknown_name),
        help("declare '{name}' with `val` or `var`, or import it from a module")
    )]
    UnknownName {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not found in scope")]
        span: SourceSpan,
    },

    /// A value of one type was used where another type was expected.
    #[error("type mismatch: expected `{expected}`, found `{actual}`")]
    #[diagnostic(
        code(tyc::type_mismatch),
        help("change the value, or update the annotation to `{actual}`")
    )]
    TypeMismatch {
        expected: String,
        actual: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("expected `{expected}`")]
        span: SourceSpan,
    },

    /// A nullable value (`T | None`) was used in a position requiring `T`.
    #[error("possibly-None value used where `{expected}` is required")]
    #[diagnostic(
        code(tyc::nullable_use),
        help("guard the value with `if {name} is not None:` to narrow it to `{expected}`")
    )]
    NullableUse {
        name: String,
        expected: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("value is `{expected} | None` here")]
        span: SourceSpan,
    },

    /// A function was called with the wrong number of positional arguments.
    #[error("wrong number of arguments to `{name}`: expected {expected}, got {actual}")]
    #[diagnostic(code(tyc::arg_count))]
    WrongArgCount {
        name: String,
        expected: usize,
        actual: usize,
        #[source_code]
        src: NamedSource<String>,
        #[label("called with {actual} argument(s) here")]
        span: SourceSpan,
    },

    /// Something that is not callable was called.
    #[error("`{typ}` is not callable")]
    #[diagnostic(code(tyc::not_callable))]
    NotCallable {
        typ: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("this value is not a function")]
        span: SourceSpan,
    },

    /// A `match` on a sealed union does not cover all variants and has no wildcard arm.
    #[error("non-exhaustive `match` on sealed union `{union_name}`: missing variant(s) {missing}")]
    #[diagnostic(
        code(tyc::non_exhaustive_match),
        help("add a `case <Variant>():` arm for each missing variant, or add a `case _:` wildcard arm")
    )]
    NonExhaustiveMatch {
        union_name: String,
        missing: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("match is not exhaustive")]
        span: SourceSpan,
    },

    /// A `comptime` binding could not be evaluated at build time.
    #[error("comptime evaluation failed for '{name}': {message}")]
    #[diagnostic(
        code(tyc::comptime),
        help("comptime expressions support: literals, env(\"NAME\"), env(\"NAME\", \"default\"), int(), str(), float(), and basic arithmetic")
    )]
    Comptime { name: String, message: String },

    /// Generic error with a human-readable message (used during early phases).
    #[error("{message}")]
    #[diagnostic(code(tyc::generic))]
    Generic { message: String },

    /// The `?` error-propagation operator was used outside a `Result`-returning function.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::invalid_question_op),
        help("the `?` operator is only valid inside a function returning `Result[T, E]`")
    )]
    InvalidQuestionOp {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("invalid use of `?` here")]
        span: SourceSpan,
    },

    /// An imported name is never used in the module.
    #[error("imported name '{name}' is never used")]
    #[diagnostic(
        code(tyc::unused_import),
        help("remove the import, or prefix it with `_` if it is intentionally unused")
    )]
    UnusedImport {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("imported here but never used")]
        span: SourceSpan,
    },

    /// A `lazy` construct was used in an unsupported form.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::lazy_usage),
        help("`lazy` supports `lazy import name = module` and `lazy val NAME: T = expr` only")
    )]
    LazyUsage {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("unsupported lazy form here")]
        span: SourceSpan,
    },

    /// A function decorated `@pure` violates one of the six purity conditions.
    #[error("`@pure` function '{name}' is not pure: {reason}")]
    #[diagnostic(
        code(tyc::impure_pure_fn),
        help("pure functions must be sync, take hashable args, perform no I/O, no entropy/clocks, no mutable module state, and not raise — return Result[T, E] for failure")
    )]
    ImpurePureFn {
        name: String,
        reason: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("declared `@pure` here")]
        span: SourceSpan,
    },

    /// `isinstance(x, Interface)` was used without an opt-in.
    #[error("`isinstance(x, {interface})` is rejected: structural interfaces only validate attribute presence at runtime")]
    #[diagnostic(
        code(tyc::interface_isinstance),
        help("opt in by decorating the interface with `@runtime_checkable` (attribute-only check) or rely on static structural typing instead")
    )]
    InterfaceIsinstance {
        interface: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("runtime check against interface")]
        span: SourceSpan,
    },

    /// A value of type `T` doesn't structurally conform to an `interface Iface`.
    #[error("`{actual}` does not structurally conform to interface `{interface}`: missing or incompatible member(s) {missing}")]
    #[diagnostic(
        code(tyc::interface_not_conforming),
        help(
            "add the missing member(s) to `{actual}` with matching parameter types and return type"
        )
    )]
    InterfaceNotConforming {
        interface: String,
        actual: String,
        missing: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not a `{interface}`")]
        span: SourceSpan,
    },
}

impl TycError {
    /// Construct a [`TycError::Io`] from a [`std::io::Error`].
    pub fn io(path: impl Into<String>, cause: &dyn std::error::Error) -> Self {
        Self::Io {
            path: path.into(),
            cause: cause.to_string(),
        }
    }

    /// Construct a [`TycError::Generic`] from any string-like message.
    pub fn generic(message: impl Into<String>) -> Self {
        Self::Generic {
            message: message.into(),
        }
    }

    /// Construct a [`TycError::Parse`] from a rustpython parse error.
    pub fn parse(
        path: impl Into<String>,
        source: impl Into<String>,
        message: impl Into<String>,
        offset: usize,
    ) -> Self {
        let path = path.into();
        let source = source.into();
        let message = message.into();
        let span = SourceSpan::new(SourceOffset::from(offset), 0usize);
        Self::Parse {
            src: NamedSource::new(path.clone(), source),
            path,
            message,
            span,
        }
    }

    /// Construct an [`TycError::UnknownName`] diagnostic.
    pub fn unknown_name(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::UnknownName {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::TypeMismatch`] diagnostic.
    pub fn type_mismatch(
        expected: impl Into<String>,
        actual: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::TypeMismatch {
            expected: expected.into(),
            actual: actual.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NullableUse`] diagnostic.
    pub fn nullable_use(
        name: impl Into<String>,
        expected: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NullableUse {
            name: name.into(),
            expected: expected.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::WrongArgCount`] diagnostic.
    pub fn wrong_arg_count(
        name: impl Into<String>,
        expected: usize,
        actual: usize,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::WrongArgCount {
            name: name.into(),
            expected,
            actual,
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NotCallable`] diagnostic.
    pub fn not_callable(
        typ: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NotCallable {
            typ: typ.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NonExhaustiveMatch`] diagnostic.
    pub fn non_exhaustive_match(
        union_name: impl Into<String>,
        missing: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NonExhaustiveMatch {
            union_name: union_name.into(),
            missing: missing.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::Comptime`] diagnostic.
    pub fn comptime(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Comptime {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Construct a [`TycError::InvalidQuestionOp`] diagnostic.
    pub fn invalid_question_op(
        message: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::InvalidQuestionOp {
            message: message.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::UnusedImport`] diagnostic.
    pub fn unused_import(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::UnusedImport {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::LazyUsage`] diagnostic.
    pub fn lazy_usage(
        message: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::LazyUsage {
            message: message.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ImpurePureFn`] diagnostic.
    pub fn impure_pure_fn(
        name: impl Into<String>,
        reason: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ImpurePureFn {
            name: name.into(),
            reason: reason.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::InterfaceIsinstance`] diagnostic.
    pub fn interface_isinstance(
        interface: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::InterfaceIsinstance {
            interface: interface.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::InterfaceNotConforming`] diagnostic.
    pub fn interface_not_conforming(
        interface: impl Into<String>,
        actual: impl Into<String>,
        missing: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::InterfaceNotConforming {
            interface: interface.into(),
            actual: actual.into(),
            missing: missing.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct an [`TycError::ImmutableAssign`] diagnostic.
    pub fn immutable_assign(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        declaration_offset: usize,
        declaration_len: usize,
        assignment_offset: usize,
        assignment_len: usize,
    ) -> Self {
        let path = path.into();
        let source = source.into();
        Self::ImmutableAssign {
            name: name.into(),
            src: NamedSource::new(path, source),
            declaration: SourceSpan::new(SourceOffset::from(declaration_offset), declaration_len),
            assignment: SourceSpan::new(SourceOffset::from(assignment_offset), assignment_len),
        }
    }
}

/// A list of diagnostics collected during a compiler phase.
#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    errors: Vec<TycError>,
    warnings: Vec<TycError>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_error(&mut self, e: TycError) {
        self.errors.push(e);
    }

    pub fn push_warning(&mut self, w: TycError) {
        self.warnings.push(w);
    }

    pub fn extend(&mut self, other: Diagnostics) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn errors(&self) -> &[TycError] {
        &self.errors
    }

    pub fn warnings(&self) -> &[TycError] {
        &self.warnings
    }

    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    pub fn warning_count(&self) -> usize {
        self.warnings.len()
    }

    /// Consume the `Diagnostics` and return `(errors, warnings)` as owned vectors,
    /// allowing callers to move diagnostics without cloning.
    pub fn into_parts(self) -> (Vec<TycError>, Vec<TycError>) {
        (self.errors, self.warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Diagnostics collection ────────────────────────────────────────────────

    #[test]
    fn diagnostics_starts_empty() {
        let d = Diagnostics::new();
        assert!(!d.has_errors());
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn push_error_increments_error_count() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("boom"));
        assert!(d.has_errors());
        assert_eq!(d.error_count(), 1);
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn push_warning_increments_warning_count_only() {
        let mut d = Diagnostics::new();
        d.push_warning(TycError::generic("heads up"));
        assert!(!d.has_errors());
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.warning_count(), 1);
    }

    #[test]
    fn extend_merges_errors_and_warnings() {
        let mut a = Diagnostics::new();
        a.push_error(TycError::generic("err-a"));

        let mut b = Diagnostics::new();
        b.push_warning(TycError::generic("warn-b"));

        a.extend(b);
        assert_eq!(a.error_count(), 1);
        assert_eq!(a.warning_count(), 1);
    }

    #[test]
    fn into_parts_moves_both_vecs() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("e"));
        d.push_warning(TycError::generic("w"));
        let (errs, warns) = d.into_parts();
        assert_eq!(errs.len(), 1);
        assert_eq!(warns.len(), 1);
    }

    // ── TycError message text ─────────────────────────────────────────────────

    #[test]
    fn generic_error_message_is_preserved() {
        let e = TycError::generic("something went wrong");
        assert_eq!(e.to_string(), "something went wrong");
    }

    #[test]
    fn io_error_message_contains_path_and_cause() {
        let e = TycError::io("src/main.ty", &std::io::Error::other("permission denied"));
        let msg = e.to_string();
        assert!(
            msg.contains("src/main.ty"),
            "path missing from IO error: {msg}"
        );
        assert!(
            msg.contains("permission denied"),
            "cause missing from IO error: {msg}"
        );
    }

    #[test]
    fn type_mismatch_message_contains_both_types() {
        let e = TycError::type_mismatch("int", "str", "f.ty", "val x: int = \"hi\"", 13, 4);
        let msg = e.to_string();
        assert!(msg.contains("int"), "expected type missing: {msg}");
        assert!(msg.contains("str"), "actual type missing: {msg}");
    }

    #[test]
    fn unknown_name_message_contains_name() {
        let e = TycError::unknown_name("foo", "f.ty", "foo()", 0, 3);
        assert!(e.to_string().contains("foo"));
    }

    #[test]
    fn wrong_arg_count_message_contains_expected_and_actual() {
        let e = TycError::wrong_arg_count("greet", 1, 3, "f.ty", "greet(1,2,3)", 0, 5);
        let msg = e.to_string();
        assert!(msg.contains('1'), "expected count missing: {msg}");
        assert!(msg.contains('3'), "actual count missing: {msg}");
        assert!(msg.contains("greet"), "function name missing: {msg}");
    }

    #[test]
    fn non_exhaustive_match_message_contains_union_and_missing() {
        let e = TycError::non_exhaustive_match("Shape", "Circle", "f.ty", "match s:", 0, 7);
        let msg = e.to_string();
        assert!(msg.contains("Shape"), "union name missing: {msg}");
        assert!(msg.contains("Circle"), "missing variant missing: {msg}");
    }

    #[test]
    fn comptime_error_message_contains_name_and_message() {
        let e = TycError::comptime("PORT", "env var not set");
        let msg = e.to_string();
        assert!(msg.contains("PORT"), "binding name missing: {msg}");
        assert!(msg.contains("env var not set"), "reason missing: {msg}");
    }

    #[test]
    fn impure_pure_fn_message_contains_name_and_reason() {
        let e = TycError::impure_pure_fn(
            "compute",
            "calls print()",
            "f.ty",
            "@pure\ndef compute(): pass",
            0,
            5,
        );
        let msg = e.to_string();
        assert!(msg.contains("compute"), "function name missing: {msg}");
        assert!(msg.contains("calls print()"), "reason missing: {msg}");
    }

    #[test]
    fn interface_not_conforming_message_contains_all_parts() {
        let e = TycError::interface_not_conforming(
            "Drawable",
            "Circle",
            "draw",
            "f.ty",
            "val c: Drawable = Circle()",
            17,
            8,
        );
        let msg = e.to_string();
        assert!(msg.contains("Drawable"), "interface missing: {msg}");
        assert!(msg.contains("Circle"), "actual type missing: {msg}");
        assert!(msg.contains("draw"), "missing member missing: {msg}");
    }

    // ── Diagnostic codes are stable ───────────────────────────────────────────

    #[test]
    fn error_codes_are_stable() {
        use miette::Diagnostic;

        let cases: &[(&str, TycError)] = &[
            ("tyc::generic", TycError::generic("x")),
            ("tyc::io", TycError::io("p", &std::io::Error::other("e"))),
            (
                "tyc::type_mismatch",
                TycError::type_mismatch("int", "str", "f.ty", "x", 0, 1),
            ),
            (
                "tyc::unknown_name",
                TycError::unknown_name("x", "f.ty", "x", 0, 1),
            ),
            (
                "tyc::arg_count",
                TycError::wrong_arg_count("f", 1, 2, "f.ty", "f(1,2)", 0, 1),
            ),
            ("tyc::comptime", TycError::comptime("X", "bad")),
        ];

        for (expected_code, err) in cases {
            let code = err
                .code()
                .expect("diagnostic should have a code")
                .to_string();
            assert_eq!(
                &code, expected_code,
                "code mismatch for {expected_code}: got {code}"
            );
        }
    }
}
