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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── TycError constructor correctness ─────────────────────────────────────

    #[test]
    fn io_error_contains_path_and_cause() {
        let e = TycError::io("foo.ty", &std::io::Error::other("disk full"));
        let msg = e.to_string();
        assert!(msg.contains("foo.ty"), "path should appear in message");
        assert!(msg.contains("disk full"), "cause should appear in message");
        assert!(matches!(e, TycError::Io { .. }));
    }

    #[test]
    fn generic_error_round_trips_message() {
        let e = TycError::generic("something went wrong");
        assert_eq!(e.to_string(), "something went wrong");
        assert!(matches!(e, TycError::Generic { .. }));
    }

    #[test]
    fn parse_error_is_correct_variant() {
        let e = TycError::parse("a.ty", "val x = 1", "unexpected token", 4);
        assert!(matches!(e, TycError::Parse { .. }));
        let msg = e.to_string();
        assert!(msg.contains("a.ty"));
    }

    #[test]
    fn unknown_name_contains_name() {
        let e = TycError::unknown_name("foo", "a.ty", "foo", 0, 3);
        assert!(matches!(e, TycError::UnknownName { .. }));
        assert!(e.to_string().contains("foo"));
    }

    #[test]
    fn type_mismatch_contains_expected_and_actual() {
        let e = TycError::type_mismatch("int", "str", "a.ty", "val x: int = \"hi\"", 0, 5);
        assert!(matches!(e, TycError::TypeMismatch { .. }));
        let msg = e.to_string();
        assert!(msg.contains("int"), "expected type should appear");
        assert!(msg.contains("str"), "actual type should appear");
    }

    #[test]
    fn nullable_use_contains_expected_type() {
        let e = TycError::nullable_use("x", "str", "a.ty", "val x: str? = None", 0, 1);
        assert!(matches!(e, TycError::NullableUse { .. }));
        // The diagnostic message embeds `expected`; the variable name appears
        // only in the source-span label rendered by miette, not in to_string().
        assert!(e.to_string().contains("str"));
    }

    #[test]
    fn wrong_arg_count_contains_name_and_counts() {
        let e = TycError::wrong_arg_count("f", 2, 3, "a.ty", "f(1, 2, 3)", 0, 9);
        assert!(matches!(e, TycError::WrongArgCount { .. }));
        let msg = e.to_string();
        assert!(msg.contains("f"));
        assert!(msg.contains('2'), "expected count should appear");
        assert!(msg.contains('3'), "actual count should appear");
    }

    #[test]
    fn not_callable_contains_type() {
        let e = TycError::not_callable("int", "a.ty", "1()", 0, 3);
        assert!(matches!(e, TycError::NotCallable { .. }));
        assert!(e.to_string().contains("int"));
    }

    #[test]
    fn non_exhaustive_match_contains_union_and_missing() {
        let e = TycError::non_exhaustive_match("Shape", "Circle", "a.ty", "match s:", 0, 7);
        assert!(matches!(e, TycError::NonExhaustiveMatch { .. }));
        let msg = e.to_string();
        assert!(msg.contains("Shape"));
        assert!(msg.contains("Circle"));
    }

    #[test]
    fn comptime_contains_name_and_message() {
        let e = TycError::comptime("PORT", "env var missing");
        assert!(matches!(e, TycError::Comptime { .. }));
        let msg = e.to_string();
        assert!(msg.contains("PORT"));
        assert!(msg.contains("env var missing"));
    }

    #[test]
    fn invalid_question_op_is_correct_variant() {
        let e = TycError::invalid_question_op("bad use", "a.ty", "x?", 0, 2);
        assert!(matches!(e, TycError::InvalidQuestionOp { .. }));
        assert!(e.to_string().contains("bad use"));
    }

    #[test]
    fn unused_import_contains_name() {
        let e = TycError::unused_import("os", "a.ty", "import os", 0, 9);
        assert!(matches!(e, TycError::UnusedImport { .. }));
        assert!(e.to_string().contains("os"));
    }

    #[test]
    fn lazy_usage_contains_message() {
        let e = TycError::lazy_usage("unsupported form", "a.ty", "lazy from x import y", 0, 20);
        assert!(matches!(e, TycError::LazyUsage { .. }));
        assert!(e.to_string().contains("unsupported form"));
    }

    #[test]
    fn impure_pure_fn_contains_name_and_reason() {
        let e = TycError::impure_pure_fn(
            "compute",
            "calls I/O",
            "a.ty",
            "@pure\ndef compute(): pass",
            0,
            5,
        );
        assert!(matches!(e, TycError::ImpurePureFn { .. }));
        let msg = e.to_string();
        assert!(msg.contains("compute"));
        assert!(msg.contains("calls I/O"));
    }

    #[test]
    fn interface_isinstance_contains_interface_name() {
        let e = TycError::interface_isinstance(
            "Serialisable",
            "a.ty",
            "isinstance(x, Serialisable)",
            0,
            26,
        );
        assert!(matches!(e, TycError::InterfaceIsinstance { .. }));
        assert!(e.to_string().contains("Serialisable"));
    }

    #[test]
    fn interface_not_conforming_contains_key_fields() {
        let e = TycError::interface_not_conforming(
            "Writer",
            "MyClass",
            "write",
            "a.ty",
            "x: Writer = MyClass()",
            0,
            20,
        );
        assert!(matches!(e, TycError::InterfaceNotConforming { .. }));
        let msg = e.to_string();
        assert!(msg.contains("Writer"));
        assert!(msg.contains("MyClass"));
        assert!(msg.contains("write"));
    }

    #[test]
    fn immutable_assign_contains_name() {
        let e = TycError::immutable_assign("x", "a.ty", "val x: int = 1\nx = 2", 4, 1, 15, 1);
        assert!(matches!(e, TycError::ImmutableAssign { .. }));
        assert!(e.to_string().contains("x"));
    }

    // ── Diagnostics collection API ────────────────────────────────────────────

    #[test]
    fn new_diagnostics_is_empty() {
        let d = Diagnostics::new();
        assert!(!d.has_errors());
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn push_error_increments_error_count() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("e1"));
        assert!(d.has_errors());
        assert_eq!(d.error_count(), 1);
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn push_warning_increments_warning_count_not_error_count() {
        let mut d = Diagnostics::new();
        d.push_warning(TycError::generic("w1"));
        assert!(!d.has_errors());
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.warning_count(), 1);
    }

    #[test]
    fn extend_merges_both_error_and_warning_lists() {
        let mut a = Diagnostics::new();
        a.push_error(TycError::generic("e1"));
        a.push_warning(TycError::generic("w1"));

        let mut b = Diagnostics::new();
        b.push_error(TycError::generic("e2"));
        b.push_warning(TycError::generic("w2"));

        a.extend(b);
        assert_eq!(a.error_count(), 2);
        assert_eq!(a.warning_count(), 2);
    }

    #[test]
    fn into_parts_separates_errors_and_warnings() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("err"));
        d.push_warning(TycError::generic("warn"));

        let (errors, warnings) = d.into_parts();
        assert_eq!(errors.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(errors[0].to_string().contains("err"));
        assert!(warnings[0].to_string().contains("warn"));
    }

    #[test]
    fn errors_and_warnings_slices_are_consistent_with_counts() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("e1"));
        d.push_error(TycError::generic("e2"));
        d.push_warning(TycError::generic("w1"));

        assert_eq!(d.errors().len(), d.error_count());
        assert_eq!(d.warnings().len(), d.warning_count());
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
