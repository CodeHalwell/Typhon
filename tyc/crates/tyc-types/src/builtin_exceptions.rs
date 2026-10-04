//! The public attribute surface of CPython's builtin exception classes.
//!
//! A user class whose only unseen base is a builtin exception
//! (`class NotFound(Exception)`) has a fully known attribute set: its own
//! declarations plus the builtin's fixed table. Without this table the
//! checker treated `Exception` like any foreign base — one that may supply
//! any attribute — so `except (A, B) as e: e.code` passed when only `A`
//! defines `code`, and raised `AttributeError` whenever a `B` was caught.
//!
//! The table is `dir(cls)` minus underscore names, from CPython 3.13, plus
//! `winerror` on the `OSError` family (Windows only). It is a superset by
//! design: listing an attribute a class lacks only keeps the checker
//! permissive.

/// Every builtin exception's public attributes (`BaseException`'s own).
const BASE: &[&str] = &["add_note", "args", "with_traceback"];

const OS_ERROR: &[&str] = &[
    "characters_written",
    "errno",
    "filename",
    "filename2",
    "strerror",
    "winerror",
];
const SYNTAX_ERROR: &[&str] = &[
    "end_lineno",
    "end_offset",
    "filename",
    "lineno",
    "msg",
    "offset",
    "print_file_and_line",
    "text",
];
const IMPORT_ERROR: &[&str] = &["msg", "name", "name_from", "path"];
const UNICODE_ERROR: &[&str] = &["encoding", "end", "object", "reason", "start"];
const EXCEPTION_GROUP: &[&str] = &["derive", "exceptions", "message", "split", "subgroup"];

/// The public attributes `name` adds to [`BASE`], or `None` when `name` is
/// not a builtin exception class.
fn extra_attrs(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "OSError"
        | "EnvironmentError"
        | "IOError"
        | "BlockingIOError"
        | "BrokenPipeError"
        | "ChildProcessError"
        | "ConnectionAbortedError"
        | "ConnectionError"
        | "ConnectionRefusedError"
        | "ConnectionResetError"
        | "FileExistsError"
        | "FileNotFoundError"
        | "InterruptedError"
        | "IsADirectoryError"
        | "NotADirectoryError"
        | "PermissionError"
        | "ProcessLookupError"
        | "TimeoutError" => OS_ERROR,
        "SyntaxError" | "IndentationError" | "TabError" => SYNTAX_ERROR,
        "ImportError" | "ModuleNotFoundError" => IMPORT_ERROR,
        "UnicodeDecodeError" | "UnicodeEncodeError" | "UnicodeTranslateError" => UNICODE_ERROR,
        "BaseExceptionGroup" | "ExceptionGroup" => EXCEPTION_GROUP,
        "AttributeError" => &["name", "obj"],
        "NameError" | "UnboundLocalError" => &["name"],
        "StopIteration" => &["value"],
        "SystemExit" => &["code"],
        "ArithmeticError"
        | "AssertionError"
        | "BaseException"
        | "BufferError"
        | "BytesWarning"
        | "DeprecationWarning"
        | "EOFError"
        | "EncodingWarning"
        | "Exception"
        | "FloatingPointError"
        | "FutureWarning"
        | "GeneratorExit"
        | "ImportWarning"
        | "IndexError"
        | "KeyError"
        | "KeyboardInterrupt"
        | "LookupError"
        | "MemoryError"
        | "NotImplementedError"
        | "OverflowError"
        | "PendingDeprecationWarning"
        | "PythonFinalizationError"
        | "RecursionError"
        | "ReferenceError"
        | "ResourceWarning"
        | "RuntimeError"
        | "RuntimeWarning"
        | "StopAsyncIteration"
        | "SyntaxWarning"
        | "SystemError"
        | "TypeError"
        | "UnicodeError"
        | "UnicodeWarning"
        | "UserWarning"
        | "ValueError"
        | "Warning"
        | "ZeroDivisionError" => &[],
        _ => return None,
    })
}

/// Whether `name` is a builtin exception class.
pub(crate) fn is_builtin_exception(name: &str) -> bool {
    extra_attrs(name).is_some()
}

/// Whether instances of builtin exception `name` have the public attribute
/// `attr`. `false` for a name that is not a builtin exception.
pub(crate) fn has_attr(name: &str, attr: &str) -> bool {
    extra_attrs(name).is_some_and(|extra| BASE.contains(&attr) || extra.contains(&attr))
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_covers_the_base_and_per_class_attributes() {
        assert!(super::has_attr("Exception", "args"));
        assert!(super::has_attr("SystemExit", "code"));
        assert!(super::has_attr("FileNotFoundError", "errno"));
        assert!(!super::has_attr("Exception", "code"));
        assert!(!super::has_attr("ValueError", "errno"));
        assert!(!super::is_builtin_exception("Thing"));
    }
}
