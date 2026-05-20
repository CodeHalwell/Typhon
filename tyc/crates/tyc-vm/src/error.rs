//! Runtime errors and non-local control flow for the tree-walking interpreter.
//!
//! `VmError` is the user-facing error type — uncaught exceptions, type errors,
//! parse failures, unsupported features. `Unwind` wraps `VmError` together with
//! the loop/function control-flow signals (`return`, `break`, `continue`) so
//! every statement evaluator returns a single `Result<(), Unwind>`.

use crate::value::Value;

#[derive(Debug, Clone)]
pub enum Unwind {
    /// A user or runtime exception. Carries the exception value (often a
    /// `VmError` boxed as a string for v1) plus a chain of frames built up
    /// during unwinding.
    Exception(VmException),
    /// `return EXPR` — propagated up until the enclosing function catches it.
    Return(Value),
    /// `break` — propagated up to the enclosing loop.
    Break,
    /// `continue` — propagated up to the enclosing loop.
    Continue,
    /// Compiler-internal sentinel for `?` short-circuit when the call site
    /// isn't inside a function (should never happen with valid Typhon).
    QuestionMark(Value),
}

#[derive(Debug, Clone)]
pub struct VmException {
    pub kind: String,
    pub message: String,
    /// User-thrown exception object, if any (e.g. from `raise ValueError("…")`).
    pub value: Option<Value>,
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub function: String,
    pub line: Option<u32>,
}

impl VmException {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            value: None,
            frames: Vec::new(),
        }
    }
    pub fn with_value(mut self, v: Value) -> Self {
        self.value = Some(v);
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("parse error: {0}")]
    Parse(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("{0}")]
    Runtime(String),
}

impl VmError {
    pub fn runtime(msg: impl Into<String>) -> Self {
        VmError::Runtime(msg.into())
    }
}

pub type VmResult<T> = Result<T, Unwind>;

pub fn type_error(msg: impl Into<String>) -> Unwind {
    Unwind::Exception(VmException::new("TypeError", msg))
}
pub fn name_error(msg: impl Into<String>) -> Unwind {
    Unwind::Exception(VmException::new("NameError", msg))
}
pub fn value_error(msg: impl Into<String>) -> Unwind {
    Unwind::Exception(VmException::new("ValueError", msg))
}
pub fn attribute_error(msg: impl Into<String>) -> Unwind {
    Unwind::Exception(VmException::new("AttributeError", msg))
}
pub fn key_error(msg: impl Into<String>) -> Unwind {
    Unwind::Exception(VmException::new("KeyError", msg))
}
pub fn index_error(msg: impl Into<String>) -> Unwind {
    Unwind::Exception(VmException::new("IndexError", msg))
}
pub fn zero_division() -> Unwind {
    Unwind::Exception(VmException::new("ZeroDivisionError", "division by zero"))
}
pub fn not_implemented(feature: &str) -> Unwind {
    Unwind::Exception(VmException::new(
        "NotImplementedError",
        format!("tyc-vm v1 does not yet support: {feature}"),
    ))
}
pub fn stop_iteration() -> Unwind {
    Unwind::Exception(VmException::new("StopIteration", ""))
}
