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
    /// Set on the active exception an `except*` handler pushes before running
    /// its body. A naked `raise` clones the active exception, so this marker
    /// (plus value identity) is how `exec_try_star` recognises a PEP 654
    /// *re-raise* — which CPython merges back into the original group — as
    /// opposed to an explicit `raise e` of the bound subgroup, which produces
    /// a fresh exception and is treated as newly raised (verified on 3.13).
    pub star_handler_reraise: bool,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub function: String,
    pub line: Option<u32>,
    /// Source file the frame's line refers to (cross-module imports run
    /// sibling files through the same interpreter).
    pub file: Option<String>,
    /// The source line's text, for CPython-style traceback rendering.
    pub line_text: Option<String>,
}

impl VmException {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            value: None,
            frames: Vec::new(),
            star_handler_reraise: false,
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

/// `KeyError(key)` for a missing mapping key. The exception *value* carries
/// the key itself as `args[0]` (so `e.args[0] == "k"` holds) while the
/// message — what the traceback and `str(e)` show — is the key's repr, as in
/// CPython (`KeyError: 'k'`). The previous shape stored the repr string as
/// the argument, so `e.args[0]` was `"'k'"` and `str(e)` double-quoted it.
pub fn key_error_for(key: &Value) -> Unwind {
    let repr = key.py_repr();
    Unwind::Exception(
        VmException::new("KeyError", repr.clone()).with_value(Value::Exception {
            kind: std::rc::Rc::new("KeyError".to_owned()),
            message: std::rc::Rc::new(repr),
            args: std::rc::Rc::new(vec![key.clone()]),
        }),
    )
}

/// `SystemExit` as raised by `sys.exit()` / `exit()` / `quit()`. It is an
/// ordinary exception value — `finally` blocks run, `except SystemExit`
/// catches it, `except Exception` does not — and `run_source` turns an
/// uncaught one into the process exit status the way CPython does: no
/// argument or `None` → 0, an `int` → that code, anything else → printed to
/// stderr with status 1.
pub fn system_exit(args: Vec<Value>) -> Unwind {
    let message = match args.first() {
        None | Some(Value::None) => String::new(),
        Some(v) => v.py_str(),
    };
    Unwind::Exception(VmException::new("SystemExit", message.clone()).with_value(
        Value::Exception {
            kind: std::rc::Rc::new("SystemExit".to_owned()),
            message: std::rc::Rc::new(message),
            args: std::rc::Rc::new(args),
        },
    ))
}
pub fn index_error(msg: impl Into<String>) -> Unwind {
    Unwind::Exception(VmException::new("IndexError", msg))
}
pub fn zero_division() -> Unwind {
    Unwind::Exception(VmException::new("ZeroDivisionError", "division by zero"))
}

/// CPython's message for `//` and `%` by zero differs from true division's.
pub fn zero_division_floor_mod() -> Unwind {
    Unwind::Exception(VmException::new(
        "ZeroDivisionError",
        "integer division or modulo by zero",
    ))
}

/// CPython's message for a zero base raised to a negative power
/// (`0 ** -1`, `0.0 ** -2.0`).
pub fn zero_division_negative_power() -> Unwind {
    Unwind::Exception(VmException::new(
        "ZeroDivisionError",
        "0.0 cannot be raised to a negative power",
    ))
}
pub fn not_implemented(feature: &str) -> Unwind {
    Unwind::Exception(VmException::new(
        "NotImplementedError",
        format!("tyc-vm v1 does not yet support: {feature}"),
    ))
}

/// Error for features that the tree-walking VM can't run yet but that
/// the compile-to-Python path handles. Surfaces the documented
/// `tyc build && python build/main.py` workaround so users aren't
/// stuck guessing why `tyc run` fails on otherwise-valid programs
/// (FINDINGS #28, #29).
pub fn vm_unsupported_use_compile(feature: &str) -> Unwind {
    Unwind::Exception(VmException::new(
        "NotImplementedError",
        format!(
            "{feature} is not yet supported in the tree-walking VM; \
             use `tyc build` then `python build/main.py` to run this program"
        ),
    ))
}
pub fn stop_iteration() -> Unwind {
    Unwind::Exception(VmException::new("StopIteration", ""))
}
