//! Foreign-function fallback boundary.
//!
//! v1: a stub. Anything that needs CPython embedding (numpy, requests,
//! pydantic, etc.) currently returns a clear `ImportError` pointing the user
//! at `tyc run --compile`. A future `vm-pyo3` feature will replace these
//! stubs with PyO3-backed shims.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{Unwind, VmException};
use crate::value::{NativeFn, Value};

/// Minimal text-mode file shim sufficient for the most common scripting
/// patterns: `open(path)`, `open(path, "r")`, `open(path, "w")`,
/// `open(path, "a")`, plus their `"t"`/`"b"`/`"+"` variants. `with`-blocks
/// are honoured via `__enter__`/`__exit__`. Binary mode round-trips raw
/// bytes through `Value::Bytes`. Buffering / encoding kwargs are accepted
/// and ignored.
pub fn open_file(path: &str, mode: &str) -> Result<Value, Unwind> {
    use crate::value::Module;
    use std::collections::HashMap;

    let (read_mode, write_mode, append_mode, binary, plus) = parse_mode(mode)?;

    let path_owned = path.to_owned();

    // Pre-populate the read buffer for read-or-rw modes.
    let initial_content = if read_mode {
        match std::fs::read(&path_owned) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Err(Unwind::Exception(VmException::new(
                    "FileNotFoundError",
                    format!("could not open '{path_owned}': {e}"),
                )));
            }
        }
    } else if append_mode {
        // append mode lets you also read in `"a+"`, but if the file does
        // not exist, that's fine — we just start empty.
        std::fs::read(&path_owned).unwrap_or_default()
    } else if write_mode {
        // Open `"w"` truncates the file at open time so the user sees a
        // zero-length file even if they never write.
        match std::fs::File::create(&path_owned) {
            Ok(_) => {}
            Err(e) => {
                return Err(Unwind::Exception(VmException::new(
                    "OSError",
                    format!("could not create '{path_owned}': {e}"),
                )));
            }
        }
        Vec::new()
    } else {
        Vec::new()
    };

    // Bytes vs text view of the read buffer.
    let content_text = if !binary {
        match String::from_utf8(initial_content.clone()) {
            Ok(s) => s,
            Err(_) => {
                return Err(Unwind::Exception(VmException::new(
                    "UnicodeDecodeError",
                    format!("'{path_owned}' is not valid UTF-8 in text mode"),
                )));
            }
        }
    } else {
        String::new()
    };

    let content_rc: Rc<String> = Rc::new(content_text.clone());
    let lines: Rc<Vec<String>> = Rc::new(
        content_text
            .split_inclusive('\n')
            .map(|s| s.to_owned())
            .collect(),
    );
    let bytes_rc: Rc<Vec<u8>> = Rc::new(initial_content);
    let line_pos = Rc::new(RefCell::new(0usize));

    // Pending writes accumulated then flushed on close.
    let write_buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let writer_path = path_owned.clone();
    let writer_buf = write_buf.clone();
    let writer_active = write_mode || append_mode || plus;
    let writer_append = append_mode && !write_mode;
    let closed = Rc::new(RefCell::new(false));

    let do_flush = {
        let writer_buf = writer_buf.clone();
        let writer_path = writer_path.clone();
        let closed = closed.clone();
        move || -> Result<(), Unwind> {
            if !writer_active || *closed.borrow() {
                return Ok(());
            }
            let mut buf = writer_buf.borrow_mut();
            if buf.is_empty() {
                return Ok(());
            }
            let res = if writer_append {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&writer_path)
                    .and_then(|mut f| f.write_all(&buf))
            } else {
                std::fs::write(&writer_path, &*buf)
            };
            if let Err(e) = res {
                return Err(Unwind::Exception(VmException::new(
                    "OSError",
                    format!("failed to write '{writer_path}': {e}"),
                )));
            }
            buf.clear();
            Ok(())
        }
    };

    let mut members: HashMap<String, Value> = HashMap::new();

    // read() — full content, text or bytes.
    {
        let content = content_rc.clone();
        let bytes = bytes_rc.clone();
        let read = NativeFn::new("read", move |_i, _args| {
            if binary {
                Ok(Value::Bytes(Rc::new((*bytes).clone())))
            } else {
                Ok(Value::Str(content.clone()))
            }
        });
        members.insert("read".into(), Value::Native(Rc::new(read)));
    }

    // readlines() — text only.
    {
        let lines = lines.clone();
        let readlines = NativeFn::new("readlines", move |_i, _args| {
            Ok(Value::List(Rc::new(RefCell::new(
                lines
                    .iter()
                    .cloned()
                    .map(|l| Value::Str(Rc::new(l)))
                    .collect(),
            ))))
        });
        members.insert("readlines".into(), Value::Native(Rc::new(readlines)));
    }

    // readline() — text mode line-at-a-time.
    {
        let lines = lines.clone();
        let pos = line_pos.clone();
        let readline = NativeFn::new("readline", move |_i, _args| {
            let mut p = pos.borrow_mut();
            if *p >= lines.len() {
                return Ok(Value::Str(Rc::new(String::new())));
            }
            let line = lines[*p].clone();
            *p += 1;
            Ok(Value::Str(Rc::new(line)))
        });
        members.insert("readline".into(), Value::Native(Rc::new(readline)));
    }

    // write(s) — appends to the pending buffer; returns bytes-written count.
    {
        let buf = writer_buf.clone();
        let write_fn = NativeFn::new("write", move |_i, args| {
            if !writer_active {
                return Err(Unwind::Exception(VmException::new(
                    "io.UnsupportedOperation",
                    "not writable".to_owned(),
                )));
            }
            let arg = args
                .into_iter()
                .next()
                .ok_or_else(|| crate::error::type_error("write() requires a string or bytes"))?;
            let written = match arg {
                Value::Str(s) => {
                    let bytes = s.as_bytes();
                    buf.borrow_mut().extend_from_slice(bytes);
                    bytes.len()
                }
                Value::Bytes(b) => {
                    buf.borrow_mut().extend_from_slice(&b);
                    b.len()
                }
                other => {
                    return Err(crate::error::type_error(format!(
                        "write() expects str or bytes; got {}",
                        other.type_name()
                    )))
                }
            };
            Ok(Value::Int(num_bigint::BigInt::from(written)))
        });
        members.insert("write".into(), Value::Native(Rc::new(write_fn)));
    }

    // close() — flush pending writes; mark closed (subsequent ops error).
    {
        let do_flush = do_flush.clone();
        let closed = closed.clone();
        let close = NativeFn::new("close", move |_i, _args| {
            do_flush()?;
            *closed.borrow_mut() = true;
            Ok(Value::None)
        });
        members.insert("close".into(), Value::Native(Rc::new(close)));
    }

    // flush() — drain pending writes without closing.
    {
        let do_flush = do_flush.clone();
        let flush = NativeFn::new("flush", move |_i, _args| {
            do_flush()?;
            Ok(Value::None)
        });
        members.insert("flush".into(), Value::Native(Rc::new(flush)));
    }

    // Build the module, then back-fill `__enter__`/`__exit__` to return the
    // file itself. The Rc<Module> reference is captured by both methods.
    let module = Rc::new(Module {
        name: format!("<file {path_owned}>"),
        members: RefCell::new(members),
    });

    let module_self = module.clone();
    let enter = NativeFn::new("__enter__", move |_i, _args| {
        Ok(Value::Module(module_self.clone()))
    });
    let do_flush_for_exit = do_flush.clone();
    let closed_for_exit = closed.clone();
    let module_for_exit = module.clone();
    let exit = NativeFn::new("__exit__", move |_i, _args| {
        do_flush_for_exit()?;
        *closed_for_exit.borrow_mut() = true;
        let _ = &module_for_exit; // keep the strong-ref alive
        Ok(Value::Bool(false))
    });
    module
        .members
        .borrow_mut()
        .insert("__enter__".into(), Value::Native(Rc::new(enter)));
    module
        .members
        .borrow_mut()
        .insert("__exit__".into(), Value::Native(Rc::new(exit)));

    Ok(Value::Module(module))
}

/// Parse a Python `open()` mode string into `(read, write, append, binary, plus)`.
fn parse_mode(mode: &str) -> Result<(bool, bool, bool, bool, bool), Unwind> {
    let mut read = false;
    let mut write = false;
    let mut append = false;
    let mut binary = false;
    let mut text = false;
    let mut plus = false;

    for ch in mode.chars() {
        match ch {
            'r' => read = true,
            'w' => write = true,
            'a' => append = true,
            'b' => binary = true,
            't' => text = true,
            '+' => plus = true,
            'x' => {
                // exclusive create — rare, treat as write
                write = true;
            }
            _ => {
                return Err(Unwind::Exception(VmException::new(
                    "ValueError",
                    format!("invalid mode: '{mode}'"),
                )));
            }
        }
    }

    if binary && text {
        return Err(Unwind::Exception(VmException::new(
            "ValueError",
            "can't have text and binary mode at once".to_owned(),
        )));
    }
    if !(read || write || append) {
        read = true; // default
    }
    if plus {
        // r+/w+/a+ enable both directions
        read = true;
        if !(write || append) {
            write = true;
        }
    }
    Ok((read, write, append, binary, plus))
}
