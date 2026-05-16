//! Diagnostic infrastructure for the Typhon compiler.
//!
//! Every compiler error or warning is represented as a [`TtcError`] that
//! implements [`miette::Diagnostic`], giving rich source-span rendering in
//! the terminal.

use miette::{Diagnostic, NamedSource, SourceOffset, SourceSpan};
use thiserror::Error;

/// A Typhon compiler error with source-location information.
#[derive(Debug, Clone, Error, Diagnostic)]
pub enum TtcError {
    /// The source file could not be read.
    #[error("could not read file '{path}': {cause}")]
    #[diagnostic(code(ttc::io), help("check that the file exists and is readable"))]
    Io { path: String, cause: String },

    /// The source file could not be parsed as a valid Typhon/Python program.
    #[error("parse error in '{path}'")]
    #[diagnostic(code(ttc::parse))]
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
        code(ttc::immutable_assign),
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
        code(ttc::unknown_name),
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
        code(ttc::type_mismatch),
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
        code(ttc::nullable_use),
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
    #[diagnostic(code(ttc::arg_count))]
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
    #[diagnostic(code(ttc::not_callable))]
    NotCallable {
        typ: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("this value is not a function")]
        span: SourceSpan,
    },

    /// Generic error with a human-readable message (used during early phases).
    #[error("{message}")]
    #[diagnostic(code(ttc::generic))]
    Generic { message: String },
}

impl TtcError {
    /// Construct a [`TtcError::Io`] from a [`std::io::Error`].
    pub fn io(path: impl Into<String>, cause: &dyn std::error::Error) -> Self {
        Self::Io {
            path: path.into(),
            cause: cause.to_string(),
        }
    }

    /// Construct a [`TtcError::Generic`] from any string-like message.
    pub fn generic(message: impl Into<String>) -> Self {
        Self::Generic {
            message: message.into(),
        }
    }

    /// Construct a [`TtcError::Parse`] from a rustpython parse error.
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

    /// Construct an [`TtcError::UnknownName`] diagnostic.
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

    /// Construct a [`TtcError::TypeMismatch`] diagnostic.
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

    /// Construct a [`TtcError::NullableUse`] diagnostic.
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

    /// Construct a [`TtcError::WrongArgCount`] diagnostic.
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

    /// Construct a [`TtcError::NotCallable`] diagnostic.
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

    /// Construct an [`TtcError::ImmutableAssign`] diagnostic.
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
            declaration: SourceSpan::new(
                SourceOffset::from(declaration_offset),
                declaration_len,
            ),
            assignment: SourceSpan::new(
                SourceOffset::from(assignment_offset),
                assignment_len,
            ),
        }
    }
}

/// A list of diagnostics collected during a compiler phase.
#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    errors: Vec<TtcError>,
    warnings: Vec<TtcError>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_error(&mut self, e: TtcError) {
        self.errors.push(e);
    }

    pub fn push_warning(&mut self, w: TtcError) {
        self.warnings.push(w);
    }

    pub fn extend(&mut self, other: Diagnostics) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn errors(&self) -> &[TtcError] {
        &self.errors
    }

    pub fn warnings(&self) -> &[TtcError] {
        &self.warnings
    }

    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    pub fn warning_count(&self) -> usize {
        self.warnings.len()
    }
}
