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

pub fn open_file(path: &str, mode: &str) -> Result<Value, Unwind> {
    // Minimal "text read" support — `open(path)` and `open(path, "r")` only.
    // Anything else hits the unsupported-feature error.
    if !matches!(mode, "r" | "rt" | "") {
        return Err(Unwind::Exception(VmException::new(
            "NotImplementedError",
            format!("tyc-vm v1 only supports open(path, 'r'); got mode '{mode}'"),
        )));
    }
    let content = std::fs::read_to_string(path).map_err(|e| {
        Unwind::Exception(VmException::new(
            "FileNotFoundError",
            format!("could not open '{path}': {e}"),
        ))
    })?;
    let pos = Rc::new(RefCell::new(0usize));
    let lines: Rc<Vec<String>> = Rc::new(content.lines().map(|s| format!("{s}\n")).collect());

    // Build a small "file" object with read/readlines/close/__enter__/__exit__/__iter__/__next__.
    use crate::value::Module;
    use std::collections::HashMap;
    let mut members: HashMap<String, Value> = HashMap::new();

    let content_rc = Rc::new(content);
    let read = {
        let content = content_rc.clone();
        NativeFn::new("read", move |_i, _args| Ok(Value::Str(content.clone())))
    };

    let readlines = {
        let lines = lines.clone();
        NativeFn::new("readlines", move |_i, _args| {
            Ok(Value::List(Rc::new(RefCell::new(
                lines
                    .iter()
                    .cloned()
                    .map(|l| Value::Str(Rc::new(l)))
                    .collect(),
            ))))
        })
    };

    let readline = {
        let lines = lines.clone();
        let pos = pos.clone();
        NativeFn::new("readline", move |_i, _args| {
            let mut p = pos.borrow_mut();
            if *p >= lines.len() {
                return Ok(Value::Str(Rc::new(String::new())));
            }
            let line = lines[*p].clone();
            *p += 1;
            Ok(Value::Str(Rc::new(line)))
        })
    };

    let close = NativeFn::new("close", |_i, _args| Ok(Value::None));

    members.insert("read".into(), Value::Native(Rc::new(read)));
    members.insert("readlines".into(), Value::Native(Rc::new(readlines)));
    members.insert("readline".into(), Value::Native(Rc::new(readline)));
    members.insert("close".into(), Value::Native(Rc::new(close)));
    // The v1 file shim deliberately omits `__enter__`/`__exit__`. Returning
    // a self-reference from `__enter__` would need a back-fill pattern this
    // crate doesn't have today. Until then `with open(p) as f:` raises an
    // `AttributeError` and users should call `f = open(p); f.read(); f.close()`.

    Ok(Value::Module(Rc::new(Module {
        name: format!("<file {path}>"),
        members: RefCell::new(members),
    })))
}
