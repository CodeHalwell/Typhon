//! Native builtins and the small stdlib the VM understands directly.
//!
//! Everything here is implemented in Rust rather than dispatched to CPython.
//! Modules that aren't supported produce a clear `ImportError`-style message.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::error::{attribute_error, index_error, key_error, type_error, value_error, Unwind};
use crate::interp::{normalize_index, Interpreter};
use crate::value::{DictMap, HashKey, IterState, Module, NativeFn, Value, VmInt};

/// Write `text` to stdout, tolerating a broken pipe.
///
/// A plain `print!`/`println!` panics ("failed printing to stdout: Broken
/// pipe") when the downstream consumer of a pipe exits early — e.g.
/// `tyc run app | head`. CPython instead terminates cleanly, so on
/// `BrokenPipe` we exit the process with status 0 rather than unwinding with a
/// Rust panic. Other write errors are ignored (best-effort, as the previous
/// code was via `.ok()`).
fn vm_write_stdout(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    match out.write_all(text.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(_) => {}
    }
}

/// Round a float to the nearest integer value using round-half-to-even
/// (banker's rounding), matching CPython's `round()`.
fn round_half_even(x: f64) -> f64 {
    let floor = x.floor();
    let diff = x - floor;
    if diff < 0.5 {
        floor
    } else if diff > 0.5 {
        floor + 1.0
    } else {
        // Exactly halfway: round to the even neighbour.
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    }
}

/// `str.split(sep, maxsplit)` with an explicit separator. `maxsplit < 0`
/// means unlimited. `from_right` selects `rsplit` behaviour.
fn split_with_sep(s: &str, sep: &str, maxsplit: i64, from_right: bool) -> Vec<String> {
    if maxsplit < 0 {
        let mut v: Vec<String> = s.split(sep).map(|p| p.to_owned()).collect();
        if from_right {
            // splitn-from-right unlimited is the same set of pieces.
            let _ = &mut v;
        }
        return v;
    }
    let limit = (maxsplit as usize) + 1;
    if from_right {
        let mut parts: Vec<String> = s.rsplitn(limit, sep).map(|p| p.to_owned()).collect();
        parts.reverse();
        parts
    } else {
        s.splitn(limit, sep).map(|p| p.to_owned()).collect()
    }
}

/// `str.splitlines([keepends])` — split on CPython's full line-boundary set,
/// not Rust's `str::lines()` (which recognises only `\n` / `\r\n`). CPython
/// breaks on `\n \r \r\n \v \f \x1c \x1d \x1e \x85    `; with
/// `keepends=True` each terminator stays attached to its line.
fn py_splitlines(s: &str, keepends: bool) -> Vec<String> {
    fn is_line_boundary(c: char) -> bool {
        matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        )
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if is_line_boundary(c) {
            if keepends {
                cur.push(c);
                // `\r\n` is a single boundary.
                if c == '\r' && chars.peek() == Some(&'\n') {
                    cur.push(chars.next().unwrap());
                }
            } else if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    // A trailing non-empty segment with no final boundary is its own line;
    // CPython does NOT emit a trailing empty string after a final boundary.
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `str.split()` / `str.split(None, maxsplit)` — whitespace split that
/// collapses runs of whitespace and drops leading/trailing empties.
fn split_whitespace_max(s: &str, maxsplit: i64, from_right: bool) -> Vec<String> {
    if maxsplit < 0 {
        return s.split_whitespace().map(|p| p.to_owned()).collect();
    }
    let max = maxsplit as usize;
    if from_right {
        // Collect words with their byte ranges, then split from the right.
        let words: Vec<(usize, usize)> = {
            let mut v = Vec::new();
            let mut start: Option<usize> = None;
            for (i, c) in s.char_indices() {
                if c.is_whitespace() {
                    if let Some(st) = start.take() {
                        v.push((st, i));
                    }
                } else if start.is_none() {
                    start = Some(i);
                }
            }
            if let Some(st) = start {
                v.push((st, s.len()));
            }
            v
        };
        if words.len() <= max {
            return words.iter().map(|(a, b)| s[*a..*b].to_owned()).collect();
        }
        // Keep the last `max` words as separate pieces; everything before
        // them stays joined (interior whitespace preserved) as the first
        // piece.
        let split_point = words.len() - max;
        let head_start = words[0].0;
        let head_end = words[split_point - 1].1;
        let mut out = vec![s[head_start..head_end].to_owned()];
        for (a, b) in &words[split_point..] {
            out.push(s[*a..*b].to_owned());
        }
        out
    } else {
        let mut out: Vec<String> = Vec::new();
        let mut chars = s.char_indices().peekable();
        let bytes = s;
        loop {
            // Skip leading whitespace.
            while let Some(&(_, c)) = chars.peek() {
                if c.is_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
            let Some(&(start, _)) = chars.peek() else {
                break;
            };
            if out.len() == max {
                // Remaining text (stripped of trailing whitespace) is the
                // final piece, with interior whitespace preserved.
                out.push(bytes[start..].trim_end().to_owned());
                break;
            }
            // Consume the word.
            let mut end = bytes.len();
            while let Some(&(i, c)) = chars.peek() {
                if c.is_whitespace() {
                    end = i;
                    break;
                }
                chars.next();
            }
            out.push(bytes[start..end].to_owned());
        }
        out
    }
}

pub fn install(interp: &mut Interpreter) {
    let root = interp.root.clone();

    macro_rules! native {
        ($name:literal, $body:expr) => {
            root.set($name, Value::Native(Rc::new(NativeFn::new($name, $body))));
        };
    }

    native!("print", |interp, args| {
        let mut out = String::new();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&interp.str_of(a)?);
        }
        out.push('\n');
        if let Some(sink) = redirected_std_stream(interp, false) {
            let write = interp.get_attr(&sink, "write")?;
            interp.call_value(write, vec![Value::Str(Rc::new(out))], &[])?;
            return Ok(Value::None);
        }
        vm_write_stdout(&out);
        Ok(Value::None)
    });

    native!("len", |interp, args| {
        let v = single(&args, "len")?;
        if let Value::Instance(_) = v {
            if let Some(r) = interp.call_dunder0(v, "__len__")? {
                // `__len__` must return a non-negative int (CPython raises
                // TypeError for non-int, ValueError for negative).
                let n = match r {
                    Value::Int(i) => i,
                    Value::Bool(b) => VmInt::from(b as i64),
                    other => {
                        return Err(type_error(format!(
                            "'{}' object cannot be interpreted as an integer",
                            other.type_name()
                        )))
                    }
                };
                if n.is_negative() {
                    return Err(value_error("__len__() should return >= 0"));
                }
                return Ok(Value::Int(n));
            }
        }
        Ok(Value::Int(VmInt::from(value_len(v)? as i64)))
    });

    native!("range", |_i, args| match args.len() {
        1 => Ok(Value::Range {
            start: 0,
            stop: args[0].to_int()?,
            step: 1,
        }),
        2 => Ok(Value::Range {
            start: args[0].to_int()?,
            stop: args[1].to_int()?,
            step: 1,
        }),
        3 => {
            let step = args[2].to_int()?;
            if step == 0 {
                return Err(value_error("range() arg 3 must not be zero"));
            }
            Ok(Value::Range {
                start: args[0].to_int()?,
                stop: args[1].to_int()?,
                step,
            })
        }
        _ => Err(type_error("range() expected 1–3 arguments")),
    });

    native!("str", |interp, args| {
        let (pos, kw) = split_kwargs(&args);
        // `str(bytes, encoding[, errors])` decodes.
        if let Some(Value::Bytes(b)) = pos.first() {
            let encoding = pos.get(1).cloned().or_else(|| {
                kw.iter()
                    .find(|(k, _)| k == "encoding")
                    .map(|(_, v)| v.clone())
            });
            if let Some(enc) = encoding {
                let errors = pos
                    .get(2)
                    .cloned()
                    .or_else(|| {
                        kw.iter()
                            .find(|(k, _)| k == "errors")
                            .map(|(_, v)| v.clone())
                    })
                    .map(|v| v.py_str())
                    .unwrap_or_else(|| "strict".to_owned());
                return Ok(Value::Str(Rc::new(crate::codecs::decode(
                    b,
                    &enc.py_str(),
                    &errors,
                )?)));
            }
        }
        Ok(Value::Str(Rc::new(match pos.first() {
            Some(v) => interp.str_of(v)?,
            None => String::new(),
        })))
    });

    native!("int", |i, args| {
        // `int()` with no argument is 0 (matches CPython; used by
        // `defaultdict(int)` as a zero-factory).
        if args.is_empty() {
            return Ok(Value::Int(VmInt::from(0)));
        }
        let v = single(&args, "int")?;
        // A user `__int__` (then `__index__`) — CPython's conversion
        // protocol, and how `int()` reaches a `lazy let` proxy.
        // `int(bytearray(b"7"))` — a bytes-like converts like `bytes`.
        let bytes_like;
        let v = match bytearray_bytes(v) {
            Some(raw) => {
                bytes_like = Value::Bytes(Rc::new(raw));
                &bytes_like
            }
            None => v,
        };
        if let Value::Instance(_) = v {
            let v = v.clone();
            for dunder in ["__int__", "__index__"] {
                if let Some(r) = i.call_dunder0(&v, dunder)? {
                    return Ok(r);
                }
            }
        }
        // A value-mixin enum member (`IntEnum` / `IntFlag` / `StrEnum`) IS
        // its value in CPython, so `int(Colour.RED)` is `1`.
        let unwrapped;
        let v = match crate::value::enum_mixin_value(v) {
            Some(inner) => {
                unwrapped = inner;
                &unwrapped
            }
            None => v,
        };
        // `int(str, base)` — parse a string in the given radix.
        if let (Value::Str(s), Some(base_v)) = (v, args.get(1)) {
            // `base` must be an integer (Python rejects float/str bases).
            if !matches!(base_v, Value::Int(_) | Value::Bool(_)) {
                return Err(type_error("int() base must be an integer"));
            }
            let mut base = base_v.to_int()?;
            if base != 0 && !(2..=36).contains(&base) {
                return Err(value_error("int() base must be >= 2 and <= 36, or 0"));
            }
            let trimmed = s.trim();
            let (neg, digits) = match trimmed.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
            };
            // `base == 0` autodetects the radix from a 0x/0o/0b prefix
            // (defaulting to 10); for an explicit base, tolerate the matching
            // conventional prefix.
            let digits = if base == 0 {
                if let Some(rest) = digits
                    .strip_prefix("0x")
                    .or_else(|| digits.strip_prefix("0X"))
                {
                    base = 16;
                    rest
                } else if let Some(rest) = digits
                    .strip_prefix("0o")
                    .or_else(|| digits.strip_prefix("0O"))
                {
                    base = 8;
                    rest
                } else if let Some(rest) = digits
                    .strip_prefix("0b")
                    .or_else(|| digits.strip_prefix("0B"))
                {
                    base = 2;
                    rest
                } else {
                    base = 10;
                    digits
                }
            } else {
                match base {
                    16 => digits
                        .strip_prefix("0x")
                        .or_else(|| digits.strip_prefix("0X"))
                        .unwrap_or(digits),
                    8 => digits
                        .strip_prefix("0o")
                        .or_else(|| digits.strip_prefix("0O"))
                        .unwrap_or(digits),
                    2 => digits
                        .strip_prefix("0b")
                        .or_else(|| digits.strip_prefix("0B"))
                        .unwrap_or(digits),
                    _ => digits,
                }
            };
            let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
            return match num_bigint::BigInt::parse_bytes(cleaned.as_bytes(), base as u32) {
                Some(n) => Ok(Value::Int(VmInt::from(if neg { -n } else { n }))),
                None => Err(value_error(format!(
                    "invalid literal for int() with base {}: '{}'",
                    base, s
                ))),
            };
        }
        Ok(Value::Int(VmInt::from(v.to_bigint()?)))
    });

    native!("divmod", |interp, args| {
        let a = args
            .first()
            .ok_or_else(|| type_error("divmod expected 2 arguments"))?;
        let b = args
            .get(1)
            .ok_or_else(|| type_error("divmod expected 2 arguments"))?;
        // A user `__divmod__` (or reflected `__rdivmod__`) wins.
        if let Value::Instance(inst) = a {
            if let Some(m) = interp.find_method(&inst.class, "__divmod__") {
                return interp.call_value(
                    Value::BoundMethod {
                        receiver: Box::new(a.clone()),
                        function: m,
                    },
                    vec![b.clone()],
                    &[],
                );
            }
        }
        if let Value::Instance(inst) = b {
            if let Some(m) = interp.find_method(&inst.class, "__rdivmod__") {
                return interp.call_value(
                    Value::BoundMethod {
                        receiver: Box::new(b.clone()),
                        function: m,
                    },
                    vec![a.clone()],
                    &[],
                );
            }
        }
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => {
                if y.is_zero() {
                    return Err(Unwind::Exception(crate::error::VmException::new(
                        "ZeroDivisionError",
                        "integer division or modulo by zero",
                    )));
                }
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Int(x.div_floor(y)),
                    Value::Int(x.mod_floor(y)),
                ])))
            }
            _ => {
                let xf = a.to_float()?;
                let yf = b.to_float()?;
                if yf == 0.0 {
                    return Err(Unwind::Exception(crate::error::VmException::new(
                        "ZeroDivisionError",
                        "float divmod()",
                    )));
                }
                // Share CPython's `float_divmod`; `(xf / yf).floor()` rounds
                // the intermediate quotient and gives the wrong pair (e.g.
                // `divmod(7.0, 0.1)` → `(70.0, …)` instead of `(69.0, …)`).
                let (q, r) = crate::interp::float_divmod(xf, yf);
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Float(q),
                    Value::Float(r),
                ])))
            }
        }
    });

    native!("pow", |interp, args| {
        let a = args
            .first()
            .ok_or_else(|| type_error("pow expected at least 2 arguments"))?;
        let b = args
            .get(1)
            .ok_or_else(|| type_error("pow expected at least 2 arguments"))?;
        // 3-arg form: modular exponentiation (ints only).
        if let Some(m) = args.get(2) {
            if let (Value::Int(base), Value::Int(exp), Value::Int(modv)) = (a, b, m) {
                if modv.is_zero() {
                    return Err(value_error("pow() 3rd argument cannot be 0"));
                }
                if exp.is_negative() {
                    // Python 3.8+: a negative exponent raises the modular
                    // *inverse* of the base to `-exp`, and reports a base
                    // with no inverse rather than refusing outright.
                    let inverse = modular_inverse(&base.as_bigint(), &modv.as_bigint())
                        .ok_or_else(|| {
                            value_error("base is not invertible for the given modulus")
                        })?;
                    let positive = exp.abs();
                    return Ok(Value::Int(
                        VmInt::from_bigint(inverse).modpow(&positive, modv),
                    ));
                }
                return Ok(Value::Int(base.modpow(exp, modv)));
            }
            return Err(type_error(
                "pow() 3rd argument not allowed unless all arguments are integers",
            ));
        }
        interp.binop(a, ruff_python_ast::Operator::Pow, b)
    });

    native!("format", |interp, args| {
        let v = args
            .first()
            .ok_or_else(|| type_error("format expected at least 1 argument"))?;
        // The format spec must be a `str` — CPython rejects a non-string spec
        // (`format(obj, 123)`) with `TypeError` before calling `__format__`.
        let spec = match args.get(1) {
            None => String::new(),
            Some(Value::Str(s)) => (**s).clone(),
            Some(other) => {
                return Err(type_error(format!(
                    "format() argument 2 must be str, not {}",
                    other.type_name()
                )))
            }
        };
        // A user `__format__(self, spec)` controls its own formatting.
        if let Some(formatted) = interp.try_user_format(v, &spec)? {
            return Ok(Value::Str(Rc::new(formatted)));
        }
        let base = interp.str_of(v)?;
        Ok(Value::Str(Rc::new(crate::interp::format_with_spec_pub(
            v, &base, &spec,
        )?)))
    });

    native!("ascii", |interp, args| {
        let v = single(&args, "ascii")?;
        let r = interp.repr_of(v)?;
        // Escape any non-ASCII characters as \xNN / \uNNNN / \UNNNNNNNN.
        let mut out = String::with_capacity(r.len());
        for c in r.chars() {
            if c.is_ascii() {
                out.push(c);
            } else {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{:02x}", n));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{:04x}", n));
                } else {
                    out.push_str(&format!("\\U{:08x}", n));
                }
            }
        }
        Ok(Value::Str(Rc::new(out)))
    });

    native!("float", |i, args| {
        let v = single(&args, "float")?;
        // A user `__float__` (then `__index__`), as CPython's conversion
        // protocol prescribes.
        // `int(bytearray(b"7"))` — a bytes-like converts like `bytes`.
        let bytes_like;
        let v = match bytearray_bytes(v) {
            Some(raw) => {
                bytes_like = Value::Bytes(Rc::new(raw));
                &bytes_like
            }
            None => v,
        };
        if let Value::Instance(_) = v {
            let v = v.clone();
            for dunder in ["__float__", "__index__"] {
                if let Some(r) = i.call_dunder0(&v, dunder)? {
                    return Ok(Value::Float(r.to_float()?));
                }
            }
        }
        Ok(Value::Float(v.to_float()?))
    });

    native!("bool", |i, args| Ok(Value::Bool(match args.first() {
        Some(v) => i.is_truthy(v)?,
        None => false,
    })));

    native!("getattr", |i, args| {
        let obj = args
            .first()
            .ok_or_else(|| type_error("getattr() requires arguments"))?
            .clone();
        let name = args
            .get(1)
            .ok_or_else(|| type_error("getattr() requires a name"))?
            .py_str();
        match i.get_attr(&obj, &name) {
            Ok(v) => Ok(v),
            // Only a *missing* attribute falls back to the default; a real
            // error from a property/__getattr__ propagates (review: codex).
            Err(e) => match args.get(2) {
                Some(default) if is_attribute_error(&e) => Ok(default.clone()),
                _ => Err(e),
            },
        }
    });
    native!("hasattr", |i, args| {
        let obj = args
            .first()
            .ok_or_else(|| type_error("hasattr() requires arguments"))?
            .clone();
        let name = args
            .get(1)
            .ok_or_else(|| type_error("hasattr() requires a name"))?
            .py_str();
        // True if present, False only for a genuine AttributeError; any other
        // exception from a descriptor/__getattr__ propagates (review: codex).
        match i.get_attr(&obj, &name) {
            Ok(_) => Ok(Value::Bool(true)),
            Err(e) if is_attribute_error(&e) => Ok(Value::Bool(false)),
            Err(e) => Err(e),
        }
    });
    native!("setattr", |i, args| {
        let obj = args
            .first()
            .ok_or_else(|| type_error("setattr() requires arguments"))?
            .clone();
        let name = args
            .get(1)
            .ok_or_else(|| type_error("setattr() requires a name"))?
            .py_str();
        let val = args
            .get(2)
            .ok_or_else(|| type_error("setattr() requires a value"))?
            .clone();
        i.set_attr(&obj, &name, val)?;
        Ok(Value::None)
    });
    native!("delattr", |_i, args| {
        let obj = args
            .first()
            .ok_or_else(|| type_error("delattr() requires arguments"))?
            .clone();
        let name = args
            .get(1)
            .ok_or_else(|| type_error("delattr() requires a name"))?
            .py_str();
        match &obj {
            Value::Instance(inst) => {
                if let Some(err) =
                    crate::interp::frozen_dataclass_error(&inst.class, &name, "delete")
                {
                    return Err(err);
                }
                if inst
                    .fields
                    .borrow_mut()
                    .shift_remove(name.as_str())
                    .is_none()
                {
                    return Err(attribute_error(format!(
                        "'{}' object has no attribute '{}'",
                        inst.class.name, name
                    )));
                }
                Ok(Value::None)
            }
            _ => Err(type_error(
                "delattr() target does not support attribute deletion",
            )),
        }
    });
    native!("dir", |_i, args| {
        fn internal(k: &str) -> bool {
            matches!(k, "__typhon_enum_base__" | "__typhon_enum_members__")
                || k.starts_with("__typhon_setter__")
        }
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        match args.first() {
            Some(Value::Instance(inst)) => {
                for k in inst.fields.borrow().keys() {
                    names.insert(k.clone());
                }
                for k in inst.class.methods.borrow().keys() {
                    names.insert(k.clone());
                }
                for k in inst.class.class_attrs.borrow().keys() {
                    if !internal(k) {
                        names.insert(k.clone());
                    }
                }
            }
            Some(Value::Module(m)) => {
                for k in m.members.borrow().keys() {
                    names.insert(k.clone());
                }
            }
            Some(Value::Class(c)) => {
                for k in c.methods.borrow().keys() {
                    names.insert(k.clone());
                }
                for k in c.class_attrs.borrow().keys() {
                    if !internal(k) {
                        names.insert(k.clone());
                    }
                }
            }
            _ => {}
        }
        Ok(Value::List(Rc::new(RefCell::new(
            names.into_iter().map(|s| Value::Str(Rc::new(s))).collect(),
        ))))
    });
    native!("vars", |_i, args| {
        match args.first() {
            Some(Value::Instance(inst)) => {
                let mut m: DictMap = IndexMap::new();
                for (k, v) in inst.fields.borrow().iter() {
                    m.insert(HashKey::Str(Rc::new(k.clone())), v.clone());
                }
                Ok(Value::Dict(Rc::new(RefCell::new(m))))
            }
            // `vars(module)` returns the module namespace (review: gemini).
            Some(Value::Module(md)) => {
                let mut m: DictMap = IndexMap::new();
                for (k, v) in md.members.borrow().iter() {
                    m.insert(HashKey::Str(Rc::new(k.clone())), v.clone());
                }
                Ok(Value::Dict(Rc::new(RefCell::new(m))))
            }
            Some(other) => Err(type_error(format!(
                "vars() argument must have __dict__, not '{}'",
                other.type_name()
            ))),
            None => Err(type_error(
                "vars() with no argument is unsupported in the VM",
            )),
        }
    });
    native!("list", |i, args| {
        let mut out = Vec::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(x) = i.iter_next(&it)? {
                out.push(x);
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });

    native!("tuple", |i, args| {
        let mut out = Vec::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(x) = i.iter_next(&it)? {
                out.push(x);
            }
        }
        Ok(Value::Tuple(Rc::new(out)))
    });

    native!("bytes", |i, args| {
        let (pos, kw) = split_kwargs(&args);
        // `bytes(str, encoding[, errors])` encodes.
        if let Some(Value::Str(text)) = pos.first() {
            let encoding = pos.get(1).cloned().or_else(|| {
                kw.iter()
                    .find(|(k, _)| k == "encoding")
                    .map(|(_, v)| v.clone())
            });
            let Some(enc) = encoding else {
                return Err(type_error("string argument without an encoding"));
            };
            let errors = pos
                .get(2)
                .cloned()
                .or_else(|| {
                    kw.iter()
                        .find(|(k, _)| k == "errors")
                        .map(|(_, v)| v.clone())
                })
                .map(|v| v.py_str())
                .unwrap_or_else(|| "strict".to_owned());
            return Ok(Value::Bytes(Rc::new(crate::codecs::encode(
                text,
                &enc.py_str(),
                &errors,
            )?)));
        }
        let args = pos.to_vec();
        match args.into_iter().next() {
            None => Ok(Value::Bytes(Rc::new(Vec::new()))),
            Some(Value::Bytes(b)) => Ok(Value::Bytes(b)),
            // bytes(int) -> that many zero bytes.
            Some(Value::Int(n)) => {
                let n = n.to_usize().ok_or_else(|| value_error("negative count"))?;
                Ok(Value::Bytes(Rc::new(vec![0u8; n])))
            }
            // bytes(str) requires an encoding in Python; not supported here.
            Some(Value::Str(_)) => Err(type_error("string argument without an encoding")),
            // bytes(iterable_of_ints).
            Some(v) => {
                let it = i.make_iter(v)?;
                let mut out: Vec<u8> = Vec::new();
                while let Some(x) = i.iter_next(&it)? {
                    let n = x.to_int()?;
                    if !(0..=255).contains(&n) {
                        return Err(value_error("bytes must be in range(0, 256)"));
                    }
                    out.push(n as u8);
                }
                Ok(Value::Bytes(Rc::new(out)))
            }
        }
    });

    // `eval(source)` — parse the string as a Python *expression* and
    // evaluate it. The VM has no frame introspection, so names resolve
    // against the module globals rather than the caller's locals: enough
    // for the calculator / expression-evaluator idiom `eval` is reached
    // for, and a `NameError` (as in CPython) for anything that needed a
    // local. `exec` is deliberately absent — it would need statement
    // execution against a caller frame the VM cannot see.
    native!("eval", |i, args| {
        let (pos, _kw) = split_kwargs(&args);
        let Some(Value::Str(src)) = pos.first() else {
            return Err(type_error(
                "eval() arg 1 must be a string, bytes or code object",
            ));
        };
        let source = src.trim().to_owned();
        let module = tyc_syntax::parse_module(&source).map_err(|e| {
            crate::error::Unwind::Exception(crate::error::VmException::new(
                "SyntaxError",
                format!("invalid syntax: {e}"),
            ))
        })?;
        let module = module.into_syntax();
        let [ruff_python_ast::Stmt::Expr(expr)] = module.body.as_slice() else {
            return Err(crate::error::Unwind::Exception(
                crate::error::VmException::new("SyntaxError", "eval() takes a single expression"),
            ));
        };
        let expr = expr.value.clone();
        let env = i.root.clone();
        i.eval_expr(&expr, &env)
    });

    native!("bytearray", |i, args| {
        let (pos, kw) = split_kwargs(&args);
        let cls = bytearray_class(i)?;
        i.call_value(cls, pos.to_vec(), &kw)
    });

    native!("set", |i, args| {
        let mut out = HashSet::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(x) = i.iter_next(&it)? {
                let k = i.hash_key(&x)?;
                let k = i.settle_key_in_set(&out, k)?;
                out.insert(k);
            }
        }
        Ok(Value::Set(Rc::new(RefCell::new(out))))
    });

    native!("dict", |i, args| {
        let mut map: DictMap = IndexMap::new();
        if let Some(v) = args.into_iter().next() {
            // `dict(other_dict)` — shallow copy of an existing mapping.
            if let Value::Dict(d) = &v {
                return Ok(Value::Dict(Rc::new(RefCell::new(d.borrow().clone()))));
            }
            // `dict(mapping_instance)` — a synthesised mapping (e.g. the
            // `defaultdict` shim) exposes the mapping protocol via a `keys`
            // method; copy key→value pairs through `__getitem__`.
            if let Value::Instance(inst) = &v {
                if i.find_method(&inst.class, "keys").is_some()
                    && i.find_method(&inst.class, "__getitem__").is_some()
                {
                    let keys_view = i.get_attr(&v, "keys")?;
                    let keys = i.call_value(keys_view, vec![], &[])?;
                    let kit = i.make_iter(keys)?;
                    while let Some(k) = i.iter_next(&kit)? {
                        let val = i.subscript(&v, &k)?;
                        let key = i.hash_key(&k)?;
                        let key = i.settle_key_in_map(&map, key)?;
                        map.insert(key, val);
                    }
                    return Ok(Value::Dict(Rc::new(RefCell::new(map))));
                }
            }
            let it = i.make_iter(v)?;
            while let Some(pair) = i.iter_next(&it)? {
                match pair {
                    Value::Tuple(t) if t.len() == 2 => {
                        let key = i.hash_key(&t[0])?;
                        let key = i.settle_key_in_map(&map, key)?;
                        map.insert(key, t[1].clone());
                    }
                    _ => return Err(type_error("dict update expected a sequence of pairs")),
                }
            }
        }
        Ok(Value::Dict(Rc::new(RefCell::new(map))))
    });

    // `slice(stop)` / `slice(start, stop[, step])` — the same marker tuple the
    // subscript syntax builds, so `isinstance(x, slice)`, `.start` and
    // `.indices()` work on both.
    native!("slice", |_i, args| {
        let (start, stop, step) = match args.len() {
            1 => (Value::None, args[0].clone(), Value::None),
            2 => (args[0].clone(), args[1].clone(), Value::None),
            3 => (args[0].clone(), args[1].clone(), args[2].clone()),
            0 => return Err(type_error("slice expected at least 1 argument, got 0")),
            n => {
                return Err(type_error(format!(
                    "slice expected at most 3 arguments, got {n}"
                )))
            }
        };
        Ok(Value::Tuple(Rc::new(vec![
            Value::Str(Rc::new("__slice__".into())),
            start,
            stop,
            step,
        ])))
    });

    native!("frozenset", |i, args| {
        let mut out = HashSet::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(x) = i.iter_next(&it)? {
                let k = i.hash_key(&x)?;
                let k = i.settle_key_in_set(&out, k)?;
                out.insert(k);
            }
        }
        // Insert the `__typhon_frozen__` sentinel so that repr(), py_str(),
        // and set_is_frozen() all recognise this as a frozenset and not a
        // plain mutable set. Matches the sentinel path used by deep_freeze_value.
        out.insert(HashKey::Str(Rc::new("__typhon_frozen__".to_owned())));
        Ok(Value::Set(Rc::new(RefCell::new(out))))
    });

    native!("repr", |interp, args| Ok(Value::Str(Rc::new(
        interp.repr_of(single(&args, "repr")?)?
    ))));

    native!("type", |_i, args| {
        let v = single(&args, "type")?;
        // Return a real type object so `type(x).__name__`, `str(type(x))`
        // (→ `<class 'int'>`), and `type(x) == int` / `== SomeClass` all work.
        // User instances map to their declaring class; builtins map to a
        // lightweight class object named after the type.
        Ok(match v {
            Value::Instance(i) => Value::Class(i.class.clone()),
            Value::Class(_) => make_builtin_type("type"),
            // `type(some_exception).__name__` should be the concrete kind
            // (e.g. `TypeError`), not the generic `Exception`.
            Value::Exception { kind, .. } => make_builtin_type(kind.as_str()),
            other => make_builtin_type(other.type_name()),
        })
    });

    native!("issubclass", |i, args| {
        if args.len() != 2 {
            return Err(type_error("issubclass() expected 2 arguments"));
        }
        let sub = i.force_alias(&args[0]);
        if !matches!(
            sub,
            Value::Class(_) | Value::Native(_) | Value::Str(_) | Value::Tuple(_)
        ) {
            return Err(type_error("issubclass() arg 1 must be a class"));
        }
        let cls = i.force_alias(&args[1]);
        Ok(Value::Bool(is_subclass_of(&sub, &cls)))
    });
    native!("isinstance", |i, args| {
        if args.len() != 2 {
            return Err(type_error("isinstance() expected 2 arguments"));
        }
        let val = &args[0];
        // Force a forward-declared `type` alias (`type AB = A | B` written
        // above `A`/`B`) used at runtime before the post-body resolution
        // pass has run — otherwise it would still be its name-string
        // fallback and the test would silently return the wrong result.
        let cls = i.force_alias(&args[1]);
        // A `@runtime_checkable` Protocol is matched *structurally* — the
        // value has to answer every member the protocol declares — and a
        // Protocol without the decorator is not usable here at all, both as
        // in CPython.
        for target in protocol_targets(&cls) {
            if let Value::Class(p) = &target {
                if !p.class_attrs.borrow().contains_key("_is_runtime_protocol") {
                    return Err(type_error(
                        "Instance and class checks can only be used with @runtime_checkable protocols",
                    ));
                }
                let members: Vec<String> = p.methods.borrow().keys().cloned().collect();
                if members.iter().all(|m| i.get_attr(val, m.as_str()).is_ok()) {
                    return Ok(Value::Bool(true));
                }
            }
        }
        Ok(Value::Bool(is_instance_of(val, &cls)))
    });

    native!("abs", |i, args| match single(&args, "abs")? {
        Value::Int(n) => Ok(Value::Int(n.abs())),
        Value::Float(x) => Ok(Value::Float(x.abs())),
        Value::Bool(b) => Ok(Value::Int(VmInt::from(*b as i64))),
        // `abs(complex)` is the Euclidean magnitude (a float), matching CPython.
        Value::Complex(re, im) => Ok(Value::Float((re * re + im * im).sqrt())),
        v @ Value::Instance(_) => {
            // User `__abs__` dunder.
            if let Some(r) = i.call_dunder0(v, "__abs__")? {
                Ok(r)
            } else if let Some(inner) = crate::value::enum_mixin_value(v) {
                // A value-mixin enum member *is* its value.
                i.call_value(i.root.get("abs").unwrap_or(Value::None), vec![inner], &[])
            } else {
                Err(type_error(format!(
                    "bad operand type for abs(): '{}'",
                    v.type_name()
                )))
            }
        }
        v => Err(type_error(format!(
            "bad operand type for abs(): '{}'",
            v.type_name()
        ))),
    });

    // `complex()` / `complex(re)` / `complex(re, im)` constructor. Accepts
    // int/float/bool numeric components (and a `Complex` first arg).
    native!("complex", |_i, args| {
        fn part(v: &Value, what: &str) -> Result<f64, Unwind> {
            match v {
                Value::Int(n) => Ok(n.to_f64()),
                Value::Float(x) => Ok(*x),
                Value::Bool(b) => Ok(*b as i64 as f64),
                _ => Err(type_error(format!(
                    "complex() {what} argument must be a number, not '{}'",
                    v.type_name()
                ))),
            }
        }
        match args.as_slice() {
            [] => Ok(Value::Complex(0.0, 0.0)),
            [Value::Complex(re, im)] => Ok(Value::Complex(*re, *im)),
            // complex("1+2j") / complex("3j") / complex("-1") string parse.
            [Value::Str(s)] => {
                let (re, im) = parse_complex_str(s.trim())
                    .ok_or_else(|| value_error("complex() arg is a malformed string"))?;
                Ok(Value::Complex(re, im))
            }
            [x] => Ok(Value::Complex(part(x, "first")?, 0.0)),
            [Value::Complex(re, im), y] => {
                // complex(c, n) → c + n*1j (imag part adds).
                Ok(Value::Complex(*re, *im + part(y, "second")?))
            }
            [x, y] => Ok(Value::Complex(part(x, "first")?, part(y, "second")?)),
            _ => Err(type_error("complex() takes at most 2 arguments")),
        }
    });

    native!("min", |i, args| reduce_minmax(i, args, true));
    native!("max", |i, args| reduce_minmax(i, args, false));

    native!("sum", |i, args| {
        let mut args = args.into_iter();
        let iterable = args
            .next()
            .ok_or_else(|| type_error("sum() requires an iterable"))?;
        // `sum(xs, start)` — the second positional argument was dropped on
        // the floor, so the call returned a total short by exactly `start`
        // with no error. (The keyword form `sum(xs, start=n)` is handled on
        // the kwargs path.)
        let start = args.next().unwrap_or(Value::Int(VmInt::from(0)));
        if args.next().is_some() {
            return Err(type_error("sum() takes at most 2 arguments"));
        }
        builtin_sum(i, iterable, start)
    });

    native!("sorted", |i, args| {
        let it = i.make_iter(
            args.into_iter()
                .next()
                .ok_or_else(|| type_error("sorted() requires an iterable"))?,
        )?;
        let mut out: Vec<Value> = Vec::new();
        while let Some(v) = i.iter_next(&it)? {
            out.push(v);
        }
        // Honour a user `__lt__` on instances via `value_cmp` (the dunder-blind
        // `py_cmp` treats every instance pair as equal, leaving the list
        // unsorted). `sort_by` can't return `Result`, so capture the first
        // comparison error and surface it after the sort completes.
        let mut sort_error: Option<Unwind> = None;
        out.sort_by(|a, b| {
            if sort_error.is_some() {
                return std::cmp::Ordering::Equal;
            }
            match i.value_cmp(a, b) {
                Ok(o) => o,
                Err(e) => {
                    sort_error = Some(e);
                    std::cmp::Ordering::Equal
                }
            }
        });
        if let Some(e) = sort_error {
            return Err(e);
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });

    native!("reversed", |i, args| {
        let seq = args
            .into_iter()
            .next()
            .ok_or_else(|| type_error("reversed() requires an iterable"))?;
        // A user `__reversed__` wins (CPython protocol).
        if let Value::Instance(_) = &seq {
            if let Some(r) = i.call_dunder0(&seq, "__reversed__")? {
                return i.make_iter(r);
            }
        }
        let it = i.make_iter(seq)?;
        let mut out: Vec<Value> = Vec::new();
        while let Some(v) = i.iter_next(&it)? {
            out.push(v);
        }
        out.reverse();
        Ok(Value::Iter(Rc::new(RefCell::new(IterState::Reversed {
            items: Rc::new(out),
            index: 0,
        }))))
    });

    native!("enumerate", |i, args| {
        let mut args = args.into_iter();
        let iterable = args
            .next()
            .ok_or_else(|| type_error("enumerate() requires an iterable"))?;
        // The positional `start` (`enumerate(xs, 1)`); the keyword form is
        // decoded in `call_with_kwargs`.
        let start = match args.next() {
            Some(v) => v.to_int()?,
            None => 0,
        };
        let inner = i.make_iter(iterable)?;
        if let Value::Iter(it) = inner {
            Ok(Value::Iter(Rc::new(RefCell::new(IterState::Enumerate {
                inner: it,
                index: start,
            }))))
        } else {
            unreachable!()
        }
    });

    native!("zip", |i, args| {
        let mut inners = Vec::new();
        for a in args {
            let inner = i.make_iter(a)?;
            if let Value::Iter(it) = inner {
                inners.push(it);
            }
        }
        Ok(Value::Iter(Rc::new(RefCell::new(IterState::Zip {
            inners,
        }))))
    });

    native!("map", |i, mut args| {
        if args.len() < 2 {
            return Err(type_error("map() requires at least 2 arguments"));
        }
        let func = args.remove(0);
        let inner = i.make_iter(args.remove(0))?;
        if let Value::Iter(it) = inner {
            Ok(Value::Iter(Rc::new(RefCell::new(IterState::Map {
                func,
                inner: it,
            }))))
        } else {
            unreachable!()
        }
    });

    native!("filter", |i, mut args| {
        if args.len() < 2 {
            return Err(type_error("filter() requires 2 arguments"));
        }
        let func = args.remove(0);
        let inner = i.make_iter(args.remove(0))?;
        if let Value::Iter(it) = inner {
            Ok(Value::Iter(Rc::new(RefCell::new(IterState::Filter {
                func,
                inner: it,
            }))))
        } else {
            unreachable!()
        }
    });

    native!("all", |i, args| {
        let it = i.make_iter(
            args.into_iter()
                .next()
                .ok_or_else(|| type_error("all() requires an iterable"))?,
        )?;
        while let Some(v) = i.iter_next(&it)? {
            if !v.truthy() {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    });
    native!("any", |i, args| {
        let it = i.make_iter(
            args.into_iter()
                .next()
                .ok_or_else(|| type_error("any() requires an iterable"))?,
        )?;
        while let Some(v) = i.iter_next(&it)? {
            if v.truthy() {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    });

    native!("next", |i, args| {
        let mut args = args.into_iter();
        let it = args
            .next()
            .ok_or_else(|| type_error("next() requires an iterator"))?;
        // A user iterator object (a class with `__next__`) is stepped through
        // its own `__next__`; StopIteration propagates as it would in CPython.
        if let Value::Instance(inst) = &it {
            if i.find_method(&inst.class, "__next__").is_some() {
                let default = args.next();
                return match i.call_dunder0(&it, "__next__") {
                    Ok(Some(v)) => Ok(v),
                    Ok(None) => Err(crate::error::stop_iteration()),
                    Err(Unwind::Exception(e)) if e.kind == "StopIteration" && default.is_some() => {
                        Ok(default.unwrap_or(Value::None))
                    }
                    Err(e) => Err(e),
                };
            }
            return Err(type_error(format!(
                "'{}' object is not an iterator",
                inst.class.name
            )));
        }
        if !matches!(it, Value::Iter(_)) {
            return Err(type_error(format!(
                "'{}' object is not an iterator",
                it.type_name()
            )));
        }
        // `next(it, default)` returns `default` on exhaustion instead of
        // raising `StopIteration` (which carries a finished generator's
        // `return` value as `.value`).
        let default = args.next();
        match i.iter_next(&it)? {
            Some(v) => Ok(v),
            None => match default {
                Some(d) => Ok(d),
                None => Err(crate::interp::stop_iteration_for(&it)),
            },
        }
    });

    native!("iter", |i, args| {
        i.make_iter(
            args.into_iter()
                .next()
                .ok_or_else(|| type_error("iter() requires an iterable"))?,
        )
    });

    native!("hex", |_i, args| based_int_repr(&args, "hex", 16, "0x"));
    native!("bin", |_i, args| based_int_repr(&args, "bin", 2, "0b"));
    native!("oct", |_i, args| based_int_repr(&args, "oct", 8, "0o"));

    native!("chr", |_i, args| {
        let n = single(&args, "chr")?.to_int()?;
        let c = char::from_u32(n as u32)
            .ok_or_else(|| value_error("chr() arg not in range(0x110000)"))?;
        Ok(Value::Str(Rc::new(c.to_string())))
    });
    native!("ord", |_i, args| {
        let v = single(&args, "ord")?;
        match v {
            Value::Str(s) => {
                let mut it = s.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => Ok(Value::Int(VmInt::from(c as i64))),
                    _ => Err(type_error(format!(
                        "ord() expected a character, but string of length {} found",
                        s.chars().count()
                    ))),
                }
            }
            _ => Err(type_error("ord() expected a string")),
        }
    });

    native!("round", |i, args| match args.first() {
        // round(int, ndigits): non-negative ndigits is a no-op; a negative
        // ndigits rounds to tens/hundreds/… (half-to-even, staying an int).
        Some(Value::Int(i)) => {
            use num_integer::Integer;
            use num_traits::Zero;
            match args.get(1) {
                Some(n) if !matches!(n, Value::None) => {
                    let nd = n.to_int()?;
                    if nd >= 0 {
                        Ok(Value::Int(i.clone()))
                    } else if -nd > 10_000 {
                        // Any in-memory integer rounds to 0 once the place
                        // value exceeds its digit count; cap to avoid an
                        // OOM/DoS building 10**(huge) (review: gemini).
                        Ok(Value::Int(VmInt::from(0i64)))
                    } else {
                        let ib = i.to_bigint();
                        let p = num_bigint::BigInt::from(10).pow((-nd) as u32);
                        // Floor-divide so the remainder is always in [0, p).
                        let q = ib.div_floor(&p);
                        let r = &ib - &q * &p;
                        let two_r = &r * 2;
                        let rounded = if two_r < p {
                            q
                        } else if two_r > p {
                            q + 1
                        } else if (&q % num_bigint::BigInt::from(2)).is_zero() {
                            q
                        } else {
                            q + 1
                        };
                        Ok(Value::Int(VmInt::from(rounded * &p)))
                    }
                }
                _ => Ok(Value::Int(i.clone())),
            }
        }
        Some(Value::Float(x)) => {
            let x = *x;
            match args.get(1) {
                // round(x, ndigits) -> float, round-half-to-even on the
                // actual f64 value. Rust's float formatting uses
                // round-ties-to-even, matching CPython (so 2.675, which is
                // really 2.67499..., rounds to 2.67).
                Some(n) if !matches!(n, Value::None) => {
                    let n = n.to_int()? as i32;
                    if !x.is_finite() {
                        return Ok(Value::Float(x));
                    }
                    if n >= 0 {
                        let s = format!("{:.*}", n as usize, x);
                        Ok(Value::Float(s.parse::<f64>().unwrap_or(x)))
                    } else {
                        // Round to a negative decimal place (tens, hundreds…).
                        let p = 10f64.powi(-n);
                        Ok(Value::Float(round_half_even(x / p) * p))
                    }
                }
                // round(x) -> int, round-half-to-even.
                // NaN and infinity have no integer value: CPython raises
                // `ValueError` / `OverflowError`, where a saturating cast
                // silently returned 0 / `i64::MAX`.
                _ => Ok(Value::Int(VmInt::from_bigint(
                    Value::Float(round_half_even(x)).to_bigint()?,
                ))),
            }
        }
        // A user `__round__(self[, ndigits])` — how `round()` reaches a
        // `lazy let` proxy, a Decimal-like wrapper, or any custom numeric.
        Some(v @ Value::Instance(_)) => {
            let v = v.clone();
            let extra: Vec<Value> = args.iter().skip(1).cloned().collect();
            match i.call_dunder(&v, "__round__", extra)? {
                Some(r) => Ok(r),
                None => Err(type_error(format!(
                    "type {} doesn't define __round__ method",
                    v.type_name()
                ))),
            }
        }
        _ => Err(type_error("round() expected a number")),
    });

    native!("input", |_i, args| {
        if let Some(prompt) = args.first() {
            use std::io::Write;
            print!("{}", prompt.py_str());
            std::io::stdout().flush().ok();
        }
        let mut s = String::new();
        let read = std::io::stdin().read_line(&mut s).map_err(|e| {
            crate::error::Unwind::Exception(crate::error::VmException::new(
                "OSError",
                format!("{e}"),
            ))
        })?;
        // A 0-byte read is end-of-input: CPython's `input()` raises `EOFError`
        // there, it does not return `""`. Returning `""` silently turned a
        // `while (line := input()) != "":` loop into a no-op under `tyc run`.
        if read == 0 {
            return Err(crate::error::Unwind::Exception(
                crate::error::VmException::new("EOFError", "EOF when reading a line"),
            ));
        }
        // Strip trailing newline.
        if s.ends_with('\n') {
            s.pop();
            if s.ends_with('\r') {
                s.pop();
            }
        }
        Ok(Value::Str(Rc::new(s)))
    });

    native!("hash", |i, args| {
        // CPython's algorithms for the builtin types; the user `__hash__` /
        // dataclass / identity protocol for instances (`hash_value`).
        let v = single(&args, "hash")?;
        Ok(Value::Int(VmInt::from(i.hash_value(v)?)))
    });

    native!("id", |_i, args| {
        // Stable per-object identity for heap-allocated values: the address
        // of the underlying `Rc` payload. Immutable scalars (int, float,
        // bool, None) hash to the address of the temporary `&Value` instead,
        // which is fine because they have value equality, not reference
        // identity — CPython behaves similarly (every freshly-boxed int gets
        // a different id).
        let v = single(&args, "id")?;
        let addr: usize = match v {
            Value::List(l) => Rc::as_ptr(l) as usize,
            Value::Tuple(t) => Rc::as_ptr(t) as usize,
            Value::Dict(d) => Rc::as_ptr(d) as usize,
            Value::Set(s) => Rc::as_ptr(s) as usize,
            Value::Str(s) => Rc::as_ptr(s) as usize,
            Value::Bytes(b) => Rc::as_ptr(b) as usize,
            Value::Class(c) => Rc::as_ptr(c) as usize,
            Value::Instance(i) => Rc::as_ptr(i) as usize,
            Value::Module(m) => Rc::as_ptr(m) as usize,
            Value::Function(f) => Rc::as_ptr(f) as usize,
            Value::Native(n) => Rc::as_ptr(n) as usize,
            Value::Iter(it) => Rc::as_ptr(it) as usize,
            other => other as *const _ as usize,
        };
        Ok(Value::Int(VmInt::from(addr as i64)))
    });

    native!("callable", |_i, args| {
        let v = single(&args, "callable")?;
        Ok(Value::Bool(matches!(
            v,
            Value::Function(_) | Value::Native(_) | Value::BoundMethod { .. } | Value::Class(_)
        )))
    });

    // `open` is `io.open`: the file object model lives in the `io` shim.
    native!("open", |i, args| {
        let (pos, kw) = split_kwargs(&args);
        let io = make_io_module(i)?;
        let open_fn = i.get_attr(&io, "open")?;
        i.call_value(open_fn, pos.to_vec(), &kw)
    });

    // `@property`, `@classmethod`, `@staticmethod`: the VM has no
    // descriptor protocol, so these decorators reduce to the identity
    // — the wrapped function is callable as `obj.name()` (not `obj.name`
    // for property). That's a documented divergence from CPython, but
    // it lets programs that decorate methods at least import and run.
    native!("property", |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    native!("classmethod", |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    native!("staticmethod", |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    // `super()` — return a stub module whose attribute access yields a
    // no-op callable. Just enough to let `super().__init__(...)` synthesised
    // by `class!` lowering work (the actual parent init for stdlib
    // exceptions does nothing useful in our minimal VM since exceptions
    // are flat Value::Exception variants). For real subclassing with
    // inherited behaviour, run via `tyc run --compile`.
    native!("super", |_i, _args| {
        use crate::value::Module;
        let mut members = std::collections::HashMap::new();
        let noop = NativeFn::new("super.method", |_i, _a| Ok(Value::None));
        members.insert("__init__".into(), Value::Native(Rc::new(noop)));
        Ok(Value::Module(Rc::new(Module {
            name: "<super>".to_owned(),
            members: std::cell::RefCell::new(members),
            env: None,
        })))
    });

    // Constants.
    root.set("True", Value::Bool(true));
    root.set("False", Value::Bool(false));
    root.set("None", Value::None);
    root.set("Ellipsis", crate::value::ellipsis_value());

    // `object` exists as a placeholder so synthesised bases (`class
    // __typhon_impl_Foo(object):` from impl-block lowering, or user code
    // declaring an explicit `object` base) don't trip up name resolution.
    // The VM treats it as a no-op base class.
    root.set(
        "object",
        Value::Class(Rc::new(crate::value::Class {
            name: "object".to_owned(),
            methods: std::cell::RefCell::new(HashMap::new()),
            fields: vec![],
            class_attrs: std::cell::RefCell::new(HashMap::new()),
            bases: vec![],
            properties: std::cell::RefCell::new(std::collections::HashSet::new()),
            classmethods: std::cell::RefCell::new(std::collections::HashSet::new()),
            is_exception: false,
            is_protocol: false,
        })),
    );
    // Common typing names that show up as zero-effort bases.
    for name in ["Protocol", "BaseModel", "Generic", "TypedDict"] {
        root.set(
            name,
            Value::Class(Rc::new(crate::value::Class {
                name: name.to_owned(),
                methods: std::cell::RefCell::new(HashMap::new()),
                fields: vec![],
                class_attrs: std::cell::RefCell::new(HashMap::new()),
                bases: vec![],
                properties: std::cell::RefCell::new(std::collections::HashSet::new()),
                classmethods: std::cell::RefCell::new(std::collections::HashSet::new()),
                is_exception: false,
                is_protocol: false,
            })),
        );
    }

    // Result / Ok / Err as native callables.
    let ok_ctor = NativeFn::new("Ok", |_i, args| {
        let v = args.into_iter().next().unwrap_or(Value::None);
        Ok(Value::ResultOk(Box::new(v)))
    });
    let err_ctor = NativeFn::new("Err", |_i, args| {
        let v = args.into_iter().next().unwrap_or(Value::None);
        Ok(Value::ResultErr(Box::new(v)))
    });
    root.set("Ok", Value::Native(Rc::new(ok_ctor)));
    root.set("Err", Value::Native(Rc::new(err_ctor)));
    // `exit()` / `quit()` (the `site` module's helpers) raise `SystemExit`
    // exactly like `sys.exit()`.
    for name in ["exit", "quit"] {
        root.set(
            name,
            Value::Native(Rc::new(NativeFn::new(name, |_i, args| {
                Err(crate::error::system_exit(args))
            }))),
        );
    }
    // `try_result(thunk[, on_err])` — exception→Result bridging combinator,
    // available as a prelude name and re-exported from the typhon_runtime
    // module shim below.
    root.set("try_result", try_result_native());
    // The `?` operator desugars to `isinstance(_, __typhon_Err__)`, so the
    // preprocessor's import alias must resolve to the VM's `Err` ctor.
    root.set(
        "__typhon_Err__",
        root.get("Err").expect("Err just registered"),
    );

    // A couple of exception types so user `raise ValueError(...)` works.
    for name in [
        "BaseException",
        "Exception",
        "ValueError",
        "TypeError",
        "KeyError",
        "IndexError",
        "RuntimeError",
        "AttributeError",
        "ZeroDivisionError",
        "AssertionError",
        "StopIteration",
        "StopAsyncIteration",
        "OSError",
        "IOError",
        "FileNotFoundError",
        "FileExistsError",
        "PermissionError",
        "IsADirectoryError",
        "NotADirectoryError",
        "TimeoutError",
        "ConnectionError",
        "BrokenPipeError",
        "ConnectionResetError",
        "ConnectionRefusedError",
        "ConnectionAbortedError",
        "BlockingIOError",
        "InterruptedError",
        "ChildProcessError",
        "ProcessLookupError",
        "FrozenInstanceError",
        "BufferError",
        "MemoryError",
        "ReferenceError",
        "SystemError",
        "IndentationError",
        "TabError",
        "SyntaxError",
        "UnicodeTranslateError",
        "EncodingWarning",
        "ResourceWarning",
        "BytesWarning",
        "ImportWarning",
        "SyntaxWarning",
        "UnicodeWarning",
        "EOFError",
        "LookupError",
        "ArithmeticError",
        "OverflowError",
        "FloatingPointError",
        "RecursionError",
        "NotImplementedError",
        "NameError",
        "UnboundLocalError",
        "ImportError",
        "ModuleNotFoundError",
        "UnicodeError",
        "UnicodeDecodeError",
        "UnicodeEncodeError",
        "KeyboardInterrupt",
        "SystemExit",
        "GeneratorExit",
        "Warning",
        "DeprecationWarning",
        "UserWarning",
        "RuntimeWarning",
        "FutureWarning",
        "PendingDeprecationWarning",
    ] {
        let n = name.to_owned();
        let ctor = NativeFn::new(Box::leak(n.clone().into_boxed_str()), move |i, args| {
            // `OSError(errno, strerror[, filename[, winerror[, filename2]]])`:
            // `args` keeps the first two, the message is
            // `[Errno N] strerror: 'filename'`, and a bare `OSError` picks the
            // subclass its errno names (`OSError(2, …)` is a
            // FileNotFoundError).
            if is_os_error_kind(&n) && args.len() >= 2 {
                let strerror = args[1].py_str();
                let filename = match args.get(2) {
                    Some(Value::None) | None => None,
                    Some(v) => Some(filename_repr(i, v)?),
                };
                let filename2 = match args.get(4) {
                    Some(Value::None) | None => None,
                    Some(v) => Some(filename_repr(i, v)?),
                };
                // A bare `OSError` with an integer errno becomes the subclass
                // that errno names; any other errno keeps the class asked for.
                let kind = match (&args[0], n.as_str()) {
                    (Value::Int(code), "OSError" | "IOError" | "EnvironmentError") => {
                        os_error_kind(code.to_i64().unwrap_or(0))
                    }
                    _ => n.as_str(),
                };
                return Ok(os_error_value_of(
                    kind,
                    args[0].clone(),
                    &strerror,
                    filename.as_deref(),
                    filename2.as_deref(),
                ));
            }
            let msg = args.first().map(|v| v.py_str()).unwrap_or_default();
            Ok(Value::Exception {
                kind: Rc::new(n.clone()),
                message: Rc::new(msg),
                args: Rc::new(args),
                chain: None,
            })
        });
        root.set(name, Value::Native(Rc::new(ctor)));
    }

    // PEP 654 exception groups. Two-argument constructors
    // (`ExceptionGroup(message, [sub, ...])`) whose value keeps CPython's own
    // layout — `args == (message, subs)` — see `value::make_exception_group`.
    // `BaseExceptionGroup` auto-downcasts to `ExceptionGroup` when every
    // member is an ordinary `Exception`, exactly as CPython's `__new__` does.
    for name in ["ExceptionGroup", "BaseExceptionGroup"] {
        let n = name.to_owned();
        let ctor = NativeFn::new(Box::leak(n.clone().into_boxed_str()), move |_i, args| {
            let message = args.first().map(|v| v.py_str()).unwrap_or_default();
            let subs: Vec<Value> = match args.get(1) {
                Some(Value::List(l)) => l.borrow().clone(),
                Some(Value::Tuple(t)) => (**t).clone(),
                Some(other) => {
                    return Err(type_error(format!(
                        "second argument (exceptions) must be a sequence, not {}",
                        other.type_name()
                    )))
                }
                None => {
                    return Err(type_error(format!(
                        "{n}() missing required argument 'exceptions'"
                    )))
                }
            };
            if subs.is_empty() {
                return Err(Unwind::Exception(crate::error::VmException::new(
                    "ValueError",
                    "second argument (exceptions) must be a non-empty sequence",
                )));
            }
            // The member test is "derives from Exception", which a nested
            // `BaseExceptionGroup` fails — CPython rejects
            // `ExceptionGroup("o", [BaseExceptionGroup("i", [KeyboardInterrupt()])])`
            // with the same TypeError it gives a bare KeyboardInterrupt.
            let kind = crate::value::exception_group_kind_for(&subs);
            if n == "ExceptionGroup" && kind == "BaseExceptionGroup" {
                return Err(type_error(
                    "Cannot nest BaseExceptions in an ExceptionGroup".to_owned(),
                ));
            }
            Ok(crate::value::make_exception_group(
                kind, &message, subs, false,
            ))
        });
        root.set(name, Value::Native(Rc::new(ctor)));
    }

    // `freeze let X = expr` lowers to `X = __typhon_freeze__(expr)`. The
    // compile path resolves this via `from typhon_runtime.freeze import
    // deep_freeze as __typhon_freeze__`. The VM implements deep_freeze
    // natively: list → tuple, set → frozenset-tagged Set with the
    // FrozenSet HashKey, dict → an IndexMap whose mutation paths surface
    // a TypeError at runtime. Programs that mutate a frozen value
    // through an aliased reference now hit the same TypeError they would
    // under `tyc build && python build/main.py`.
    root.set(
        "__typhon_freeze__",
        Value::Native(Rc::new(NativeFn::new("__typhon_freeze__", |_i, args| {
            let v = args.into_iter().next().unwrap_or(Value::None);
            deep_freeze_value(v)
        }))),
    );

    // `EXPR as! TYPE` lowers to `__typhon_checked_cast__(EXPR, TYPE)`,
    // resolved on the compile path via `from typhon_runtime.cast import
    // checked_cast as __typhon_checked_cast__`. The *primary* VM path is the
    // direct-call intercept in `Interpreter::eval_call`, which interprets the
    // type-descriptor AST and runs the same recursive structural check as
    // `typhon_runtime/cast.py` (so `tyc run` rejects a wrong-shaped value just
    // like `tyc build && python`). This native binding is only the *indirect*
    // fallback — `checked_cast` referenced as a value rather than called
    // directly with two args — where the type descriptor isn't available as an
    // AST, so it degrades to identity.
    root.set(
        "__typhon_checked_cast__",
        Value::Native(Rc::new(NativeFn::new(
            "__typhon_checked_cast__",
            |_i, args| Ok(args.into_iter().next().unwrap_or(Value::None)),
        ))),
    );

    // `newtype Foo = int` lowers to `Foo = NewType("Foo", int)`. CPython's
    // `NewType` at runtime is effectively `lambda x: x`, so we mirror that:
    // a two-argument callable that returns a callable identity function.
    // VM-mode coverage of #24.
    root.set(
        "NewType",
        Value::Native(Rc::new(NativeFn::new("NewType", |_i, _args| {
            // Discard `(name, base)`; return an identity callable that
            // accepts any single argument and returns it unchanged.
            Ok(Value::Native(Rc::new(NativeFn::new(
                "NewTypeAlias",
                |_i, args| Ok(args.into_iter().next().unwrap_or(Value::None)),
            ))))
        }))),
    );
}

/// `sum(iterable, start)` with CPython 3.12+'s numeric paths: exact integer
/// accumulation, then — once the running total is a float — Neumaier
/// compensated summation of float items (`sum([0.1] * 10) == 1.0`), with
/// small ints folded in uncompensated exactly as `builtin_sum_impl` does.
/// Anything else falls back to `+`.
fn builtin_sum(i: &mut Interpreter, iterable: Value, start: Value) -> Result<Value, Unwind> {
    match &start {
        Value::Str(_) => {
            return Err(type_error(
                "sum() can't sum strings [use ''.join(seq) instead]",
            ))
        }
        Value::Bytes(_) => {
            return Err(type_error(
                "sum() can't sum bytes [use b''.join(seq) instead]",
            ))
        }
        _ => {}
    }
    let it = i.make_iter(iterable)?;
    let mut result = start;
    loop {
        if let Value::Float(f0) = result {
            let mut f_result = f0;
            let mut c = 0.0f64;
            loop {
                let Some(item) = i.iter_next(&it)? else {
                    // Don't let the compensation turn an infinite / overflowed
                    // sum into a NaN.
                    if c != 0.0 && c.is_finite() {
                        f_result += c;
                    }
                    return Ok(Value::Float(f_result));
                };
                match &item {
                    Value::Float(x) => {
                        let x = *x;
                        let t = f_result + x;
                        if f_result.abs() >= x.abs() {
                            c += (f_result - t) + x;
                        } else {
                            c += (x - t) + f_result;
                        }
                        f_result = t;
                    }
                    Value::Int(n) if n.to_i64().is_some() => {
                        f_result += n.to_i64().unwrap_or(0) as f64;
                    }
                    Value::Bool(b) => {
                        f_result += *b as i64 as f64;
                    }
                    _ => {
                        if c != 0.0 && c.is_finite() {
                            f_result += c;
                        }
                        result = i.binop(
                            &Value::Float(f_result),
                            ruff_python_ast::Operator::Add,
                            &item,
                        )?;
                        break;
                    }
                }
            }
            continue;
        }
        let Some(item) = i.iter_next(&it)? else {
            return Ok(result);
        };
        result = i.binop(&result, ruff_python_ast::Operator::Add, &item)?;
    }
}

/// CPython's `math_1` error contract for a one-argument float function: a NaN
/// result from a non-NaN argument is a domain error, an infinite result from a
/// finite argument is a range error (or a domain error for functions that
/// cannot overflow, such as `log(0)`).
fn math_1(x: f64, r: f64, can_overflow: bool) -> Result<Value, Unwind> {
    if r.is_nan() && !x.is_nan() {
        return Err(value_error("math domain error"));
    }
    if r.is_infinite() && x.is_finite() {
        if can_overflow {
            return Err(Unwind::Exception(crate::error::VmException::new(
                "OverflowError",
                "math range error",
            )));
        }
        return Err(value_error("math domain error"));
    }
    Ok(Value::Float(r))
}

/// A `math` module argument as a float, with CPython's error for an int too
/// large to convert.
fn math_arg(v: &Value) -> Result<f64, Unwind> {
    match v {
        Value::Int(n) => {
            let f = n.to_f64();
            if f.is_infinite() {
                return Err(Unwind::Exception(crate::error::VmException::new(
                    "OverflowError",
                    "int too large to convert to float",
                )));
            }
            Ok(f)
        }
        Value::Float(x) => Ok(*x),
        Value::Bool(b) => Ok(*b as i64 as f64),
        Value::Instance(_) => v.to_float(),
        other => Err(type_error(format!(
            "must be real number, not {}",
            other.type_name()
        ))),
    }
}

/// `math.fsum`: Shewchuk's exactly-rounded summation, ported from CPython's
/// `math_fsum` (partials array + final correctly-rounded collapse).
fn math_fsum(xs: &[f64]) -> Result<f64, Unwind> {
    let mut partials: Vec<f64> = Vec::new();
    let mut special_sum = 0.0f64;
    let mut inf_sum = 0.0f64;
    for &x0 in xs {
        let mut x = x0;
        if !x.is_finite() {
            if x.is_infinite() {
                inf_sum += x;
            }
            special_sum += x;
            continue;
        }
        let mut i = 0usize;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        partials.push(x);
    }
    if special_sum != 0.0 {
        if inf_sum.is_nan() {
            return Err(value_error("-inf + inf in fsum"));
        }
        return Ok(special_sum);
    }
    let mut n = partials.len();
    if n == 0 {
        return Ok(0.0);
    }
    n -= 1;
    let mut hi = partials[n];
    let mut lo = 0.0f64;
    while n > 0 {
        let x = hi;
        n -= 1;
        let y = partials[n];
        hi = x + y;
        let yr = hi - x;
        lo = y - yr;
        if lo != 0.0 {
            break;
        }
    }
    if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
        let y = lo * 2.0;
        let x = hi + y;
        let yr = x - hi;
        if y == yr {
            hi = x;
        }
    }
    Ok(hi)
}

fn single<'a>(args: &'a [Value], name: &str) -> Result<&'a Value, Unwind> {
    args.first()
        .ok_or_else(|| type_error(format!("{}() requires an argument", name)))
}

/// `hex` / `bin` / `oct`: render an integer in the given radix with CPython's
/// exact spelling — a leading `-` before the base prefix for negatives
/// (`hex(-42) == "-0x2a"`), and arbitrary precision (`hex(2**64)` must not
/// overflow). The previous implementation formatted a lossy `i64` with Rust's
/// `{:x}`/`{:b}`/`{:o}`, which prints the two's-complement bit pattern for a
/// negative and rejected any value outside `i64`.
fn based_int_repr(args: &[Value], name: &str, radix: u32, prefix: &str) -> Result<Value, Unwind> {
    let vi: VmInt = match single(args, name)? {
        Value::Int(i) => i.clone(),
        Value::Bool(b) => VmInt::from(*b as i64),
        other => {
            return Err(type_error(format!(
                "{}() argument must be an integer, not '{}'",
                name,
                other.type_name()
            )))
        }
    };
    let sign = if vi.is_negative() { "-" } else { "" };
    Ok(Value::Str(Rc::new(format!(
        "{sign}{prefix}{}",
        vi.abs().to_str_radix(radix)
    ))))
}

/// The bytes behind a `bytearray` shim instance (its `_data` list of ints),
/// so the conversions that accept a bytes-like see one.
pub(crate) fn bytearray_bytes(v: &Value) -> Option<Vec<u8>> {
    let Value::Instance(inst) = v else {
        return None;
    };
    if inst.class.name != "bytearray" {
        return None;
    }
    let data = match inst.fields.borrow().get("_data") {
        Some(Value::List(d)) => d.clone(),
        _ => return None,
    };
    let raw = data.borrow().clone();
    raw.iter()
        .map(|b| b.to_int().ok().map(|n| n as u8))
        .collect()
}

/// The modular inverse of `a` mod `m` by the extended Euclidean algorithm,
/// or `None` when `gcd(a, m) != 1`. Backs Python 3.8+'s `pow(a, -e, m)`.
fn modular_inverse(a: &num_bigint::BigInt, m: &num_bigint::BigInt) -> Option<num_bigint::BigInt> {
    use num_bigint::BigInt;
    use num_integer::Integer;
    use num_traits::{One, Signed, Zero};
    let m_abs = m.abs();
    if m_abs.is_one() {
        return Some(BigInt::zero());
    }
    let (mut old_r, mut r) = (a.mod_floor(&m_abs), m_abs.clone());
    let (mut old_s, mut s) = (BigInt::one(), BigInt::zero());
    while !r.is_zero() {
        let q = &old_r / &r;
        let new_r = &old_r - &q * &r;
        old_r = std::mem::replace(&mut r, new_r);
        let new_s = &old_s - &q * &s;
        old_s = std::mem::replace(&mut s, new_s);
    }
    if !old_r.is_one() {
        return None;
    }
    Some(old_s.mod_floor(&m_abs))
}

fn value_len(v: &Value) -> Result<usize, Unwind> {
    // A `StrEnum` member *is* its string, so `len(StrE.X)` is the value's.
    if let Some(inner) = crate::value::enum_mixin_value(v) {
        return value_len(&inner);
    }
    // `len(Colour)` is the enum's member count, as CPython's `EnumType` has
    // it — an enum class is iterable and sized.
    if let Value::Class(c) = v {
        if let Some(members) = crate::interp::enum_members_pub(c) {
            return Ok(members.len());
        }
    }
    Ok(match v {
        Value::Str(s) => s.chars().count(),
        Value::Bytes(b) => b.len(),
        Value::List(l) => l.borrow().len(),
        Value::Tuple(t) => t.len(),
        Value::Dict(d) => d
            .borrow()
            .keys()
            .filter(|k| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
            .count(),
        Value::Set(s) => s
            .borrow()
            .iter()
            .filter(|k| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
            .count(),
        Value::Range { start, stop, step } => {
            if *step > 0 {
                ((stop - start).max(0) as usize).div_ceil(*step as usize)
            } else if *step < 0 {
                ((start - stop).max(0) as usize).div_ceil((-*step) as usize)
            } else {
                0
            }
        }
        // A dict-view's length is the number of items it exposes.
        Value::DictView { items, .. } => items.len(),
        other => {
            return Err(type_error(format!(
                "object of type '{}' has no len()",
                other.type_name()
            )))
        }
    })
}

/// Resolve the optional `start` / `end` positional arguments that CPython's
/// search methods (`find`, `rfind`, `index`, `rindex`, `count`, `startswith`,
/// `endswith`) accept after the needle, into a clamped `[start, end)` range
/// over a sequence of `len` items.
///
/// These arguments were being dropped entirely: `single()` reads `args[0]`
/// and ignores the rest, so `s.find(x, i)` searched from the beginning every
/// time. That is not merely a wrong number — the canonical "scan for every
/// occurrence" loop, `while (i := s.find(x, i + 1)) != -1:`, never advances,
/// so it never terminates.
///
/// Python's rules (CPython's `ADJUST_INDICES`): a negative index counts from
/// the end and clamps at 0; `end` clamps down to `len`; but a positive
/// `start` is *not* clamped, so `start > end` marks the range as no-match
/// territory — observable with an empty needle, where CPython answers
/// `"abc".find("", 4) == -1`, `"abc".startswith("", 5) is False`,
/// `"abc".count("", 2, 1) == 0` rather than treating the range as empty at
/// `len` (all verified against 3.13). `None` signals that no-match state;
/// callers map it to `-1` / `ValueError` / `False` / `0`.
fn search_range(args: &[Value], len: usize) -> Result<Option<(usize, usize)>, Unwind> {
    let len = len as i64;
    let resolve = |v: Option<&Value>, default: i64| -> Result<i64, Unwind> {
        match v {
            None | Some(Value::None) => Ok(default),
            Some(v) => v.to_int(),
        }
    };
    let mut start = resolve(args.get(1), 0)?;
    if start < 0 {
        start = (start + len).max(0);
    }
    let mut end = resolve(args.get(2), len)?;
    if end > len {
        end = len;
    } else if end < 0 {
        end = (end + len).max(0);
    }
    if start > end {
        return Ok(None);
    }
    Ok(Some((start as usize, end as usize)))
}

/// Byte offsets of the character range `[start, end)` within `s`, for
/// re-slicing a string by CPython character indices.
fn char_range_bytes(s: &str, start: usize, end: usize) -> (usize, usize) {
    let mut it = s.char_indices().map(|(i, _)| i);
    let bs = it.clone().nth(start).unwrap_or(s.len());
    let be = it.nth(end).unwrap_or(s.len());
    (bs, be)
}

/// `issubclass(cls, classinfo)` — the class-level counterpart of
/// [`is_instance_of`]. `classinfo` may be a tuple, as in CPython.
pub(crate) fn is_subclass_of(sub: &Value, cls: &Value) -> bool {
    if let Value::Tuple(t) = cls {
        return t.iter().any(|c| is_subclass_of(sub, c));
    }
    let want = match cls {
        Value::Native(n) => n.name.to_owned(),
        Value::Class(c) => c.name.clone(),
        Value::Str(s) => (**s).clone(),
        _ => return false,
    };
    match sub {
        Value::Class(c) => {
            if want == "object" {
                return true;
            }
            if let Value::Class(target) = cls {
                if class_in_chain_rc(c, target) {
                    return true;
                }
            }
            class_in_chain(c, &want)
        }
        // An exception *kind* reaches here as a bare native / string (the VM
        // models builtin exception types by name), so relate them through
        // the same hierarchy `except` uses.
        Value::Native(n) => {
            n.name == want
                || want == "object"
                // `bool` really is a subclass of `int` in CPython.
                || (n.name == "bool" && want == "int")
                || crate::interp::builtin_exc_is_a(n.name, &want)
        }
        Value::Str(s) => {
            s.as_str() == want
                || want == "object"
                || crate::interp::builtin_exc_is_a(s.as_str(), &want)
        }
        _ => false,
    }
}

/// The Protocol classes among an `isinstance` target (which may be a tuple
/// of alternatives). Empty when nothing in the target is a Protocol.
fn protocol_targets(cls: &Value) -> Vec<Value> {
    match cls {
        Value::Tuple(t) => t.iter().flat_map(protocol_targets).collect(),
        Value::Class(c) if crate::interp::class_is_protocol_pub(c) => vec![cls.clone()],
        _ => Vec::new(),
    }
}

pub(crate) fn is_instance_of(val: &Value, cls: &Value) -> bool {
    let want_name = match cls {
        Value::Native(n) => Some(n.name.to_owned()),
        Value::Class(c) => Some(c.name.clone()),
        Value::Str(s) => Some((**s).clone()),
        Value::Tuple(t) => {
            return t.iter().any(|c| is_instance_of(val, c));
        }
        _ => None,
    };
    let Some(name) = want_name else {
        return false;
    };
    match (name.as_str(), val) {
        // Every value is an `object` — the root of Python's type hierarchy.
        // Without this arm `isinstance(x, object)` was uniformly `False`,
        // which silently inverts any control flow written around it.
        ("object", _) => true,
        ("int", Value::Int(_)) => true,
        // `bool` is a subclass of `int` in CPython, so `isinstance(True, int)`
        // is `True` there. The VM answered `False`, taking the opposite branch
        // from the compiled program on the same source.
        ("int", Value::Bool(_)) => true,
        ("float", Value::Float(_)) => true,
        ("bool", Value::Bool(_)) => true,
        ("str", Value::Str(_)) => true,
        ("bytes", Value::Bytes(_)) => true,
        ("list", Value::List(_)) => true,
        ("tuple", Value::Tuple(t)) => !crate::value::is_slice_marker(t),
        ("slice", Value::Tuple(t)) => crate::value::is_slice_marker(t),
        ("dict", Value::Dict(_)) => true,
        ("set", Value::Set(_)) => true,
        ("frozenset", Value::Set(_)) => true,
        ("range", Value::Range { .. }) => true,
        ("complex", Value::Complex(..)) => true,
        ("Ok", Value::ResultOk(_)) => true,
        ("Err", Value::ResultErr(_)) => true,
        // Exception kind match — exact, or through the builtin exception
        // hierarchy (`isinstance(e, Exception)` where e is a ValueError;
        // the same relation `except` clauses use).
        (k, Value::Exception { kind, .. })
            if k == kind.as_str() || crate::interp::builtin_exc_is_a(kind.as_str(), k) =>
        {
            true
        }
        // Class membership.
        (_, Value::Instance(inst)) => class_in_chain(&inst.class, &name),
        _ => {
            if let Value::Class(target) = cls {
                if let Value::Instance(inst) = val {
                    return class_in_chain_rc(&inst.class, target);
                }
            }
            false
        }
    }
}

fn class_in_chain(c: &Rc<crate::value::Class>, name: &str) -> bool {
    if c.name == name {
        return true;
    }
    // A builtin base (`class Counter(dict)`) is a native, not a `Class`, so
    // `build_class` records its name instead of keeping it in `bases`.
    if let Some(Value::Tuple(names)) = c.class_attrs.borrow().get("__typhon_builtin_bases__") {
        if names.iter().any(|n| n.py_str() == name) {
            return true;
        }
    }
    // A builtin *exception* base (`class AppError(ValueError)`, or the
    // synthesised `json.JSONDecodeError`) is recorded by name as well, and
    // `isinstance` has to see through the builtin hierarchy above it —
    // `except` already does.
    if crate::interp::class_has_builtin_exc_base(c, name) {
        return true;
    }
    c.bases.iter().any(|b| class_in_chain(b, name))
}

fn class_in_chain_rc(c: &Rc<crate::value::Class>, target: &Rc<crate::value::Class>) -> bool {
    if Rc::ptr_eq(c, target) {
        return true;
    }
    c.bases.iter().any(|b| class_in_chain_rc(b, target))
}

fn reduce_minmax(
    interp: &mut Interpreter,
    mut args: Vec<Value>,
    want_min: bool,
) -> Result<Value, Unwind> {
    let candidates: Vec<Value> = if args.len() == 1 {
        let it = interp.make_iter(args.remove(0))?;
        let mut out = Vec::new();
        while let Some(v) = interp.iter_next(&it)? {
            out.push(v);
        }
        out
    } else {
        args
    };
    let mut iter = candidates.into_iter();
    let mut best = iter.next().ok_or_else(|| {
        value_error(format!(
            "{}() iterable argument is empty",
            if want_min { "min" } else { "max" }
        ))
    })?;
    for v in iter {
        // Route through `value_cmp` (not the dunder-blind `Value::py_cmp`) so a
        // user-defined `__lt__` on instances is honoured — matching `list.sort`
        // and CPython. `py_cmp` returns `None` for instances, which collapsed to
        // `Equal` here and made `min`/`max` return the first element.
        let cmp = interp.value_cmp(&v, &best)?;
        if (want_min && cmp == std::cmp::Ordering::Less)
            || (!want_min && cmp == std::cmp::Ordering::Greater)
        {
            best = v;
        }
    }
    Ok(best)
}

// ── Helper-class compilation ────────────────────────────────────────────────
//
// Several stdlib shims (`defaultdict`, `datetime`, `pathlib.Path`) need real
// dunder methods (`__getitem__`, `__missing__`, `__setitem__`, `__add__`,
// `__sub__`, `__truediv__`) that the interpreter dispatches through
// `find_method` — which only consults `class.methods` (AST-backed
// `value::Function`s), not instance fields holding native closures. Rather
// than hand-build AST `Function`s, we compile a small Python source snippet
// through the same parse + desugar pipeline the VM uses for user modules and
// pull the resulting `Value::Class` objects out of a throwaway scope. This
// reuses every bit of the real method-call/dunder machinery.

/// Compile `source` in a fresh child of `interp.root` and return the named
/// top-level bindings (classes / functions) it produced.
/// Stdlib shims written in Python — interpreted by the VM itself, so they get
/// dunder dispatch, inheritance and lazy generators for free. Each file is
/// plain CPython-valid Python validated against the real module.
mod shims {
    pub const DATETIME: &str = include_str!("shims/datetime.py");
    pub const TIME_EXTRAS: &str = include_str!("shims/time_extras.py");
    pub const ARGPARSE: &str = include_str!("shims/argparse.py");
    pub const COLLECTIONS: &str = include_str!("shims/collections.py");
    pub const ITERTOOLS: &str = include_str!("shims/itertools.py");
    pub const RANDOM: &str = include_str!("shims/random.py");
    pub const HASHLIB: &str = include_str!("shims/hashlib.py");
    pub const IO: &str = include_str!("shims/io.py");
    pub const POSIXPATH: &str = include_str!("shims/posixpath.py");
    pub const OS: &str = include_str!("shims/os.py");
    pub const PATHLIB: &str = include_str!("shims/pathlib.py");
    pub const SHUTIL: &str = include_str!("shims/shutil.py");
    pub const TEMPFILE: &str = include_str!("shims/tempfile.py");
    pub const GLOB: &str = include_str!("shims/glob.py");
    pub const CONTEXTLIB_EXTRA: &str = include_str!("shims/contextlib_extra.py");
    pub const STRING: &str = include_str!("shims/string.py");
    pub const OPERATOR: &str = include_str!("shims/operator.py");
    pub const BISECT: &str = include_str!("shims/bisect.py");
    pub const BASE64: &str = include_str!("shims/base64.py");
    pub const CSV: &str = include_str!("shims/csv.py");
    pub const FUNCTOOLS_EXTRA: &str = include_str!("shims/functools_extra.py");
    pub const BYTEARRAY: &str = include_str!("shims/bytearray.py");
    pub const LAZY: &str = include_str!("shims/lazy.py");
}

fn compile_helpers(interp: &mut Interpreter, source: &str) -> Result<Vec<(String, Value)>, Unwind> {
    compile_helpers_seeded(interp, source, Vec::new())
}

/// Compile `source` with `seed` bindings visible to it (natives a shim needs
/// that are not importable, such as the raw `time()` clock).
fn compile_helpers_seeded(
    interp: &mut Interpreter,
    source: &str,
    seed: Vec<(&str, Value)>,
) -> Result<Vec<(String, Value)>, Unwind> {
    compile_shim(interp, source, seed).map(|(members, _)| members)
}

/// Run a Python shim in a fresh module namespace seeded with `seed`;
/// returns the resulting bindings and the namespace itself.
fn compile_shim(
    interp: &mut Interpreter,
    source: &str,
    seed: Vec<(&str, Value)>,
) -> Result<(Vec<(String, Value)>, crate::env::EnvRef), Unwind> {
    use tyc_syntax::preprocess;
    let expanded = preprocess::expand_question_ops(&preprocess::expand_inline_question_ops(
        // Shared with the CLI: the VM omitted both of these, so an
        // inline `?` (`f(g()?)`, `elif h()? > 1:`) failed to parse under
        // `tyc run` on a program `tyc build` compiles and runs.
        &preprocess::expand_compound_question_headers(&preprocess::expand_pipes(
            &preprocess::expand_with_chains(&preprocess::expand_go_calls(
                &preprocess::expand_gather_blocks(&preprocess::expand_multiline_guards(
                    &preprocess::expand_typed_let_unpack(&preprocess::expand_lazy_lets(source)),
                )),
            )),
        )),
    ));
    let prep = preprocess::preprocess(&expanded);
    let parsed = tyc_syntax::parse_module(&prep.python_source).map_err(|e| {
        crate::error::Unwind::Exception(crate::error::VmException::new(
            "ImportError",
            format!("internal stdlib shim parse error: {e}"),
        ))
    })?;
    let mut module = parsed.into_syntax();
    // Shim sources are plain Python validated against CPython: every class
    // is emitted exactly as written (no `@dataclass` decoration, no
    // synthesised `__init__`), so the VM's dataclass semantics — slots
    // enforcement, field-tuple hashing, generated constructors — never
    // apply to a helper class CPython would run as a bare class.
    let plain_class_lines: Vec<usize> = prep
        .python_source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("class "))
        .map(|(i, _)| i)
        .collect();
    let desugar_out = tyc_desugar::desugar_module_with(
        &module,
        tyc_desugar::DesugarOptions {
            plain_class_line_starts: preprocess::line_byte_starts(
                &prep.python_source,
                &plain_class_lines,
            ),
            ..Default::default()
        },
    );
    module = desugar_out.module;

    let env = crate::env::Env::new_module(&interp.root);
    for (name, value) in seed {
        env.set(name, value);
    }
    interp.exec_block(&module.body, &env)?;
    Ok((env.snapshot(), env))
}

/// A module whose members are the top-level bindings of a Python shim.
fn module_from_shim(interp: &mut Interpreter, name: &str, source: &str) -> Result<Value, Unwind> {
    let (members, env) = compile_shim(interp, source, Vec::new())?;
    let entries: Vec<(&str, Value)> = members
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    Ok(make_module_env(name, entries, env))
}

fn make_datetime_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    module_from_shim(interp, "datetime", shims::DATETIME)
}

fn make_argparse_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    module_from_shim(interp, "argparse", shims::ARGPARSE)
}

fn make_itertools_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    module_from_shim(interp, "itertools", shims::ITERTOOLS)
}

/// `collections`: the Python shim (Counter / deque / OrderedDict / ChainMap)
/// plus the native `defaultdict` and a `namedtuple` factory that builds a real
/// class named after the type from the shim's `_NamedTupleBase` template.
fn make_collections_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    let members = compile_helpers(interp, shims::COLLECTIONS)?;
    let mut entries: Vec<(&str, Value)> = Vec::new();
    let mut template: Option<Rc<crate::value::Class>> = None;
    for (k, v) in &members {
        if k == "_NamedTupleBase" {
            if let Value::Class(c) = v {
                template = Some(c.clone());
            }
        }
        if !k.starts_with('_') {
            entries.push((k.as_str(), v.clone()));
        }
    }
    let Some(template) = template else {
        return Err(type_error(
            "internal: collections shim did not define _NamedTupleBase",
        ));
    };
    let namedtuple = nf("namedtuple", move |i, args| {
        let (pos, kw) = split_kwargs(&args);
        let typename = pos
            .first()
            .ok_or_else(|| type_error("namedtuple() missing required argument: 'typename'"))?
            .py_str();
        let field_names = pos
            .get(1)
            .ok_or_else(|| type_error("namedtuple() missing required argument: 'field_names'"))?
            .clone();
        let fields: Vec<String> = match &field_names {
            Value::Str(s) => s
                .replace(',', " ")
                .split_whitespace()
                .map(String::from)
                .collect(),
            other => {
                let it = i.make_iter(other.clone())?;
                let mut out = Vec::new();
                while let Some(v) = i.iter_next(&it)? {
                    out.push(v.py_str());
                }
                out
            }
        };
        let mut defaults: Vec<Value> = Vec::new();
        if let Some((_, d)) = kw.iter().find(|(k, _)| k == "defaults") {
            if !matches!(d, Value::None) {
                let it = i.make_iter(d.clone())?;
                while let Some(v) = i.iter_next(&it)? {
                    defaults.push(v);
                }
            }
        }
        if defaults.len() > fields.len() {
            return Err(type_error("Got more default values than field names"));
        }
        let mut field_defaults: DictMap = IndexMap::new();
        let offset = fields.len() - defaults.len();
        for (name, d) in fields[offset..].iter().zip(defaults) {
            field_defaults.insert(HashKey::Str(Rc::new(name.clone())), d);
        }
        let mut class_attrs: HashMap<String, Value> = HashMap::new();
        class_attrs.insert(
            "_fields".to_owned(),
            Value::Tuple(Rc::new(
                fields
                    .iter()
                    .map(|f| Value::Str(Rc::new(f.clone())))
                    .collect(),
            )),
        );
        class_attrs.insert(
            "_field_defaults".to_owned(),
            Value::Dict(Rc::new(RefCell::new(field_defaults))),
        );
        let cls = Rc::new(crate::value::Class {
            name: typename,
            methods: RefCell::new(template.methods.borrow().clone()),
            fields: vec![],
            class_attrs: RefCell::new(class_attrs),
            bases: vec![template.clone()],
            properties: RefCell::new(template.properties.borrow().clone()),
            classmethods: RefCell::new(template.classmethods.borrow().clone()),
            is_exception: false,
            is_protocol: false,
        });
        Ok(Value::Class(cls))
    });
    entries.push(("namedtuple", namedtuple));
    entries.push(("abc", make_collections_abc_module()));
    let defaultdict = nf("defaultdict", |i, mut args| {
        // `defaultdict(factory[, mapping])` constructs a synthesised mapping
        // instance whose `__missing__` calls `factory()` to materialise a
        // default for absent keys (foundation `__missing__` hook). The
        // backing store is a plain dict held in `self._data`.
        let factory = if args.is_empty() {
            Value::None
        } else {
            args.remove(0)
        };
        let cls = defaultdict_class(i)?;
        let initial = args.into_iter().next().unwrap_or(Value::None);
        i.call_value(cls, vec![factory, initial], &[])
    });
    entries.push(("defaultdict", defaultdict));
    Ok(make_module("collections", entries))
}

/// Compile `source`, then look up a single named binding from it.
fn compile_helper(interp: &mut Interpreter, source: &str, name: &str) -> Result<Value, Unwind> {
    compile_helpers(interp, source)?
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
        .ok_or_else(|| type_error(format!("stdlib shim did not define '{name}'")))
}

/// Compile (once, memoised in `module_cache`) and return a synthesised helper
/// class by `cache_key`, defined by `source` under `name`.
fn cached_helper_class(
    interp: &mut Interpreter,
    cache_key: &str,
    source: &str,
    name: &str,
) -> Result<Value, Unwind> {
    if let Some(v) = interp.module_cache.get(cache_key) {
        return Ok(v.clone());
    }
    let v = compile_helper(interp, source, name)?;
    interp.module_cache.insert(cache_key.to_owned(), v.clone());
    Ok(v)
}

/// Source for the `defaultdict` shim class. Backing store lives in `_data`;
/// the missing-key path runs `__missing__`, which the foundation's subscript
/// hook invokes when `__getitem__` raises `KeyError`.
const DEFAULTDICT_SRC: &str = r#"
class _DefaultDict:
    __typhon_builtin_bases__ = ("dict", "defaultdict")
    def __init__(self, default_factory, initial):
        self._data = {}
        self._factory = default_factory
        if initial is not None:
            for k in initial:
                self._data[k] = initial[k]
    def __getitem__(self, key):
        if key in self._data:
            return self._data[key]
        raise KeyError(key)
    def __setitem__(self, key, value):
        self._data[key] = value
    def __missing__(self, key):
        if self._factory is None:
            raise KeyError(key)
        value = self._factory()
        self._data[key] = value
        return value
    def __contains__(self, key):
        return key in self._data
    def __repr__(self):
        return "defaultdict(%r, %r)" % (self._factory, self._data)
    def __len__(self):
        return len(self._data)
    def __iter__(self):
        return iter(self._data)
    def keys(self):
        return self._data.keys()
    def values(self):
        return self._data.values()
    def items(self):
        return self._data.items()
    def get(self, key, default=None):
        if key in self._data:
            return self._data[key]
        return default
    def __eq__(self, other):
        if isinstance(other, _DefaultDict):
            return self._data == other._data
        return self._data == other
    def __ne__(self, other):
        if isinstance(other, _DefaultDict):
            return self._data != other._data
        return self._data != other
"#;

/// The `_NamedTupleBase` template from the `collections` shim, cached.
///
/// `class Point(NamedTuple):` gets it as a base so the instance behaves like
/// the tuple CPython actually returns — indexable, iterable, comparable with
/// a plain tuple, and rendered `Point(x=1, y=2)` — rather than a bare object
/// whose fields happen to be set.
pub(crate) fn namedtuple_base_class(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_helper_class(
        interp,
        "__shim_namedtuple_base__",
        shims::COLLECTIONS,
        "_NamedTupleBase",
    )
}

/// The `bytearray` class, cached. `Value::Bytes` is immutable, so the
/// mutable sibling is a shim class whose `__typhon_builtin_bases__` marks
/// it as a `bytearray` for `isinstance`.
/// The `lazy let` proxy class, cached. Mirrors the emitted runtime's
/// `_LazyValue`: the factory runs on first *use*, not at the binding.
fn lazy_value_class(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_helper_class(interp, "__shim_lazy_value__", shims::LAZY, "_LazyValue")
}

pub(crate) fn bytearray_class(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_helper_class(interp, "__shim_bytearray__", shims::BYTEARRAY, "bytearray")
}

fn defaultdict_class(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_helper_class(
        interp,
        "__shim_defaultdict__",
        DEFAULTDICT_SRC,
        "_DefaultDict",
    )
}

// ── Module resolution ──────────────────────────────────────────────────────

/// Every module `resolve_module` below can hand back, by top-level name.
///
/// `tyc run` consults this *before* executing anything: a program that
/// imports something outside this set is run through the compiled path
/// instead, so the VM stays a drop-in rather than failing where
/// `tyc build` + CPython succeeds. Keeping it a plain list (rather than
/// probing `resolve_module`, which has to build the module to answer)
/// means the check costs nothing; `vm_modelled_modules_all_resolve` holds
/// the two in sync.
pub const MODELLED_MODULE_ROOTS: &[&str] = &[
    "__future__",
    "abc",
    "argparse",
    "asyncio",
    "base64",
    "bisect",
    "collections",
    "contextlib",
    "csv",
    "dataclasses",
    "datetime",
    "enum",
    "functools",
    "glob",
    "hashlib",
    "heapq",
    "io",
    "itertools",
    "json",
    "math",
    "operator",
    "os",
    "pathlib",
    "posixpath",
    "pydantic",
    "random",
    "re",
    "shutil",
    "string",
    "sys",
    "tempfile",
    "time",
    "typhon_runtime",
    "typing",
];

/// True when the VM can serve `name` (or the package it belongs to)
/// without falling back to CPython.
pub fn models_module(name: &str) -> bool {
    let root = name.split('.').next().unwrap_or(name);
    MODELLED_MODULE_ROOTS.contains(&root)
}

pub fn resolve_module(interp: &mut Interpreter, name: &str) -> Result<Value, Unwind> {
    match name {
        "typhon_runtime" => Ok(make_typhon_runtime_module(interp)),
        "math" => Ok(make_math_module()),
        "os" => make_os_module(interp),
        "os.path" | "posixpath" => os_path_module(interp),
        "io" => make_io_module(interp),
        "shutil" => make_shutil_module(interp),
        "tempfile" => make_tempfile_module(interp),
        "glob" => make_glob_module(interp),
        "sys" => Ok(make_sys_module(interp)),
        "json" => Ok(make_json_module()),
        "time" => make_time_module(interp),
        "argparse" => make_argparse_module(interp),
        "random" => make_random_module(interp),
        "hashlib" => make_hashlib_module(interp),
        "typing" => Ok(make_typing_module()),
        "re" => Ok(make_re_module()),
        "collections" => make_collections_module(interp),
        // `from collections.abc import Callable / Iterator / ...` — the
        // canonical home for the abstract container types. Annotation-only
        // at runtime, so identity natives (mirroring the `typing` shim)
        // are all the VM needs.
        "collections.abc" => Ok(make_collections_abc_module()),
        // `abc` — `ABC` / `ABCMeta` are annotation-/base-only at runtime in
        // the VM (a non-`Value::Class` base is ignored), and the abstract-*
        // decorators are identity wrappers, so identity natives suffice for
        // `class H(ABC): @abstractmethod def handle(...): ...`.
        "abc" => Ok(make_abc_module()),
        // Cooperative (sequential) asyncio: coroutines are thunks forced at
        // await points, tasks complete at creation, and Queue.get on an
        // empty queue fails loudly instead of deadlocking. Programs whose
        // CORRECTNESS depends on interleaving need `tyc run --compile`.
        "asyncio" => Ok(make_asyncio_module()),
        "functools" => Ok(make_functools_module(interp)),
        "itertools" => make_itertools_module(interp),
        "dataclasses" => Ok(make_dataclasses_module()),
        "pathlib" => make_pathlib_module(interp),
        "datetime" => make_datetime_module(interp),
        "heapq" => Ok(make_heapq_module()),
        "contextlib" => Ok(make_contextlib_module(interp)),
        // `from __future__ import annotations` (and friends): CPython's
        // compiler consumes the statement, and the module only has to carry
        // a truthy feature object per name.
        "__future__" => Ok(make_future_module()),
        "string" => module_from_shim(interp, "string", shims::STRING),
        "operator" => module_from_shim(interp, "operator", shims::OPERATOR),
        "bisect" => module_from_shim(interp, "bisect", shims::BISECT),
        "base64" => module_from_shim(interp, "base64", shims::BASE64),
        "csv" => module_from_shim(interp, "csv", shims::CSV),
        "pydantic" => Ok(make_pydantic_module()),
        // Typhon-runtime submodules — return the matching submodule so
        // `from typhon_runtime.freeze import deep_freeze` and friends
        // resolve their member directly. Falls back to the root module
        // for unrecognised submodule names so legacy code paths keep
        // working.
        n if n.starts_with("typhon_runtime.") => {
            let root = make_typhon_runtime_module(interp);
            let sub = n.strip_prefix("typhon_runtime.").unwrap_or("");
            if let Value::Module(m) = &root {
                if let Some(v) = m.members.borrow().get(sub) {
                    if matches!(v, Value::Module(_)) {
                        return Ok(v.clone());
                    }
                }
            }
            Ok(root)
        }
        // CPython's exception for a module the VM does not model (it has a
        // small native stdlib; `tyc run --compile` runs the full
        // interpreter): same type, same message, so `except ImportError`
        // and printed messages agree with `tyc build && python`.
        _ => Err(crate::error::Unwind::Exception(
            crate::error::VmException::new(
                "ModuleNotFoundError",
                format!("No module named '{name}'"),
            ),
        )),
    }
}

fn make_module(name: &str, entries: Vec<(&str, Value)>) -> Value {
    let mut map = HashMap::new();
    for (k, v) in entries {
        map.insert(k.to_owned(), v);
    }
    Value::Module(Rc::new(Module {
        name: name.to_owned(),
        members: RefCell::new(map),
        env: None,
    }))
}

/// [`make_module`] for a module whose body the VM executed: keeps the
/// namespace so later rebinding of its globals stays visible.
fn make_module_env(name: &str, entries: Vec<(&str, Value)>, env: crate::env::EnvRef) -> Value {
    let mut map = HashMap::new();
    for (k, v) in entries {
        map.insert(k.to_owned(), v);
    }
    Value::Module(Rc::new(Module {
        name: name.to_owned(),
        members: RefCell::new(map),
        env: Some(env),
    }))
}

fn nf(
    name: &'static str,
    f: impl Fn(&mut Interpreter, Vec<Value>) -> Result<Value, Unwind> + 'static,
) -> Value {
    Value::Native(Rc::new(NativeFn::new(name, f)))
}

/// Native backing for the `try_result(thunk[, on_err])` exception→Result
/// bridging combinator: run `thunk()`, returning `Ok(result)`; on a raised
/// exception return `Err(on_err(exc))` (or `Err(exc)` with no mapper).
/// Mirrors `typhon_runtime.try_result` on the compile path, and materialises
/// the caught exception as the same value an `except E as e:` handler binds,
/// so a mapper like `lambda e: str(e)` works under `tyc run`.
fn try_result_native() -> Value {
    nf("try_result", |i, args| {
        // Enforce arity (1 or 2 positional args) rather than silently ignoring
        // extras — matches the runtime `def try_result(thunk, on_err=None)`
        // signature on the compiled path, where Python raises on a 3rd arg.
        if args.is_empty() || args.len() > 2 {
            return Err(type_error(format!(
                "try_result() takes 1 or 2 positional arguments but {} were given",
                args.len()
            )));
        }
        let mut it = args.into_iter();
        let thunk = it.next().unwrap_or(Value::None);
        let on_err = it.next();
        match i.call_value(thunk, vec![], &[]) {
            Ok(v) => Ok(Value::ResultOk(Box::new(v))),
            Err(Unwind::Exception(exc)) => {
                let exc_value = match &exc.value {
                    Some(v @ (Value::Instance(_) | Value::Exception { .. })) => v.clone(),
                    _ => {
                        let exc_args = if exc.message.is_empty() {
                            Vec::new()
                        } else {
                            vec![Value::Str(Rc::new(exc.message.clone()))]
                        };
                        Value::Exception {
                            kind: Rc::new(exc.kind.clone()),
                            message: Rc::new(exc.message.clone()),
                            args: Rc::new(exc_args),
                            chain: None,
                        }
                    }
                };
                let mapped = match on_err {
                    Some(f) if !matches!(f, Value::None) => {
                        i.call_value(f, vec![exc_value], &[])?
                    }
                    _ => exc_value,
                };
                Ok(Value::ResultErr(Box::new(mapped)))
            }
            // `return`/`break`/`continue` can't escape a thunk call here, but
            // propagate anything non-exception unchanged for completeness.
            Err(other) => Err(other),
        }
    })
}

fn make_typhon_runtime_module(interp: &Interpreter) -> Value {
    let ok = interp.root.get("Ok").unwrap();
    let err = interp.root.get("Err").unwrap();
    // Submodules.
    let tasks = make_module(
        "typhon_runtime.tasks",
        vec![(
            "spawn",
            nf("spawn", |i, args| {
                // Sequential "spawn": force the coroutine now (a bare
                // callable is invoked, matching the old behaviour) and
                // wrap the result so `go f(x) -> task; await task` works.
                let v = args.into_iter().next().unwrap_or(Value::None);
                let result = match v {
                    Value::Coroutine(_) => i.force_awaitable(v)?,
                    other => i.call_value(other, vec![], &[])?,
                };
                Ok(make_task_value(result))
            }),
        )],
    );
    let lazy = make_module(
        "typhon_runtime.lazy",
        vec![
            (
                "lazy_let",
                nf("lazy_let", |i, args| {
                    // The factory must NOT run here: `lazy let X = helper()`
                    // at module level is lowered above the `def helper`, so
                    // calling it eagerly failed with `NameError` on a
                    // program `tyc build` runs fine. Hand back the same
                    // materialise-on-first-use proxy the emitted runtime
                    // uses.
                    let f = args.into_iter().next().unwrap_or(Value::None);
                    let cls = lazy_value_class(i)?;
                    i.call_value(cls, vec![f], &[])
                }),
            ),
            (
                "lazy_import",
                nf("lazy_import", |_i, _args| Ok(Value::None)),
            ),
        ],
    );
    // `freeze let X = expr` lowers to a call to `deep_freeze` imported from
    // `typhon_runtime.freeze`. The shim now performs a real deep freeze
    // (list → tuple, dict → mappingproxy-tagged dict, recursive) so that
    // `tyc run` matches `tyc build && python build/main.py` for immutability
    // semantics. Without this, mutations through aliased references
    // succeed silently in the VM where CPython would raise a TypeError.
    let freeze = make_module(
        "typhon_runtime.freeze",
        vec![(
            "deep_freeze",
            nf("deep_freeze", |_i, args| {
                let v = args.into_iter().next().unwrap_or(Value::None);
                deep_freeze_value(v)
            }),
        )],
    );
    // `EXPR as! TYPE` lowers to a call to `checked_cast` imported from
    // `typhon_runtime.cast`. The VM treats it as an identity passthrough (the
    // checker already pinned the static type; the recursive structural check
    // is enforced on the `tyc build && python` path), so the shim just returns
    // the value unchanged.
    let cast = make_module(
        "typhon_runtime.cast",
        vec![(
            "checked_cast",
            nf("checked_cast", |_i, args| {
                Ok(args.into_iter().next().unwrap_or(Value::None))
            }),
        )],
    );
    // `Result` is exposed as a type marker — the type checker uses
    // `Result[T, E]` as a typing construct; at runtime the desugared
    // module only needs the name to be defined. An identity callable
    // suffices since user code never invokes `Result(...)` directly.
    let result_marker = nf("Result", |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    let try_result = interp.root.get("try_result").unwrap();
    make_module(
        "typhon_runtime",
        vec![
            ("Ok", ok),
            ("Err", err),
            ("Result", result_marker),
            ("try_result", try_result),
            ("tasks", tasks),
            ("lazy", lazy),
            ("freeze", freeze),
            ("cast", cast),
        ],
    )
}

fn make_math_module() -> Value {
    /// Compute factorial of n as a BigInt. CPython raises ValueError for
    /// negative inputs and TypeError for non-integers.
    // Integer-domain math functions reject non-integers (CPython raises
    // `TypeError` — a `float` "cannot be interpreted as an integer", even an
    // integral one like `5.0`).
    fn require_int(v: &Value, func: &str) -> Result<num_bigint::BigInt, Unwind> {
        match v {
            Value::Int(i) => Ok(i.to_bigint()),
            Value::Bool(b) => Ok(num_bigint::BigInt::from(*b as i64)),
            _ => Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer (math.{}())",
                v.type_name(),
                func
            ))),
        }
    }

    fn bigint_factorial(n: &num_bigint::BigInt) -> Result<num_bigint::BigInt, Unwind> {
        use num_bigint::BigInt;
        use num_traits::{One, Signed};
        if n.is_negative() {
            return Err(value_error(
                "math.factorial() not defined for negative values",
            ));
        }
        let mut result = BigInt::one();
        let mut i = BigInt::from(2);
        while i <= *n {
            result *= &i;
            i += BigInt::one();
        }
        Ok(result)
    }

    /// Integer square root (floor of square root). CPython raises ValueError
    /// for negative inputs.
    fn bigint_isqrt(n: &num_bigint::BigInt) -> Result<num_bigint::BigInt, Unwind> {
        use num_bigint::BigInt;
        use num_traits::{Signed, Zero};
        if n.is_negative() {
            return Err(value_error("math.isqrt() argument must be nonnegative"));
        }
        if n.is_zero() {
            return Ok(BigInt::from(0));
        }
        // Newton's method for integer square root.
        let one = BigInt::from(1u32);
        let two = BigInt::from(2u32);
        let mut x = n.clone();
        let mut y = (n + &one) / &two;
        while y < x {
            x = y.clone();
            y = (&x + n / &x) / &two;
        }
        Ok(x)
    }

    // Binomial coefficient — uses BigInt arithmetic throughout so that large
    // inputs don't truncate via to_i64.
    fn bigint_comb_full(
        n_val: num_bigint::BigInt,
        k_val: num_bigint::BigInt,
    ) -> Result<num_bigint::BigInt, Unwind> {
        use num_bigint::BigInt;
        use num_traits::{One, Signed, Zero};
        if n_val.is_negative() || k_val.is_negative() {
            return Err(value_error("math.comb() requires non-negative integers"));
        }
        if k_val > n_val {
            return Ok(BigInt::zero());
        }
        let k2 = std::cmp::min(k_val.clone(), n_val.clone() - k_val);
        let mut result = BigInt::one();
        let mut i = BigInt::zero();
        while i < k2 {
            result = result * (n_val.clone() - i.clone()) / (i.clone() + BigInt::one());
            i += BigInt::one();
        }
        Ok(result)
    }

    fn bigint_perm(
        n_val: num_bigint::BigInt,
        k_val: num_bigint::BigInt,
    ) -> Result<num_bigint::BigInt, Unwind> {
        use num_bigint::BigInt;
        use num_traits::{One, Signed, Zero};
        if n_val.is_negative() || k_val.is_negative() {
            return Err(value_error("math.perm() requires non-negative integers"));
        }
        if k_val > n_val {
            return Ok(BigInt::zero());
        }
        let mut result = BigInt::one();
        let mut i = BigInt::zero();
        while i < k_val {
            result *= n_val.clone() - i.clone();
            i += BigInt::one();
        }
        Ok(result)
    }

    fn bigint_gcd(a: num_bigint::BigInt, b: num_bigint::BigInt) -> num_bigint::BigInt {
        use num_traits::{Signed, Zero};
        let mut a = a.abs();
        let mut b = b.abs();
        while !b.is_zero() {
            let t = b.clone();
            b = a % &t;
            a = t;
        }
        a
    }

    make_module(
        "math",
        vec![
            // Constants
            ("pi", Value::Float(std::f64::consts::PI)),
            ("e", Value::Float(std::f64::consts::E)),
            // tau = 2*pi, added in Python 3.6
            ("tau", Value::Float(std::f64::consts::TAU)),
            ("inf", Value::Float(f64::INFINITY)),
            ("nan", Value::Float(f64::NAN)),
            // ── floating-point predicates ─────────────────────────────────────
            (
                "isnan",
                nf("isnan", |_i, args| {
                    Ok(Value::Bool(single(&args, "isnan")?.to_float()?.is_nan()))
                }),
            ),
            (
                "isinf",
                nf("isinf", |_i, args| {
                    Ok(Value::Bool(
                        single(&args, "isinf")?.to_float()?.is_infinite(),
                    ))
                }),
            ),
            (
                "isfinite",
                nf("isfinite", |_i, args| {
                    Ok(Value::Bool(
                        single(&args, "isfinite")?.to_float()?.is_finite(),
                    ))
                }),
            ),
            // ── floating-point basic ──────────────────────────────────────────
            (
                "sqrt",
                nf("sqrt", |_i, args| {
                    let x = math_arg(single(&args, "sqrt")?)?;
                    math_1(x, x.sqrt(), false)
                }),
            ),
            (
                "fsum",
                nf("fsum", |i, args| {
                    let it = i.make_iter(single(&args, "fsum")?.clone())?;
                    let mut xs = Vec::new();
                    while let Some(v) = i.iter_next(&it)? {
                        xs.push(math_arg(&v)?);
                    }
                    Ok(Value::Float(math_fsum(&xs)?))
                }),
            ),
            // `floor` / `ceil` / `trunc` all return a Python `int`, which is
            // arbitrary-precision. Going through `as i64` saturated at
            // `i64::MAX` for any |x| >= 2^63, so `math.floor(1e30)` silently
            // produced 9223372036854775807 — a wrong answer with no error, in
            // the one place the VM's bignum support is supposed to matter.
            (
                "floor",
                nf("floor", |_i, args| {
                    Value::Float(single(&args, "floor")?.to_float()?.floor())
                        .to_bigint()
                        .map(|b| Value::Int(VmInt::from(b)))
                }),
            ),
            (
                "ceil",
                nf("ceil", |_i, args| {
                    Value::Float(single(&args, "ceil")?.to_float()?.ceil())
                        .to_bigint()
                        .map(|b| Value::Int(VmInt::from(b)))
                }),
            ),
            (
                "trunc",
                nf("trunc", |_i, args| {
                    // Returns an int, consistent with CPython math.trunc.
                    Value::Float(single(&args, "trunc")?.to_float()?.trunc())
                        .to_bigint()
                        .map(|b| Value::Int(VmInt::from(b)))
                }),
            ),
            (
                "fabs",
                nf("fabs", |_i, args| {
                    Ok(Value::Float(single(&args, "fabs")?.to_float()?.abs()))
                }),
            ),
            (
                "copysign",
                nf("copysign", |_i, args| {
                    let x = args
                        .first()
                        .ok_or_else(|| type_error("copysign() needs args"))?
                        .to_float()?;
                    let y = args
                        .get(1)
                        .ok_or_else(|| type_error("copysign() needs args"))?
                        .to_float()?;
                    Ok(Value::Float(x.copysign(y)))
                }),
            ),
            (
                "prod",
                nf("prod", |i, args| {
                    // math.prod(iterable, *, start=1) — multiply all elements.
                    let it = i.make_iter(single(&args, "prod")?.clone())?;
                    let mut acc = Value::Int(VmInt::from(1));
                    while let Some(v) = i.iter_next(&it)? {
                        acc = i.binop(&acc, ruff_python_ast::Operator::Mult, &v)?;
                    }
                    Ok(acc)
                }),
            ),
            (
                "hypot",
                nf("hypot", |_i, args| {
                    // CPython math.hypot accepts multiple args; we handle
                    // the common 2-arg case and fall back to n-arg norm.
                    let sum_sq: f64 = args
                        .iter()
                        .map(|v| v.to_float().map(|x| x * x))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .sum();
                    Ok(Value::Float(sum_sq.sqrt()))
                }),
            ),
            (
                "dist",
                nf("dist", |i, args| {
                    // math.dist(p, q) — Euclidean distance between two points.
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("dist() needs two args"))?
                        .clone();
                    let q = args
                        .get(1)
                        .ok_or_else(|| type_error("dist() needs two args"))?
                        .clone();
                    let mut ps: Vec<f64> = Vec::new();
                    let it = i.make_iter(p)?;
                    while let Some(v) = i.iter_next(&it)? {
                        ps.push(v.to_float()?);
                    }
                    let mut qs: Vec<f64> = Vec::new();
                    let it2 = i.make_iter(q)?;
                    while let Some(v) = i.iter_next(&it2)? {
                        qs.push(v.to_float()?);
                    }
                    if ps.len() != qs.len() {
                        return Err(value_error(
                            "math.dist() points must have the same dimension",
                        ));
                    }
                    let sum_sq: f64 = ps
                        .iter()
                        .zip(qs.iter())
                        .map(|(a, b)| (a - b) * (a - b))
                        .sum();
                    Ok(Value::Float(sum_sq.sqrt()))
                }),
            ),
            // ── logarithms / exponentials ─────────────────────────────────────
            (
                "log",
                nf("log", |_i, args| {
                    // `math.log(x[, base])`: CPython takes the natural log of
                    // each operand (a huge int through its bit length) and
                    // divides; `log(0)` / `log(-x)` are domain errors and a
                    // base of 1 is a float division by zero.
                    fn ln_of(v: &Value) -> Result<f64, Unwind> {
                        if let Value::Int(n) = v {
                            if n.is_negative() || n.is_zero() {
                                return Err(value_error("math domain error"));
                            }
                            let f = n.to_f64();
                            if f.is_infinite() {
                                // ln(n) = ln(n / 2^k) + k·ln 2 for a bignum.
                                let bits = n.bits();
                                let shift = bits.saturating_sub(53) as usize;
                                let top = n.shr(shift).to_f64();
                                return Ok(top.ln() + shift as f64 * std::f64::consts::LN_2);
                            }
                            return Ok(f.ln());
                        }
                        let x = math_arg(v)?;
                        if x.is_nan() {
                            return Ok(x);
                        }
                        if x <= 0.0 {
                            return Err(value_error("math domain error"));
                        }
                        Ok(x.ln())
                    }
                    let x = ln_of(
                        args.first()
                            .ok_or_else(|| type_error("log() needs an arg"))?,
                    )?;
                    match args.get(1) {
                        Some(b) => {
                            let den = ln_of(b)?;
                            if den == 0.0 {
                                return Err(Unwind::Exception(crate::error::VmException::new(
                                    "ZeroDivisionError",
                                    "float division by zero",
                                )));
                            }
                            Ok(Value::Float(x / den))
                        }
                        None => Ok(Value::Float(x)),
                    }
                }),
            ),
            (
                "log2",
                nf("log2", |_i, args| {
                    let x = math_arg(single(&args, "log2")?)?;
                    math_1(x, x.log2(), false)
                }),
            ),
            (
                "log10",
                nf("log10", |_i, args| {
                    let x = math_arg(single(&args, "log10")?)?;
                    math_1(x, x.log10(), false)
                }),
            ),
            (
                "expm1",
                nf("expm1", |_i, args| {
                    // exp(x) - 1 with better precision near 0, matching
                    // CPython math.expm1.
                    let x = math_arg(single(&args, "expm1")?)?;
                    math_1(x, x.exp_m1(), true)
                }),
            ),
            (
                "log1p",
                nf("log1p", |_i, args| {
                    // log(1 + x) with better precision near 0, matching
                    // CPython math.log1p.
                    let x = math_arg(single(&args, "log1p")?)?;
                    math_1(x, x.ln_1p(), false)
                }),
            ),
            (
                "exp",
                nf("exp", |_i, args| {
                    let x = math_arg(single(&args, "exp")?)?;
                    math_1(x, x.exp(), true)
                }),
            ),
            // ── trig ──────────────────────────────────────────────────────────
            (
                "sin",
                nf("sin", |_i, args| {
                    let x = math_arg(single(&args, "sin")?)?;
                    math_1(x, x.sin(), false)
                }),
            ),
            (
                "cos",
                nf("cos", |_i, args| {
                    let x = math_arg(single(&args, "cos")?)?;
                    math_1(x, x.cos(), false)
                }),
            ),
            (
                "tan",
                nf("tan", |_i, args| {
                    let x = math_arg(single(&args, "tan")?)?;
                    math_1(x, x.tan(), false)
                }),
            ),
            (
                "asin",
                nf("asin", |_i, args| {
                    let x = math_arg(single(&args, "asin")?)?;
                    math_1(x, x.asin(), false)
                }),
            ),
            (
                "acos",
                nf("acos", |_i, args| {
                    let x = math_arg(single(&args, "acos")?)?;
                    math_1(x, x.acos(), false)
                }),
            ),
            (
                "atan",
                nf("atan", |_i, args| {
                    Ok(Value::Float(single(&args, "atan")?.to_float()?.atan()))
                }),
            ),
            (
                "atan2",
                nf("atan2", |_i, args| {
                    let y = args
                        .first()
                        .ok_or_else(|| type_error("atan2() needs args"))?
                        .to_float()?;
                    let x = args
                        .get(1)
                        .ok_or_else(|| type_error("atan2() needs args"))?
                        .to_float()?;
                    Ok(Value::Float(y.atan2(x)))
                }),
            ),
            (
                "degrees",
                nf("degrees", |_i, args| {
                    Ok(Value::Float(
                        single(&args, "degrees")?.to_float()?.to_degrees(),
                    ))
                }),
            ),
            (
                "radians",
                nf("radians", |_i, args| {
                    Ok(Value::Float(
                        single(&args, "radians")?.to_float()?.to_radians(),
                    ))
                }),
            ),
            // ── floating-point remainder ──────────────────────────────────────
            (
                "fmod",
                nf("fmod", |_i, args| {
                    let x = args
                        .first()
                        .ok_or_else(|| type_error("fmod() needs args"))?
                        .to_float()?;
                    let y = args
                        .get(1)
                        .ok_or_else(|| type_error("fmod() needs args"))?
                        .to_float()?;
                    // Rust's % for floats matches C fmod semantics.
                    Ok(Value::Float(x % y))
                }),
            ),
            (
                "remainder",
                nf("remainder", |_i, args| {
                    let x = args
                        .first()
                        .ok_or_else(|| type_error("remainder() needs args"))?
                        .to_float()?;
                    let y = args
                        .get(1)
                        .ok_or_else(|| type_error("remainder() needs args"))?
                        .to_float()?;
                    // IEEE 754 remainder — round-half-to-even.
                    Ok(Value::Float(x - (x / y).round_ties_even() * y))
                }),
            ),
            (
                "pow",
                nf("pow", |_i, args| {
                    let a = math_arg(args.first().ok_or_else(|| type_error("pow() needs args"))?)?;
                    let b = math_arg(args.get(1).ok_or_else(|| type_error("pow() needs args"))?)?;
                    let r = a.powf(b);
                    // CPython's `math_pow`: a NaN from non-NaN operands (a
                    // negative base with a fractional exponent) is a domain
                    // error; an overflow is a range error, except that a zero
                    // base with a negative exponent is a domain error too.
                    if r.is_nan() && !a.is_nan() && !b.is_nan() {
                        return Err(value_error("math domain error"));
                    }
                    if r.is_infinite() && a.is_finite() && b.is_finite() {
                        if a == 0.0 {
                            return Err(value_error("math domain error"));
                        }
                        return Err(Unwind::Exception(crate::error::VmException::new(
                            "OverflowError",
                            "math range error",
                        )));
                    }
                    Ok(Value::Float(r))
                }),
            ),
            // ── integer-domain (return int) ───────────────────────────────────
            (
                "gcd",
                nf("gcd", |_i, args| {
                    if args.is_empty() {
                        return Ok(Value::Int(VmInt::from(0)));
                    }
                    let mut acc = require_int(&args[0], "gcd")?;
                    for v in &args[1..] {
                        acc = bigint_gcd(acc, require_int(v, "gcd")?);
                    }
                    Ok(Value::Int(VmInt::from(acc)))
                }),
            ),
            (
                "lcm",
                nf("lcm", |_i, args| {
                    use num_traits::{Signed, Zero};
                    if args.is_empty() {
                        return Ok(Value::Int(VmInt::from(1)));
                    }
                    let mut acc = require_int(&args[0], "lcm")?;
                    for v in &args[1..] {
                        let b = require_int(v, "lcm")?;
                        if acc.is_zero() || b.is_zero() {
                            acc = num_bigint::BigInt::from(0);
                        } else {
                            let g = bigint_gcd(acc.clone(), b.clone());
                            acc = (acc / g) * b;
                        }
                    }
                    Ok(Value::Int(VmInt::from(acc.abs())))
                }),
            ),
            (
                "factorial",
                nf("factorial", |_i, args| {
                    let n = require_int(single(&args, "factorial")?, "factorial")?;
                    Ok(Value::Int(VmInt::from(bigint_factorial(&n)?)))
                }),
            ),
            (
                "isqrt",
                nf("isqrt", |_i, args| {
                    let n = require_int(single(&args, "isqrt")?, "isqrt")?;
                    Ok(Value::Int(VmInt::from(bigint_isqrt(&n)?)))
                }),
            ),
            (
                "comb",
                nf("comb", |_i, args| {
                    let n = require_int(
                        args.first()
                            .ok_or_else(|| type_error("comb() needs args"))?,
                        "comb",
                    )?;
                    let k = require_int(
                        args.get(1).ok_or_else(|| type_error("comb() needs args"))?,
                        "comb",
                    )?;
                    Ok(Value::Int(VmInt::from(bigint_comb_full(n, k)?)))
                }),
            ),
            (
                "perm",
                nf("perm", |_i, args| {
                    let n = require_int(
                        args.first()
                            .ok_or_else(|| type_error("perm() needs args"))?,
                        "perm",
                    )?;
                    // If k is omitted, perm(n) = n!
                    let k = match args.get(1) {
                        Some(v) => require_int(v, "perm")?,
                        None => n.clone(),
                    };
                    Ok(Value::Int(VmInt::from(bigint_perm(n, k)?)))
                }),
            ),
        ],
    )
}

// ── Filesystem natives shared by the `os` / `io` / `pathlib` / `shutil` /
// `tempfile` / `glob` shims ────────────────────────────────────────────────

/// How an `OSError` names a filename argument: a str quoted, any other
/// object by its repr.
fn filename_repr(interp: &mut Interpreter, v: &Value) -> Result<String, Unwind> {
    match v {
        Value::Str(s) => Ok(crate::value::python_repr_str(s)),
        other => interp.repr_of(other),
    }
}

/// `os.fspath(v)`: a str as is, bytes decoded, an object with `__fspath__`
/// (a `Path`) through it. Returns the path together with the display form
/// CPython puts in an `OSError` — the argument's repr, so a `Path` given to
/// a raw `os` function shows as `PosixPath('…')` while a str shows quoted.
pub(crate) fn fspath_pair(interp: &mut Interpreter, v: &Value) -> Result<(String, String), Unwind> {
    match v {
        Value::Str(s) => Ok(((**s).clone(), crate::value::python_repr_str(s))),
        Value::Bytes(b) => {
            let s = String::from_utf8_lossy(b).into_owned();
            Ok((s, v.py_repr()))
        }
        Value::Instance(inst) => {
            let Some(m) = interp.find_method(&inst.class, "__fspath__") else {
                return Err(type_error(format!(
                    "expected str, bytes or os.PathLike object, not {}",
                    inst.class.name
                )));
            };
            let r = interp.call_value(
                Value::BoundMethod {
                    receiver: Box::new(v.clone()),
                    function: m,
                },
                vec![],
                &[],
            )?;
            match r {
                Value::Str(s) => {
                    let display = interp.repr_of(v)?;
                    Ok(((*s).clone(), display))
                }
                other => Err(type_error(format!(
                    "expected {}.__fspath__() to return str or bytes, not {}",
                    inst.class.name,
                    other.type_name()
                ))),
            }
        }
        other => Err(type_error(format!(
            "expected str, bytes or os.PathLike object, not {}",
            other.type_name()
        ))),
    }
}

pub(crate) fn fspath_of(interp: &mut Interpreter, v: &Value) -> Result<String, Unwind> {
    fspath_pair(interp, v).map(|(p, _)| p)
}

/// The exception kinds that carry `errno` / `strerror` / `filename`.
pub(crate) fn is_os_error_kind(kind: &str) -> bool {
    matches!(
        kind,
        "OSError"
            | "IOError"
            | "EnvironmentError"
            | "FileNotFoundError"
            | "FileExistsError"
            | "PermissionError"
            | "IsADirectoryError"
            | "NotADirectoryError"
            | "InterruptedError"
            | "TimeoutError"
            | "BlockingIOError"
            | "ChildProcessError"
            | "ProcessLookupError"
            | "ConnectionError"
            | "BrokenPipeError"
            | "ConnectionResetError"
            | "ConnectionRefusedError"
            | "ConnectionAbortedError"
    )
}

/// `OSError(errno, …)` maps itself onto the matching subclass.
pub(crate) fn os_error_kind(errno: i64) -> &'static str {
    match errno {
        2 => "FileNotFoundError",
        17 => "FileExistsError",
        21 => "IsADirectoryError",
        20 => "NotADirectoryError",
        1 | 13 => "PermissionError",
        3 => "ProcessLookupError",
        4 => "InterruptedError",
        10 => "ChildProcessError",
        11 | 115 => "BlockingIOError",
        32 => "BrokenPipeError",
        103 => "ConnectionAbortedError",
        104 => "ConnectionResetError",
        111 => "ConnectionRefusedError",
        110 => "TimeoutError",
        _ => "OSError",
    }
}

/// `os.strerror`.
pub(crate) fn strerror_text(errno: i64) -> String {
    match errno {
        1 => "Operation not permitted",
        2 => "No such file or directory",
        3 => "No such process",
        4 => "Interrupted system call",
        5 => "Input/output error",
        9 => "Bad file descriptor",
        11 => "Resource temporarily unavailable",
        12 => "Cannot allocate memory",
        13 => "Permission denied",
        17 => "File exists",
        20 => "Not a directory",
        21 => "Is a directory",
        22 => "Invalid argument",
        24 => "Too many open files",
        28 => "No space left on device",
        32 => "Broken pipe",
        36 => "File name too long",
        39 => "Directory not empty",
        110 => "Connection timed out",
        111 => "Connection refused",
        _ => return format!("Unknown error {errno}"),
    }
    .to_owned()
}

/// An `OSError`-family value in CPython's shape: `args == (errno, strerror)`,
/// `str()` is `[Errno N] strerror: 'filename'` (`-> 'filename2'` for the
/// two-path calls). The filename display strings are already reprs.
pub(crate) fn os_error_value(
    kind: &str,
    errno: i64,
    strerror: &str,
    filename: Option<&str>,
    filename2: Option<&str>,
) -> Value {
    os_error_value_of(
        kind,
        Value::Int(VmInt::from(errno)),
        strerror,
        filename,
        filename2,
    )
}

/// As `os_error_value`, but for an errno CPython never interpreted: it maps
/// 2–5 constructor arguments onto `(errno, strerror, filename, winerror,
/// filename2)` whatever their types, so `OSError('a', 'b', 'c')` still reads
/// `[Errno a] b: 'c'`. Only an *integer* errno selects a concrete subclass.
pub(crate) fn os_error_value_of(
    kind: &str,
    errno: Value,
    strerror: &str,
    filename: Option<&str>,
    filename2: Option<&str>,
) -> Value {
    let mut message = format!("[Errno {}] {strerror}", errno.py_str());
    if let Some(f) = filename {
        message.push_str(": ");
        message.push_str(f);
        if let Some(f2) = filename2 {
            message.push_str(" -> ");
            message.push_str(f2);
        }
    }
    Value::Exception {
        kind: Rc::new(kind.to_owned()),
        message: Rc::new(message),
        args: Rc::new(vec![errno, Value::Str(Rc::new(strerror.to_owned()))]),
        chain: None,
    }
}

pub(crate) fn os_error_unwind(
    errno: i64,
    strerror: &str,
    filename: Option<&str>,
    filename2: Option<&str>,
) -> Unwind {
    let v = os_error_value(os_error_kind(errno), errno, strerror, filename, filename2);
    let Value::Exception { kind, message, .. } = &v else {
        unreachable!()
    };
    Unwind::Exception(
        crate::error::VmException::new(kind.as_str(), (**message).clone()).with_value(v),
    )
}

/// Undo a Python string repr (`'a\'b'` → `a'b`) for `OSError.filename`.
fn unrepr_str(text: &str) -> Option<String> {
    let quote = text.chars().next()?;
    if quote != '\'' && quote != '"' || !text.ends_with(quote) || text.len() < 2 {
        return None;
    }
    let inner = &text[1..text.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'x' => {
                let h: String = chars.by_ref().take(2).collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
            }
            'u' => {
                let h: String = chars.by_ref().take(4).collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
            }
            'U' => {
                let h: String = chars.by_ref().take(8).collect();
                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Some(out)
}

/// `OSError.filename` / `filename2`, recovered from the message the VM built
/// (`[Errno N] strerror: 'name' -> 'name2'`).
pub(crate) fn os_error_filename(message: &str, second: bool) -> Value {
    let Some(rest) = message.strip_prefix("[Errno ") else {
        return Value::None;
    };
    let Some(idx) = rest.find("] ") else {
        return Value::None;
    };
    let rest = &rest[idx + 2..];
    // The filename part starts at the first ": " followed by a repr.
    let mut start = None;
    let mut search = 0;
    while let Some(pos) = rest[search..].find(": ") {
        let candidate = &rest[search + pos + 2..];
        if candidate.starts_with('\'')
            || candidate.starts_with('"')
            || candidate
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
                && candidate.contains('(')
        {
            start = Some(search + pos + 2);
            break;
        }
        search += pos + 2;
    }
    let Some(start) = start else {
        return Value::None;
    };
    let names = &rest[start..];
    let (first, second_name) = match names.rfind("' -> ") {
        Some(p) => (&names[..p + 1], Some(&names[p + 5..])),
        None => (names, None),
    };
    let chosen = if second { second_name } else { Some(first) };
    match chosen {
        Some(text) => match unrepr_str(text) {
            Some(s) => Value::Str(Rc::new(s)),
            None => Value::Str(Rc::new(text.to_owned())),
        },
        None => Value::None,
    }
}

/// `strerror` for an `io::Error`: its message with Rust's " (os error N)"
/// suffix removed.
fn strerror_of(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(i) => text[..i].to_owned(),
        None => text,
    }
}

fn errno_of(e: &std::io::Error) -> i64 {
    if let Some(n) = e.raw_os_error() {
        return n as i64;
    }
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => 2,
        ErrorKind::PermissionDenied => 13,
        ErrorKind::AlreadyExists => 17,
        ErrorKind::InvalidInput => 22,
        _ => 5,
    }
}

/// Map an `std::io::Error` from a filesystem native onto CPython's
/// exception: the errno-selected `OSError` subclass with the
/// `[Errno N] strerror: 'path'` message. `display` is the path's repr.
pub(crate) fn fs_unwind(display: &str, e: std::io::Error) -> Unwind {
    let errno = errno_of(&e);
    let strerror = if e.raw_os_error().is_some() {
        strerror_of(&e)
    } else {
        strerror_text(errno)
    };
    os_error_unwind(errno, &strerror, Some(display), None)
}

fn fs_unwind2(display_src: &str, display_dst: &str, e: std::io::Error) -> Unwind {
    let errno = errno_of(&e);
    let strerror = if e.raw_os_error().is_some() {
        strerror_of(&e)
    } else {
        strerror_text(errno)
    };
    os_error_unwind(errno, &strerror, Some(display_src), Some(display_dst))
}

fn path_arg(
    interp: &mut Interpreter,
    args: &[Value],
    idx: usize,
    fname: &str,
) -> Result<(String, String), Unwind> {
    let v = args
        .get(idx)
        .ok_or_else(|| type_error(format!("{fname}() missing required argument")))?;
    fspath_pair(interp, v)
}

#[cfg(unix)]
fn stat_tuple(m: &std::fs::Metadata) -> Value {
    use std::os::unix::fs::MetadataExt;
    let int = |n: i64| Value::Int(VmInt::from(n));
    let secs = |s: i64, ns: i64| Value::Float(s as f64 + ns as f64 * 1e-9);
    Value::Tuple(Rc::new(vec![
        int(m.mode() as i64),
        int(m.ino() as i64),
        int(m.dev() as i64),
        int(m.nlink() as i64),
        int(m.uid() as i64),
        int(m.gid() as i64),
        int(m.size() as i64),
        int(m.atime()),
        int(m.mtime()),
        int(m.ctime()),
        secs(m.atime(), m.atime_nsec()),
        secs(m.mtime(), m.mtime_nsec()),
        secs(m.ctime(), m.ctime_nsec()),
        int(m.atime() * 1_000_000_000 + m.atime_nsec()),
        int(m.mtime() * 1_000_000_000 + m.mtime_nsec()),
        int(m.ctime() * 1_000_000_000 + m.ctime_nsec()),
        int(m.blksize() as i64),
        int(m.blocks() as i64),
    ]))
}

#[cfg(not(unix))]
fn stat_tuple(m: &std::fs::Metadata) -> Value {
    let int = |n: i64| Value::Int(VmInt::from(n));
    let when = |t: std::io::Result<std::time::SystemTime>| -> (i64, i64) {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64, d.subsec_nanos() as i64))
            .unwrap_or((0, 0))
    };
    let (a, an) = when(m.accessed());
    let (mt, mn) = when(m.modified());
    let (c, cn) = when(m.created());
    let mode: i64 = if m.is_dir() { 0o040755 } else { 0o100644 };
    Value::Tuple(Rc::new(vec![
        int(mode),
        int(0),
        int(0),
        int(1),
        int(0),
        int(0),
        int(m.len() as i64),
        int(a),
        int(mt),
        int(c),
        Value::Float(a as f64 + an as f64 * 1e-9),
        Value::Float(mt as f64 + mn as f64 * 1e-9),
        Value::Float(c as f64 + cn as f64 * 1e-9),
        int(a * 1_000_000_000 + an),
        int(mt * 1_000_000_000 + mn),
        int(c * 1_000_000_000 + cn),
        int(4096),
        int(m.len() as i64 / 512),
    ]))
}

/// Lexical `os.path.normpath(os.path.abspath(p))`, the non-strict
/// `realpath` fallback for a path that does not exist.
fn lexical_abspath(path: &str) -> String {
    let mut full = if path.starts_with('/') {
        path.to_owned()
    } else {
        let cwd = std::env::current_dir()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_owned());
        format!("{cwd}/{path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    let owned = std::mem::take(&mut full);
    for comp in owned.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The `_fs_*` natives every filesystem shim is seeded with.
fn fs_natives() -> Vec<(&'static str, Value)> {
    vec![
        (
            "_fspath",
            nf("_fspath", |i, args| {
                let v = single(&args, "fspath")?;
                match v {
                    Value::Bytes(_) => Ok(v.clone()),
                    _ => Ok(Value::Str(Rc::new(fspath_of(i, v)?))),
                }
            }),
        ),
        (
            "_fs_read",
            nf("_fs_read", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "read")?;
                std::fs::read(&path)
                    .map(|b| Value::Bytes(Rc::new(b)))
                    .map_err(|e| fs_unwind(&display, e))
            }),
        ),
        (
            "_fs_write",
            nf("_fs_write", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "write")?;
                let data: Vec<u8> = match args.get(1) {
                    Some(Value::Bytes(b)) => (**b).clone(),
                    Some(Value::Str(s)) => s.as_bytes().to_vec(),
                    _ => return Err(type_error("a bytes-like object is required")),
                };
                let mode = args
                    .get(2)
                    .map(|v| v.py_str())
                    .unwrap_or_else(|| "w".into());
                use std::io::Write;
                let result = match mode.as_str() {
                    "a" => std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .and_then(|mut f| f.write_all(&data)),
                    "x" => std::fs::File::create_new(&path).and_then(|mut f| f.write_all(&data)),
                    _ => std::fs::write(&path, &data),
                };
                result.map_err(|e| fs_unwind(&display, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_exists",
            nf("_fs_exists", |i, args| {
                let (path, _) = path_arg(i, &args, 0, "exists")?;
                Ok(Value::Bool(std::path::Path::new(&path).exists()))
            }),
        ),
        (
            "_fs_stat",
            nf("_fs_stat", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "stat")?;
                let follow = args.get(1).map(|v| v.truthy()).unwrap_or(true);
                let meta = if follow {
                    std::fs::metadata(&path)
                } else {
                    std::fs::symlink_metadata(&path)
                };
                meta.map(|m| stat_tuple(&m))
                    .map_err(|e| fs_unwind(&display, e))
            }),
        ),
        (
            "_fs_listdir",
            nf("_fs_listdir", |i, args| {
                let (path, display) = match args.first() {
                    Some(v) => fspath_pair(i, v)?,
                    None => (".".to_owned(), "'.'".to_owned()),
                };
                let mut names: Vec<Value> = Vec::new();
                for entry in std::fs::read_dir(&path).map_err(|e| fs_unwind(&display, e))? {
                    let entry = entry.map_err(|e| fs_unwind(&display, e))?;
                    names.push(Value::Str(Rc::new(
                        entry.file_name().to_string_lossy().into_owned(),
                    )));
                }
                Ok(Value::List(Rc::new(RefCell::new(names))))
            }),
        ),
        (
            "_fs_mkdir",
            nf("_fs_mkdir", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "mkdir")?;
                std::fs::create_dir(&path).map_err(|e| fs_unwind(&display, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_rmdir",
            nf("_fs_rmdir", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "rmdir")?;
                std::fs::remove_dir(&path).map_err(|e| fs_unwind(&display, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_unlink",
            nf("_fs_unlink", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "unlink")?;
                std::fs::remove_file(&path).map_err(|e| fs_unwind(&display, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_rename",
            nf("_fs_rename", |i, args| {
                let (src, dsrc) = path_arg(i, &args, 0, "rename")?;
                let (dst, ddst) = path_arg(i, &args, 1, "rename")?;
                std::fs::rename(&src, &dst).map_err(|e| fs_unwind2(&dsrc, &ddst, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_getcwd",
            nf("_fs_getcwd", |_i, _args| {
                let cwd = std::env::current_dir().map_err(|e| fs_unwind("'.'", e))?;
                Ok(Value::Str(Rc::new(cwd.to_string_lossy().into_owned())))
            }),
        ),
        (
            "_fs_chdir",
            nf("_fs_chdir", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "chdir")?;
                std::env::set_current_dir(&path).map_err(|e| fs_unwind(&display, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_realpath",
            nf("_fs_realpath", |i, args| {
                let (path, _) = path_arg(i, &args, 0, "realpath")?;
                let resolved = match std::fs::canonicalize(&path) {
                    Ok(p) => p.to_string_lossy().into_owned(),
                    Err(_) => lexical_abspath(&path),
                };
                Ok(Value::Str(Rc::new(resolved)))
            }),
        ),
        (
            "_fs_readlink",
            nf("_fs_readlink", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "readlink")?;
                std::fs::read_link(&path)
                    .map(|p| Value::Str(Rc::new(p.to_string_lossy().into_owned())))
                    .map_err(|e| fs_unwind(&display, e))
            }),
        ),
        (
            "_fs_symlink",
            nf("_fs_symlink", |i, args| {
                let (src, _) = path_arg(i, &args, 0, "symlink")?;
                let (dst, ddst) = path_arg(i, &args, 1, "symlink")?;
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&src, &dst).map_err(|e| fs_unwind(&ddst, e))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = (src, ddst);
                    return Err(os_error("symlink is unsupported on this platform".into()));
                }
                Ok(Value::None)
            }),
        ),
        (
            "_fs_home",
            nf("_fs_home", |_i, _args| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
                Ok(Value::Str(Rc::new(home)))
            }),
        ),
        (
            "_fs_touch",
            nf("_fs_touch", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "touch")?;
                let p = std::path::Path::new(&path);
                let result = if p.exists() {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(p)
                        .and_then(|f| f.set_modified(std::time::SystemTime::now()))
                } else {
                    std::fs::File::create(p).map(|_| ())
                };
                result.map_err(|e| fs_unwind(&display, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_copyfile",
            nf("_fs_copyfile", |i, args| {
                let (src, dsrc) = path_arg(i, &args, 0, "copyfile")?;
                let (dst, ddst) = path_arg(i, &args, 1, "copyfile")?;
                // Errors on the source (missing, a directory) name the source;
                // everything after that names the destination.
                if std::path::Path::new(&src).is_dir() {
                    return Err(os_error_unwind(21, "Is a directory", Some(&dsrc), None));
                }
                let data = std::fs::read(&src).map_err(|e| fs_unwind(&dsrc, e))?;
                std::fs::write(&dst, &data).map_err(|e| fs_unwind(&ddst, e))?;
                Ok(Value::None)
            }),
        ),
        (
            "_fs_samefile",
            nf("_fs_samefile", |i, args| {
                let (a, da) = path_arg(i, &args, 0, "samefile")?;
                let (b, db) = path_arg(i, &args, 1, "samefile")?;
                let ma = std::fs::metadata(&a).map_err(|e| fs_unwind(&da, e))?;
                let mb = std::fs::metadata(&b).map_err(|e| fs_unwind(&db, e))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    Ok(Value::Bool(ma.dev() == mb.dev() && ma.ino() == mb.ino()))
                }
                #[cfg(not(unix))]
                {
                    let _ = (ma, mb);
                    Ok(Value::Bool(
                        std::fs::canonicalize(&a).ok() == std::fs::canonicalize(&b).ok(),
                    ))
                }
            }),
        ),
        (
            "_fs_chmod",
            nf("_fs_chmod", |i, args| {
                let (path, display) = path_arg(i, &args, 0, "chmod")?;
                let mode = args
                    .get(1)
                    .map(|v| v.to_int())
                    .transpose()?
                    .unwrap_or(0o644);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode as u32))
                        .map_err(|e| fs_unwind(&display, e))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = (path, display, mode);
                }
                Ok(Value::None)
            }),
        ),
        (
            "_fs_getpid",
            nf("_fs_getpid", |_i, _args| {
                Ok(Value::Int(VmInt::from(std::process::id() as i64)))
            }),
        ),
        (
            "_fs_getppid",
            nf("_fs_getppid", |_i, _args| {
                let ppid = std::fs::read_to_string("/proc/self/stat")
                    .ok()
                    .and_then(|s| {
                        let after = s.rfind(')')?;
                        s[after + 1..]
                            .split_whitespace()
                            .nth(1)?
                            .parse::<i64>()
                            .ok()
                    })
                    .unwrap_or(1);
                Ok(Value::Int(VmInt::from(ppid)))
            }),
        ),
        (
            "_fs_getuid",
            nf("_fs_getuid", |_i, _args| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let uid = std::fs::metadata("/proc/self")
                        .map(|m| m.uid())
                        .unwrap_or(0);
                    Ok(Value::Int(VmInt::from(uid as i64)))
                }
                #[cfg(not(unix))]
                {
                    Ok(Value::Int(VmInt::from(0)))
                }
            }),
        ),
        (
            "_fs_getgid",
            nf("_fs_getgid", |_i, _args| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let gid = std::fs::metadata("/proc/self")
                        .map(|m| m.gid())
                        .unwrap_or(0);
                    Ok(Value::Int(VmInt::from(gid as i64)))
                }
                #[cfg(not(unix))]
                {
                    Ok(Value::Int(VmInt::from(0)))
                }
            }),
        ),
        (
            "_fs_cpu_count",
            nf("_fs_cpu_count", |_i, _args| {
                let n = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1);
                Ok(Value::Int(VmInt::from(n as i64)))
            }),
        ),
        (
            "_fs_system",
            nf("_fs_system", |_i, args| {
                let cmd = single(&args, "system")?.py_str();
                let status = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .status()
                    .map_err(|e| fs_unwind("'sh'", e))?;
                // POSIX wait status: the exit code in the high byte.
                let code = status.code().unwrap_or(1) as i64;
                Ok(Value::Int(VmInt::from(code << 8)))
            }),
        ),
        (
            "_fs_urandom",
            nf("_fs_urandom", |_i, args| {
                use std::hash::{BuildHasher, Hasher};
                let n = single(&args, "urandom")?.to_int()?.max(0) as usize;
                let mut out = Vec::with_capacity(n);
                while out.len() < n {
                    let word = std::collections::hash_map::RandomState::new()
                        .build_hasher()
                        .finish();
                    out.extend_from_slice(&word.to_le_bytes());
                }
                out.truncate(n);
                Ok(Value::Bytes(Rc::new(out)))
            }),
        ),
        (
            "_fs_tempdir",
            nf("_fs_tempdir", |_i, _args| {
                for var in ["TMPDIR", "TEMP", "TMP"] {
                    if let Ok(dir) = std::env::var(var) {
                        if !dir.is_empty() && std::path::Path::new(&dir).is_dir() {
                            return Ok(Value::Str(Rc::new(dir.trim_end_matches('/').to_owned())));
                        }
                    }
                }
                for dir in ["/tmp", "/var/tmp", "/usr/tmp"] {
                    if std::path::Path::new(dir).is_dir() {
                        return Ok(Value::Str(Rc::new(dir.to_owned())));
                    }
                }
                Ok(Value::Str(Rc::new(
                    std::env::current_dir()
                        .map(|c| c.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| ".".to_owned()),
                )))
            }),
        ),
        (
            "_fs_disk_usage",
            nf("_fs_disk_usage", |_i, _args| {
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Int(VmInt::from(0)),
                    Value::Int(VmInt::from(0)),
                ])))
            }),
        ),
        (
            "_fs_strerror",
            nf("_fs_strerror", |_i, args| {
                let code = single(&args, "strerror")?.to_int()?;
                Ok(Value::Str(Rc::new(strerror_text(code))))
            }),
        ),
    ]
}

/// A module built from a Python shim once per VM run.
fn cached_shim_module(
    interp: &mut Interpreter,
    key: &str,
    build: impl FnOnce(&mut Interpreter) -> Result<Value, Unwind>,
) -> Result<Value, Unwind> {
    let cache_key = format!("__builtin__:{key}");
    if let Some(v) = interp.module_cache.get(&cache_key) {
        return Ok(v.clone());
    }
    let v = build(interp)?;
    interp.module_cache.insert(cache_key, v.clone());
    Ok(v)
}

fn module_from_members(
    name: &str,
    members: Vec<(String, Value)>,
    env: crate::env::EnvRef,
) -> Value {
    let entries: Vec<(&str, Value)> = members
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    make_module_env(name, entries, env)
}

fn public_members(members: Vec<(String, Value)>) -> Vec<(String, Value)> {
    members
        .into_iter()
        .filter(|(k, _)| !k.starts_with("_fs_") && k != "_fspath" && k != "_environ")
        .collect()
}

fn make_os_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_shim_module(interp, "os", |interp| {
        let env_dict = {
            let mut m: DictMap = IndexMap::new();
            for (k, v) in std::env::vars() {
                m.insert(HashKey::Str(Rc::new(k)), Value::Str(Rc::new(v)));
            }
            Value::Dict(Rc::new(RefCell::new(m)))
        };
        let mut seed = fs_natives();
        seed.push(("_environ", env_dict.clone()));
        let (path_members, path_env) = compile_shim(interp, shims::POSIXPATH, seed)?;
        let path_module = module_from_members("posixpath", public_members(path_members), path_env);
        let mut seed = fs_natives();
        seed.push(("path", path_module.clone()));
        seed.push(("environ", env_dict));
        let (members, env) = compile_shim(interp, shims::OS, seed)?;
        let mut members = public_members(members);
        members.push(("path".to_owned(), path_module));
        Ok(module_from_members("os", members, env))
    })
}

fn os_path_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    let os = make_os_module(interp)?;
    interp.get_attr(&os, "path")
}

fn make_io_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_shim_module(interp, "io", |interp| {
        let (members, env) = compile_shim(interp, shims::IO, fs_natives())?;
        Ok(module_from_members("io", public_members(members), env))
    })
}

fn make_pathlib_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_shim_module(interp, "pathlib", |interp| {
        let os = make_os_module(interp)?;
        let mut seed = fs_natives();
        seed.push(("os", os));
        let (members, env) = compile_shim(interp, shims::PATHLIB, seed)?;
        Ok(module_from_members("pathlib", public_members(members), env))
    })
}

fn pathlib_fnmatch(interp: &mut Interpreter) -> Result<Value, Unwind> {
    let pathlib = make_pathlib_module(interp)?;
    interp.get_attr(&pathlib, "_fnmatch")
}

fn make_shutil_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_shim_module(interp, "shutil", |interp| {
        let os = make_os_module(interp)?;
        let fnmatch = pathlib_fnmatch(interp)?;
        let mut seed = fs_natives();
        seed.push(("os", os));
        seed.push(("_pathlib_fnmatch", fnmatch));
        let (members, env) = compile_shim(interp, shims::SHUTIL, seed)?;
        Ok(module_from_members("shutil", public_members(members), env))
    })
}

fn make_tempfile_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_shim_module(interp, "tempfile", |interp| {
        let os = make_os_module(interp)?;
        let shutil = make_shutil_module(interp)?;
        let random = interp.import_module("random")?;
        let mut seed = fs_natives();
        seed.push(("os", os));
        seed.push(("shutil", shutil));
        seed.push(("random", random));
        let (members, env) = compile_shim(interp, shims::TEMPFILE, seed)?;
        Ok(module_from_members(
            "tempfile",
            public_members(members),
            env,
        ))
    })
}

fn make_glob_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_shim_module(interp, "glob", |interp| {
        let os = make_os_module(interp)?;
        let fnmatch = pathlib_fnmatch(interp)?;
        let mut seed = fs_natives();
        seed.push(("os", os));
        seed.push(("_pathlib_fnmatch", fnmatch));
        let (members, env) = compile_shim(interp, shims::GLOB, seed)?;
        Ok(module_from_members("glob", public_members(members), env))
    })
}

/// `sys.modules` — a dict view over the VM's import cache, built fresh on
/// each read so it stays live as CPython's is. Internal cache keys (the
/// `__builtin__:` / `__shim_` entries the shim machinery memoises under) are
/// not modules and are filtered out.
pub(crate) fn sys_modules_dict(interp: &Interpreter) -> Value {
    let mut map = crate::value::DictMap::new();
    // CPython always has the entry module here, under the name it runs as.
    // A program checking `"__main__" in sys.modules` (or reaching for it to
    // find its own globals) got a `KeyError`.
    // Its namespace is the interpreter's globals, live — so
    // `sys.modules[__name__].__all__` (the list `pub` synthesises) and any
    // other module-level name read back through it.
    map.insert(
        HashKey::Str(Rc::new("__main__".to_owned())),
        make_module_env("__main__", vec![], interp.root.clone()),
    );
    let mut names: Vec<&String> = interp
        .module_cache
        .keys()
        .filter(|k| !k.starts_with("__builtin__:") && !k.starts_with("__shim_"))
        .collect();
    names.sort();
    for name in names {
        if let Some(v) = interp.module_cache.get(name) {
            map.insert(HashKey::Str(Rc::new(name.clone())), v.clone());
        }
    }
    Value::Dict(Rc::new(RefCell::new(map)))
}

/// The current `sys.stdout` / `sys.stderr` when user code has replaced it
/// (`sys.stdout = buf`, `contextlib.redirect_stdout(buf)`). `None` means the
/// stream is still the VM's own, which is written directly.
///
/// CPython's `print` resolves `sys.stdout` on every call, so a redirect that
/// happens after the first `print` still takes effect; matching that is what
/// makes `redirect_stdout` work under `tyc run` as it does after `tyc build`.
fn redirected_std_stream(interp: &Interpreter, stderr: bool) -> Option<Value> {
    let (member, default_name) = if stderr {
        ("stderr", "sys.stderr")
    } else {
        ("stdout", "sys.stdout")
    };
    let Some(Value::Module(sys)) = interp.module_cache.get("sys") else {
        return None;
    };
    let current = sys.members.borrow().get(member).cloned()?;
    match &current {
        Value::Module(m) if m.name == default_name => None,
        Value::None => None,
        _ => Some(current),
    }
}

fn make_sys_module(interp: &Interpreter) -> Value {
    // sys.argv reflects the user's script + its arguments, not the host
    // `tyc` process's own argv. Populated via `Interpreter.script_argv`.
    let argv: Vec<Value> = interp
        .script_argv
        .iter()
        .map(|a| Value::Str(Rc::new(a.clone())))
        .collect();
    make_module(
        "sys",
        vec![
            ("argv", Value::List(Rc::new(RefCell::new(argv)))),
            (
                "platform",
                Value::Str(Rc::new(std::env::consts::OS.to_owned())),
            ),
            (
                "version",
                Value::Str(Rc::new(format!(
                    "tyc-vm {} (Typhon)",
                    env!("CARGO_PKG_VERSION")
                ))),
            ),
            (
                "exit",
                nf("exit", |_i, args| Err(crate::error::system_exit(args))),
            ),
            ("stdout", make_std_stream("sys.stdout", false)),
            ("stderr", make_std_stream("sys.stderr", true)),
            ("stdin", make_stdin_stream()),
            ("maxsize", Value::Int(VmInt::from(i64::MAX))),
            (
                "version_info",
                Value::Tuple(Rc::new(vec![
                    Value::Int(VmInt::from(3)),
                    Value::Int(VmInt::from(13)),
                    Value::Int(VmInt::from(0)),
                    Value::Str(Rc::new("final".to_owned())),
                    Value::Int(VmInt::from(0)),
                ])),
            ),
            ("byteorder", Value::Str(Rc::new("little".to_owned()))),
            (
                "exc_info",
                nf("exc_info", |i, _args| {
                    // `(type, value, traceback)` for the exception currently
                    // being handled, `(None, None, None)` outside a handler.
                    // The VM has no traceback object, so the third slot is
                    // always None.
                    let Some(exc) = i.active_exceptions.last().cloned() else {
                        return Ok(Value::Tuple(Rc::new(vec![
                            Value::None,
                            Value::None,
                            Value::None,
                        ])));
                    };
                    let value = exc.value.clone().unwrap_or_else(|| Value::Exception {
                        kind: Rc::new(exc.kind.clone()),
                        message: Rc::new(exc.message.clone()),
                        args: Rc::new(vec![Value::Str(Rc::new(exc.message.clone()))]),
                        chain: None,
                    });
                    // Slot 0 is the exception *type*: the raised instance's
                    // own class when there is one, else the builtin type
                    // object for its kind — either way `.__name__` reads.
                    let kind = match &value {
                        Value::Instance(inst) => Value::Class(inst.class.clone()),
                        _ => make_builtin_type(&exc.kind),
                    };
                    Ok(Value::Tuple(Rc::new(vec![kind, value, Value::None])))
                }),
            ),
            (
                "getrecursionlimit",
                nf("getrecursionlimit", |i, _args| {
                    Ok(Value::Int(VmInt::from(i.max_stack_depth as i64)))
                }),
            ),
            (
                "setrecursionlimit",
                nf("setrecursionlimit", |i, args| {
                    let n = single(&args, "setrecursionlimit")?.to_int()?;
                    if n < 1 {
                        return Err(value_error(
                            "recursion limit must be greater or equal than 1",
                        ));
                    }
                    i.max_stack_depth = n as usize;
                    Ok(Value::None)
                }),
            ),
        ],
    )
}

/// `sys.stdin`: `read` / `readline` / `readlines` over the process's stdin,
/// and iteration line by line (`for line in sys.stdin`).
fn make_stdin_stream() -> Value {
    fn read_line() -> Result<Option<String>, Unwind> {
        let mut s = String::new();
        let n = std::io::stdin().read_line(&mut s).map_err(|e| {
            crate::error::Unwind::Exception(crate::error::VmException::new(
                "OSError",
                format!("{e}"),
            ))
        })?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(s))
    }
    let read = nf("read", |_i, _args| {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| {
            crate::error::Unwind::Exception(crate::error::VmException::new(
                "OSError",
                format!("{e}"),
            ))
        })?;
        Ok(Value::Str(Rc::new(s)))
    });
    let readline = nf("readline", |_i, _args| {
        Ok(Value::Str(Rc::new(read_line()?.unwrap_or_default())))
    });
    let readlines = nf("readlines", |_i, _args| {
        let mut out = Vec::new();
        while let Some(line) = read_line()? {
            out.push(Value::Str(Rc::new(line)));
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let iter = nf("__iter__", |_i, _args| {
        let mut out = Vec::new();
        while let Some(line) = read_line()? {
            out.push(Value::Str(Rc::new(line)));
        }
        Ok(Value::Iter(Rc::new(RefCell::new(IterState::List {
            items: Rc::new(RefCell::new(out)),
            index: 0,
        }))))
    });
    native_object(
        "TextIOWrapper",
        vec![
            ("read", read),
            ("readline", readline),
            ("readlines", readlines),
            ("__iter__", iter),
            (
                "fileno",
                nf("fileno", |_i, _args| Ok(Value::Int(VmInt::from(0)))),
            ),
            ("isatty", nf("isatty", |_i, _args| Ok(Value::Bool(false)))),
            ("close", nf("close", |_i, _args| Ok(Value::None))),
        ],
    )
}

/// A minimal text-stream object for `sys.stdout` / `sys.stderr`: enough
/// surface (`write`, `flush`) for logging-style code and for
/// `print(file=sys.stderr)` to route correctly.
fn make_std_stream(name: &'static str, is_err: bool) -> Value {
    make_module(
        name,
        vec![
            (
                "write",
                nf("write", move |interp, args| {
                    let text = match args.first() {
                        Some(v) => interp.str_of(v)?,
                        None => return Err(type_error("write() requires an argument")),
                    };
                    use std::io::Write;
                    if is_err {
                        eprint!("{text}");
                        let _ = std::io::stderr().flush();
                    } else {
                        print!("{text}");
                        let _ = std::io::stdout().flush();
                    }
                    // CPython's `write()` returns the number of *characters*
                    // written, not the UTF-8 byte length.
                    Ok(Value::Int(VmInt::from(text.chars().count() as i64)))
                }),
            ),
            ("flush", nf("flush", |_i, _args| Ok(Value::None))),
        ],
    )
}

fn make_json_module() -> Value {
    make_module(
        "json",
        vec![
            (
                "dumps",
                nf("dumps", |_i, args| {
                    Ok(Value::Str(Rc::new(json_dumps_with(
                        single(&args, "dumps")?,
                        &JsonDumpOpts::defaults(),
                    )?)))
                }),
            ),
            // The exception class `loads` raises — exported so
            // `except json.JSONDecodeError` matches by identity.
            ("JSONDecodeError", Value::Class(json_decode_error_class())),
            (
                "loads",
                nf("loads", |_i, args| {
                    json_loads(&single(&args, "loads")?.py_str())
                }),
            ),
            (
                "load",
                nf("load", |interp, args| {
                    // json.load(fp) — read() the file-like, then loads().
                    let fp = single(&args, "load")?.clone();
                    let read = interp.get_attr(&fp, "read")?;
                    let body = interp.call_value(read, vec![], &[])?;
                    json_loads(&body.py_str())
                }),
            ),
            (
                "dump",
                nf("dump", |interp, args| {
                    // json.dump(obj, fp) — dumps(), then fp.write().
                    if args.len() < 2 {
                        return Err(crate::error::type_error("dump() requires (obj, fp)"));
                    }
                    let serialised = json_dumps_with(&args[0], &JsonDumpOpts::defaults())?;
                    let fp = args[1].clone();
                    let write = interp.get_attr(&fp, "write")?;
                    interp.call_value(write, vec![Value::Str(Rc::new(serialised))], &[])?;
                    Ok(Value::None)
                }),
            ),
        ],
    )
}

fn make_time_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    let time_fn = nf("time", |_i, _args| {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Ok(Value::Float(t))
    });
    // The calendar side (`struct_time`, `gmtime`, `strftime`, `strptime`, …)
    // is a Python shim that only needs the raw clock.
    let extras =
        compile_helpers_seeded(interp, shims::TIME_EXTRAS, vec![("time", time_fn.clone())])?;
    let mut entries: Vec<(&str, Value)> = vec![
        ("time", time_fn),
        (
            "time_ns",
            nf("time_ns", |_i, _args| {
                let ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                Ok(Value::Int(VmInt::from(num_bigint::BigInt::from(ns))))
            }),
        ),
        (
            "monotonic_ns",
            nf("monotonic_ns", |_i, _args| {
                Ok(Value::Int(VmInt::from((monotonic_secs() * 1e9) as i64)))
            }),
        ),
        (
            "perf_counter_ns",
            nf("perf_counter_ns", |_i, _args| {
                Ok(Value::Int(VmInt::from((monotonic_secs() * 1e9) as i64)))
            }),
        ),
        (
            "process_time_ns",
            nf("process_time_ns", |_i, _args| {
                Ok(Value::Int(VmInt::from((monotonic_secs() * 1e9) as i64)))
            }),
        ),
    ];
    for (k, v) in &extras {
        entries.push((k.as_str(), v.clone()));
    }
    let module = make_module(
        "time",
        vec![
            (
                "sleep",
                nf("sleep", |_i, args| {
                    let secs = single(&args, "sleep")?.to_float()?;
                    if secs > 0.0 {
                        std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                    }
                    Ok(Value::None)
                }),
            ),
            (
                "monotonic",
                nf("monotonic", |_i, _args| Ok(Value::Float(monotonic_secs()))),
            ),
            (
                "perf_counter",
                nf("perf_counter", |_i, _args| {
                    Ok(Value::Float(monotonic_secs()))
                }),
            ),
            (
                "process_time",
                nf("process_time", |_i, _args| {
                    Ok(Value::Float(monotonic_secs()))
                }),
            ),
        ],
    );
    if let Value::Module(m) = &module {
        let mut members = m.members.borrow_mut();
        for (k, v) in entries {
            members.insert(k.to_owned(), v);
        }
    }
    Ok(module)
}

/// Seconds since the first call (a fixed reference point), so `monotonic`,
/// `perf_counter`, and `process_time` return increasing values across calls.
fn monotonic_secs() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn make_random_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    use std::cell::RefCell;
    /// A non-deterministic seed, standing in for the OS entropy CPython
    /// seeds from. Shared by the unseeded default and `seed()` / `seed(None)`.
    ///
    /// Two independent sources are mixed. `RandomState` is the stdlib's
    /// hash-randomisation source: its keys come from OS randomness once per
    /// process, and each instantiation yields a different pair, so it varies
    /// across processes *and* across threads within one process — which
    /// matters because the RNG states below are `thread_local`. Wall-clock
    /// nanos are XORed in as a second source, so a platform whose clock is
    /// coarse (Windows ticks at ~15ms, which alone would collide across fast
    /// successive runs) cannot by itself make two seeds equal. XOR keeps the
    /// result at least as unpredictable as the stronger of the two.
    ///
    /// This is emphatically not a CSPRNG seed — `random` is not cryptographic
    /// on either surface, matching CPython, where `secrets` is the answer.
    fn entropy_seed() -> u64 {
        use std::hash::{BuildHasher, Hasher};
        let os_derived = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        os_derived ^ nanos
    }
    // CPython-compatible MT19937 (`_random.Random`) so seeded programs
    // produce IDENTICAL sequences under `tyc run` and `tyc build && python`.
    // The Python-level algorithms (randrange, choice, shuffle, sample, the
    // distributions) live in `shims/random.py`, transcribed from random.py;
    // the natives below are `_random.Random`'s methods plus fast paths for
    // the hottest module-level functions on state slot 0.
    struct Mt19937 {
        mt: [u32; 624],
        index: usize,
    }
    impl Mt19937 {
        fn new() -> Self {
            let mut s = Self {
                mt: [0u32; 624],
                index: 625,
            };
            // CPython seeds from urandom at construction, so an *unseeded*
            // generator draws a different sequence on every run; an explicit
            // seed reseeds deterministically.
            s.seed_int(&num_bigint::BigInt::from(entropy_seed()));
            s
        }
        fn init_genrand(&mut self, seed: u32) {
            self.mt[0] = seed;
            for i in 1..624usize {
                self.mt[i] = 1812433253u32
                    .wrapping_mul(self.mt[i - 1] ^ (self.mt[i - 1] >> 30))
                    .wrapping_add(i as u32);
            }
            self.index = 624;
        }
        fn init_by_array(&mut self, key: &[u32]) {
            self.init_genrand(19650218);
            let mut i: usize = 1;
            let mut j: usize = 0;
            let mut k = std::cmp::max(624, key.len());
            while k > 0 {
                self.mt[i] = (self.mt[i]
                    ^ (self.mt[i - 1] ^ (self.mt[i - 1] >> 30)).wrapping_mul(1664525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
                i += 1;
                j += 1;
                if i >= 624 {
                    self.mt[0] = self.mt[623];
                    i = 1;
                }
                if j >= key.len() {
                    j = 0;
                }
                k -= 1;
            }
            k = 623;
            while k > 0 {
                self.mt[i] = (self.mt[i]
                    ^ (self.mt[i - 1] ^ (self.mt[i - 1] >> 30)).wrapping_mul(1566083941))
                .wrapping_sub(i as u32);
                i += 1;
                if i >= 624 {
                    self.mt[0] = self.mt[623];
                    i = 1;
                }
                k -= 1;
            }
            self.mt[0] = 0x8000_0000;
            self.index = 624;
        }
        fn generate(&mut self) {
            const M: usize = 397;
            const MATRIX_A: u32 = 0x9908_b0df;
            const UPPER: u32 = 0x8000_0000;
            const LOWER: u32 = 0x7fff_ffff;
            for i in 0..624usize {
                let y = (self.mt[i] & UPPER) | (self.mt[(i + 1) % 624] & LOWER);
                let mut next = self.mt[(i + M) % 624] ^ (y >> 1);
                if y & 1 != 0 {
                    next ^= MATRIX_A;
                }
                self.mt[i] = next;
            }
            self.index = 0;
        }
        fn genrand_u32(&mut self) -> u32 {
            if self.index >= 624 {
                self.generate();
            }
            let mut y = self.mt[self.index];
            self.index += 1;
            y ^= y >> 11;
            y ^= (y << 7) & 0x9d2c_5680;
            y ^= (y << 15) & 0xefc6_0000;
            y ^= y >> 18;
            y
        }
        /// `random()` — genrand_res53.
        fn random(&mut self) -> f64 {
            let a = (self.genrand_u32() >> 5) as f64;
            let b = (self.genrand_u32() >> 6) as f64;
            (a * 67108864.0 + b) / 9007199254740992.0
        }
        /// `getrandbits(k)` for any `k >= 0`: 32-bit words drawn
        /// little-endian, the last truncated — `_random.Random.getrandbits`.
        fn getrandbits(&mut self, k: u64) -> num_bigint::BigInt {
            if k == 0 {
                return num_bigint::BigInt::from(0);
            }
            if k <= 32 {
                return num_bigint::BigInt::from(self.genrand_u32() >> (32 - k as u32));
            }
            let words = ((k - 1) / 32 + 1) as usize;
            let mut buf: Vec<u32> = Vec::with_capacity(words);
            let mut remaining = k;
            for _ in 0..words {
                let mut r = self.genrand_u32();
                if remaining < 32 {
                    r >>= 32 - remaining as u32;
                }
                buf.push(r);
                remaining = remaining.saturating_sub(32);
            }
            num_bigint::BigInt::from(num_bigint::BigUint::from_slice(&buf))
        }
        /// `getrandbits(k)` for `k <= 64`, on the machine word.
        fn getrandbits_u64(&mut self, k: u32) -> u64 {
            if k == 0 {
                return 0;
            }
            if k <= 32 {
                return (self.genrand_u32() >> (32 - k)) as u64;
            }
            let low = self.genrand_u32() as u64;
            let hi_bits = k - 32;
            let high = (self.genrand_u32() >> (32 - hi_bits)) as u64;
            (high << 32) | low
        }
        /// `_randbelow(n)` — rejection sampling, exactly CPython.
        fn randbelow(&mut self, n: u64) -> u64 {
            if n == 0 {
                return 0;
            }
            let k = 64 - n.leading_zeros();
            let mut r = self.getrandbits_u64(k);
            while r >= n {
                r = self.getrandbits_u64(k);
            }
            r
        }
        fn seed_int(&mut self, n: &num_bigint::BigInt) {
            use num_traits::Signed;
            // CPython: key = absolute value split into 32-bit words,
            // little-endian; zero seeds as a single zero word.
            let mag = n.abs();
            let (_, bytes) = mag.to_bytes_le();
            let mut words: Vec<u32> = bytes
                .chunks(4)
                .map(|c| {
                    let mut w = [0u8; 4];
                    w[..c.len()].copy_from_slice(c);
                    u32::from_le_bytes(w)
                })
                .collect();
            if words.is_empty() {
                words.push(0);
            }
            self.init_by_array(&words);
        }
        /// `Random.seed(a)` (version 2): `None` draws from entropy, an int
        /// seeds by absolute value, a float by its hash, and str / bytes by
        /// `int.from_bytes(a + sha512(a).digest())`.
        fn seed_value(&mut self, v: &Value) -> Result<(), Unwind> {
            match v {
                Value::None => self.seed_int(&num_bigint::BigInt::from(entropy_seed())),
                Value::Int(n) => self.seed_int(&n.to_bigint()),
                Value::Bool(b) => self.seed_int(&num_bigint::BigInt::from(*b as i64)),
                Value::Float(f) => self.seed_int(&num_bigint::BigInt::from(
                    crate::pyhash::float_hash(*f) as u64,
                )),
                Value::Str(s) => self.seed_bytes(s.as_bytes()),
                Value::Bytes(b) => self.seed_bytes(b),
                _ => {
                    return Err(type_error(
                        "The only supported seed types are:\nNone, int, float, str, bytes, and bytearray.",
                    ))
                }
            }
            Ok(())
        }
        fn seed_bytes(&mut self, a: &[u8]) {
            let mut material = a.to_vec();
            material.extend(crate::hashes::sha512(a));
            let n = num_bigint::BigInt::from_bytes_be(num_bigint::Sign::Plus, &material);
            self.seed_int(&n);
        }
        /// `_random.Random.getstate()`: the 624 state words plus the index.
        fn state_value(&self) -> Value {
            let mut items: Vec<Value> = self
                .mt
                .iter()
                .map(|w| Value::Int(VmInt::from(*w as i64)))
                .collect();
            items.push(Value::Int(VmInt::from(self.index as i64)));
            Value::Tuple(Rc::new(items))
        }
        fn set_state(&mut self, state: &Value) -> Result<(), Unwind> {
            let Value::Tuple(items) = state else {
                return Err(type_error("state vector must be a tuple"));
            };
            if items.len() != 625 {
                return Err(value_error("state vector is the wrong size"));
            }
            for (slot, item) in self.mt.iter_mut().zip(items.iter()) {
                let word = item.to_int()?;
                if !(0..=u32::MAX as i64).contains(&word) {
                    return Err(Unwind::Exception(crate::error::VmException::new(
                        "OverflowError",
                        "Python int too large to convert to C unsigned long",
                    )));
                }
                *slot = word as u32;
            }
            let index = items[624].to_int()?;
            if !(0..=624).contains(&index) {
                return Err(value_error("invalid state"));
            }
            self.index = index as usize;
            Ok(())
        }
    }
    thread_local! {
        static RNGS: RefCell<Vec<Mt19937>> = const { RefCell::new(Vec::new()) };
    }
    /// Run `f` on state slot `id`, creating (entropy-seeded) slots on demand.
    fn with_rng<R>(id: usize, f: impl FnOnce(&mut Mt19937) -> R) -> R {
        RNGS.with(|r| {
            let mut states = r.borrow_mut();
            while states.len() <= id {
                states.push(Mt19937::new());
            }
            f(&mut states[id])
        })
    }
    fn new_rng() -> usize {
        RNGS.with(|r| {
            let mut states = r.borrow_mut();
            if states.is_empty() {
                states.push(Mt19937::new());
            }
            states.push(Mt19937::new());
            states.len() - 1
        })
    }
    fn slot(v: Option<&Value>) -> Result<usize, Unwind> {
        let id = v
            .ok_or_else(|| type_error("random state id required"))?
            .to_int()?;
        if id < 0 {
            return Err(value_error("invalid random state id"));
        }
        Ok(id as usize)
    }
    fn bits_arg(v: Option<&Value>) -> Result<u64, Unwind> {
        let k = match v {
            Some(Value::Int(n)) => n.to_bigint(),
            Some(Value::Bool(b)) => num_bigint::BigInt::from(*b as i64),
            Some(other) => {
                return Err(type_error(format!(
                    "'{}' object cannot be interpreted as an integer",
                    other.type_name()
                )))
            }
            None => return Err(type_error("getrandbits() missing required argument 'k'")),
        };
        use num_traits::{Signed, ToPrimitive};
        if k.is_negative() {
            return Err(value_error("number of bits must be non-negative"));
        }
        k.to_u64().ok_or_else(|| {
            Unwind::Exception(crate::error::VmException::new(
                "OverflowError",
                "Python int too large to convert to C int",
            ))
        })
    }

    let natives: Vec<(&str, Value)> = vec![
        (
            "_rng_new",
            nf("_rng_new", |_i, _args| {
                Ok(Value::Int(VmInt::from(new_rng() as i64)))
            }),
        ),
        (
            "_rng_seed",
            nf("_rng_seed", |_i, args| {
                let id = slot(args.first())?;
                let a = args.get(1).cloned().unwrap_or(Value::None);
                with_rng(id, |m| m.seed_value(&a))?;
                Ok(Value::None)
            }),
        ),
        (
            "_rng_random",
            nf("_rng_random", |_i, args| {
                let id = slot(args.first())?;
                Ok(Value::Float(with_rng(id, |m| m.random())))
            }),
        ),
        (
            "_rng_getrandbits",
            nf("_rng_getrandbits", |_i, args| {
                let id = slot(args.first())?;
                let k = bits_arg(args.get(1))?;
                Ok(Value::Int(VmInt::from(with_rng(id, |m| m.getrandbits(k)))))
            }),
        ),
        (
            "_rng_getstate",
            nf("_rng_getstate", |_i, args| {
                let id = slot(args.first())?;
                Ok(with_rng(id, |m| m.state_value()))
            }),
        ),
        (
            "_rng_setstate",
            nf("_rng_setstate", |_i, args| {
                let id = slot(args.first())?;
                let state = args.get(1).cloned().unwrap_or(Value::None);
                with_rng(id, |m| m.set_state(&state))?;
                Ok(Value::None)
            }),
        ),
    ];
    let members = compile_helpers_seeded(interp, shims::RANDOM, natives)?;
    let mut entries: Vec<(String, Value)> = members
        .into_iter()
        .filter(|(k, _)| !k.starts_with("_rng_"))
        .collect();
    // Fast native paths for the hottest module-level functions, on state
    // slot 0 — the same draws as the shim methods they stand in for.
    let fast: Vec<(&str, Value)> = vec![
        (
            "random",
            nf("random", |_i, _args| {
                Ok(Value::Float(with_rng(0, |m| m.random())))
            }),
        ),
        (
            "getrandbits",
            nf("getrandbits", |_i, args| {
                let k = bits_arg(args.first())?;
                Ok(Value::Int(VmInt::from(with_rng(0, |m| m.getrandbits(k)))))
            }),
        ),
        (
            "uniform",
            nf("uniform", |_i, args| {
                let a = args
                    .first()
                    .ok_or_else(|| type_error("uniform() missing required argument 'a'"))?
                    .to_float()?;
                let b = args
                    .get(1)
                    .ok_or_else(|| type_error("uniform() missing required argument 'b'"))?
                    .to_float()?;
                let t = with_rng(0, |m| m.random());
                Ok(Value::Float(a + (b - a) * t))
            }),
        ),
        (
            "randint",
            nf("randint", |_i, args| {
                let (a, b) = match (args.first(), args.get(1)) {
                    (Some(Value::Int(a)), Some(Value::Int(b))) => (a.clone(), b.clone()),
                    (Some(Value::Bool(a)), Some(Value::Bool(b))) => {
                        (VmInt::from(*a as i64), VmInt::from(*b as i64))
                    }
                    (Some(Value::Int(a)), Some(Value::Bool(b))) => {
                        (a.clone(), VmInt::from(*b as i64))
                    }
                    (Some(Value::Bool(a)), Some(Value::Int(b))) => {
                        (VmInt::from(*a as i64), b.clone())
                    }
                    (Some(other), _) | (_, Some(other)) if !matches!(other, Value::Int(_)) => {
                        return Err(type_error(format!(
                            "'{}' object cannot be interpreted as an integer",
                            other.type_name()
                        )))
                    }
                    _ => return Err(type_error("randint() missing required argument 'b'")),
                };
                let (Some(a), Some(b)) = (a.to_i64(), b.to_i64()) else {
                    return Err(value_error("randint() bounds too large for the VM"));
                };
                if b < a {
                    return Err(value_error(format!(
                        "empty range in randrange({a}, {})",
                        b + 1
                    )));
                }
                let span = (b as i128 - a as i128 + 1) as u64;
                let pick = with_rng(0, |m| m.randbelow(span)) as i128;
                Ok(Value::Int(VmInt::from((a as i128 + pick) as i64)))
            }),
        ),
        (
            "choice",
            nf("choice", |interp, args| {
                let seq = args
                    .first()
                    .ok_or_else(|| type_error("choice() missing required argument 'seq'"))?;
                let n = match seq {
                    Value::List(l) => l.borrow().len(),
                    Value::Tuple(t) => t.len(),
                    Value::Str(s) => s.chars().count(),
                    Value::Bytes(b) => b.len(),
                    Value::Range { .. } | Value::Instance(_) => value_len(seq)?,
                    other => {
                        return Err(type_error(format!(
                            "object of type '{}' has no len()",
                            other.type_name()
                        )))
                    }
                };
                if n == 0 {
                    return Err(Unwind::Exception(crate::error::VmException::new(
                        "IndexError",
                        "Cannot choose from an empty sequence",
                    )));
                }
                let idx = with_rng(0, |m| m.randbelow(n as u64)) as i64;
                interp.subscript(seq, &Value::Int(VmInt::from(idx)))
            }),
        ),
        (
            "shuffle",
            nf("shuffle", |_i, args| {
                let lst = match args.first() {
                    Some(Value::List(l)) => l.clone(),
                    Some(other) => {
                        return Err(type_error(format!(
                            "'{}' object does not support item assignment",
                            other.type_name()
                        )))
                    }
                    None => return Err(type_error("shuffle() missing required argument 'x'")),
                };
                let n = lst.borrow().len();
                for i in (1..n).rev() {
                    let j = with_rng(0, |m| m.randbelow(i as u64 + 1)) as usize;
                    lst.borrow_mut().swap(i, j);
                }
                Ok(Value::None)
            }),
        ),
    ];
    for (k, v) in fast {
        entries.retain(|(name, _)| name != k);
        entries.push((k.to_owned(), v));
    }
    let refs: Vec<(&str, Value)> = entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    Ok(make_module("random", refs))
}

fn make_hashlib_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    fn name_arg(v: Option<&Value>) -> Result<String, Unwind> {
        match v {
            Some(Value::Str(s)) => Ok((**s).clone()),
            _ => Err(type_error("hash name must be a str")),
        }
    }
    let natives: Vec<(&str, Value)> = vec![
        (
            "_hash_digest",
            nf("_hash_digest", |_i, args| {
                let name = name_arg(args.first())?;
                let data = match args.get(1) {
                    Some(Value::Bytes(b)) => b.clone(),
                    _ => return Err(type_error("object supporting the buffer API required")),
                };
                crate::hashes::digest(&name, &data)
                    .map(|d| Value::Bytes(Rc::new(d)))
                    .ok_or_else(|| value_error(format!("unsupported hash type {name}")))
            }),
        ),
        (
            "_hash_sizes",
            nf("_hash_sizes", |_i, args| {
                let name = name_arg(args.first())?;
                let (digest, block) = crate::hashes::sizes(&name)
                    .ok_or_else(|| value_error(format!("unsupported hash type {name}")))?;
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Int(VmInt::from(digest as i64)),
                    Value::Int(VmInt::from(block as i64)),
                ])))
            }),
        ),
    ];
    let members = compile_helpers_seeded(interp, shims::HASHLIB, natives)?;
    let entries: Vec<(&str, Value)> = members
        .iter()
        .filter(|(k, _)| !k.starts_with("_hash_"))
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    Ok(make_module("hashlib", entries))
}

fn identity_native(name: &'static str) -> Value {
    Value::Native(Rc::new(NativeFn::new(name, |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    })))
}

/// `typing` shim.
///
/// Provides identity-callable stubs for every name the static checker
/// emits as a forward reference plus a couple of runtime constructors
/// (`NewType`, `TypeVar`). All "subscriptable" names (`List`, `Dict`,
/// `Optional`, …) round-trip through the identity callable when
/// indexed because the VM treats `name[x]` as a `__getitem__` call on
/// the callable; the resulting value is an opaque marker that nobody
/// reads at runtime — only its presence at parse-time matters.
fn make_typing_module() -> Value {
    let mut entries: Vec<(&str, Value)> = Vec::new();
    for name in [
        "Callable",
        "Optional",
        "Union",
        "List",
        "Dict",
        "Set",
        "Tuple",
        "FrozenSet",
        "Any",
        "Protocol",
        "Iterable",
        "Iterator",
        "Sequence",
        "Mapping",
        "MutableMapping",
        "MutableSequence",
        "ClassVar",
        "Final",
        "Literal",
        "Type",
        "Generic",
        "TypedDict",
        "NamedTuple",
        "Hashable",
        "Sized",
        "Container",
        "Awaitable",
        "Coroutine",
        "AsyncIterable",
        "AsyncIterator",
        "Generator",
        "AsyncGenerator",
        "ContextManager",
        "AsyncContextManager",
        "Annotated",
        "Self",
        "Never",
        "NoReturn",
        // Type-narrowing and variadic forms. All are erased at runtime —
        // `typing` only has to expose the name so the import resolves.
        "TypeGuard",
        "TypeIs",
        "TypeAlias",
        "Required",
        "NotRequired",
        "Unpack",
        "Concatenate",
        "LiteralString",
        "ParamSpec",
        "TypeVarTuple",
        "SupportsIndex",
        "SupportsInt",
        "SupportsFloat",
        "SupportsBytes",
        "SupportsAbs",
        "SupportsRound",
        "OrderedDict",
        "DefaultDict",
        "Counter",
        "Deque",
        "ChainMap",
        "AbstractSet",
        "Collection",
        "Reversible",
        "ItemsView",
        "KeysView",
        "ValuesView",
        "IO",
        "TextIO",
        "BinaryIO",
        "AnyStr",
    ] {
        entries.push((name, identity_native(name)));
    }
    // `NewType("Foo", base)` returns an identity callable. Mirrors the
    // root-level `NewType` builtin for users who import it explicitly.
    entries.push((
        "NewType",
        Value::Native(Rc::new(NativeFn::new("NewType", |_i, _args| {
            Ok(Value::Native(Rc::new(NativeFn::new(
                "NewTypeAlias",
                |_i, args| Ok(args.into_iter().next().unwrap_or(Value::None)),
            ))))
        }))),
    ));
    // `TypeVar("T", ...)` — return a placeholder. The static type system
    // is the only consumer; at runtime the value just needs to exist.
    entries.push((
        "TypeVar",
        Value::Native(Rc::new(NativeFn::new("TypeVar", |_i, args| {
            Ok(args.into_iter().next().unwrap_or(Value::None))
        }))),
    ));
    // `cast(type, value)` — return the value unchanged.
    entries.push((
        "cast",
        Value::Native(Rc::new(NativeFn::new("cast", |_i, args| {
            Ok(args.into_iter().nth(1).unwrap_or(Value::None))
        }))),
    ));
    // `TYPE_CHECKING` — always False at runtime, like CPython.
    entries.push(("TYPE_CHECKING", Value::Bool(false)));
    // `overload` decorator — strip (identity for the decorated fn).
    entries.push((
        "overload",
        Value::Native(Rc::new(NativeFn::new("overload", |_i, args| {
            Ok(args.into_iter().next().unwrap_or(Value::None))
        }))),
    ));
    // `runtime_checkable` decorator — identity.
    entries.push((
        "runtime_checkable",
        Value::Native(Rc::new(NativeFn::new("runtime_checkable", |_i, args| {
            Ok(args.into_iter().next().unwrap_or(Value::None))
        }))),
    ));
    // `@override` / `@final` / `@no_type_check` — checker-only decorators
    // CPython also implements as the identity.
    for name in ["override", "final", "no_type_check", "dataclass_transform"] {
        entries.push((
            name,
            Value::Native(Rc::new(NativeFn::new(name, |_i, args| {
                Ok(args.into_iter().next().unwrap_or(Value::None))
            }))),
        ));
    }
    // `get_args` / `get_origin` on an erased annotation have nothing to
    // report, and `NoDefault` is a sentinel.
    entries.push((
        "get_args",
        Value::Native(Rc::new(NativeFn::new("get_args", |_i, _args| {
            Ok(Value::Tuple(Rc::new(Vec::new())))
        }))),
    ));
    entries.push((
        "get_origin",
        Value::Native(Rc::new(NativeFn::new("get_origin", |_i, _args| {
            Ok(Value::None)
        }))),
    ));
    make_module("typing", entries)
}

/// `re` shim.
///
/// Implemented: `re.compile`, `re.match`, `re.search`, `re.sub`,
/// `re.findall`, `re.split`, `re.fullmatch`. Patterns are compiled with
/// the Rust `regex` crate via a string-only path — Python-specific syntax
/// like `(?P<name>…)` named groups falls through unchanged (named groups
/// in Rust's regex use `(?<name>…)`; we rewrite the common `?P<` form to
/// `?<` so most Python regexes work). The returned match objects expose
/// `group`, `groups`, `start`, `end`, `span`. Backreferences in `sub`
/// replacement strings (`\\1`, `\\g<name>`) map to Rust's `${n}` form.
///
/// Not implemented: `re.IGNORECASE` / `re.MULTILINE` / flag arguments,
/// `re.finditer`, `re.purge`, the `Pattern` / `Match` object protocol
/// (only the named methods above work), and lookaheads/lookbehinds
/// (Rust's `regex` is a finite-automaton engine that rejects them).
fn make_re_module() -> Value {
    use crate::value::Class;
    // Compile a Python-shaped pattern into a Rust regex by rewriting
    // `(?P<name>` to `(?<name>`. The `(?P=name)` back-reference form is
    // left as-is so the Rust engine rejects it with a legible error —
    // there's no equivalent in `regex` (a finite-automaton engine).
    fn to_rust_pattern(p: &str) -> String {
        p.replace("(?P<", "(?<")
    }
    fn compile_one(p: &str) -> Result<regex::Regex, Unwind> {
        regex::Regex::new(&to_rust_pattern(p))
            .map_err(|e| value_error(format!("invalid regex: {e}")))
    }
    // name -> group index for a compiled pattern's named groups.
    fn name_indices(re: &regex::Regex) -> HashMap<String, usize> {
        let mut m = HashMap::new();
        for (i, n) in re.capture_names().enumerate() {
            if let Some(n) = n {
                m.insert(n.to_owned(), i);
            }
        }
        m
    }
    // Expand a Python replacement template (`\1`, `\g<name>`, `\g<N>`, `\\`,
    // `\n`/`\t`/`\r`) against a captures.
    fn expand_template(
        tpl: &str,
        caps: &regex::Captures,
        names: &HashMap<String, usize>,
    ) -> String {
        let ch: Vec<char> = tpl.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < ch.len() {
            if ch[i] == '\\' && i + 1 < ch.len() {
                let n = ch[i + 1];
                if n.is_ascii_digit() {
                    let mut j = i + 1;
                    let mut num = String::new();
                    while j < ch.len() && ch[j].is_ascii_digit() && num.len() < 2 {
                        num.push(ch[j]);
                        j += 1;
                    }
                    if let Ok(idx) = num.parse::<usize>() {
                        if let Some(m) = caps.get(idx) {
                            out.push_str(m.as_str());
                        }
                    }
                    i = j;
                    continue;
                } else if n == 'g' && i + 2 < ch.len() && ch[i + 2] == '<' {
                    let mut j = i + 3;
                    let mut nm = String::new();
                    while j < ch.len() && ch[j] != '>' {
                        nm.push(ch[j]);
                        j += 1;
                    }
                    if j < ch.len() {
                        j += 1; // consume '>'
                    }
                    let idx = nm.parse::<usize>().ok().or_else(|| names.get(&nm).copied());
                    if let Some(idx) = idx {
                        if let Some(m) = caps.get(idx) {
                            out.push_str(m.as_str());
                        }
                    }
                    i = j;
                    continue;
                } else if n == '\\' {
                    out.push('\\');
                    i += 2;
                    continue;
                } else if n == 'n' {
                    out.push('\n');
                    i += 2;
                    continue;
                } else if n == 't' {
                    out.push('\t');
                    i += 2;
                    continue;
                } else if n == 'r' {
                    out.push('\r');
                    i += 2;
                    continue;
                }
            }
            out.push(ch[i]);
            i += 1;
        }
        out
    }
    // `re.sub` honouring a callable replacement (called with each Match) or a
    // Python-syntax template string. `count == 0` means replace all.
    fn re_sub_apply(
        interp: &mut crate::interp::Interpreter,
        re: &regex::Regex,
        repl: &Value,
        s: &str,
        count: usize,
    ) -> Result<(String, usize), Unwind> {
        let names = name_indices(re);
        let callable = matches!(
            repl,
            Value::Function(_) | Value::Native(_) | Value::BoundMethod { .. } | Value::Class(_)
        );
        let mut out = String::new();
        let mut last = 0usize;
        let mut n = 0usize;
        for caps in re.captures_iter(s) {
            if count != 0 && n >= count {
                break;
            }
            let m0 = caps.get(0).unwrap();
            let (ms, me) = (m0.start(), m0.end());
            out.push_str(&s[last..ms]);
            if callable {
                let mv = captures_to_value(Some(caps), &names);
                let r = interp.call_value(repl.clone(), vec![mv], &[])?;
                out.push_str(&r.py_str());
            } else {
                let tpl = repl.py_str();
                out.push_str(&expand_template(&tpl, &caps, &names));
            }
            last = me;
            n += 1;
        }
        out.push_str(&s[last..]);
        Ok((out, n))
    }
    // `re.split` — includes captured groups between splits (CPython semantics).
    // `maxsplit == 0` means unlimited.
    fn re_split_apply(re: &regex::Regex, s: &str, maxsplit: usize) -> Vec<Value> {
        let ngroups = re.captures_len().saturating_sub(1);
        let mut out: Vec<Value> = Vec::new();
        let mut last = 0usize;
        for (n, caps) in re.captures_iter(s).enumerate() {
            if maxsplit != 0 && n >= maxsplit {
                break;
            }
            let m0 = caps.get(0).unwrap();
            out.push(Value::Str(Rc::new(s[last..m0.start()].to_owned())));
            for gi in 1..=ngroups {
                match caps.get(gi) {
                    Some(m) => out.push(Value::Str(Rc::new(m.as_str().to_owned()))),
                    None => out.push(Value::None),
                }
            }
            last = m0.end();
        }
        out.push(Value::Str(Rc::new(s[last..].to_owned())));
        out
    }
    // A Pattern object holding a compiled regex plus a thin method table
    // so `pattern.match(s)` etc. work.
    fn pattern_value(p: regex::Regex) -> Value {
        let p_rc = Rc::new(p);
        let mut attrs: crate::value::FieldMap = crate::value::FieldMap::new();
        let p1 = p_rc.clone();
        attrs.insert(
            "match".into(),
            Value::Native(Rc::new(NativeFn::new("match", move |_i, args| {
                let s = single(&args, "match")?.py_str();
                let caps = p1.captures(&s).filter(|c| c.get(0).unwrap().start() == 0);
                Ok(captures_to_value(caps, &name_indices(&p1)))
            }))),
        );
        let p2 = p_rc.clone();
        attrs.insert(
            "search".into(),
            Value::Native(Rc::new(NativeFn::new("search", move |_i, args| {
                let s = single(&args, "search")?.py_str();
                Ok(captures_to_value(p2.captures(&s), &name_indices(&p2)))
            }))),
        );
        let p2f = p_rc.clone();
        attrs.insert(
            "finditer".into(),
            Value::Native(Rc::new(NativeFn::new("finditer", move |_i, args| {
                let s = single(&args, "finditer")?.py_str();
                let names = name_indices(&p2f);
                let out: Vec<Value> = p2f
                    .captures_iter(&s)
                    .map(|c| captures_to_value(Some(c), &names))
                    .collect();
                Ok(Value::List(Rc::new(RefCell::new(out))))
            }))),
        );
        let p3 = p_rc.clone();
        attrs.insert(
            "findall".into(),
            Value::Native(Rc::new(NativeFn::new("findall", move |_i, args| {
                let s = single(&args, "findall")?.py_str();
                let hits = re_findall_hits(&p3, &s);
                Ok(Value::List(Rc::new(RefCell::new(hits))))
            }))),
        );
        let p4 = p_rc.clone();
        attrs.insert(
            "sub".into(),
            Value::Native(Rc::new(NativeFn::new("sub", move |i, args| {
                let repl = args
                    .first()
                    .ok_or_else(|| type_error("sub() needs replacement"))?
                    .clone();
                let s = args
                    .get(1)
                    .ok_or_else(|| type_error("sub() needs string"))?
                    .py_str();
                let count = match args.get(2) {
                    Some(c) if !matches!(c, Value::None) => c.to_int()?.max(0) as usize,
                    _ => 0,
                };
                let (out, _) = re_sub_apply(i, &p4, &repl, &s, count)?;
                Ok(Value::Str(Rc::new(out)))
            }))),
        );
        let p4n = p_rc.clone();
        attrs.insert(
            "subn".into(),
            Value::Native(Rc::new(NativeFn::new("subn", move |i, args| {
                let repl = args
                    .first()
                    .ok_or_else(|| type_error("subn() needs replacement"))?
                    .clone();
                let s = args
                    .get(1)
                    .ok_or_else(|| type_error("subn() needs string"))?
                    .py_str();
                let (out, n) = re_sub_apply(i, &p4n, &repl, &s, 0)?;
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Str(Rc::new(out)),
                    Value::Int(VmInt::from(n as i64)),
                ])))
            }))),
        );
        let p5 = p_rc.clone();
        attrs.insert(
            "split".into(),
            Value::Native(Rc::new(NativeFn::new("split", move |_i, args| {
                let s = single(&args, "split")?.py_str();
                Ok(Value::List(Rc::new(RefCell::new(re_split_apply(
                    &p5, &s, 0,
                )))))
            }))),
        );
        // Wrap the attrs in a Class-shaped value with an Instance.
        // The interpreter checks instance.fields before class.methods, so
        // exposing the natives as instance fields is enough.
        let cls = Rc::new(Class {
            name: "Pattern".into(),
            methods: RefCell::new(HashMap::new()),
            fields: vec![],
            class_attrs: RefCell::new(HashMap::new()),
            bases: vec![],
            properties: std::cell::RefCell::new(std::collections::HashSet::new()),
            classmethods: std::cell::RefCell::new(std::collections::HashSet::new()),
            is_exception: false,
            is_protocol: false,
        });
        Value::Instance(Rc::new(crate::value::Instance {
            class: cls,
            fields: RefCell::new(attrs),
            chain: RefCell::new(None),
        }))
    }
    // Build a match object from a `regex::Captures`. Group 0 is the whole
    // match; groups 1.. are the capture groups. Non-participating optional
    // groups are represented as `None`.
    /// CPython's `findall` result shape: the whole match when the pattern has no
    /// capture groups, the single group when it has one, and a tuple of groups
    /// when it has more. An unmatched optional group is the empty string.
    fn re_findall_hits(re: &regex::Regex, text: &str) -> Vec<Value> {
        let groups = re.captures_len() - 1;
        if groups == 0 {
            return re
                .find_iter(text)
                .map(|m| Value::Str(Rc::new(m.as_str().to_owned())))
                .collect();
        }
        re.captures_iter(text)
            .map(|c| {
                let mut parts: Vec<Value> = (1..=groups)
                    .map(|i| {
                        Value::Str(Rc::new(
                            c.get(i).map(|m| m.as_str()).unwrap_or("").to_owned(),
                        ))
                    })
                    .collect();
                if groups == 1 {
                    parts.remove(0)
                } else {
                    Value::Tuple(Rc::new(parts))
                }
            })
            .collect()
    }

    fn captures_to_value(
        caps: Option<regex::Captures<'_>>,
        names: &HashMap<String, usize>,
    ) -> Value {
        let Some(caps) = caps else { return Value::None };
        let whole = caps.get(0).expect("group 0 always present");
        let start = whole.start() as i64;
        let end = whole.end() as i64;
        // Collect each group's optional captured text by index.
        let group_texts: Vec<Option<String>> = (0..caps.len())
            .map(|i| caps.get(i).map(|m| m.as_str().to_owned()))
            .collect();
        let names = names.clone();
        let mut attrs: crate::value::FieldMap = crate::value::FieldMap::new();
        // `.group()`/`.group(n)`/`.group("name")`/`.group(a, b, ...)`.
        let gt = group_texts.clone();
        let names_g = names.clone();
        attrs.insert(
            "group".into(),
            Value::Native(Rc::new(NativeFn::new("group", move |_i, args| {
                // Resolve an int index or a string group name.
                let resolve = |a: &Value| -> Result<usize, Unwind> {
                    if let Value::Str(s) = a {
                        names_g
                            .get(s.as_str())
                            .copied()
                            .ok_or_else(|| index_error(format!("no such group: '{}'", s)))
                    } else {
                        Ok(a.to_int()? as usize)
                    }
                };
                let pick = |idx: usize| -> Result<Value, Unwind> {
                    match gt.get(idx) {
                        None => Err(index_error("no such group")),
                        Some(None) => Ok(Value::None),
                        Some(Some(s)) => Ok(Value::Str(Rc::new(s.clone()))),
                    }
                };
                if args.is_empty() {
                    return pick(0);
                }
                if args.len() == 1 {
                    return pick(resolve(&args[0])?);
                }
                let mut out = Vec::with_capacity(args.len());
                for a in &args {
                    out.push(pick(resolve(a)?)?);
                }
                Ok(Value::Tuple(Rc::new(out)))
            }))),
        );
        // `.groupdict()` — {name: text} for every named group.
        let gt_d = group_texts.clone();
        let names_d = names.clone();
        attrs.insert(
            "groupdict".into(),
            Value::Native(Rc::new(NativeFn::new("groupdict", move |_i, _args| {
                let mut d: DictMap = IndexMap::new();
                for (name, idx) in &names_d {
                    let v = match gt_d.get(*idx) {
                        Some(Some(s)) => Value::Str(Rc::new(s.clone())),
                        _ => Value::None,
                    };
                    d.insert(HashKey::Str(Rc::new(name.clone())), v);
                }
                Ok(Value::Dict(Rc::new(RefCell::new(d))))
            }))),
        );
        // `.groups()` returns groups 1.. (not group 0).
        let gt2 = group_texts.clone();
        attrs.insert(
            "groups".into(),
            Value::Native(Rc::new(NativeFn::new("groups", move |_i, args| {
                let default = args.first().cloned().unwrap_or(Value::None);
                let out: Vec<Value> = gt2
                    .iter()
                    .skip(1)
                    .map(|g| match g {
                        Some(s) => Value::Str(Rc::new(s.clone())),
                        None => default.clone(),
                    })
                    .collect();
                Ok(Value::Tuple(Rc::new(out)))
            }))),
        );
        attrs.insert(
            "start".into(),
            Value::Native(Rc::new(NativeFn::new("start", move |_i, _args| {
                Ok(Value::Int(VmInt::from(start)))
            }))),
        );
        attrs.insert(
            "end".into(),
            Value::Native(Rc::new(NativeFn::new("end", move |_i, _args| {
                Ok(Value::Int(VmInt::from(end)))
            }))),
        );
        attrs.insert(
            "span".into(),
            Value::Native(Rc::new(NativeFn::new("span", move |_i, _args| {
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Int(VmInt::from(start)),
                    Value::Int(VmInt::from(end)),
                ])))
            }))),
        );
        let cls = Rc::new(crate::value::Class {
            name: "Match".into(),
            methods: RefCell::new(HashMap::new()),
            fields: vec![],
            class_attrs: RefCell::new(HashMap::new()),
            bases: vec![],
            properties: std::cell::RefCell::new(std::collections::HashSet::new()),
            classmethods: std::cell::RefCell::new(std::collections::HashSet::new()),
            is_exception: false,
            is_protocol: false,
        });
        Value::Instance(Rc::new(crate::value::Instance {
            class: cls,
            fields: RefCell::new(attrs),
            chain: RefCell::new(None),
        }))
    }
    make_module(
        "re",
        vec![
            (
                "compile",
                nf("compile", move |_i, args| {
                    let p = single(&args, "compile")?.py_str();
                    let r = compile_one(&p)?;
                    Ok(pattern_value(r))
                }),
            ),
            (
                "match",
                nf("match", move |_i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("match() needs pattern"))?
                        .py_str();
                    let s = args
                        .get(1)
                        .ok_or_else(|| type_error("match() needs string"))?
                        .py_str();
                    let r = compile_one(&p)?;
                    // Python's `re.match` only matches at the start of the
                    // string (unlike `re.search`). The Rust `regex` crate
                    // returns the leftmost match anywhere, so anchor by
                    // requiring `start() == 0`.
                    let caps = r.captures(&s).filter(|c| c.get(0).unwrap().start() == 0);
                    let names = name_indices(&r);
                    Ok(captures_to_value(caps, &names))
                }),
            ),
            (
                "search",
                nf("search", move |_i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("search() needs pattern"))?
                        .py_str();
                    let s = args
                        .get(1)
                        .ok_or_else(|| type_error("search() needs string"))?
                        .py_str();
                    let r = compile_one(&p)?;
                    let names = name_indices(&r);
                    Ok(captures_to_value(r.captures(&s), &names))
                }),
            ),
            (
                "fullmatch",
                nf("fullmatch", move |_i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("fullmatch() needs pattern"))?
                        .py_str();
                    let s = args
                        .get(1)
                        .ok_or_else(|| type_error("fullmatch() needs string"))?
                        .py_str();
                    let anchored = format!("^(?:{p})$");
                    let r = compile_one(&anchored)?;
                    let names = name_indices(&r);
                    Ok(captures_to_value(r.captures(&s), &names))
                }),
            ),
            (
                "sub",
                nf("sub", move |i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("sub() needs pattern"))?
                        .py_str();
                    let repl = args
                        .get(1)
                        .ok_or_else(|| type_error("sub() needs replacement"))?
                        .clone();
                    let s = args
                        .get(2)
                        .ok_or_else(|| type_error("sub() needs string"))?
                        .py_str();
                    let count = match args.get(3) {
                        Some(c) if !matches!(c, Value::None) => c.to_int()?.max(0) as usize,
                        _ => 0,
                    };
                    let r = compile_one(&p)?;
                    let (out, _) = re_sub_apply(i, &r, &repl, &s, count)?;
                    Ok(Value::Str(Rc::new(out)))
                }),
            ),
            (
                "subn",
                nf("subn", move |i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("subn() needs pattern"))?
                        .py_str();
                    let repl = args
                        .get(1)
                        .ok_or_else(|| type_error("subn() needs replacement"))?
                        .clone();
                    let s = args
                        .get(2)
                        .ok_or_else(|| type_error("subn() needs string"))?
                        .py_str();
                    let r = compile_one(&p)?;
                    let (out, n) = re_sub_apply(i, &r, &repl, &s, 0)?;
                    Ok(Value::Tuple(Rc::new(vec![
                        Value::Str(Rc::new(out)),
                        Value::Int(VmInt::from(n as i64)),
                    ])))
                }),
            ),
            (
                "finditer",
                nf("finditer", move |_i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("finditer() needs pattern"))?
                        .py_str();
                    let s = args
                        .get(1)
                        .ok_or_else(|| type_error("finditer() needs string"))?
                        .py_str();
                    let r = compile_one(&p)?;
                    let names = name_indices(&r);
                    let out: Vec<Value> = r
                        .captures_iter(&s)
                        .map(|c| captures_to_value(Some(c), &names))
                        .collect();
                    Ok(Value::List(Rc::new(RefCell::new(out))))
                }),
            ),
            (
                "findall",
                nf("findall", move |_i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("findall() needs pattern"))?
                        .py_str();
                    let s = args
                        .get(1)
                        .ok_or_else(|| type_error("findall() needs string"))?
                        .py_str();
                    let r = compile_one(&p)?;
                    let hits = re_findall_hits(&r, &s);
                    Ok(Value::List(Rc::new(RefCell::new(hits))))
                }),
            ),
            (
                "split",
                nf("split", move |_i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("split() needs pattern"))?
                        .py_str();
                    let s = args
                        .get(1)
                        .ok_or_else(|| type_error("split() needs string"))?
                        .py_str();
                    let maxsplit = match args.get(2) {
                        Some(c) if !matches!(c, Value::None) => c.to_int()?.max(0) as usize,
                        _ => 0,
                    };
                    let r = compile_one(&p)?;
                    Ok(Value::List(Rc::new(RefCell::new(re_split_apply(
                        &r, &s, maxsplit,
                    )))))
                }),
            ),
            (
                "escape",
                nf("escape", move |_i, args| {
                    let s = single(&args, "escape")?.py_str();
                    Ok(Value::Str(Rc::new(regex::escape(&s))))
                }),
            ),
            // Flag constants — accepted but currently ignored. The shim
            // engine has no flag plumbing; users that rely on
            // IGNORECASE/MULTILINE will see incorrect behaviour and
            // should fall back to `tyc run --compile`.
            ("IGNORECASE", Value::Int(VmInt::from(2))),
            ("MULTILINE", Value::Int(VmInt::from(8))),
            ("DOTALL", Value::Int(VmInt::from(16))),
            ("VERBOSE", Value::Int(VmInt::from(64))),
            ("ASCII", Value::Int(VmInt::from(256))),
        ],
    )
}

fn exception_unwind_value(e: &crate::error::VmException) -> Value {
    e.value.clone().unwrap_or_else(|| Value::Exception {
        kind: Rc::new(e.kind.clone()),
        message: Rc::new(e.message.clone()),
        args: Rc::new(if e.message.is_empty() {
            Vec::new()
        } else {
            vec![Value::Str(Rc::new(e.message.clone()))]
        }),
        chain: None,
    })
}

/// CPython `TaskGroup._is_base_error`: exactly `KeyboardInterrupt` and
/// `SystemExit` (instances, so subclasses count). Deliberately narrower than
/// [`crate::value::is_base_only_exception`] — a bare `BaseException` or a
/// `GeneratorExit` is *not* a base error to a TaskGroup and gets wrapped in
/// the group like any other failure (verified against 3.13).
fn is_taskgroup_base_error(v: &Value) -> bool {
    match v {
        Value::Exception { kind, .. } => {
            let k = kind.as_str();
            k == "KeyboardInterrupt"
                || k == "SystemExit"
                || crate::interp::builtin_exc_is_a(k, "KeyboardInterrupt")
                || crate::interp::builtin_exc_is_a(k, "SystemExit")
        }
        Value::Instance(inst) => {
            crate::interp::class_has_builtin_exc_base(&inst.class, "KeyboardInterrupt")
                || crate::interp::class_has_builtin_exc_base(&inst.class, "SystemExit")
        }
        _ => false,
    }
}

/// A task whose coroutine raised. `TaskGroup.__aexit__` re-raises the whole
/// batch as an `ExceptionGroup` before the `gather:` lowering ever reads a
/// `.result()`, so this only matters for hand-written TaskGroup use — where
/// CPython's `Task.result()` likewise re-raises the task's exception.
fn make_failed_task_value(exc: Value) -> Value {
    let for_result = exc.clone();
    make_module(
        "Task",
        vec![
            // `await task` consults this sentinel (see `force_awaitable`)
            // and re-raises, exactly like `result()` below — CPython's
            // `await` on a completed-with-exception task re-raises it.
            ("__typhon_task_error__", exc.clone()),
            (
                "result",
                Value::Native(Rc::new(NativeFn::new("result", move |i, _args| {
                    Err(i.value_to_exception(for_result.clone()))
                }))),
            ),
            ("done", nf("done", |_i, _args| Ok(Value::Bool(true)))),
            ("cancel", nf("cancel", |_i, _args| Ok(Value::Bool(false)))),
            (
                "cancelled",
                nf("cancelled", |_i, _args| Ok(Value::Bool(false))),
            ),
            (
                "exception",
                Value::Native(Rc::new(NativeFn::new("exception", move |_i, _args| {
                    Ok(exc.clone())
                }))),
            ),
        ],
    )
}

fn make_task_value(result: Value) -> Value {
    let result_for_member = result.clone();
    make_module(
        "Task",
        vec![
            ("__typhon_task_result__", result.clone()),
            (
                "result",
                Value::Native(Rc::new(NativeFn::new("result", move |_i, _args| {
                    Ok(result_for_member.clone())
                }))),
            ),
            ("done", nf("done", |_i, _args| Ok(Value::Bool(true)))),
            ("cancel", nf("cancel", |_i, _args| Ok(Value::Bool(false)))),
            (
                "cancelled",
                nf("cancelled", |_i, _args| Ok(Value::Bool(false))),
            ),
        ],
    )
}

/// The cooperative `asyncio` shim. Semantics: every coroutine runs to
/// completion at its force point, in program order. That preserves
/// results for the dominant shapes (sequential awaits, `gather:` over
/// independent calls, retries, timeouts) and turns genuinely
/// interleaving-dependent programs (producer/consumer hand-off in the
/// same gather) into a *loud* RuntimeError instead of a silent hang.
fn make_asyncio_module() -> Value {
    let task_group = nf("TaskGroup", |_i, _args| {
        let tg = make_module("asyncio.TaskGroup", vec![]);
        let tg_for_enter = tg.clone();
        // F5 — a child task that fails must NOT propagate its bare exception
        // out of `create_task`. CPython's TaskGroup collects child failures
        // and re-raises them from `__aexit__` wrapped in
        // `ExceptionGroup('unhandled errors in a TaskGroup', [...])`, so a
        // surrounding `except ValueError:` does not match and only an
        // `except* ValueError:` does. Before this, the VM raised the bare
        // exception straight out of `create_task`, so the identical `.ty`
        // source caught the error under `tyc run` and died with an uncaught
        // ExceptionGroup under `tyc build && python` — opposite outcomes from
        // a clean check.
        //
        // Divergence that remains (inherent to the VM's sequential execution,
        // documented in docs/vm.md): CPython *cancels* the sibling tasks when
        // one fails, so a task that had not started contributes nothing to the
        // group. The VM runs every `create_task` body to completion at its
        // force point, so every failure is collected. The single-failure case
        // — overwhelmingly the common one for `gather:` — is identical.
        let failures: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
        let failures_for_create = failures.clone();
        let failures_for_exit = failures.clone();
        if let Value::Module(m) = &tg {
            let mut members = m.members.borrow_mut();
            members.insert(
                "__aenter__".to_owned(),
                Value::Native(Rc::new(NativeFn::new("__aenter__", move |_i, _args| {
                    Ok(tg_for_enter.clone())
                }))),
            );
            members.insert(
                "__aexit__".to_owned(),
                Value::Native(Rc::new(NativeFn::new("__aexit__", move |i, args| {
                    let mut pending = failures_for_exit.borrow_mut();
                    // CPython wraps a body-raised exception into the same
                    // group (verified against 3.13), so fold it in here —
                    // unless it is a task failure the group already
                    // collected, re-raised into the body by `await t` on the
                    // failed task. CPython's body is cancelled at that await
                    // and the group holds the error exactly once (3.13:
                    // `ExceptionGroup('unhandled errors in a TaskGroup',
                    // [ValueError('boom')])`, one member).
                    if let Some(body_exc) = args.get(1) {
                        if !matches!(body_exc, Value::None)
                            && !pending
                                .iter()
                                .any(|p| crate::value::exception_values_identical(p, body_exc))
                        {
                            pending.push(body_exc.clone());
                        }
                    }
                    if pending.is_empty() {
                        return Ok(Value::Bool(false));
                    }
                    let subs: Vec<Value> = pending.drain(..).collect();
                    drop(pending);
                    // CPython's TaskGroup singles out KeyboardInterrupt /
                    // SystemExit (`_is_base_error`) as `_base_error` — the
                    // first one observed is re-raised BARE, never wrapped in
                    // the group, and every other collected failure is
                    // dropped (verified against 3.13 for body- and
                    // child-raised cases). A bare `BaseException` is *not* a
                    // base error there and stays in the group.
                    if let Some(base) = subs.iter().find(|v| is_taskgroup_base_error(v)) {
                        return Err(i.value_to_exception(base.clone()));
                    }
                    let kind = crate::value::exception_group_kind_for(&subs);
                    let group = crate::value::make_exception_group(
                        kind,
                        "unhandled errors in a TaskGroup",
                        subs,
                        false,
                    );
                    Err(Unwind::Exception(
                        crate::error::VmException::new(kind, group.py_str()).with_value(group),
                    ))
                }))),
            );
            members.insert(
                "create_task".to_owned(),
                Value::Native(Rc::new(NativeFn::new("create_task", move |i, args| {
                    let coro = args
                        .into_iter()
                        .next()
                        .ok_or_else(|| type_error("create_task() requires a coroutine"))?;
                    match i.force_awaitable(coro) {
                        Ok(result) => Ok(make_task_value(result)),
                        Err(Unwind::Exception(e)) => {
                            let value = exception_unwind_value(&e);
                            failures_for_create.borrow_mut().push(value.clone());
                            Ok(make_failed_task_value(value))
                        }
                        Err(other) => Err(other),
                    }
                }))),
            );
        }
        Ok(tg)
    });
    let timeout_cm = nf("timeout", |_i, args| {
        // Sequential execution can't interrupt the body mid-await, but the
        // *observable* control flow converges with CPython by checking the
        // wall clock at exit: a body that overran the budget raises
        // TimeoutError there (after its side effects, unlike a real
        // cancellation — documented divergence).
        let budget = args.first().and_then(|v| v.to_float().ok()).unwrap_or(0.0);
        let started = std::time::Instant::now();
        let cm = make_module("asyncio.timeout", vec![]);
        let cm_for_enter = cm.clone();
        if let Value::Module(m) = &cm {
            let mut members = m.members.borrow_mut();
            members.insert(
                "__aenter__".to_owned(),
                Value::Native(Rc::new(NativeFn::new("__aenter__", move |_i, _args| {
                    Ok(cm_for_enter.clone())
                }))),
            );
            members.insert(
                "__aexit__".to_owned(),
                Value::Native(Rc::new(NativeFn::new("__aexit__", move |_i, args| {
                    // Re-raising over an in-flight exception would mask it;
                    // only convert a clean exit into TimeoutError.
                    let body_raised = !matches!(args.first(), Some(Value::None) | None);
                    if !body_raised && started.elapsed().as_secs_f64() > budget {
                        return Err(Unwind::Exception(crate::error::VmException::new(
                            "TimeoutError",
                            "",
                        )));
                    }
                    Ok(Value::Bool(false))
                }))),
            );
        }
        Ok(cm)
    });
    let queue = nf("Queue", |_i, args| Ok(make_asyncio_queue(&args, &[])));
    // The exception classes `asyncio` re-exports or defines. `TimeoutError`
    // has been the builtin since 3.11; `CancelledError` derives from
    // `BaseException`; `QueueEmpty` / `QueueFull` are what `Queue.get_nowait`
    // / `put_nowait` raise. Each constructs the same `Value::Exception` shape
    // the builtin constructors do, so `except asyncio.X` matches by kind.
    let exc_ctor = |kind: &'static str| -> (&'static str, Value) {
        (
            kind,
            Value::Native(Rc::new(NativeFn::new(kind, move |_i, args| {
                let msg = args.first().map(|v| v.py_str()).unwrap_or_default();
                Ok(Value::Exception {
                    kind: Rc::new(kind.to_owned()),
                    message: Rc::new(msg),
                    args: Rc::new(args),
                    chain: None,
                })
            }))),
        )
    };
    make_module(
        "asyncio",
        vec![
            exc_ctor("TimeoutError"),
            exc_ctor("CancelledError"),
            exc_ctor("InvalidStateError"),
            exc_ctor("QueueEmpty"),
            exc_ctor("QueueFull"),
            (
                "run",
                nf("run", |i, args| {
                    let coro = args
                        .into_iter()
                        .next()
                        .ok_or_else(|| type_error("asyncio.run() requires a coroutine"))?;
                    i.force_awaitable(coro)
                }),
            ),
            (
                "sleep",
                nf("sleep", |_i, args| {
                    if let Some(v) = args.first() {
                        let secs = v.to_float().unwrap_or(0.0);
                        if secs > 0.0 {
                            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                        }
                    }
                    Ok(Value::None)
                }),
            ),
            (
                "gather",
                nf("gather", |i, args| {
                    // Positional-only fast path (return_exceptions arrives
                    // via the kwargs table in `call_with_kwargs`).
                    let mut out: Vec<Value> = Vec::with_capacity(args.len());
                    for coro in args {
                        out.push(i.force_awaitable(coro)?);
                    }
                    Ok(Value::List(Rc::new(RefCell::new(out))))
                }),
            ),
            (
                "wait_for",
                nf("wait_for", |i, args| {
                    let coro = args
                        .into_iter()
                        .next()
                        .ok_or_else(|| type_error("wait_for() requires a coroutine"))?;
                    i.force_awaitable(coro)
                }),
            ),
            ("TaskGroup", task_group),
            ("timeout", timeout_cm),
            ("Queue", queue),
            (
                "create_task",
                nf("create_task", |i, args| {
                    let coro = args
                        .into_iter()
                        .next()
                        .ok_or_else(|| type_error("create_task() requires a coroutine"))?;
                    let result = i.force_awaitable(coro)?;
                    Ok(make_task_value(result))
                }),
            ),
            // `asyncio.to_thread(fn, *args, **kwargs)` — under the VM's
            // sequential scheduler there is no other thread to move the call
            // to, so it runs inline and its result is handed back as a
            // completed awaitable. Observationally identical for the
            // overwhelmingly common `await asyncio.to_thread(blocking_fn)`;
            // a program depending on the *concurrency* needs
            // `tyc run --compile`, like the rest of the asyncio shim.
            (
                "to_thread",
                nf("to_thread", |i, args| {
                    let (pos, kw) = split_kwargs(&args);
                    let Some((func, rest)) = pos.split_first() else {
                        return Err(type_error("to_thread() requires a callable"));
                    };
                    let result = i.call_value(func.clone(), rest.to_vec(), &kw)?;
                    Ok(make_task_value(result))
                }),
            ),
            // `asyncio.Lock` / `Semaphore` / `Event` / `Condition` — the
            // sequential scheduler never has two coroutines inside the same
            // critical section, so acquisition always succeeds immediately.
            // Modelled so `async with lock:` runs rather than raising.
            ("Lock", nf("Lock", |_i, _args| Ok(make_asyncio_lock()))),
            (
                "Semaphore",
                nf("Semaphore", |_i, _args| Ok(make_asyncio_lock())),
            ),
            (
                "BoundedSemaphore",
                nf("BoundedSemaphore", |_i, _args| Ok(make_asyncio_lock())),
            ),
            ("Event", nf("Event", |_i, _args| Ok(make_asyncio_event()))),
        ],
    )
}

/// `asyncio.Lock` / `Semaphore` under the sequential scheduler: acquisition
/// always succeeds at once, so the object only has to satisfy the async
/// context-manager protocol and the explicit `acquire` / `release` pair.
fn make_asyncio_lock() -> Value {
    let acquired = Rc::new(std::cell::Cell::new(false));
    let enter_flag = acquired.clone();
    let exit_flag = acquired.clone();
    let acquire_flag = acquired.clone();
    let release_flag = acquired.clone();
    let locked_flag = acquired.clone();
    native_object(
        "asyncio.Lock",
        vec![
            (
                "__aenter__",
                Value::Native(Rc::new(NativeFn::new("__aenter__", move |_i, _args| {
                    enter_flag.set(true);
                    Ok(Value::None)
                }))),
            ),
            (
                "__aexit__",
                Value::Native(Rc::new(NativeFn::new("__aexit__", move |_i, _args| {
                    exit_flag.set(false);
                    Ok(Value::Bool(false))
                }))),
            ),
            (
                "acquire",
                Value::Native(Rc::new(NativeFn::new("acquire", move |_i, _args| {
                    acquire_flag.set(true);
                    Ok(Value::Bool(true))
                }))),
            ),
            (
                "release",
                Value::Native(Rc::new(NativeFn::new("release", move |_i, _args| {
                    release_flag.set(false);
                    Ok(Value::None)
                }))),
            ),
            (
                "locked",
                Value::Native(Rc::new(NativeFn::new("locked", move |_i, _args| {
                    Ok(Value::Bool(locked_flag.get()))
                }))),
            ),
        ],
    )
}

/// `asyncio.Event` under the sequential scheduler. `wait()` on an unset
/// event would deadlock, so it fails loudly with the same reasoning as
/// `Queue.get` on an empty queue.
fn make_asyncio_event() -> Value {
    let flag = Rc::new(std::cell::Cell::new(false));
    let set_flag = flag.clone();
    let clear_flag = flag.clone();
    let is_set_flag = flag.clone();
    let wait_flag = flag.clone();
    native_object(
        "asyncio.Event",
        vec![
            (
                "set",
                Value::Native(Rc::new(NativeFn::new("set", move |_i, _args| {
                    set_flag.set(true);
                    Ok(Value::None)
                }))),
            ),
            (
                "clear",
                Value::Native(Rc::new(NativeFn::new("clear", move |_i, _args| {
                    clear_flag.set(false);
                    Ok(Value::None)
                }))),
            ),
            (
                "is_set",
                Value::Native(Rc::new(NativeFn::new("is_set", move |_i, _args| {
                    Ok(Value::Bool(is_set_flag.get()))
                }))),
            ),
            (
                "wait",
                Value::Native(Rc::new(NativeFn::new("wait", move |_i, _args| {
                    if wait_flag.get() {
                        return Ok(Value::Bool(true));
                    }
                    Err(crate::error::Unwind::Exception(
                        crate::error::VmException::new(
                            "RuntimeError",
                            "asyncio.Event.wait() on an unset event would deadlock under the \
                             VM's sequential scheduler — set the event before awaiting it, or \
                             run with `tyc run --compile`",
                        ),
                    ))
                }))),
            ),
        ],
    )
}

/// List-backed `asyncio.Queue`. `get` on an empty queue (or `put` past
/// `maxsize`) would deadlock under sequential semantics — both raise a
/// RuntimeError naming the fix instead of hanging.
fn make_asyncio_queue(args: &[Value], kwargs: &[(String, Value)]) -> Value {
    let maxsize: i64 = kwargs
        .iter()
        .find(|(k, _)| k == "maxsize")
        .and_then(|(_, v)| v.to_int().ok())
        .or_else(|| args.first().and_then(|v| v.to_int().ok()))
        .unwrap_or(0);
    let buf: Rc<RefCell<std::collections::VecDeque<Value>>> =
        Rc::new(RefCell::new(std::collections::VecDeque::new()));
    let q = make_module("asyncio.Queue", vec![]);
    if let Value::Module(m) = &q {
        let mut members = m.members.borrow_mut();
        let b = buf.clone();
        members.insert(
            "put".to_owned(),
            Value::Native(Rc::new(NativeFn::new("put", move |_i, args| {
                if maxsize > 0 && b.borrow().len() as i64 >= maxsize {
                    return Err(Unwind::Exception(crate::error::VmException::new(
                        "RuntimeError",
                        "asyncio.Queue.put on a full queue would deadlock under the VM's \
                         sequential scheduler — run with `tyc run --compile` for real \
                         concurrency",
                    )));
                }
                let v = args.into_iter().next().unwrap_or(Value::None);
                b.borrow_mut().push_back(v);
                Ok(Value::None)
            }))),
        );
        let b = buf.clone();
        members.insert(
            "put_nowait".to_owned(),
            Value::Native(Rc::new(NativeFn::new("put_nowait", move |_i, args| {
                let v = args.into_iter().next().unwrap_or(Value::None);
                b.borrow_mut().push_back(v);
                Ok(Value::None)
            }))),
        );
        let b = buf.clone();
        members.insert(
            "get".to_owned(),
            Value::Native(Rc::new(NativeFn::new("get", move |_i, _args| {
                b.borrow_mut().pop_front().ok_or_else(|| {
                    Unwind::Exception(crate::error::VmException::new(
                        "RuntimeError",
                        "asyncio.Queue.get on an empty queue would deadlock under the VM's \
                         sequential scheduler — run with `tyc run --compile` for real \
                         concurrency",
                    ))
                })
            }))),
        );
        let b = buf.clone();
        members.insert(
            "get_nowait".to_owned(),
            Value::Native(Rc::new(NativeFn::new("get_nowait", move |_i, _args| {
                b.borrow_mut().pop_front().ok_or_else(|| {
                    Unwind::Exception(crate::error::VmException::new(
                        "QueueEmpty",
                        "queue is empty",
                    ))
                })
            }))),
        );
        let b = buf.clone();
        members.insert(
            "qsize".to_owned(),
            Value::Native(Rc::new(NativeFn::new("qsize", move |_i, _args| {
                Ok(Value::Int(VmInt::from(b.borrow().len() as i64)))
            }))),
        );
        let b = buf.clone();
        members.insert(
            "empty".to_owned(),
            Value::Native(Rc::new(NativeFn::new("empty", move |_i, _args| {
                Ok(Value::Bool(b.borrow().is_empty()))
            }))),
        );
    }
    q
}

/// `collections.abc` shim — every abstract base name maps to an identity
/// native. These names appear in annotations (evaluated at def time) and
/// occasionally as bases; nothing in the VM dispatches through them.
fn make_collections_abc_module() -> Value {
    let mut entries: Vec<(&str, Value)> = Vec::new();
    for name in [
        "Callable",
        "Iterable",
        "Iterator",
        "Generator",
        "AsyncIterable",
        "AsyncIterator",
        "AsyncGenerator",
        "Awaitable",
        "Coroutine",
        "Sequence",
        "MutableSequence",
        "Mapping",
        "MutableMapping",
        "Set",
        "MutableSet",
        "Collection",
        "Container",
        "Reversible",
        "Hashable",
        "Sized",
        "KeysView",
        "ValuesView",
        "ItemsView",
        "MappingView",
        "ByteString",
        "Buffer",
    ] {
        entries.push((name, identity_native(name)));
    }
    make_module("collections.abc", entries)
}

fn make_abc_module() -> Value {
    let mut entries: Vec<(&str, Value)> = Vec::new();
    for name in [
        "ABC",
        "ABCMeta",
        "abstractmethod",
        "abstractproperty",
        "abstractclassmethod",
        "abstractstaticmethod",
        "update_abstractmethods",
    ] {
        entries.push((name, identity_native(name)));
    }
    make_module("abc", entries)
}

/// `heapq` shim — implements the small-but-essential surface for
/// priority-queue-style algorithms (Dijkstra, A*, top-K, merge-K-sorted).
/// Internally we re-heapify on every push/pop since the VM's lists don't
/// expose a stable heap-invariant; the algorithmic complexity goes from
/// O(log n) to O(n log n) but the surface is correct.
fn make_heapq_module() -> Value {
    fn sift_down(list: &mut [Value], start: usize, pos: usize) {
        let mut pos = pos;
        let new_item = list[pos].clone();
        while pos > start {
            let parent = (pos - 1) >> 1;
            if value_lt(&new_item, &list[parent]) {
                list[pos] = list[parent].clone();
                pos = parent;
            } else {
                break;
            }
        }
        list[pos] = new_item;
    }
    fn sift_up(list: &mut [Value], pos: usize) {
        let endpos = list.len();
        let startpos = pos;
        let mut pos = pos;
        let new_item = list[pos].clone();
        let mut child = 2 * pos + 1;
        while child < endpos {
            let right = child + 1;
            if right < endpos && !value_lt(&list[child], &list[right]) {
                child = right;
            }
            list[pos] = list[child].clone();
            pos = child;
            child = 2 * pos + 1;
        }
        list[pos] = new_item;
        sift_down(list, startpos, pos);
    }
    fn value_lt(a: &Value, b: &Value) -> bool {
        // Reuse the VM's general comparison. Returns false on
        // incomparable types — which matches CPython at least to the
        // extent that the program then sees a non-sensical heap ordering
        // instead of crashing.
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x < y,
            (Value::Float(x), Value::Float(y)) => x < y,
            (Value::Int(x), Value::Float(y)) => x.to_f64() < *y,
            (Value::Float(x), Value::Int(y)) => *x < y.to_f64(),
            (Value::Str(x), Value::Str(y)) => x < y,
            (Value::Tuple(x), Value::Tuple(y)) => {
                for (xi, yi) in x.iter().zip(y.iter()) {
                    if value_lt(xi, yi) {
                        return true;
                    }
                    if value_lt(yi, xi) {
                        return false;
                    }
                }
                x.len() < y.len()
            }
            _ => false,
        }
    }
    let heappush = nf("heappush", |_i, args| {
        if args.len() != 2 {
            return Err(type_error("heappush(heap, item) takes 2 arguments"));
        }
        let heap = match &args[0] {
            Value::List(l) => l.clone(),
            _ => return Err(type_error("heappush expects a list")),
        };
        let item = args[1].clone();
        let mut h = heap.borrow_mut();
        h.push(item);
        let last = h.len() - 1;
        sift_down(&mut h, 0, last);
        Ok(Value::None)
    });
    let heappop = nf("heappop", |_i, args| {
        let heap = match args.first() {
            Some(Value::List(l)) => l.clone(),
            _ => return Err(type_error("heappop expects a list")),
        };
        let mut h = heap.borrow_mut();
        let last = h
            .pop()
            .ok_or_else(|| crate::error::index_error("pop from an empty heap"))?;
        if h.is_empty() {
            Ok(last)
        } else {
            let returned = std::mem::replace(&mut h[0], last);
            sift_up(&mut h, 0);
            Ok(returned)
        }
    });
    let heapify = nf("heapify", |_i, args| {
        let heap = match args.first() {
            Some(Value::List(l)) => l.clone(),
            _ => return Err(type_error("heapify expects a list")),
        };
        let mut h = heap.borrow_mut();
        if h.len() >= 2 {
            for i in (0..h.len() / 2).rev() {
                sift_up(&mut h, i);
            }
        }
        Ok(Value::None)
    });
    let nsmallest = nf("nsmallest", |i, args| {
        if args.len() < 2 {
            return Err(type_error("nsmallest(n, iterable) takes 2 arguments"));
        }
        let n = args[0].to_int()?;
        let it = i.make_iter(args[1].clone())?;
        let mut items: Vec<Value> = Vec::new();
        while let Some(x) = i.iter_next(&it)? {
            items.push(x);
        }
        items.sort_by(|a, b| {
            if value_lt(a, b) {
                std::cmp::Ordering::Less
            } else if value_lt(b, a) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        items.truncate(n.max(0) as usize);
        Ok(Value::List(Rc::new(RefCell::new(items))))
    });
    let nlargest = nf("nlargest", |i, args| {
        if args.len() < 2 {
            return Err(type_error("nlargest(n, iterable) takes 2 arguments"));
        }
        let n = args[0].to_int()?;
        let it = i.make_iter(args[1].clone())?;
        let mut items: Vec<Value> = Vec::new();
        while let Some(x) = i.iter_next(&it)? {
            items.push(x);
        }
        items.sort_by(|a, b| {
            if value_lt(b, a) {
                std::cmp::Ordering::Less
            } else if value_lt(a, b) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        items.truncate(n.max(0) as usize);
        Ok(Value::List(Rc::new(RefCell::new(items))))
    });
    make_module(
        "heapq",
        vec![
            ("heappush", heappush),
            ("heappop", heappop),
            ("heapify", heapify),
            ("nsmallest", nsmallest),
            ("nlargest", nlargest),
        ],
    )
}

/// Whether a `Value::Dict` carries the `__typhon_frozen__` sentinel
/// (inserted by `deep_freeze_value`). Used by the dict mutators to
/// raise the same TypeError CPython's MappingProxy produces.
pub fn dict_is_frozen(d: &Rc<RefCell<DictMap>>) -> bool {
    let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
    matches!(d.borrow().get(&frozen_key), Some(Value::Bool(true)))
}

/// Whether a `Value::Set` carries the `__typhon_frozen__` sentinel
/// (inserted by `deep_freeze_value`'s Set arm). `set_method` mutators
/// (`add`, `remove`, `discard`, `pop`, `clear`, `update`, etc.) refuse
/// to operate on a frozen set; iteration / len / repr filter the
/// sentinel out of user-visible output. Review thread codex + copilot
/// on PR #147.
pub fn set_is_frozen(s: &Rc<RefCell<std::collections::HashSet<HashKey>>>) -> bool {
    let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
    s.borrow().contains(&frozen_key)
}

/// Deep-freeze a value the same way `typhon_runtime.freeze.deep_freeze`
/// does in the compile path: list → tuple of frozen elements, dict and
/// set get marked frozen so subsequent mutation operations refuse, and
/// frozen-class instances are passed through. Anything not freezable
/// (open file handles, generators) raises `TypeError`. The marker is
/// stored as a sentinel entry whose presence the list/dict/set method
/// dispatch table checks before mutating.
fn deep_freeze_value(v: Value) -> Result<Value, Unwind> {
    match v {
        Value::Coroutine(_) => Err(type_error("cannot freeze a coroutine object".to_string())),
        Value::None
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Complex(..)
        | Value::Str(_)
        | Value::Bytes(_)
        | Value::Range { .. }
        | Value::Class(_)
        | Value::Function(_)
        | Value::Native(_)
        | Value::Module(_)
        | Value::Exception { .. } => Ok(v),
        Value::List(l) => {
            // Recursively freeze elements and surface as a tuple. The
            // compile path turns list → tuple to make the immutability
            // hold at the Python level.
            let frozen: Vec<Value> = l
                .borrow()
                .iter()
                .cloned()
                .map(deep_freeze_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Tuple(Rc::new(frozen)))
        }
        Value::Tuple(items) => {
            let frozen: Vec<Value> = items
                .iter()
                .cloned()
                .map(deep_freeze_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Tuple(Rc::new(frozen)))
        }
        Value::Dict(d) => {
            // Build a fresh dict, freeze each value, then insert a hidden
            // `__typhon_frozen__` sentinel that the dict method dispatch
            // table consults before mutation.
            let mut new_map: DictMap = IndexMap::new();
            for (k, val) in d.borrow().iter() {
                let frozen_val = deep_freeze_value(val.clone())?;
                new_map.insert(k.clone(), frozen_val);
            }
            new_map.insert(
                HashKey::Str(Rc::new("__typhon_frozen__".to_owned())),
                Value::Bool(true),
            );
            Ok(Value::Dict(Rc::new(RefCell::new(new_map))))
        }
        Value::Set(s) => {
            // Tag the resulting set with the same `__typhon_frozen__`
            // sentinel the Dict path uses; `set_is_frozen` checks for
            // it before every mutator and refuses `add`/`remove`/
            // `clear` (review threads codex and copilot on PR #147).
            // Iteration / len / repr filter the sentinel so it never
            // leaks into user-visible output.
            let mut elements: std::collections::HashSet<HashKey> =
                s.borrow().iter().cloned().collect();
            elements.insert(HashKey::Str(Rc::new("__typhon_frozen__".to_owned())));
            Ok(Value::Set(Rc::new(RefCell::new(elements))))
        }
        Value::Instance(inst) => {
            // Freeze every field in place; a frozen-class declaration on
            // the type already keeps individual field assignments rejected
            // at desugar time, so this is belt-and-braces.
            let mut new_fields: crate::value::FieldMap = crate::value::FieldMap::new();
            for (k, val) in inst.fields.borrow().iter() {
                new_fields.insert(k.clone(), deep_freeze_value(val.clone())?);
            }
            Ok(Value::Instance(Rc::new(crate::value::Instance {
                class: inst.class.clone(),
                fields: RefCell::new(new_fields),
                chain: RefCell::new(None),
            })))
        }
        Value::ResultOk(v) => Ok(Value::ResultOk(Box::new(deep_freeze_value(*v)?))),
        Value::ResultErr(v) => Ok(Value::ResultErr(Box::new(deep_freeze_value(*v)?))),
        // TODO(builtins agent): a dict-view is an ephemeral, already-read-only
        // value; freezing it is a no-op here. Revisit if `freeze let v = d.keys()`
        // ever needs view-specific semantics.
        Value::DictView { .. } => Ok(v),
        Value::Iter(_) | Value::BoundMethod { .. } => Err(crate::error::Unwind::Exception(
            crate::error::VmException::new(
                "TypeError",
                "deep_freeze cannot freeze this value; types without an immutable \
                 equivalent (open handles, generators, non-frozen dataclasses, ...) \
                 must not appear in a `freeze let` value",
            ),
        )),
    }
}

/// Minimal `pydantic` shim — exposes a `BaseModel` placeholder class and
/// a `ConfigDict` no-op constructor so that emitted `model Foo:` classes
/// import cleanly under `tyc run`. Field validation, `.model_validate`,
/// `.model_dump_json`, etc. are not implemented — programs that need those
/// must run via `tyc run --compile`. Without this shim, even declaring
/// (not instantiating) a `model` class makes the file unrunnable in the VM.
fn make_pydantic_module() -> Value {
    let base_model = Value::Class(Rc::new(crate::value::Class {
        name: "BaseModel".to_owned(),
        methods: std::cell::RefCell::new(HashMap::new()),
        fields: vec![],
        class_attrs: std::cell::RefCell::new(HashMap::new()),
        bases: vec![],
        properties: std::cell::RefCell::new(std::collections::HashSet::new()),
        classmethods: std::cell::RefCell::new(std::collections::HashSet::new()),
        is_exception: false,
        is_protocol: false,
    }));
    let config_dict = nf("ConfigDict", |_i, _args| {
        // Accept any kwargs and ignore — purely a config-record stub.
        Ok(Value::Dict(Rc::new(RefCell::new(IndexMap::new()))))
    });
    make_module(
        "pydantic",
        vec![("BaseModel", base_model), ("ConfigDict", config_dict)],
    )
}

/// Minimal `contextlib` shim — exposes `@contextmanager` and
/// `@asynccontextmanager` as identity decorators that return the
/// generator function itself. The user's `with cm() as x:` lowering goes
/// through the VM's `__enter__`/`__exit__` protocol on the resulting
/// object; `@contextmanager` semantics around `yield` aren't fully
/// reproduced (those need generator support in the VM), but the
/// decorator no longer raises on import.
/// `__future__` — one feature flag object per name CPython defines. The
/// statement itself is compile-time, so the values only need to exist and be
/// truthy.
fn make_future_module() -> Value {
    let entries: Vec<(&str, Value)> = [
        "annotations",
        "nested_scopes",
        "generators",
        "division",
        "absolute_import",
        "with_statement",
        "print_function",
        "unicode_literals",
        "barry_as_FLUFL",
        "generator_stop",
    ]
    .into_iter()
    .map(|name| (name, native_object("_Feature", vec![])))
    .collect();
    make_module("__future__", entries)
}

fn make_contextlib_module(interp: &mut Interpreter) -> Value {
    // `@contextmanager` wraps a generator function into a factory whose calls
    // hand back a context manager driving that generator: `__enter__` runs
    // the body up to its `yield` (the yielded value is what `as` binds) and
    // `__exit__` resumes it — throwing the `with` body's exception in at the
    // `yield` when there is one — so setup and teardown run around the block
    // exactly as under CPython's `contextlib._GeneratorContextManager`.
    let contextmanager = nf("contextmanager", |_i, args| {
        let func = args.into_iter().next().unwrap_or(Value::None);
        Ok(Value::Native(Rc::new(NativeFn::new(
            "contextmanager_factory",
            move |i, call_args| {
                let (pos, kw) = split_kwargs(&call_args);
                let gen = i.call_value(func.clone(), pos.to_vec(), &kw)?;
                Ok(generator_context_manager(gen))
            },
        ))))
    });
    let asynccontextmanager = nf("asynccontextmanager", |_i, args| {
        let func = args.into_iter().next().unwrap_or(Value::None);
        Ok(Value::Native(Rc::new(NativeFn::new(
            "asynccontextmanager_factory",
            move |i, call_args| {
                let (pos, kw) = split_kwargs(&call_args);
                let gen = i.call_value(func.clone(), pos.to_vec(), &kw)?;
                // An `async def` *with* a `yield` is an async generator, and
                // calling it already produces the generator object. Forcing
                // that would run the body past its `yield` — the teardown
                // after the yield would happen before `__aenter__` returned,
                // printing "close" before the `async with` body. Only a
                // plain coroutine (a decorator applied to a non-generator)
                // needs forcing.
                let gen = if crate::interp::as_generator(&gen).is_some() {
                    gen
                } else {
                    i.force_awaitable(gen)?
                };
                Ok(async_generator_context_manager(gen))
            },
        ))))
    });
    let mut entries = vec![
        ("contextmanager", contextmanager),
        ("asynccontextmanager", asynccontextmanager),
    ];
    // `suppress` / `nullcontext` / `closing` / `redirect_*` / `ExitStack` are
    // ordinary Python classes — the shim is both shorter and closer to
    // CPython than hand-rolled natives would be.
    let extras = compile_helpers(interp, shims::CONTEXTLIB_EXTRA).unwrap_or_default();
    for (k, v) in extras {
        if k.starts_with('_') {
            continue;
        }
        entries.push((intern_shim_name(k), v));
    }
    make_module("contextlib", entries)
}

/// Intern a shim's exported binding name so it can be used where a
/// `&'static str` is required. Shim namespaces are fixed and small, so each
/// name is allocated at most once per process.
fn intern_shim_name(name: String) -> &'static str {
    thread_local! {
        static INTERNED: RefCell<HashMap<String, &'static str>> = RefCell::new(HashMap::new());
    }
    INTERNED.with(|map| {
        let mut map = map.borrow_mut();
        if let Some(existing) = map.get(&name) {
            return *existing;
        }
        let leaked: &'static str = Box::leak(name.clone().into_boxed_str());
        map.insert(name, leaked);
        leaked
    })
}

/// An object exposing native functions as attributes — the shape the VM
/// uses for stdlib objects that need methods but no user-visible class body
/// (`re.Pattern`, `re.Match`, a `@contextmanager` manager, …). Attribute
/// reads hit the instance fields first, so the natives dispatch directly.
pub(crate) fn native_object(class_name: &str, fields: Vec<(&str, Value)>) -> Value {
    let cls = Rc::new(crate::value::Class {
        name: class_name.to_owned(),
        methods: RefCell::new(HashMap::new()),
        fields: vec![],
        class_attrs: RefCell::new(HashMap::new()),
        bases: vec![],
        properties: RefCell::new(std::collections::HashSet::new()),
        classmethods: RefCell::new(std::collections::HashSet::new()),
        is_exception: false,
        is_protocol: false,
    });
    let fields: crate::value::FieldMap =
        fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    Value::Instance(Rc::new(crate::value::Instance {
        class: cls,
        fields: RefCell::new(fields),
        chain: RefCell::new(None),
    }))
}

/// The context manager `@contextmanager` builds around one generator object.
fn generator_context_manager(gen: Value) -> Value {
    let gen_enter = gen.clone();
    let enter = NativeFn::new("__enter__", move |i, _args| {
        match i.iter_next(&gen_enter)? {
            Some(v) => Ok(v),
            None => Err(Unwind::Exception(crate::error::VmException::new(
                "RuntimeError",
                "generator didn't yield",
            ))),
        }
    });
    let gen_exit = gen.clone();
    let exit = NativeFn::new("__exit__", move |i, args| {
        let exc_value = args.get(1).cloned().unwrap_or(Value::None);
        if matches!(exc_value, Value::None) {
            // Normal exit: the body must run to completion.
            return match i.iter_next(&gen_exit)? {
                Some(_) => Err(Unwind::Exception(crate::error::VmException::new(
                    "RuntimeError",
                    "generator didn't stop",
                ))),
                None => Ok(Value::Bool(false)),
            };
        }
        // The `with` body raised: throw it in at the `yield`. A generator
        // that swallows it and finishes suppresses the exception; one that
        // lets the same exception out does not; a *different* exception
        // propagates instead.
        let Some(g) = crate::interp::as_generator(&gen_exit) else {
            return Ok(Value::Bool(false));
        };
        let Unwind::Exception(exc) = i.value_to_exception(exc_value.clone()) else {
            return Ok(Value::Bool(false));
        };
        match i.generator_resume(&g, None, Some(exc)) {
            Ok(Some(_)) => Err(Unwind::Exception(crate::error::VmException::new(
                "RuntimeError",
                "generator didn't stop after throw()",
            ))),
            Ok(None) => Ok(Value::Bool(true)),
            Err(Unwind::Exception(e)) => {
                let same = match &e.value {
                    Some(v) => crate::value::exception_values_identical(v, &exc_value),
                    None => false,
                };
                if same {
                    Ok(Value::Bool(false))
                } else {
                    Err(Unwind::Exception(e))
                }
            }
            Err(other) => Err(other),
        }
    });
    native_object(
        "_GeneratorContextManager",
        vec![
            ("__enter__", Value::Native(Rc::new(enter))),
            ("__exit__", Value::Native(Rc::new(exit))),
        ],
    )
}

/// The `@asynccontextmanager` counterpart. The VM forces every coroutine at
/// its await point and materialises async generators, so the *driving* is
/// identical to the sync case — only the protocol names differ. Without
/// this, `asynccontextmanager` was an identity decorator, so
/// `async with session(...)` saw the raw generator and raised
/// "does not support the asynchronous context manager protocol" on a
/// program `tyc build` runs fine.
fn async_generator_context_manager(gen: Value) -> Value {
    let sync = generator_context_manager(gen);
    let Value::Instance(inner) = &sync else {
        return sync;
    };
    let enter_src = inner.fields.borrow().get("__enter__").cloned();
    let exit_src = inner.fields.borrow().get("__exit__").cloned();
    let mut fields: Vec<(&str, Value)> = Vec::new();
    if let Some(f) = enter_src {
        fields.push(("__aenter__", f));
    }
    if let Some(f) = exit_src {
        fields.push(("__aexit__", f));
    }
    native_object("_AsyncGeneratorContextManager", fields)
}

/// `functools` shim.
///
/// Implemented: `cache` / `lru_cache` (memoising wrapper, identical
/// semantics to the `@memo` decorator), `reduce`, `partial` (returns a
/// callable that prepends the captured args), `cached_property`
/// (identity wrapper — see FINDINGS #26: callers must invoke the
/// resulting method as `obj.x()` rather than `obj.x` because the VM
/// has no descriptor protocol).
///
/// `wraps`, `singledispatch`, and `total_ordering` are not implemented.
fn make_functools_module(interp: &mut Interpreter) -> Value {
    fn make_cache(_i: &mut Interpreter, args: Vec<Value>) -> Result<Value, Unwind> {
        let inner = args.into_iter().next().unwrap_or(Value::None);
        let cache: Rc<RefCell<HashMap<HashKey, Value>>> = Rc::new(RefCell::new(HashMap::new()));
        Ok(Value::Native(Rc::new(NativeFn::new(
            "memo",
            move |interp, call_args| {
                let mut keys = Vec::with_capacity(call_args.len());
                for a in &call_args {
                    keys.push(interp.hash_key(a)?);
                }
                let key = HashKey::Tuple(Rc::new(keys));
                if let Some(v) = cache.borrow().get(&key).cloned() {
                    return Ok(v);
                }
                let result = interp.call_value(inner.clone(), call_args, &[])?;
                cache.borrow_mut().insert(key, result.clone());
                Ok(result)
            },
        ))))
    }
    let cache_fn = nf("cache", make_cache);
    // `lru_cache(maxsize=None)` returns a decorator; we accept both shapes.
    let lru_cache = nf("lru_cache", |_i, args| {
        // If the first argument is callable, decorate immediately.
        let first = args.into_iter().next().unwrap_or(Value::None);
        if matches!(
            first,
            Value::Function(_) | Value::Native(_) | Value::BoundMethod { .. }
        ) {
            return make_cache(_i, vec![first]);
        }
        // Otherwise return a decorator that captures the configuration.
        Ok(Value::Native(Rc::new(NativeFn::new(
            "lru_cache_inner",
            make_cache,
        ))))
    });
    let reduce = nf("reduce", |i, mut args| {
        if args.len() < 2 {
            return Err(type_error("reduce() expected at least 2 arguments"));
        }
        let func = args.remove(0);
        let iterable = args.remove(0);
        let initial = args.into_iter().next();
        let it = i.make_iter(iterable)?;
        let mut acc = match initial {
            Some(v) => v,
            None => i
                .iter_next(&it)?
                .ok_or_else(|| type_error("reduce() of empty iterable with no initial value"))?,
        };
        while let Some(v) = i.iter_next(&it)? {
            acc = i.call_value(func.clone(), vec![acc, v], &[])?;
        }
        Ok(acc)
    });
    let partial = nf("partial", |_i, args| {
        // `partial(fn, *bound, **bound_kw)` — the keyword half was dropped,
        // so `partial(pow, exp=2)` raised "partial() does not accept keyword
        // arguments" on a call CPython runs.
        let (pos, captured_kw) = split_kwargs(&args);
        let Some((func, captured)) = pos.split_first() else {
            return Err(type_error("partial() needs a callable"));
        };
        let func = func.clone();
        let captured: Vec<Value> = captured.to_vec();
        Ok(Value::Native(Rc::new(NativeFn::new(
            "partial_call",
            move |i, call_args| {
                let (call_pos, call_kw) = split_kwargs(&call_args);
                let mut all = captured.clone();
                all.extend(call_pos.iter().cloned());
                // A later keyword overrides the bound one, as in CPython.
                let mut kw = captured_kw.clone();
                for (k, v) in call_kw {
                    match kw.iter_mut().find(|(existing, _)| *existing == k) {
                        Some(slot) => slot.1 = v,
                        None => kw.push((k, v)),
                    }
                }
                i.call_value(func.clone(), all, &kw)
            },
        ))))
    });
    // `cached_property` in VM mode: identity wrapper. The wrapped method
    // stays a method (callers use `obj.x()`). Documented in FINDINGS #26.
    let cached_property = nf("cached_property", |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    // `@functools.wraps(fn)` copies the wrapped function's identity onto
    // the wrapper. As a pure identity it left `wrapper.__name__` as
    // `"wrapper"`, so any program printing a decorated function's name
    // disagreed with the compiled path.
    let wraps = nf("wraps", |_i, args| {
        let wrapped = args.into_iter().next().unwrap_or(Value::None);
        Ok(Value::Native(Rc::new(NativeFn::new(
            "wraps_inner",
            move |i, args| {
                let wrapper = args.into_iter().next().unwrap_or(Value::None);
                if let Value::Function(f) = &wrapper {
                    for name in ["__name__", "__qualname__", "__doc__", "__module__"] {
                        if let Ok(v) = i.get_attr(&wrapped, name) {
                            f.attrs.borrow_mut().insert(name.to_owned(), v);
                        }
                    }
                    f.attrs
                        .borrow_mut()
                        .insert("__wrapped__".to_owned(), wrapped.clone());
                }
                Ok(wrapper)
            },
        ))))
    });
    let mut entries = vec![
        ("cache", cache_fn),
        ("lru_cache", lru_cache),
        ("reduce", reduce),
        ("partial", partial),
        ("cached_property", cached_property),
        ("wraps", wraps),
    ];
    // `cmp_to_key` / `singledispatch` / `total_ordering` are pure Python in
    // CPython too, so the shim is the honest implementation rather than a
    // native approximation of one.
    let extras = compile_helpers(interp, shims::FUNCTOOLS_EXTRA).unwrap_or_default();
    for (k, v) in extras {
        if k.starts_with('_') {
            continue;
        }
        entries.push((intern_shim_name(k), v));
    }
    make_module("functools", entries)
}

/// `dataclasses` shim.
///
/// `dataclass` is exposed as an identity decorator — the Typhon desugar
/// pass has already lowered user `class` declarations to plain Python
/// classes with synthesised `__init__`, so the decorator is a no-op in
/// VM mode. `field` returns its `default` kwarg (or `None`).
fn make_dataclasses_module() -> Value {
    let dataclass = nf("dataclass", |_i, args| {
        // Two-shape call: `@dataclass` (decorating a class directly) or
        // `@dataclass(slots=True, …)` (returning a decorator). Detect by
        // the first argument's value type — Class implies direct
        // decoration.
        if let Some(v) = args.first() {
            if matches!(v, Value::Class(_)) {
                return Ok(v.clone());
            }
        }
        Ok(Value::Native(Rc::new(NativeFn::new(
            "dataclass_inner",
            |_i, args| Ok(args.into_iter().next().unwrap_or(Value::None)),
        ))))
    });
    let field = nf("field", |_i, args| {
        // Approximate signature: `field(default=…, default_factory=…)`.
        // Without kwargs wiring we just return the first positional if
        // present, otherwise None. The desugar pass synthesises field
        // defaults as plain AnnAssign values already, so this rarely
        // matters in VM mode.
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    let asdict = nf("asdict", |_i, args| {
        let v = single(&args, "asdict")?;
        match v {
            Value::Instance(inst) if crate::value::class_is_dataclass(&inst.class) => {
                Ok(dataclass_convert(v, false))
            }
            _ => Err(type_error(
                "asdict() should be called on dataclass instances",
            )),
        }
    });
    let astuple = nf("astuple", |_i, args| {
        let v = single(&args, "astuple")?;
        match v {
            Value::Instance(inst) if crate::value::class_is_dataclass(&inst.class) => {
                Ok(dataclass_convert(v, true))
            }
            _ => Err(type_error(
                "astuple() should be called on dataclass instances",
            )),
        }
    });
    let is_dataclass = nf("is_dataclass", |_i, args| {
        let v = single(&args, "is_dataclass")?;
        Ok(Value::Bool(match v {
            Value::Instance(inst) => crate::value::class_is_dataclass(&inst.class),
            Value::Class(class) => crate::value::class_is_dataclass(class),
            _ => false,
        }))
    });
    let fields = nf("fields", |_i, args| {
        let v = single(&args, "fields")?;
        let class = match v {
            Value::Instance(inst) if crate::value::class_is_dataclass(&inst.class) => {
                inst.class.clone()
            }
            Value::Class(class) if crate::value::class_is_dataclass(class) => class.clone(),
            _ => {
                return Err(type_error(
                    "must be called with a dataclass type or instance",
                ))
            }
        };
        let field_class = dataclass_field_class();
        let items: Vec<Value> = class
            .fields
            .iter()
            .map(|f| {
                let mut attrs: crate::value::FieldMap = crate::value::FieldMap::new();
                attrs.insert("name".to_owned(), Value::Str(Rc::new(f.name.clone())));
                attrs.insert(
                    "type".to_owned(),
                    f.annotation
                        .as_ref()
                        .map(|a| Value::Str(Rc::new(a.clone())))
                        .unwrap_or(Value::None),
                );
                attrs.insert(
                    "default".to_owned(),
                    f.default.clone().unwrap_or(Value::None),
                );
                Value::Instance(Rc::new(crate::value::Instance {
                    class: field_class.clone(),
                    fields: RefCell::new(attrs),
                    chain: RefCell::new(None),
                }))
            })
            .collect();
        Ok(Value::Tuple(Rc::new(items)))
    });
    // `replace(obj, **changes)` — the keyword-carrying call is routed through
    // `call_with_kwargs`; the bare positional form is a copy.
    let replace = nf("dataclasses.replace", |interp, args| {
        dataclass_replace(interp, args, &[])
    });
    make_module(
        "dataclasses",
        vec![
            ("dataclass", dataclass),
            ("field", field),
            ("asdict", asdict),
            ("astuple", astuple),
            ("fields", fields),
            ("is_dataclass", is_dataclass),
            ("replace", replace),
            ("FrozenInstanceError", exception_ctor("FrozenInstanceError")),
        ],
    )
}

/// A constructor native for a builtin-style exception kind that lives in a
/// module rather than the prelude (`dataclasses.FrozenInstanceError`).
fn exception_ctor(name: &'static str) -> Value {
    Value::Native(Rc::new(NativeFn::new(name, move |_i, args| {
        let msg = args.first().map(|v| v.py_str()).unwrap_or_default();
        Ok(Value::Exception {
            kind: Rc::new(name.to_owned()),
            message: Rc::new(msg),
            args: Rc::new(args),
            chain: None,
        })
    })))
}

/// `dataclasses.asdict` / `astuple` value conversion: a dataclass instance
/// becomes a dict (or tuple) of its fields in declaration order, containers
/// are rebuilt with converted elements, and everything else is copied as is.
/// Recursion follows CPython's `_asdict_inner` — nested dataclasses, lists,
/// tuples and dict values are all converted.
fn dataclass_convert(v: &Value, as_tuple: bool) -> Value {
    match v {
        Value::Instance(inst) if crate::value::class_is_dataclass(&inst.class) => {
            let fields = inst.fields.borrow();
            let values = inst
                .class
                .fields
                .iter()
                .filter_map(|f| fields.get(&f.name).map(|x| (f.name.clone(), x)));
            if as_tuple {
                Value::Tuple(Rc::new(
                    values.map(|(_, x)| dataclass_convert(x, true)).collect(),
                ))
            } else {
                let mut map: DictMap = IndexMap::new();
                for (name, x) in values {
                    map.insert(HashKey::Str(Rc::new(name)), dataclass_convert(x, false));
                }
                Value::Dict(Rc::new(RefCell::new(map)))
            }
        }
        Value::List(l) => Value::List(Rc::new(RefCell::new(
            l.borrow()
                .iter()
                .map(|x| dataclass_convert(x, as_tuple))
                .collect(),
        ))),
        Value::Tuple(t) => Value::Tuple(Rc::new(
            t.iter().map(|x| dataclass_convert(x, as_tuple)).collect(),
        )),
        Value::Dict(d) => {
            let mut map: DictMap = IndexMap::new();
            for (k, x) in d.borrow().iter() {
                map.insert(k.clone(), dataclass_convert(x, as_tuple));
            }
            Value::Dict(Rc::new(RefCell::new(map)))
        }
        other => other.clone(),
    }
}

/// `dataclasses.replace(obj, **changes)`: a new instance of the same class
/// built through the ordinary constructor path (so `__post_init__` runs, as
/// in CPython) from the current field values with `changes` applied. An
/// unknown field name raises CPython's `TypeError`.
pub(crate) fn dataclass_replace(
    interp: &mut Interpreter,
    args: Vec<Value>,
    changes: &[(String, Value)],
) -> Result<Value, Unwind> {
    let inst = match args.first() {
        Some(Value::Instance(inst)) if crate::value::class_is_dataclass(&inst.class) => {
            inst.clone()
        }
        _ => {
            return Err(type_error(
                "replace() should be called on dataclass instances",
            ))
        }
    };
    let class = inst.class.clone();
    let mut kwargs: Vec<(String, Value)> = {
        let fields = inst.fields.borrow();
        class
            .fields
            .iter()
            .filter_map(|f| fields.get(&f.name).map(|v| (f.name.clone(), v.clone())))
            .collect()
    };
    for (k, v) in changes {
        match kwargs.iter_mut().find(|(name, _)| name == k) {
            Some(slot) => slot.1 = v.clone(),
            None => {
                return Err(type_error(format!(
                    "{}.__init__() got an unexpected keyword argument '{}'",
                    class.name, k
                )))
            }
        }
    }
    interp.instantiate(&class, vec![], &kwargs)
}

thread_local! {
    /// The `dataclasses.Field` stand-in `fields()` returns: `name`, `type`
    /// (annotation text) and `default`.
    static DATACLASS_FIELD_CLASS: Rc<crate::value::Class> = Rc::new(crate::value::Class {
        name: "Field".to_owned(),
        methods: RefCell::new(HashMap::new()),
        fields: ["name", "type", "default"]
            .iter()
            .map(|n| crate::value::ClassField {
                name: (*n).to_owned(),
                default: None,
                annotation: None,
            })
            .collect(),
        class_attrs: RefCell::new(HashMap::new()),
        bases: vec![],
        properties: RefCell::new(HashSet::new()),
        classmethods: RefCell::new(HashSet::new()),
        is_exception: false,
        is_protocol: false,
    });
}

fn dataclass_field_class() -> Rc<crate::value::Class> {
    DATACLASS_FIELD_CLASS.with(|c| c.clone())
}

// ── Method dispatch on built-in types ──────────────────────────────────────

pub fn method_for(_value: &Value, _attr: &str) -> Option<()> {
    // Stub — actual dispatch happens through `dispatch_method`.
    None
}

/// Peel a trailing keyword-argument sentinel (built by `call_with_kwargs`)
/// off a method's positional args, returning the remaining positional args
/// and the keyword map. Methods that don't care about kwargs simply ignore
/// the returned map.
fn split_kwargs_map(args: &[Value]) -> (&[Value], HashMap<String, Value>) {
    if let Some(Value::Tuple(t)) = args.last() {
        if t.len() == 2 {
            if let (Value::Str(tag), Value::Dict(d)) = (&t[0], &t[1]) {
                if tag.as_str() == "__typhon_kwargs__" {
                    let mut map = HashMap::new();
                    for (k, v) in d.borrow().iter() {
                        if let HashKey::Str(s) = k {
                            map.insert((**s).clone(), v.clone());
                        }
                    }
                    return (&args[..args.len() - 1], map);
                }
            }
        }
    }
    (args, HashMap::new())
}

pub fn dispatch_method(
    interp: &mut Interpreter,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, Unwind> {
    let receiver = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("method called without receiver"))?;
    let (rest, kwargs) = split_kwargs_map(&args[1..]);
    // The universal dunders CPython exposes on every object. They are
    // ordinary methods there (`(5).__repr__()`, `"a".__len__()`), and the
    // per-type tables below do not carry them.
    match name {
        "__repr__" => return Ok(Value::Str(Rc::new(interp.repr_of(&receiver)?))),
        "__str__" | "__format__" if rest.is_empty() || name == "__str__" => {
            return Ok(Value::Str(Rc::new(interp.str_of(&receiver)?)))
        }
        "__len__" => {
            return Ok(Value::Int(VmInt::from(value_len(&receiver)? as i64)));
        }
        "__bool__" => return Ok(Value::Bool(receiver.truthy())),
        "__hash__" => return Ok(Value::Int(VmInt::from(interp.hash_value(&receiver)?))),
        "__class__" => return Ok(make_builtin_type(receiver.type_name())),
        _ => {}
    }
    match (&receiver, name) {
        // ── str methods ────────────────────────────────────────────────────
        (Value::Str(s), m) => str_method(interp, s, m, rest, &kwargs),
        // ── bytes methods ──────────────────────────────────────────────────
        (Value::Bytes(b), m) => bytes_method(b, m, rest, &kwargs),
        // ── list methods ───────────────────────────────────────────────────
        (Value::List(l), m) => list_method(interp, l, m, rest),
        // ── dict methods ───────────────────────────────────────────────────
        (Value::Dict(d), m) => dict_method(interp, d, m, rest),
        // ── set methods ────────────────────────────────────────────────────
        (Value::Set(s), m) => set_method(interp, s, m, rest),
        // ── tuple methods ──────────────────────────────────────────────────
        (Value::Tuple(t), m) => tuple_method(t, m, rest),
        // ── int/float/bool method calls ────────────────────────────────────
        (Value::Int(_) | Value::Float(_) | Value::Bool(_), m) => num_method(&receiver, m, rest),
        _ => Err(attribute_error(format!(
            "'{}' object has no method '{}'",
            receiver.type_name(),
            name
        ))),
    }
}

/// The optional `chars` argument to `strip`/`lstrip`/`rstrip`. Python requires
/// it to be a `str` (or `None`) — non-string arguments raise `TypeError`.
fn strip_chars(args: &[Value], method: &str) -> Result<Option<Vec<char>>, Unwind> {
    match args.first() {
        None | Some(Value::None) => Ok(None),
        Some(Value::Str(cs)) => Ok(Some(cs.chars().collect())),
        Some(other) => Err(type_error(format!(
            "{}() arg must be None or str, not {}",
            method,
            other.type_name()
        ))),
    }
}

/// `dict.fromkeys(iterable[, value])` — a new dict with each key drawn
/// from `iterable`, all mapped to `value` (default `None`). This is a
/// classmethod on the `dict` type object, so the unbound-method
/// dispatcher (which routes `T.m(x)` to `x.m(...)`) can't model it;
/// `interp.rs` intercepts `dict.fromkeys` and calls here directly.
pub fn dict_fromkeys(interp: &mut Interpreter, args: Vec<Value>) -> Result<Value, Unwind> {
    let mut it = args.into_iter();
    let iterable = it
        .next()
        .ok_or_else(|| type_error("fromkeys() expected at least 1 argument, got 0"))?;
    let fill = it.next().unwrap_or(Value::None);
    let mut map: DictMap = IndexMap::new();
    let iter = interp.make_iter(iterable)?;
    while let Some(k) = interp.iter_next(&iter)? {
        // Last write wins on a duplicate key, matching CPython.
        let key = interp.hash_key(&k)?;
        let key = interp.settle_key_in_map(&map, key)?;
        map.insert(key, fill.clone());
    }
    Ok(Value::Dict(Rc::new(RefCell::new(map))))
}

/// `str.maketrans(x[, y[, z]])` — build the translation table dict that
/// `str.translate` consumes. One-arg form: `x` is a dict keyed by 1-char
/// strings or int ordinals. Two/three-arg form: equal-length strings `x`
/// and `y` map char-by-char, and an optional `z` lists characters mapped
/// to `None` (deleted). A staticmethod on the `str` type object, so
/// `interp.rs` intercepts it the same way as `dict.fromkeys`.
pub fn str_maketrans(args: &[Value]) -> Result<Value, Unwind> {
    let as_str = |v: &Value| -> Result<String, Unwind> {
        match v {
            Value::Str(s) => Ok((**s).clone()),
            other => Err(type_error(format!(
                "maketrans() arguments must be str, not {}",
                other.type_name()
            ))),
        }
    };
    let mut map: DictMap = IndexMap::new();
    match args.len() {
        1 => {
            let Value::Dict(d) = &args[0] else {
                return Err(type_error(
                    "if you give only one argument to maketrans it must be a dict",
                ));
            };
            for (k, v) in d.borrow().iter() {
                let ord = match k {
                    HashKey::Int(i) => HashKey::Int(i.clone()),
                    HashKey::Str(s) => {
                        let mut chars = s.chars();
                        match (chars.next(), chars.next()) {
                            (Some(c), None) => HashKey::Int(VmInt::from(c as u32)),
                            _ => {
                                return Err(value_error(
                                    "string keys in translate table must be of length 1",
                                ))
                            }
                        }
                    }
                    _ => {
                        return Err(type_error(
                            "keys in translate table must be strings or integers",
                        ))
                    }
                };
                map.insert(ord, v.clone());
            }
        }
        2 | 3 => {
            let from = as_str(&args[0])?;
            let to = as_str(&args[1])?;
            if from.chars().count() != to.chars().count() {
                return Err(value_error(
                    "the first two maketrans arguments must have equal length",
                ));
            }
            for (fc, tc) in from.chars().zip(to.chars()) {
                map.insert(
                    HashKey::Int(VmInt::from(fc as u32)),
                    Value::Int(VmInt::from(tc as u32)),
                );
            }
            if let Some(third) = args.get(2) {
                for dc in as_str(third)?.chars() {
                    map.insert(HashKey::Int(VmInt::from(dc as u32)), Value::None);
                }
            }
        }
        n => {
            return Err(type_error(format!(
                "maketrans() takes 1 to 3 arguments ({} given)",
                n
            )))
        }
    }
    Ok(Value::Dict(Rc::new(RefCell::new(map))))
}

fn str_method(
    interp: &mut Interpreter,
    s: &Rc<String>,
    name: &str,
    args: &[Value],
    _kwargs: &HashMap<String, Value>,
) -> Result<Value, Unwind> {
    Ok(match name {
        "upper" => Value::Str(Rc::new(s.to_uppercase())),
        "lower" => Value::Str(Rc::new(s.to_lowercase())),
        "translate" => {
            // `s.translate(table)` — `table` maps a Unicode ordinal (int key)
            // to a replacement: an int ordinal, a str, or `None` (delete the
            // character). Ordinals absent from the table pass through. This is
            // the dict produced by `str.maketrans(...)`.
            let Some(Value::Dict(table)) = args.first() else {
                return Err(type_error(
                    "translate() argument must be a dict (use str.maketrans to build one)",
                ));
            };
            let table = table.borrow();
            let mut out = String::with_capacity(s.len());
            for ch in s.chars() {
                let key = HashKey::Int(VmInt::from(ch as u32));
                match table.get(&key) {
                    None => out.push(ch),
                    Some(Value::None) => {} // mapped to None → delete
                    Some(Value::Str(rep)) => out.push_str(rep),
                    Some(Value::Int(code)) => {
                        let cp = code.to_u32().and_then(char::from_u32).ok_or_else(|| {
                            value_error("character mapping must be in range(0x110000)")
                        })?;
                        out.push(cp);
                    }
                    Some(_) => {
                        return Err(type_error(
                            "character mapping must return integer, None or str",
                        ))
                    }
                }
            }
            Value::Str(Rc::new(out))
        }
        "strip" => match strip_chars(args, "strip")? {
            Some(chars) => Value::Str(Rc::new(s.trim_matches(|c| chars.contains(&c)).to_owned())),
            None => Value::Str(Rc::new(s.trim().to_owned())),
        },
        "lstrip" => match strip_chars(args, "lstrip")? {
            Some(chars) => Value::Str(Rc::new(
                s.trim_start_matches(|c| chars.contains(&c)).to_owned(),
            )),
            None => Value::Str(Rc::new(s.trim_start().to_owned())),
        },
        "rstrip" => match strip_chars(args, "rstrip")? {
            Some(chars) => Value::Str(Rc::new(
                s.trim_end_matches(|c| chars.contains(&c)).to_owned(),
            )),
            None => Value::Str(Rc::new(s.trim_end().to_owned())),
        },
        "split" | "rsplit" => {
            // Keyword arguments (`maxsplit=`, `sep=`) arrive via the trailing
            // sentinel that `call_value` appends for bound builtin methods.
            // `dispatch_method` does not populate the `kwargs` map for str
            // methods (its `split_kwargs_map` uses a different marker), so we
            // unpack the sentinel here the same way `str.format` does.
            let (args, kw) = split_kwargs(args);
            let kw_get = |name: &str| kw.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v);
            // Separator: positional arg 0 or keyword `sep` (None ⇒ whitespace).
            let sep_kw = kw_get("sep");
            let sep_arg = args
                .first()
                .or(sep_kw)
                .filter(|v| !matches!(v, Value::None));
            // maxsplit: positional arg 1 or keyword `maxsplit` (-1 ⇒ no limit).
            let maxsplit = match args.get(1) {
                Some(v) if !matches!(v, Value::None) => v.to_int()?,
                _ => match kw_get("maxsplit") {
                    Some(v) if !matches!(v, Value::None) => v.to_int()?,
                    _ => -1,
                },
            };
            let from_right = name == "rsplit";
            let pieces: Vec<String> = match sep_arg {
                Some(v) => {
                    let sep = v.py_str();
                    split_with_sep(s, &sep, maxsplit, from_right)
                }
                None => split_whitespace_max(s, maxsplit, from_right),
            };
            Value::List(Rc::new(RefCell::new(
                pieces.into_iter().map(|p| Value::Str(Rc::new(p))).collect(),
            )))
        }
        "splitlines" => {
            // A bound method call appends its keywords as a
            // `__typhon_kwargs_sentinel__` tuple, which the `_kwargs` map (a
            // different marker) does not decode — so `splitlines(keepends=False)`
            // would see the sentinel as a truthy positional arg and behave like
            // `keepends=True`. Unpack it here exactly as `split` does.
            let (args, kw) = split_kwargs(args);
            let keepends = match args.first() {
                Some(v) => v.truthy(),
                None => kw
                    .iter()
                    .rev()
                    .find(|(k, _)| k == "keepends")
                    .map(|(_, v)| v.truthy())
                    .unwrap_or(false),
            };
            let lines: Vec<Value> = py_splitlines(s, keepends)
                .into_iter()
                .map(|l| Value::Str(Rc::new(l)))
                .collect();
            Value::List(Rc::new(RefCell::new(lines)))
        }
        "join" => {
            let iterable = args
                .first()
                .ok_or_else(|| type_error("str.join requires an iterable"))?
                .clone();
            let mut parts: Vec<String> = Vec::new();
            let it = interp.make_iter(iterable)?;
            while let Some(v) = interp.iter_next(&it)? {
                // A `StrEnum` member *is* its string, so it joins like one.
                let v = match crate::value::enum_mixin_value(&v) {
                    Some(inner @ Value::Str(_)) => inner,
                    _ => v,
                };
                match v {
                    Value::Str(s) => parts.push((*s).clone()),
                    other => {
                        return Err(type_error(format!(
                            "sequence item: expected str instance, {} found",
                            other.type_display_name()
                        )))
                    }
                }
            }
            Value::Str(Rc::new(parts.join(s)))
        }
        "replace" => {
            let from = args
                .first()
                .ok_or_else(|| type_error("str.replace requires args"))?
                .py_str();
            let to = args
                .get(1)
                .ok_or_else(|| type_error("str.replace requires args"))?
                .py_str();
            // Optional third `count` arg: replace at most `count` occurrences
            // (a negative count means "replace all", matching CPython).
            let replaced = match args.get(2) {
                Some(c) => {
                    let count = c.to_int()?;
                    if count < 0 {
                        s.replace(&from, &to)
                    } else {
                        s.replacen(&from, &to, count as usize)
                    }
                }
                None => s.replace(&from, &to),
            };
            Value::Str(Rc::new(replaced))
        }
        // Each of these takes optional `start` / `end` character offsets after
        // the needle. They were ignored; see `search_range`.
        "startswith" | "endswith" => {
            let arg = single(args, name)?;
            let Some((cs, ce)) = search_range(args, s.chars().count())? else {
                return Ok(Value::Bool(false));
            };
            let (bs, be) = char_range_bytes(s, cs, ce);
            let window = &s[bs..be];
            let matches_one = |needle: &str| {
                if name == "startswith" {
                    window.starts_with(needle)
                } else {
                    window.ends_with(needle)
                }
            };
            // CPython accepts either a single string or a *tuple* of strings
            // (`p.endswith((".py", ".ty"))`) and enforces the element type: a
            // non-`str` first arg — or a non-`str` element reached before any
            // match — raises `TypeError`. Validation is lazy and in iteration
            // order, so a match found before an invalid element returns `True`
            // without examining the rest (`"a".startswith(("a", 1))` is `True`,
            // but `"a".startswith((1, "a"))` raises).
            let result = match arg {
                Value::Tuple(items) => {
                    let mut matched = false;
                    for it in items.iter() {
                        let Value::Str(needle) = it else {
                            return Err(type_error(format!(
                                "tuple for {name} must only contain str, not {}",
                                it.type_name()
                            )));
                        };
                        if matches_one(needle.as_str()) {
                            matched = true;
                            break;
                        }
                    }
                    matched
                }
                Value::Str(needle) => matches_one(needle.as_str()),
                other => {
                    return Err(type_error(format!(
                        "{name} first arg must be str or a tuple of str, not {}",
                        other.type_name()
                    )))
                }
            };
            Value::Bool(result)
        }
        "find" | "rfind" | "index" | "rindex" => {
            let needle = single(args, name)?.py_str();
            let hit = match search_range(args, s.chars().count())? {
                None => None,
                Some((cs, ce)) => {
                    let (bs, be) = char_range_bytes(s, cs, ce);
                    let window = &s[bs..be];
                    let hit = if name == "find" || name == "index" {
                        window.find(&needle)
                    } else {
                        window.rfind(&needle)
                    };
                    // CPython indices are character offsets, but Rust's
                    // `str::find` returns a byte offset. Convert so
                    // `s[s.find(x):]` matches CPython on non-ASCII text (the
                    // byte index is a char boundary), and re-base onto the
                    // window's start.
                    hit.map(|i| cs + window[..i].chars().count())
                }
            };
            match hit {
                Some(i) => Value::Int(VmInt::from(i as i64)),
                None if name == "index" || name == "rindex" => {
                    return Err(value_error("substring not found"))
                }
                None => Value::Int(VmInt::from(-1)),
            }
        }
        "count" => {
            let needle = single(args, "count")?.py_str();
            let Some((cs, ce)) = search_range(args, s.chars().count())? else {
                return Ok(Value::Int(VmInt::from(0)));
            };
            let (bs, be) = char_range_bytes(s, cs, ce);
            let window = &s[bs..be];
            if needle.is_empty() {
                Value::Int(VmInt::from(window.chars().count() as i64 + 1))
            } else {
                Value::Int(VmInt::from(window.matches(&needle).count() as i64))
            }
        }
        "isdigit" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())),
        // CPython: the empty string IS ascii (unlike every other `is*`).
        "isascii" => Value::Bool(s.is_ascii()),
        "isalpha" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_alphabetic())),
        "isalnum" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_alphanumeric())),
        "isspace" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_whitespace())),
        // CPython: true iff there is at least one cased character and no
        // character of the opposite case (uncased chars like ',', ' ', digits
        // do not satisfy the predicate on their own).
        "isupper" => {
            Value::Bool(s.chars().any(|c| c.is_uppercase()) && !s.chars().any(|c| c.is_lowercase()))
        }
        "islower" => {
            Value::Bool(s.chars().any(|c| c.is_lowercase()) && !s.chars().any(|c| c.is_uppercase()))
        }
        "title" => Value::Str(Rc::new(title_case(s))),
        "capitalize" => Value::Str(Rc::new(capitalize(s))),
        "swapcase" => Value::Str(Rc::new(
            s.chars()
                .map(|c| {
                    if c.is_uppercase() {
                        c.to_lowercase().next().unwrap_or(c)
                    } else if c.is_lowercase() {
                        c.to_uppercase().next().unwrap_or(c)
                    } else {
                        c
                    }
                })
                .collect(),
        )),
        "isnumeric" | "isdecimal" => {
            Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_numeric()))
        }
        "istitle" => Value::Bool(is_title_case(s)),
        // CPython: an identifier starts with a letter or `_` and continues
        // with letters, digits or `_` (the XID_Start / XID_Continue rule,
        // approximated here by Unicode alphabetic + `_` + numeric).
        "isidentifier" => {
            let mut chars = s.chars();
            Value::Bool(match chars.next() {
                None => false,
                Some(first) => {
                    (first.is_alphabetic() || first == '_')
                        && chars.all(|c| c.is_alphanumeric() || c == '_')
                }
            })
        }
        // CPython: the empty string IS printable; a string is printable when
        // no character is "non-printable" (control / separator other than
        // ASCII space / unassigned).
        "isprintable" => Value::Bool(
            s.chars()
                .all(|c| c == ' ' || (!c.is_control() && !c.is_whitespace())),
        ),
        "casefold" => {
            // Full Unicode case folding via the embedded C+F mappings, for
            // byte-exact parity with CPython's `str.casefold()`. `to_lowercase`
            // and an uppercase round-trip both diverge (see `casefold_str`).
            Value::Str(Rc::new(casefold_str(s)))
        }
        "removeprefix" => {
            let p = single(args, "removeprefix")?.py_str();
            Value::Str(Rc::new(s.strip_prefix(&p).unwrap_or(s).to_owned()))
        }
        "removesuffix" => {
            let p = single(args, "removesuffix")?.py_str();
            Value::Str(Rc::new(s.strip_suffix(&p).unwrap_or(s).to_owned()))
        }
        "center" | "ljust" | "rjust" => {
            let width = single(args, name)?.to_int()?.max(0) as usize;
            // `fillchar` (optional) must be exactly one character.
            let fill = match args.get(1) {
                Some(Value::Str(fs)) => {
                    let mut chars = fs.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => c,
                        _ => {
                            return Err(type_error(
                                "The fill character must be exactly one character long",
                            ))
                        }
                    }
                }
                Some(_) => {
                    return Err(type_error(
                        "The fill character must be a unicode character, not int",
                    ))
                }
                None => ' ',
            };
            let len = s.chars().count();
            if len >= width {
                Value::Str(s.clone())
            } else {
                let pad = width - len;
                let pad_str = |n: usize| fill.to_string().repeat(n);
                let out = match name {
                    "ljust" => format!("{}{}", s, pad_str(pad)),
                    "rjust" => format!("{}{}", pad_str(pad), s),
                    _ => {
                        // CPython's `str.center` biases the extra character
                        // to the *right* only when both the padding and the
                        // width are odd: `left = marg / 2 + (marg & width & 1)`
                        // in `unicodeobject.c`. A plain `pad / 2` put it on
                        // the wrong side for half the odd cases
                        // (`"ab".center(5)` → `" ab  "`, not `"  ab "`).
                        let left = pad / 2 + (pad & width & 1);
                        format!("{}{}{}", pad_str(left), s, pad_str(pad - left))
                    }
                };
                Value::Str(Rc::new(out))
            }
        }
        "zfill" => {
            let width = single(args, "zfill")?.to_int().unwrap_or(0).max(0) as usize;
            let len = s.chars().count();
            if len >= width {
                Value::Str(s.clone())
            } else {
                let pad = "0".repeat(width - len);
                let out = if let Some(rest) = s.strip_prefix('-') {
                    format!("-{}{}", pad, rest)
                } else if let Some(rest) = s.strip_prefix('+') {
                    format!("+{}{}", pad, rest)
                } else {
                    format!("{}{}", pad, s)
                };
                Value::Str(Rc::new(out))
            }
        }
        "partition" | "rpartition" => {
            let sep = single(args, name)?.py_str();
            let found = if name == "partition" {
                s.find(&sep)
            } else {
                s.rfind(&sep)
            };
            let triple = match found {
                Some(i) => vec![
                    Value::Str(Rc::new(s[..i].to_owned())),
                    Value::Str(Rc::new(sep.clone())),
                    Value::Str(Rc::new(s[i + sep.len()..].to_owned())),
                ],
                None => {
                    if name == "partition" {
                        vec![
                            Value::Str(s.clone()),
                            Value::Str(Rc::new(String::new())),
                            Value::Str(Rc::new(String::new())),
                        ]
                    } else {
                        vec![
                            Value::Str(Rc::new(String::new())),
                            Value::Str(Rc::new(String::new())),
                            Value::Str(s.clone()),
                        ]
                    }
                }
            };
            Value::Tuple(Rc::new(triple))
        }
        "expandtabs" => {
            // Expand tabs so the next column is a multiple of `tabsize`
            // (CPython semantics), resetting the column on newlines.
            let tabsize = match args.first() {
                Some(v) => v.to_int()?.max(0) as usize,
                None => 8,
            };
            let mut out = String::with_capacity(s.len());
            let mut column = 0usize;
            for c in s.chars() {
                match c {
                    '\t' => {
                        if tabsize > 0 {
                            let spaces = tabsize - (column % tabsize);
                            out.push_str(&" ".repeat(spaces));
                            column += spaces;
                        }
                    }
                    '\n' | '\r' => {
                        out.push(c);
                        column = 0;
                    }
                    _ => {
                        out.push(c);
                        column += 1;
                    }
                }
            }
            Value::Str(Rc::new(out))
        }
        "format" => {
            // Positional and named `str.format()`. Keyword arguments arrive
            // via the trailing kwargs sentinel that `call_value` appends for
            // bound builtin methods (see make_kwargs_sentinel / split_kwargs).
            let (pos_args, kwargs) = split_kwargs(args);
            return str_format(interp, s, pos_args, &kwargs);
        }
        // `str.format_map(m)` is `format(**m)` without copying the mapping —
        // and, unlike `format`, it accepts non-string keys, which simply
        // never match a field name.
        "format_map" => {
            let mapping = single(args, "format_map")?;
            let Value::Dict(d) = mapping else {
                return Err(type_error(format!(
                    "format_map() argument must be a mapping, not {}",
                    mapping.type_name()
                )));
            };
            let kwargs: Vec<(String, Value)> = d
                .borrow()
                .iter()
                .filter_map(|(k, v)| match k {
                    HashKey::Str(name) => Some(((**name).clone(), v.clone())),
                    _ => None,
                })
                .collect();
            return str_format(interp, s, &[], &kwargs);
        }
        "encode" => {
            // Keywords arrive as a trailing sentinel (see `splitlines`).
            let (args, kw) = split_kwargs(args);
            let find = |name: &str| kw.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
            let encoding = match args.first().cloned().or_else(|| find("encoding")) {
                Some(v) => v.py_str(),
                None => "utf-8".to_owned(),
            };
            let errors = match args.get(1).cloned().or_else(|| find("errors")) {
                Some(v) => v.py_str(),
                None => "strict".to_owned(),
            };
            Value::Bytes(Rc::new(crate::codecs::encode(s, &encoding, &errors)?))
        }
        _ => return Err(attribute_error(format!("str has no method '{}'", name))),
    })
}

/// One `.attr` or `[key]` step in a `str.format` field name.
enum FieldAccess {
    Attr(String),
    Index(String),
}

/// Split `name[!conv][:spec]`. The `!` and `:` separators only count
/// outside `[...]`, so `{d[a:b]}` keeps its whole key — CPython's rule.
fn split_format_field(field: &str) -> Result<(String, Option<char>, String), Unwind> {
    let chars: Vec<char> = field.chars().collect();
    let mut depth = 0usize;
    let mut cut: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '!' | ':' if depth == 0 => {
                cut = Some(i);
                break;
            }
            _ => {}
        }
    }
    let Some(cut) = cut else {
        return Ok((field.to_owned(), None, String::new()));
    };
    let name: String = chars[..cut].iter().collect();
    if chars[cut] == ':' {
        return Ok((name, None, chars[cut + 1..].iter().collect()));
    }
    // `!conv` — exactly one character, then an optional `:spec`.
    let Some(conv) = chars.get(cut + 1).copied() else {
        return Err(value_error(
            "end of string while looking for conversion specifier",
        ));
    };
    let rest = &chars[cut + 2..];
    match rest.first() {
        None => Ok((name, Some(conv), String::new())),
        Some(':') => Ok((name, Some(conv), rest[1..].iter().collect())),
        Some(_) => Err(value_error("expected ':' after conversion specifier")),
    }
}

/// Split `base.attr[key]…` into the base name and its accessor chain.
fn split_field_accessors(field: &str) -> (&str, Vec<FieldAccess>) {
    let end = field.find(['.', '[']).unwrap_or(field.len());
    let (base, mut rest) = field.split_at(end);
    let mut out = Vec::new();
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('.') {
            let stop = tail.find(['.', '[']).unwrap_or(tail.len());
            out.push(FieldAccess::Attr(tail[..stop].to_owned()));
            rest = &tail[stop..];
        } else if let Some(tail) = rest.strip_prefix('[') {
            match tail.find(']') {
                Some(stop) => {
                    out.push(FieldAccess::Index(tail[..stop].to_owned()));
                    rest = &tail[stop + 1..];
                }
                None => break,
            }
        } else {
            break;
        }
    }
    (base, out)
}

/// `ascii()`'s escaping of a `repr` string: every non-ASCII character
/// becomes its `\xNN` / `\uNNNN` / `\UNNNNNNNN` escape.
fn ascii_repr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else if (c as u32) <= 0xff {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else if (c as u32) <= 0xffff {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push_str(&format!("\\U{:08x}", c as u32));
        }
    }
    out
}

/// Implementation of `str.format(...)`. Supports:
/// - `{}` auto-numbered positional fields
/// - `{0}`, `{1}` explicit positional fields
/// - `{name}` named fields
/// - `{0.attr}`, `{name[key]}` attribute / index accessors
/// - `{x!r}` / `{x!s}` / `{x!a}` conversions
/// - `{0:.2f}`, `{name:05d}` format-spec fields
/// - `{{` and `}}` as literal braces
fn str_format(
    interp: &mut Interpreter,
    template: &str,
    pos_args: &[Value],
    kwargs: &[(String, Value)],
) -> Result<Value, Unwind> {
    let mut out = String::new();
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0usize;
    let mut auto_idx: usize = 0;

    while i < chars.len() {
        match chars[i] {
            '{' => {
                if i + 1 < chars.len() && chars[i + 1] == '{' {
                    // `{{` → literal `{`
                    out.push('{');
                    i += 2;
                    continue;
                }
                // Find matching `}`
                let start = i + 1;
                let mut depth = 1usize;
                let mut j = start;
                while j < chars.len() && depth > 0 {
                    if chars[j] == '{' {
                        depth += 1;
                    } else if chars[j] == '}' {
                        depth -= 1;
                    }
                    j += 1;
                }
                if depth != 0 {
                    return Err(value_error("Single '{' is not allowed in str.format()"));
                }
                // Field content is chars[start..j-1]
                let field: String = chars[start..j - 1].iter().collect();
                i = j;

                // Split `field_name[!conversion][:format_spec]`. The
                // separators only count outside `[...]`, so `{d[a:b]}` keeps
                // its whole key — CPython's own rule.
                let (field_ref, conversion, spec) = split_format_field(&field)?;
                let field_ref = field_ref.as_str();

                // `{0.attr}` / `{name[key]}` — the field name may be
                // followed by attribute and index accessors.
                let (base_ref, accessors) = split_field_accessors(field_ref);
                let field_ref = base_ref;

                // Resolve the value
                let value = if field_ref.is_empty() {
                    // `{}` auto-numbering
                    let v = pos_args.get(auto_idx).ok_or_else(|| {
                        index_error(format!(
                            "Replacement index {} out of range for positional args",
                            auto_idx
                        ))
                    })?;
                    auto_idx += 1;
                    v.clone()
                } else if let Ok(idx) = field_ref.parse::<usize>() {
                    // `{0}`, `{1}` etc.
                    pos_args
                        .get(idx)
                        .ok_or_else(|| {
                            index_error(format!(
                                "Replacement index {idx} out of range for positional args"
                            ))
                        })?
                        .clone()
                } else {
                    // `{name}` — look up in kwargs
                    kwargs
                        .iter()
                        .find(|(k, _)| k == field_ref)
                        .map(|(_, v)| v.clone())
                        .ok_or_else(|| key_error(format!("'{}'", field_ref)))?
                };

                let mut value = value;
                for access in accessors {
                    value = match access {
                        FieldAccess::Attr(name) => interp.get_attr(&value, &name)?,
                        FieldAccess::Index(key) => {
                            // A bare integer indexes; anything else is a
                            // mapping key, as in CPython.
                            let key = match key.parse::<i64>() {
                                Ok(n) => Value::Int(VmInt::from(n)),
                                Err(_) => Value::Str(Rc::new(key)),
                            };
                            interp.subscript(&value, &key)?
                        }
                    };
                }

                // `!r` / `!s` / `!a` convert *before* the spec applies, and
                // the spec then formats the resulting string.
                let (value, spec_target) = match conversion {
                    Some('r') => {
                        let text = interp.repr_of(&value)?;
                        (Value::Str(Rc::new(text)), true)
                    }
                    Some('s') => {
                        let text = interp.str_of(&value)?;
                        (Value::Str(Rc::new(text)), true)
                    }
                    Some('a') => {
                        let text = ascii_repr(&interp.repr_of(&value)?);
                        (Value::Str(Rc::new(text)), true)
                    }
                    Some(other) => {
                        return Err(value_error(format!("Unknown conversion specifier {other}")))
                    }
                    None => (value, false),
                };

                // A nested replacement field inside the spec itself —
                // `{n:>{w}}` — is resolved against the same arguments
                // before the spec is applied.
                let spec = if spec.contains('{') {
                    str_format(interp, &spec, pos_args, kwargs)?.py_str()
                } else {
                    spec
                };

                // A user `__format__(self, spec)` controls its own
                // formatting for any spec (including the empty `{}` spec,
                // which CPython routes through `__format__("")`). A
                // converted value is already a plain string, so it never
                // reaches the user hook.
                let formatted = match if spec_target {
                    None
                } else {
                    interp.try_user_format(&value, &spec)?
                } {
                    Some(custom) => custom,
                    None => {
                        // The default stringification honours a user
                        // `__str__` (via `str_of`), matching `print` / `str`.
                        let default = interp.str_of(&value)?;
                        if spec.is_empty() {
                            default
                        } else {
                            crate::interp::format_with_spec_pub(&value, &default, &spec)?
                        }
                    }
                };
                out.push_str(&formatted);
            }
            '}' => {
                if i + 1 < chars.len() && chars[i + 1] == '}' {
                    // `}}` → literal `}`
                    out.push('}');
                    i += 2;
                } else {
                    return Err(value_error("Single '}' is not allowed in str.format()"));
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(Value::Str(Rc::new(out)))
}

fn bytes_method(
    b: &Rc<Vec<u8>>,
    name: &str,
    args: &[Value],
    _kwargs: &HashMap<String, Value>,
) -> Result<Value, Unwind> {
    Ok(match name {
        // `.decode()` / `.decode("utf-8")` -> str. Only UTF-8/ASCII handled;
        // other encodings fall back to a lossy UTF-8 decode.
        "decode" => {
            // Keywords arrive as a trailing sentinel (see `str.splitlines`).
            let (args, kw) = split_kwargs(args);
            let find = |name: &str| kw.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
            let encoding = match args.first().cloned().or_else(|| find("encoding")) {
                Some(v) => v.py_str(),
                None => "utf-8".to_owned(),
            };
            let errors = match args.get(1).cloned().or_else(|| find("errors")) {
                Some(v) => v.py_str(),
                None => "strict".to_owned(),
            };
            Value::Str(Rc::new(crate::codecs::decode(b, &encoding, &errors)?))
        }
        // `.hex()` -> lowercase hex string with no separators.
        "hex" => {
            let mut out = String::with_capacity(b.len() * 2);
            for byte in b.iter() {
                out.push_str(&format!("{:02x}", byte));
            }
            Value::Str(Rc::new(out))
        }
        "upper" => Value::Bytes(Rc::new(b.iter().map(|c| c.to_ascii_uppercase()).collect())),
        "lower" => Value::Bytes(Rc::new(b.iter().map(|c| c.to_ascii_lowercase()).collect())),
        // `.split(sep=None)` — on whitespace when no separator, else on the
        // separator bytes. Returns a list of bytes.
        "split" | "rsplit" => {
            let parts: Vec<Vec<u8>> = match args.first() {
                None | Some(Value::None) => {
                    // Split on ASCII whitespace runs, dropping empties.
                    b.split(|c| c.is_ascii_whitespace())
                        .filter(|p| !p.is_empty())
                        .map(|p| p.to_vec())
                        .collect()
                }
                Some(sep) => {
                    let sep = bytes_arg(sep)?;
                    if sep.is_empty() {
                        return Err(value_error("empty separator"));
                    }
                    split_bytes(b, &sep)
                }
            };
            Value::List(Rc::new(RefCell::new(
                parts
                    .into_iter()
                    .map(|p| Value::Bytes(Rc::new(p)))
                    .collect(),
            )))
        }
        "strip" | "lstrip" | "rstrip" => {
            let pred: Box<dyn Fn(u8) -> bool> = match args.first() {
                None | Some(Value::None) => Box::new(|c: u8| c.is_ascii_whitespace()),
                Some(chars) => {
                    let set = bytes_arg(chars)?;
                    Box::new(move |c: u8| set.contains(&c))
                }
            };
            let mut start = 0;
            let mut end = b.len();
            if name != "rstrip" {
                while start < end && pred(b[start]) {
                    start += 1;
                }
            }
            if name != "lstrip" {
                while end > start && pred(b[end - 1]) {
                    end -= 1;
                }
            }
            Value::Bytes(Rc::new(b[start..end].to_vec()))
        }
        "startswith" => {
            let pre = bytes_arg(single(args, "startswith")?)?;
            Value::Bool(b.starts_with(&pre))
        }
        "endswith" => {
            let suf = bytes_arg(single(args, "endswith")?)?;
            Value::Bool(b.ends_with(&suf))
        }
        "find" | "index" if !args.is_empty() => {
            let needle = bytes_arg(&args[0])?;
            // Optional `start` / `end` bound the search like a slice.
            let len = b.len() as i64;
            let clamp = |v: Option<&Value>, default: i64| -> Result<usize, Unwind> {
                let mut i = match v {
                    None | Some(Value::None) => default,
                    Some(v) => v.to_int()?,
                };
                if i < 0 {
                    i += len;
                }
                Ok(i.clamp(0, len) as usize)
            };
            let start = clamp(args.get(1), 0)?;
            let end = clamp(args.get(2), len)?;
            let found = if start <= end {
                find_subslice(&b[start..end], &needle).map(|i| i + start)
            } else {
                None
            };
            match found {
                Some(i) => Value::Int(VmInt::from(i as i64)),
                None if name == "find" => Value::Int(VmInt::from(-1)),
                None => return Err(value_error("subsection not found")),
            }
        }
        // `b.count(sub[, start[, end]])` — a bytes-like *subsequence* or a
        // single byte value, counted without overlaps, as in CPython.
        "count" if !args.is_empty() => {
            let len = b.len() as i64;
            let clamp = |v: Option<&Value>, default: i64| -> Result<usize, Unwind> {
                let mut i = match v {
                    None | Some(Value::None) => default,
                    Some(v) => v.to_int()?,
                };
                if i < 0 {
                    i += len;
                }
                Ok(i.clamp(0, len) as usize)
            };
            let start = clamp(args.get(1), 0)?;
            let end = clamp(args.get(2), len)?;
            if start > end {
                return Ok(Value::Int(VmInt::from(0)));
            }
            let window = &b[start..end];
            let needle = match &args[0] {
                Value::Int(_) | Value::Bool(_) => {
                    let n = args[0].to_int()?;
                    if !(0..=255).contains(&n) {
                        return Err(value_error("byte must be in range(0, 256)"));
                    }
                    vec![n as u8]
                }
                other => bytes_arg(other)?,
            };
            if needle.is_empty() {
                // CPython counts the empty needle at every position plus one.
                return Ok(Value::Int(VmInt::from(window.len() as i64 + 1)));
            }
            let mut count = 0i64;
            let mut at = 0usize;
            while at + needle.len() <= window.len() {
                if &window[at..at + needle.len()] == needle.as_slice() {
                    count += 1;
                    at += needle.len();
                } else {
                    at += 1;
                }
            }
            Value::Int(VmInt::from(count))
        }
        "replace" => {
            let from = bytes_arg(
                args.first()
                    .ok_or_else(|| type_error("replace needs args"))?,
            )?;
            let to = bytes_arg(
                args.get(1)
                    .ok_or_else(|| type_error("replace needs args"))?,
            )?;
            // Optional third `count` arg (negative = replace all), matching
            // bytes.replace in CPython.
            let max = match args.get(2) {
                Some(c) => {
                    let n = c.to_int()?;
                    if n < 0 {
                        usize::MAX
                    } else {
                        n as usize
                    }
                }
                None => usize::MAX,
            };
            Value::Bytes(Rc::new(replace_bytes(b, &from, &to, max)))
        }
        "join" => {
            // b",".join([b"a", b"b"]) -> b"a,b"
            let it = single(args, "join")?;
            let mut out: Vec<u8> = Vec::new();
            if let Value::List(l) = it {
                for (i, part) in l.borrow().iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b);
                    }
                    out.extend_from_slice(&bytes_arg(part)?);
                }
            }
            Value::Bytes(Rc::new(out))
        }
        _ => return Err(attribute_error(format!("bytes has no method '{}'", name))),
    })
}

/// Parse a Python complex-literal string like `"1+2j"`, `"3j"`, `"-1"`,
/// `"2-3J"`, `"j"`. Returns `(real, imag)`.
fn parse_complex_str(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_start_matches('(').trim_end_matches(')');
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    // Pure imaginary if it ends in `j` and has no internal sign splitting
    // real/imag.
    let ends_j = lower.ends_with('j');
    // Find a `+`/`-` that splits real and imaginary (not the leading sign,
    // not part of an exponent `e+`/`e-`).
    let bytes = lower.as_bytes();
    let mut split: Option<usize> = None;
    for i in 1..bytes.len() {
        let c = bytes[i] as char;
        if (c == '+' || c == '-') && bytes[i - 1] as char != 'e' {
            split = Some(i);
        }
    }
    let parse_imag = |t: &str| -> Option<f64> {
        let t = t.trim_end_matches('j');
        match t {
            "" | "+" => Some(1.0),
            "-" => Some(-1.0),
            _ => t.parse::<f64>().ok(),
        }
    };
    if let Some(i) = split {
        let (a, b) = lower.split_at(i);
        if ends_j {
            // real = a, imag = b (b includes its sign and trailing j)
            let re = a.parse::<f64>().ok()?;
            let im = parse_imag(b)?;
            Some((re, im))
        } else {
            None // a real with an internal sign but no j is malformed here
        }
    } else if ends_j {
        Some((0.0, parse_imag(&lower)?))
    } else {
        Some((lower.parse::<f64>().ok()?, 0.0))
    }
}

/// Coerce a `bytes`/`bytearray` (or a single int) argument into a byte vec.
/// Whether an unwind is a raised `AttributeError` (vs. some other exception
/// or control-flow). Lets `getattr`/`hasattr` distinguish a genuinely
/// missing attribute from an error raised inside a descriptor / `__getattr__`.
/// Whether an unwind is an `AttributeError` (used by the `with` statement to
/// turn a missing `__enter__` into CPython's protocol `TypeError`).
pub(crate) fn is_attribute_error_unwind(u: &Unwind) -> bool {
    is_attribute_error(u)
}

fn is_attribute_error(u: &Unwind) -> bool {
    matches!(u, Unwind::Exception(e) if e.kind == "AttributeError")
}

fn bytes_arg(v: &Value) -> Result<Vec<u8>, Unwind> {
    match v {
        Value::Bytes(b) => Ok((**b).clone()),
        Value::Int(i) => {
            let n = i.to_u32().and_then(|n| u8::try_from(n).ok());
            n.map(|b| vec![b])
                .ok_or_else(|| value_error("byte must be in range(0, 256)"))
        }
        _ => Err(type_error(format!(
            "a bytes-like object is required, not '{}'",
            v.type_name()
        ))),
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn split_bytes(hay: &[u8], sep: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = hay;
    while let Some(i) = find_subslice(rest, sep) {
        out.push(rest[..i].to_vec());
        rest = &rest[i + sep.len()..];
    }
    out.push(rest.to_vec());
    out
}

fn replace_bytes(hay: &[u8], from: &[u8], to: &[u8], max: usize) -> Vec<u8> {
    if from.is_empty() || max == 0 {
        return hay.to_vec();
    }
    let mut out = Vec::with_capacity(hay.len());
    let mut rest = hay;
    let mut done = 0usize;
    while done < max {
        match find_subslice(rest, from) {
            Some(i) => {
                out.extend_from_slice(&rest[..i]);
                out.extend_from_slice(to);
                rest = &rest[i + from.len()..];
                done += 1;
            }
            None => break,
        }
    }
    out.extend_from_slice(rest);
    out
}

fn list_method(
    interp: &mut Interpreter,
    l: &Rc<RefCell<Vec<Value>>>,
    name: &str,
    args: &[Value],
) -> Result<Value, Unwind> {
    // Strip any trailing keyword-argument sentinel so positional handlers see
    // only positional args; `sort` consults `kw`.
    let (args, kw) = split_kwargs(args);
    match name {
        "append" => {
            l.borrow_mut().push(single(args, "append")?.clone());
            Ok(Value::None)
        }
        "extend" => {
            let it = interp.make_iter(single(args, "extend")?.clone())?;
            while let Some(v) = interp.iter_next(&it)? {
                l.borrow_mut().push(v);
            }
            Ok(Value::None)
        }
        "insert" => {
            let idx = args
                .first()
                .ok_or_else(|| type_error("list.insert requires args"))?
                .to_int()?;
            let val = args
                .get(1)
                .ok_or_else(|| type_error("list.insert requires args"))?
                .clone();
            let mut l = l.borrow_mut();
            let len = l.len();
            let i = if idx < 0 {
                ((idx + len as i64).max(0)) as usize
            } else {
                (idx as usize).min(len)
            };
            l.insert(i, val);
            Ok(Value::None)
        }
        "pop" => {
            let mut l = l.borrow_mut();
            let idx = args
                .first()
                .map(|v| v.to_int())
                .transpose()?
                .unwrap_or(l.len() as i64 - 1);
            // CPython distinguishes the two failures: popping an empty
            // list says so, a bad index on a non-empty one is out of range.
            let i = normalize_index(idx, l.len()).ok_or_else(|| {
                index_error(if l.is_empty() {
                    "pop from empty list"
                } else {
                    "pop index out of range"
                })
            })?;
            Ok(l.remove(i))
        }
        "remove" => {
            let target = single(args, "remove")?.clone();
            let items = l.borrow().clone();
            let mut found: Option<usize> = None;
            for (i, v) in items.iter().enumerate() {
                if interp.values_equal(v, &target)? {
                    found = Some(i);
                    break;
                }
            }
            let pos = found.ok_or_else(|| value_error("list.remove(x): x not in list"))?;
            l.borrow_mut().remove(pos);
            Ok(Value::None)
        }
        "index" => {
            // `list.index(x[, start[, stop]])` — the bounds are clamped like
            // a slice (negatives count from the end).
            let target = args
                .first()
                .ok_or_else(|| type_error("index expected at least 1 argument, got 0"))?
                .clone();
            let items = l.borrow().clone();
            let len = items.len() as i64;
            let clamp = |raw: i64| -> usize {
                let v = if raw < 0 { raw + len } else { raw };
                v.clamp(0, len) as usize
            };
            let start = match args.get(1) {
                Some(v) => clamp(v.to_int()?),
                None => 0,
            };
            let stop = match args.get(2) {
                Some(v) => clamp(v.to_int()?),
                None => items.len(),
            };
            for (i, v) in items.iter().enumerate().take(stop).skip(start) {
                if interp.values_equal(v, &target)? {
                    return Ok(Value::Int(VmInt::from(i as i64)));
                }
            }
            Err(value_error(format!("{} is not in list", target.py_repr())))
        }
        "count" => {
            let target = single(args, "count")?.clone();
            let items = l.borrow().clone();
            let mut n: i64 = 0;
            for v in &items {
                if interp.values_equal(v, &target)? {
                    n += 1;
                }
            }
            Ok(Value::Int(VmInt::from(n)))
        }
        "clear" => {
            l.borrow_mut().clear();
            Ok(Value::None)
        }
        "reverse" => {
            l.borrow_mut().reverse();
            Ok(Value::None)
        }
        "sort" => {
            let mut reverse = false;
            let mut key_fn: Option<Value> = None;
            for (k, v) in &kw {
                match k.as_str() {
                    "reverse" => reverse = v.truthy(),
                    "key" => key_fn = Some(v.clone()),
                    _ => {
                        return Err(type_error(format!(
                            "sort() got an unexpected keyword argument '{}'",
                            k
                        )))
                    }
                }
            }
            let mut items = l.borrow().clone();
            // `sort_by` closures can't return `Result`, so capture the first
            // error (from a user `__lt__`/`__eq__`) and surface it afterwards.
            let mut sort_error: Option<Unwind> = None;
            if let Some(f) = key_fn {
                let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(items.len());
                for v in items {
                    let kv = interp.call_value(f.clone(), vec![v.clone()], &[])?;
                    keyed.push((kv, v));
                }
                keyed.sort_by(|a, b| {
                    if sort_error.is_some() {
                        return std::cmp::Ordering::Equal;
                    }
                    match interp.value_cmp(&a.0, &b.0) {
                        // Reverse the comparator (not the sorted list) so equal
                        // keys keep their original relative order — CPython's
                        // `sort(reverse=True)` is stable.
                        Ok(o) => {
                            if reverse {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                        Err(e) => {
                            sort_error = Some(e);
                            std::cmp::Ordering::Equal
                        }
                    }
                });
                if let Some(e) = sort_error {
                    return Err(e);
                }
                items = keyed.into_iter().map(|(_, v)| v).collect();
            } else {
                items.sort_by(|a, b| {
                    if sort_error.is_some() {
                        return std::cmp::Ordering::Equal;
                    }
                    match interp.value_cmp(a, b) {
                        // Stable reverse: flip the comparator, not the list.
                        Ok(o) => {
                            if reverse {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                        Err(e) => {
                            sort_error = Some(e);
                            std::cmp::Ordering::Equal
                        }
                    }
                });
                if let Some(e) = sort_error {
                    return Err(e);
                }
            }
            *l.borrow_mut() = items;
            Ok(Value::None)
        }
        "copy" => Ok(Value::List(Rc::new(RefCell::new(l.borrow().clone())))),
        // `collections.deque` is implemented as a `Value::List`, so it
        // shares the list method table. These four are deque-specific.
        "popleft" => {
            let mut l = l.borrow_mut();
            if l.is_empty() {
                return Err(index_error("pop from an empty deque"));
            }
            Ok(l.remove(0))
        }
        "appendleft" => {
            let v = single(args, "appendleft")?.clone();
            l.borrow_mut().insert(0, v);
            Ok(Value::None)
        }
        "extendleft" => {
            let it = interp.make_iter(single(args, "extendleft")?.clone())?;
            let mut to_prepend: Vec<Value> = Vec::new();
            while let Some(v) = interp.iter_next(&it)? {
                to_prepend.push(v);
            }
            // extendleft reverses element order to match CPython
            // (each pushed-left element ends up *before* the previous).
            let mut l = l.borrow_mut();
            for v in to_prepend {
                l.insert(0, v);
            }
            Ok(Value::None)
        }
        "rotate" => {
            let n = args.first().map(|v| v.to_int()).transpose()?.unwrap_or(1);
            let mut l = l.borrow_mut();
            if l.is_empty() {
                return Ok(Value::None);
            }
            let len = l.len() as i64;
            let n_mod = ((n % len) + len) % len;
            if n_mod != 0 {
                let n = n_mod as usize;
                l.rotate_right(n);
            }
            Ok(Value::None)
        }
        _ => Err(attribute_error(format!("list has no method '{}'", name))),
    }
}

fn dict_method(
    interp: &mut Interpreter,
    d: &Rc<RefCell<DictMap>>,
    name: &str,
    args: &[Value],
) -> Result<Value, Unwind> {
    // Strip any trailing keyword-argument sentinel; `popitem` /
    // `move_to_end` consult `kw`.
    let (args, kw) = split_kwargs(args);
    // Refuse mutations on a `freeze let`-tagged dict so the VM matches
    // the compile path's `MappingProxyType` semantics: CPython surfaces
    // missing-method calls as `AttributeError` (review thread copilot
    // on PR #147 — item assignment is a `TypeError`, handled in
    // `assign_target_subscript`). Read-only methods (`get`, `keys`,
    // `values`, `items`, `copy`) fall through.
    let is_mutator = matches!(
        name,
        "pop"
            | "update"
            | "setdefault"
            | "clear"
            | "popitem"
            | "move_to_end"
            | "__setitem__"
            | "__delitem__"
    );
    if is_mutator && dict_is_frozen(d) {
        return Err(attribute_error(format!(
            "'mappingproxy' object has no attribute '{}'",
            name
        )));
    }
    match name {
        "get" => {
            let k = interp.dict_probe_key(d, single(args, "get")?)?;
            let default = args.get(1).cloned().unwrap_or(Value::None);
            Ok(d.borrow().get(&k).cloned().unwrap_or(default))
        }
        "keys" => Ok(Value::DictView {
            kind: crate::value::DictViewKind::Keys,
            items: d
                .borrow()
                .keys()
                .filter(|k| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .cloned()
                .map(HashKey::into_value)
                .collect(),
        }),
        "values" => Ok(Value::DictView {
            kind: crate::value::DictViewKind::Values,
            items: d
                .borrow()
                .iter()
                .filter(|(k, _)| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .map(|(_, v)| v.clone())
                .collect(),
        }),
        "items" => Ok(Value::DictView {
            kind: crate::value::DictViewKind::Items,
            items: d
                .borrow()
                .iter()
                .filter(|(k, _)| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .map(|(k, v)| Value::Tuple(Rc::new(vec![k.clone().into_value(), v.clone()])))
                .collect(),
        }),
        "pop" => {
            let k = interp.dict_probe_key(d, single(args, "pop")?)?;
            let default = args.get(1).cloned();
            // `shift_remove` preserves the insertion order of remaining
            // keys (matches CPython `dict.pop` semantics).
            match d.borrow_mut().shift_remove(&k) {
                Some(v) => Ok(v),
                None => default.ok_or_else(|| crate::error::key_error_for(&k.clone().into_value())),
            }
        }
        "update" => {
            let arg = single(args, "update")?;
            match arg {
                Value::Dict(other) => {
                    for (k, v) in other.borrow().iter() {
                        d.borrow_mut().insert(k.clone(), v.clone());
                    }
                    Ok(Value::None)
                }
                _ => {
                    let it = interp.make_iter(arg.clone())?;
                    while let Some(pair) = interp.iter_next(&it)? {
                        if let Value::Tuple(t) = pair {
                            if t.len() == 2 {
                                let key = interp.dict_probe_key(d, &t[0])?;
                                d.borrow_mut().insert(key, t[1].clone());
                                continue;
                            }
                        }
                        return Err(type_error("dict update needs pairs"));
                    }
                    Ok(Value::None)
                }
            }
        }
        "setdefault" => {
            let k = interp.dict_probe_key(d, single(args, "setdefault")?)?;
            let default = args.get(1).cloned().unwrap_or(Value::None);
            let mut m = d.borrow_mut();
            Ok(m.entry(k).or_insert(default).clone())
        }
        "clear" => {
            d.borrow_mut().clear();
            Ok(Value::None)
        }
        "copy" => Ok(Value::Dict(Rc::new(RefCell::new(d.borrow().clone())))),
        "popitem" => {
            // Remove and return the last inserted (key, value) pair (LIFO),
            // matching CPython 3.7+. `OrderedDict.popitem(last=False)` pops
            // FIFO instead. `IndexMap` preserves insertion order, so `pop()`
            // removes the most-recently-added entry and `shift_remove_index(0)`
            // the oldest. (Frozen dicts are rejected earlier by the
            // `is_mutator` guard, so no `__typhon_frozen__` sentinel here.)
            let last = kw
                .iter()
                .find(|(k, _)| k == "last")
                .map(|(_, v)| v.truthy())
                .unwrap_or(true);
            let popped = if last {
                d.borrow_mut().pop()
            } else {
                d.borrow_mut().shift_remove_index(0)
            };
            match popped {
                Some((k, v)) => Ok(Value::Tuple(Rc::new(vec![k.into_value(), v]))),
                None => Err(key_error("'popitem(): dictionary is empty'")),
            }
        }
        "move_to_end" => {
            // OrderedDict.move_to_end(key, last=True): reposition an existing
            // key at either end, preserving the relative order of the rest
            // (`shift_remove` keeps order; plain `swap_remove` would not).
            let key = args
                .first()
                .ok_or_else(|| type_error("move_to_end() requires a key"))?;
            let key = interp.dict_probe_key(d, key)?;
            let last = kw
                .iter()
                .find(|(k, _)| k == "last")
                .map(|(_, v)| v.truthy())
                .unwrap_or(true);
            let mut m = d.borrow_mut();
            let Some(v) = m.shift_remove(&key) else {
                return Err(key_error(key.into_value().py_repr()));
            };
            if last {
                m.insert(key, v);
            } else {
                // Re-insert at the front in place. `shift_insert(0, ..)` shifts
                // the existing entries up by one without rebuilding/reallocating
                // the whole map — O(n) shift, no per-entry clone — so the LRU
                // idiom `move_to_end(k, last=False)` stays linear rather than
                // quadratic over a sequence of front-moves.
                m.shift_insert(0, key, v);
            }
            Ok(Value::None)
        }
        // ── Counter-specific methods ──────────────────────────────────────────
        // These are safe to expose on all dicts because:
        //  * `most_common` on an int-value dict is always meaningful.
        //  * `elements` on a non-Counter dict (whose values are not ints) will
        //    simply skip negative/non-int counts, matching CPython Counter.
        // Added to support `collections.Counter` (PR #N16).
        "most_common" => {
            // `most_common([n])` — sort by count descending, return
            // list of (key, count) tuples. Optional arg `n` limits the
            // result to the top n entries (all if omitted / None).
            let limit: Option<usize> = match args.first() {
                Some(v) => Some(v.to_int()? as usize),
                None => None,
            };
            let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
            let mut pairs: Vec<(Value, i64)> = d
                .borrow()
                .iter()
                .filter(|(k, _)| **k != frozen_key)
                .map(|(k, v)| {
                    let count = match v {
                        Value::Int(n) => n.to_i64().unwrap_or(0),
                        _ => 0,
                    };
                    (k.clone().into_value(), count)
                })
                .collect();
            // Stable sort: count descending, then preserve insertion order
            // for ties (stable_sort_by gives that guarantee in Rust).
            pairs.sort_by(|a, b| b.1.cmp(&a.1));
            let result: Vec<Value> = pairs
                .into_iter()
                .take(limit.unwrap_or(usize::MAX))
                .map(|(k, count)| Value::Tuple(Rc::new(vec![k, Value::Int(VmInt::from(count))])))
                .collect();
            Ok(Value::List(Rc::new(RefCell::new(result))))
        }
        "elements" => {
            // `elements()` — iterate over each element repeated by its count.
            // Elements with count ≤ 0 are ignored (matches CPython Counter).
            let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
            let mut out: Vec<Value> = Vec::new();
            for (k, v) in d.borrow().iter() {
                if *k == frozen_key {
                    continue;
                }
                let count = match v {
                    Value::Int(n) => n.to_i64().unwrap_or(0),
                    _ => 0,
                };
                for _ in 0..count.max(0) {
                    out.push(k.clone().into_value());
                }
            }
            Ok(Value::List(Rc::new(RefCell::new(out))))
        }
        _ => Err(attribute_error(format!("dict has no method '{}'", name))),
    }
}

fn set_method(
    interp: &mut Interpreter,
    s: &Rc<RefCell<HashSet<HashKey>>>,
    name: &str,
    args: &[Value],
) -> Result<Value, Unwind> {
    // Refuse mutators on a `freeze let`-tagged set so the VM matches
    // the compile path's `frozenset` semantics (review thread codex
    // and copilot on PR #147). Read-only methods are unaffected.
    let is_mutator = matches!(
        name,
        "add" | "remove" | "discard" | "pop" | "clear" | "update"
    );
    if is_mutator && set_is_frozen(s) {
        return Err(attribute_error(format!(
            "'frozenset' object has no attribute '{}'",
            name
        )));
    }
    match name {
        "add" => {
            let k = interp.set_probe_key(s, single(args, "add")?)?;
            s.borrow_mut().insert(k);
            Ok(Value::None)
        }
        "remove" | "discard" => {
            let k = interp.set_probe_key(s, single(args, name)?)?;
            let removed = s.borrow_mut().remove(&k);
            if name == "remove" && !removed {
                return Err(crate::error::key_error_for(&k.clone().into_value()));
            }
            Ok(Value::None)
        }
        "clear" => {
            s.borrow_mut().clear();
            Ok(Value::None)
        }
        "copy" => {
            // `copy()` on a frozen set returns a fresh *unfrozen* copy
            // (the sentinel is filtered out) — matching CPython's
            // `frozenset.copy()` returning a new frozenset with the
            // same elements but no shared mutability link.
            let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
            let copied: HashSet<HashKey> = s
                .borrow()
                .iter()
                .filter(|k| **k != frozen_key)
                .cloned()
                .collect();
            Ok(Value::Set(Rc::new(RefCell::new(copied))))
        }
        "union" | "intersection" | "difference" | "symmetric_difference" => {
            let a = set_keys_no_sentinel(s);
            let mut acc: HashSet<HashKey> = a;
            for arg in args {
                let b = value_to_key_set(arg)?;
                acc = match name {
                    "union" => acc.union(&b).cloned().collect(),
                    "intersection" => acc.intersection(&b).cloned().collect(),
                    "difference" => acc.difference(&b).cloned().collect(),
                    _ => acc.symmetric_difference(&b).cloned().collect(),
                };
            }
            // A set operation on a `frozenset` yields a `frozenset` — carry the
            // immutability sentinel over so the result stays read-only.
            if set_is_frozen(s) {
                acc.insert(HashKey::Str(Rc::new("__typhon_frozen__".to_owned())));
            }
            Ok(Value::Set(Rc::new(RefCell::new(acc))))
        }
        "issubset" | "issuperset" | "isdisjoint" => {
            let a = set_keys_no_sentinel(s);
            let b = value_to_key_set(single(args, name)?)?;
            let result = match name {
                "issubset" => a.is_subset(&b),
                "issuperset" => a.is_superset(&b),
                _ => a.is_disjoint(&b),
            };
            Ok(Value::Bool(result))
        }
        "update" => {
            for arg in args {
                let b = value_to_key_set(arg)?;
                s.borrow_mut().extend(b);
            }
            Ok(Value::None)
        }
        _ => Err(attribute_error(format!("set has no method '{}'", name))),
    }
}

/// The members of a set, excluding the internal `freeze let` sentinel.
pub fn set_keys_no_sentinel(s: &Rc<RefCell<HashSet<HashKey>>>) -> HashSet<HashKey> {
    let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
    s.borrow()
        .iter()
        .filter(|k| **k != frozen_key)
        .cloned()
        .collect()
}

/// Coerce a set-method argument (set / list / tuple / frozenset) into a key set.
fn value_to_key_set(v: &Value) -> Result<HashSet<HashKey>, Unwind> {
    match v {
        Value::Set(other) => Ok(set_keys_no_sentinel(other)),
        Value::List(l) => l.borrow().iter().map(|x| x.to_hash_key()).collect(),
        Value::Tuple(t) => t.iter().map(|x| x.to_hash_key()).collect(),
        _ => Err(type_error(format!(
            "'{}' object is not a valid set operand",
            v.type_name()
        ))),
    }
}

fn tuple_method(t: &Rc<Vec<Value>>, name: &str, args: &[Value]) -> Result<Value, Unwind> {
    match name {
        "count" => {
            let target = single(args, "count")?;
            Ok(Value::Int(VmInt::from(
                t.iter().filter(|v| v.py_eq(target)).count() as i64,
            )))
        }
        "index" => {
            let target = single(args, "index")?;
            t.iter()
                .position(|v| v.py_eq(target))
                .map(|p| Value::Int(VmInt::from(p as i64)))
                .ok_or_else(|| value_error("tuple.index(x): x not in tuple"))
        }
        _ => Err(attribute_error(format!("tuple has no method '{}'", name))),
    }
}

fn num_method(v: &Value, name: &str, args: &[Value]) -> Result<Value, Unwind> {
    match (v, name) {
        (Value::Float(x), "is_integer") => Ok(Value::Bool(x.fract() == 0.0 && x.is_finite())),
        // The `numbers.Real` surface every int/float carries. `conjugate()`
        // is the identity for a real; `imag` is always 0 / 0.0.
        (Value::Int(i), "conjugate") => Ok(Value::Int(i.clone())),
        // `(5).as_integer_ratio()` → `(5, 1)`; a float's is exact.
        (Value::Int(i), "as_integer_ratio") => Ok(Value::Tuple(Rc::new(vec![
            Value::Int(i.clone()),
            Value::Int(VmInt::from(1)),
        ]))),
        (Value::Bool(b), "as_integer_ratio") => Ok(Value::Tuple(Rc::new(vec![
            Value::Int(VmInt::from(i64::from(*b))),
            Value::Int(VmInt::from(1)),
        ]))),
        (Value::Float(x), "as_integer_ratio") => {
            if x.is_nan() {
                return Err(value_error("cannot convert NaN to integer ratio"));
            }
            if x.is_infinite() {
                return Err(crate::error::Unwind::Exception(
                    crate::error::VmException::new(
                        "OverflowError",
                        "cannot convert Infinity to integer ratio",
                    ),
                ));
            }
            // A finite f64 is exactly `mantissa * 2^exp`; scale until the
            // value is integral, which is what CPython's own implementation
            // does. At most 1074 doublings for a subnormal.
            let mut num = *x;
            let mut denom = num_bigint::BigInt::from(1);
            while num.fract() != 0.0 {
                num *= 2.0;
                denom *= 2;
            }
            let numer = num_bigint::BigInt::from(num as i128);
            Ok(Value::Tuple(Rc::new(vec![
                Value::Int(VmInt::from_bigint(numer)),
                Value::Int(VmInt::from_bigint(denom)),
            ])))
        }
        // `(2.5).hex()` → `0x1.4000000000000p+1`. IEEE-754 decomposition:
        // sign, an 11-bit biased exponent and a 52-bit mantissa, which is
        // exactly 13 hex digits.
        (Value::Float(x), "hex") => {
            let v = *x;
            if v.is_nan() {
                return Ok(Value::Str(Rc::new("nan".to_owned())));
            }
            if v.is_infinite() {
                return Ok(Value::Str(Rc::new(
                    if v < 0.0 { "-inf" } else { "inf" }.to_owned(),
                )));
            }
            let bits = v.to_bits();
            let sign = if bits >> 63 == 1 { "-" } else { "" };
            let exponent = ((bits >> 52) & 0x7ff) as i64;
            let mantissa = bits & 0x000f_ffff_ffff_ffff;
            let text = if exponent == 0 && mantissa == 0 {
                format!("{sign}0x0.0p+0")
            } else if exponent == 0 {
                // Subnormal: the implicit leading bit is 0 and the exponent
                // is the minimum, which CPython prints as p-1022.
                format!("{sign}0x0.{mantissa:013x}p-1022")
            } else {
                format!("{sign}0x1.{mantissa:013x}p{:+}", exponent - 1023)
            };
            Ok(Value::Str(Rc::new(text)))
        }
        (Value::Float(x), "conjugate") => Ok(Value::Float(*x)),
        (Value::Bool(b), "conjugate") => Ok(Value::Int(VmInt::from(i64::from(*b)))),
        (Value::Int(i), "bit_length") => Ok(Value::Int(VmInt::from(i.bits() as i64))),
        // `(n).bit_count()` — number of set bits in the absolute value.
        (Value::Int(i), "bit_count") => {
            let (_, bytes) = i.as_bigint().to_bytes_be();
            let count: u32 = bytes.iter().map(|b| b.count_ones()).sum();
            Ok(Value::Int(VmInt::from(count)))
        }
        // `(n).to_bytes(length=1, byteorder="big", *, signed=False)`. All
        // three are keyword-capable, and `signed=True` encodes a negative
        // int in two's complement, as CPython does.
        (Value::Int(i), "to_bytes") => {
            let (pos, kw) = split_kwargs(args);
            let kwarg = |name: &str| kw.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
            let length = match pos.first().cloned().or_else(|| kwarg("length")) {
                Some(n) => n.to_int()?.max(0) as usize,
                None => 1,
            };
            let big_endian = match pos.get(1).cloned().or_else(|| kwarg("byteorder")) {
                Some(Value::Str(s)) => s.as_str() != "little",
                _ => true,
            };
            let signed = kwarg("signed").map(|v| v.truthy()).unwrap_or(false);
            let value = i.as_bigint().into_owned();
            let mut out = if signed {
                if length == 0 {
                    return Err(crate::error::Unwind::Exception(
                        crate::error::VmException::new("OverflowError", "int too big to convert"),
                    ));
                }
                // Two's complement over exactly `length` bytes: encode
                // `value mod 2^(8*length)` and check it fits back signed.
                let modulus = num_bigint::BigInt::from(1) << (8 * length);
                let half = num_bigint::BigInt::from(1) << (8 * length - 1);
                if value >= half || value < -half.clone() {
                    return Err(crate::error::Unwind::Exception(
                        crate::error::VmException::new("OverflowError", "int too big to convert"),
                    ));
                }
                let wrapped = if value.sign() == num_bigint::Sign::Minus {
                    value + modulus
                } else {
                    value
                };
                let (_, raw) = wrapped.to_bytes_be();
                let mut out = vec![0u8; length.saturating_sub(raw.len())];
                out.extend_from_slice(&raw[raw.len().saturating_sub(length)..]);
                out
            } else {
                if i.is_negative() {
                    return Err(crate::error::Unwind::Exception(
                        crate::error::VmException::new(
                            "OverflowError",
                            "can't convert negative int to unsigned",
                        ),
                    ));
                }
                let (_, raw) = value.to_bytes_be();
                // `BigInt::to_bytes_be` renders zero as a single 0 byte.
                let raw = if raw == [0] { Vec::new() } else { raw };
                if raw.len() > length {
                    return Err(crate::error::Unwind::Exception(
                        crate::error::VmException::new("OverflowError", "int too big to convert"),
                    ));
                }
                let mut out = vec![0u8; length - raw.len()];
                out.extend_from_slice(&raw);
                out
            };
            if !big_endian {
                out.reverse();
            }
            Ok(Value::Bytes(Rc::new(out)))
        }
        _ => Err(attribute_error(format!(
            "'{}' object has no method '{}'",
            v.type_name(),
            name
        ))),
    }
}

// ── JSON ───────────────────────────────────────────────────────────────────

// ── json ─────────────────────────────────────────────────────────────────────
//
// `json.dumps` / `json.loads` are modelled on CPython's `json` package closely
// enough that a program's stdout does not depend on which execution surface
// ran it: the encoder honours `indent`, `separators`, `sort_keys`,
// `ensure_ascii` and `allow_nan` and raises the same `TypeError` /
// `ValueError` CPython does; the decoder reports the same `JSONDecodeError`
// messages at the same (character-indexed) positions, decodes `\uXXXX`
// escapes and surrogate pairs, rejects raw control characters, and accepts
// `NaN` / `Infinity` / `-Infinity`. Earlier versions pushed each UTF-8 *byte*
// of a decoded string as its own character (`"héllo"` came back as `hÃ©llo`),
// rejected every `\u` escape, and raised a bare `ValueError` with a private
// message — a program catching `json.JSONDecodeError` never caught it.

/// Encoder options: the `json.dumps` keyword arguments the VM honours, plus one
/// internal switch pydantic's `model_dump_json` needs.
#[derive(Clone, Debug)]
pub struct JsonDumpOpts {
    /// `None` renders on one line. `Some(unit)` renders one element per line,
    /// each nesting level prefixed by one more copy of `unit` (CPython accepts
    /// an `int` count of spaces or a string).
    pub indent: Option<String>,
    pub sort_keys: bool,
    /// Escape every non-ASCII character as `\uXXXX` — CPython's default.
    pub ensure_ascii: bool,
    /// Render `nan` / `inf` as `NaN` / `Infinity`; `false` raises `ValueError`.
    pub allow_nan: bool,
    pub item_sep: String,
    pub key_sep: String,
    /// Serialise a user instance's declared fields as an object instead of
    /// raising `TypeError`. `json.dumps` never does this — CPython raises
    /// "Object of type X is not JSON serializable" — but pydantic's
    /// `model_dump_json` renders nested models that way.
    pub instances_as_objects: bool,
}

impl JsonDumpOpts {
    /// `json.dumps(obj)` with no keyword arguments.
    pub fn defaults() -> Self {
        Self {
            indent: None,
            sort_keys: false,
            ensure_ascii: true,
            allow_nan: true,
            item_sep: ", ".to_owned(),
            key_sep: ": ".to_owned(),
            instances_as_objects: false,
        }
    }

    /// pydantic's `model_dump_json()`: compact separators, UTF-8 passthrough,
    /// nested models rendered as objects.
    pub fn pydantic_compact() -> Self {
        Self {
            indent: None,
            sort_keys: false,
            ensure_ascii: false,
            allow_nan: true,
            item_sep: ",".to_owned(),
            key_sep: ":".to_owned(),
            instances_as_objects: true,
        }
    }
}

/// Decode the keyword arguments of `json.dumps` / `json.dump`.
///
/// Mirrors CPython: an `indent` switches the default item separator from
/// `", "` to `","` unless `separators` is given explicitly; `default=`,
/// `cls=`, `skipkeys=` and `check_circular=` are accepted and ignored (the VM
/// has no encoder subclassing and detects no cycles).
pub fn json_dump_opts_from_kwargs(
    interp: &mut Interpreter,
    kwargs: &[(String, Value)],
) -> Result<JsonDumpOpts, Unwind> {
    let mut opts = JsonDumpOpts::defaults();
    let mut explicit_separators = false;
    for (k, v) in kwargs {
        match k.as_str() {
            "indent" => {
                opts.indent = match v {
                    Value::None => None,
                    Value::Int(_) => {
                        let n = v.to_int()?.max(0) as usize;
                        Some(" ".repeat(n))
                    }
                    Value::Str(s) => Some((**s).clone()),
                    other => {
                        return Err(type_error(format!(
                            "indent must be an int, a str or None, not {}",
                            other.type_name()
                        )))
                    }
                };
            }
            "sort_keys" => opts.sort_keys = interp.is_truthy(v)?,
            "ensure_ascii" => opts.ensure_ascii = interp.is_truthy(v)?,
            "allow_nan" => opts.allow_nan = interp.is_truthy(v)?,
            "separators" => {
                let parts: Vec<Value> = match v {
                    Value::None => continue,
                    Value::Tuple(t) => (**t).clone(),
                    Value::List(l) => l.borrow().clone(),
                    other => {
                        return Err(type_error(format!(
                            "separators must be a (item, key) pair, not {}",
                            other.type_name()
                        )))
                    }
                };
                match (parts.first(), parts.get(1), parts.len()) {
                    (Some(Value::Str(item)), Some(Value::Str(key)), 2) => {
                        opts.item_sep = (**item).clone();
                        opts.key_sep = (**key).clone();
                        explicit_separators = true;
                    }
                    _ => return Err(type_error("separators must be a pair of two strings")),
                }
            }
            "default" | "cls" | "skipkeys" | "check_circular" => {}
            other => {
                return Err(type_error(format!(
                    "dumps() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    if opts.indent.is_some() && !explicit_separators {
        opts.item_sep = ",".to_owned();
    }
    Ok(opts)
}

/// `json.dumps(v)` with the given options. Errors are the exceptions CPython
/// raises: `TypeError` for an unserialisable value or key, `ValueError` for a
/// non-finite float under `allow_nan=False`.
pub fn json_dumps_with(v: &Value, opts: &JsonDumpOpts) -> Result<String, Unwind> {
    let mut out = String::new();
    json_write(v, opts, 0, &mut out)?;
    Ok(out)
}

/// Serialise a pydantic `model` instance the way `model_dump_json()` does.
pub fn json_dumps_model(v: &Value) -> Result<String, Unwind> {
    json_dumps_with(v, &JsonDumpOpts::pydantic_compact())
}

fn json_newline(opts: &JsonDumpOpts, level: usize, out: &mut String) {
    if let Some(unit) = &opts.indent {
        out.push('\n');
        for _ in 0..level {
            out.push_str(unit);
        }
    }
}

fn json_write(
    v: &Value,
    opts: &JsonDumpOpts,
    level: usize,
    out: &mut String,
) -> Result<(), Unwind> {
    match v {
        Value::None => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(x) => out.push_str(&json_float(*x, opts.allow_nan)?),
        Value::Str(s) => json_string_into(s, opts.ensure_ascii, out),
        Value::List(l) => {
            let items = l.borrow();
            json_write_seq(&items, opts, level, out)?;
        }
        Value::Tuple(t) => json_write_seq(t, opts, level, out)?,
        Value::Dict(d) => {
            let d = d.borrow();
            // (sort key, rendered key, value). Sorting uses the original
            // string key, as CPython's `sorted(dct.items())` does — the
            // rendered form would order `"é"` before `"z"`.
            let mut entries: Vec<(String, String, &Value)> = Vec::with_capacity(d.len());
            for (k, val) in d.iter() {
                // The `__typhon_frozen__` sentinel a `freeze let` inserts is
                // not part of the value.
                if matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__") {
                    continue;
                }
                let key_value = k.clone().into_value();
                let rendered = json_key(&key_value, opts)?;
                let sort_key = match &key_value {
                    Value::Str(s) => (**s).clone(),
                    _ => rendered.clone(),
                };
                entries.push((sort_key, rendered, val));
            }
            if opts.sort_keys {
                entries.sort_by(|a, b| a.0.cmp(&b.0));
            }
            let pairs: Vec<(String, &Value)> =
                entries.into_iter().map(|(_, k, v)| (k, v)).collect();
            json_write_object(&pairs, opts, level, out)?;
        }
        Value::Instance(inst) if opts.instances_as_objects && !inst.class.fields.is_empty() => {
            let fields = inst.fields.borrow();
            let mut pairs: Vec<(String, &Value)> = Vec::with_capacity(inst.class.fields.len());
            for f in &inst.class.fields {
                if let Some(val) = fields.get(&f.name) {
                    pairs.push((json_string(&f.name, opts.ensure_ascii), val));
                }
            }
            json_write_object(&pairs, opts, level, out)?;
        }
        other => {
            return Err(type_error(format!(
                "Object of type {} is not JSON serializable",
                other.type_name()
            )))
        }
    }
    Ok(())
}

fn json_write_seq(
    items: &[Value],
    opts: &JsonDumpOpts,
    level: usize,
    out: &mut String,
) -> Result<(), Unwind> {
    if items.is_empty() {
        out.push_str("[]");
        return Ok(());
    }
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(&opts.item_sep);
        }
        json_newline(opts, level + 1, out);
        json_write(item, opts, level + 1, out)?;
    }
    json_newline(opts, level, out);
    out.push(']');
    Ok(())
}

fn json_write_object(
    pairs: &[(String, &Value)],
    opts: &JsonDumpOpts,
    level: usize,
    out: &mut String,
) -> Result<(), Unwind> {
    if pairs.is_empty() {
        out.push_str("{}");
        return Ok(());
    }
    out.push('{');
    for (i, (key, val)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push_str(&opts.item_sep);
        }
        json_newline(opts, level + 1, out);
        out.push_str(key);
        out.push_str(&opts.key_sep);
        json_write(val, opts, level + 1, out)?;
    }
    json_newline(opts, level, out);
    out.push('}');
    Ok(())
}

/// Render a dict key as a JSON object key (always a quoted string).
///
/// CPython coerces scalar keys to strings (`json.dumps({1: "a"})` →
/// `{"1": "a"}`, `True` → `"true"`, `None` → `"null"`, floats via `repr`) and
/// raises `TypeError` for anything else.
fn json_key(v: &Value, opts: &JsonDumpOpts) -> Result<String, Unwind> {
    Ok(match v {
        Value::Str(s) => json_string(s, opts.ensure_ascii),
        Value::Int(i) => json_string(&i.to_string(), true),
        Value::Bool(b) => json_string(if *b { "true" } else { "false" }, true),
        Value::Float(x) => json_string(&json_float(*x, opts.allow_nan)?, true),
        Value::None => json_string("null", true),
        other => {
            return Err(type_error(format!(
                "keys must be str, int, float, bool or None, not {}",
                other.type_name()
            )))
        }
    })
}

/// Render a float as a JSON number token, matching CPython's `json` encoder.
///
/// Finite values use the VM's canonical formatter (shortest round-trip, keeps
/// a trailing `.0`, so `1.0` stays `1.0`). Non-finite values use CPython's
/// `json.dumps` spellings (`NaN`, `Infinity`, `-Infinity`) — or raise
/// `ValueError` when `allow_nan` is off.
fn json_float(x: f64, allow_nan: bool) -> Result<String, Unwind> {
    if x.is_nan() || x.is_infinite() {
        if !allow_nan {
            return Err(value_error(format!(
                "Out of range float values are not JSON compliant: {}",
                Value::Float(x).py_repr()
            )));
        }
        return Ok(if x.is_nan() {
            "NaN".to_owned()
        } else if x > 0.0 {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        });
    }
    Ok(Value::Float(x).py_str())
}

fn json_string(s: &str, ensure_ascii: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    json_string_into(s, ensure_ascii, &mut out);
    out
}

/// Quote a string the way CPython's encoder does: `"`, `\` and the named
/// control escapes (`\b \f \n \r \t`), other control characters as `\u00XX`,
/// and — under `ensure_ascii` — every character outside printable ASCII as
/// `\uXXXX` (a UTF-16 surrogate pair beyond the BMP).
fn json_string_into(s: &str, ensure_ascii: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ensure_ascii && (c as u32) > 0x7e => {
                let cp = c as u32;
                if cp > 0xFFFF {
                    let v = cp - 0x10000;
                    let high = 0xD800 + (v >> 10);
                    let low = 0xDC00 + (v & 0x3FF);
                    out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                } else {
                    out.push_str(&format!("\\u{cp:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

thread_local! {
    /// The one `json.JSONDecodeError` class per interpreter thread. It is both
    /// what the decoder raises and what the `json` module exports, so
    /// `except json.JSONDecodeError` matches by class identity while
    /// `except ValueError` matches through the recorded builtin base.
    static JSON_DECODE_ERROR_CLASS: Rc<crate::value::Class> = {
        let mut attrs: HashMap<String, Value> = HashMap::new();
        attrs.insert(
            "__typhon_exc_bases__".to_owned(),
            Value::Tuple(Rc::new(vec![Value::Str(Rc::new("ValueError".to_owned()))])),
        );
        Rc::new(crate::value::Class {
            name: "JSONDecodeError".to_owned(),
            methods: RefCell::new(HashMap::new()),
            fields: vec![],
            class_attrs: RefCell::new(attrs),
            bases: vec![],
            properties: RefCell::new(HashSet::new()),
            classmethods: RefCell::new(HashSet::new()),
            is_exception: true,
            is_protocol: false,
        })
    };
}

fn json_decode_error_class() -> Rc<crate::value::Class> {
    JSON_DECODE_ERROR_CLASS.with(|c| c.clone())
}

/// CPython's `(lineno, colno)` for a character offset into `doc`:
/// `lineno = doc.count('\n', 0, pos) + 1`, `colno = pos - doc.rfind('\n', 0, pos)`
/// (so a first-line offset is 1-based).
fn json_line_col(chars: &[char], pos: usize) -> (usize, usize) {
    let upto = &chars[..pos.min(chars.len())];
    let lineno = upto.iter().filter(|c| **c == '\n').count() + 1;
    let colno = match upto.iter().rposition(|c| *c == '\n') {
        Some(idx) => pos - idx,
        None => pos + 1,
    };
    (lineno, colno)
}

/// Build `json.JSONDecodeError(msg, doc, pos)` — a `ValueError` subclass whose
/// `str()` is `"{msg}: line {lineno} column {colno} (char {pos})"` and which
/// carries `msg` / `doc` / `pos` / `lineno` / `colno`. `pos` is a character
/// index, as in CPython.
pub fn json_decode_error(msg: &str, doc: &str, pos: usize) -> Unwind {
    let chars: Vec<char> = doc.chars().collect();
    let (lineno, colno) = json_line_col(&chars, pos);
    let full = format!("{msg}: line {lineno} column {colno} (char {pos})");
    let mut fields: crate::value::FieldMap = crate::value::FieldMap::new();
    fields.insert(
        "args".to_owned(),
        Value::Tuple(Rc::new(vec![Value::Str(Rc::new(full.clone()))])),
    );
    fields.insert("msg".to_owned(), Value::Str(Rc::new(msg.to_owned())));
    fields.insert("doc".to_owned(), Value::Str(Rc::new(doc.to_owned())));
    fields.insert("pos".to_owned(), Value::Int(VmInt::from(pos)));
    fields.insert("lineno".to_owned(), Value::Int(VmInt::from(lineno)));
    fields.insert("colno".to_owned(), Value::Int(VmInt::from(colno)));
    let inst = Value::Instance(Rc::new(crate::value::Instance {
        class: json_decode_error_class(),
        fields: RefCell::new(fields),
        chain: RefCell::new(None),
    }));
    Unwind::Exception(crate::error::VmException::new("JSONDecodeError", full).with_value(inst))
}

/// `json.loads` over a `Value` argument, for callers outside this module.
pub(crate) fn json_loads_value(_interp: &mut Interpreter, raw: &Value) -> Result<Value, Unwind> {
    match raw {
        Value::Str(s) => json_loads(s),
        Value::Bytes(b) => json_loads(&String::from_utf8_lossy(b)),
        other => Err(type_error(format!(
            "the JSON object must be str, bytes or bytearray, not {}",
            other.type_display_name()
        ))),
    }
}

fn json_loads(s: &str) -> Result<Value, Unwind> {
    let mut p = JsonParser {
        doc: s,
        chars: s.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    if p.pos < p.chars.len() {
        return Err(p.err("Extra data", p.pos));
    }
    Ok(v)
}

/// A recursive-descent decoder over the document's characters, so every
/// reported position is the character index CPython reports.
struct JsonParser<'a> {
    doc: &'a str,
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser<'_> {
    fn err(&self, msg: &str, pos: usize) -> Unwind {
        json_decode_error(msg, self.doc, pos)
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    /// JSON whitespace is exactly ` \t\n\r` — not Python's `str.isspace()`.
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }
    fn starts_with(&self, lit: &str) -> bool {
        lit.chars()
            .enumerate()
            .all(|(i, c)| self.chars.get(self.pos + i) == Some(&c))
    }
    fn parse_value(&mut self) -> Result<Value, Unwind> {
        let Some(c) = self.peek() else {
            return Err(self.err("Expecting value", self.pos));
        };
        match c {
            '{' => self.parse_object(),
            '[' => self.parse_array(),
            '"' => self.parse_string().map(|s| Value::Str(Rc::new(s))),
            'n' if self.starts_with("null") => {
                self.pos += 4;
                Ok(Value::None)
            }
            't' if self.starts_with("true") => {
                self.pos += 4;
                Ok(Value::Bool(true))
            }
            'f' if self.starts_with("false") => {
                self.pos += 5;
                Ok(Value::Bool(false))
            }
            'N' if self.starts_with("NaN") => {
                self.pos += 3;
                Ok(Value::Float(f64::NAN))
            }
            'I' if self.starts_with("Infinity") => {
                self.pos += 8;
                Ok(Value::Float(f64::INFINITY))
            }
            '-' if self.starts_with("-Infinity") => {
                self.pos += 9;
                Ok(Value::Float(f64::NEG_INFINITY))
            }
            '-' | '0'..='9' => self.parse_number(),
            _ => Err(self.err("Expecting value", self.pos)),
        }
    }
    fn parse_object(&mut self) -> Result<Value, Unwind> {
        self.pos += 1; // {
        let mut map: DictMap = IndexMap::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Dict(Rc::new(RefCell::new(map))));
        }
        loop {
            if self.peek() != Some('"') {
                return Err(self.err(
                    "Expecting property name enclosed in double quotes",
                    self.pos,
                ));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(self.err("Expecting ':' delimiter", self.pos));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.parse_value()?;
            map.insert(HashKey::Str(Rc::new(key)), value);
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.pos += 1;
                    break;
                }
                Some(',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some('}') {
                        return Err(self.err("Illegal trailing comma before end of object", comma));
                    }
                }
                _ => return Err(self.err("Expecting ',' delimiter", self.pos)),
            }
        }
        Ok(Value::Dict(Rc::new(RefCell::new(map))))
    }
    fn parse_array(&mut self) -> Result<Value, Unwind> {
        self.pos += 1; // [
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::List(Rc::new(RefCell::new(out))));
        }
        loop {
            out.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                Some(',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        return Err(self.err("Illegal trailing comma before end of array", comma));
                    }
                }
                _ => return Err(self.err("Expecting ',' delimiter", self.pos)),
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    }
    /// Four hex digits of a `\uXXXX` escape. `err_pos` is where CPython points
    /// on failure: the `u` after the backslash.
    fn parse_hex4(&mut self, err_pos: usize) -> Result<u32, Unwind> {
        let mut cp: u32 = 0;
        for i in 0..4 {
            let digit = self
                .chars
                .get(self.pos + i)
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.err("Invalid \\uXXXX escape", err_pos))?;
            cp = (cp << 4) | digit;
        }
        self.pos += 4;
        Ok(cp)
    }
    fn parse_string(&mut self) -> Result<String, Unwind> {
        let start = self.pos; // the opening quote
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("Unterminated string starting at", start));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\\' => {
                    let backslash = self.pos;
                    self.pos += 1;
                    let Some(esc) = self.peek() else {
                        return Err(self.err("Unterminated string starting at", start));
                    };
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            self.pos += 1;
                            let mut cp = self.parse_hex4(backslash + 1)?;
                            // A high surrogate followed by a `\uDC00–\uDFFF`
                            // escape is one astral character.
                            if (0xD800..0xDC00).contains(&cp) && self.starts_with("\\u") {
                                let save = self.pos;
                                self.pos += 2;
                                let low = self.parse_hex4(save + 1)?;
                                if (0xDC00..0xE000).contains(&low) {
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                                } else {
                                    // Not a pair: leave the second escape to be
                                    // decoded on its own.
                                    self.pos = save;
                                }
                            }
                            // A lone surrogate has no `char`; CPython keeps it
                            // as an unpaired code unit, which cannot be printed
                            // either — U+FFFD is the closest representable value.
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            continue;
                        }
                        _ => return Err(self.err("Invalid \\escape", backslash)),
                    }
                    self.pos += 1;
                }
                c if (c as u32) < 0x20 => {
                    return Err(self.err("Invalid control character at", self.pos));
                }
                c => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }
    /// CPython's number grammar: `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?`.
    /// A leading zero, a bare `.` or a dangling exponent stop the number and
    /// are reported by the caller as `Extra data` / a delimiter error, exactly
    /// as CPython's scanner leaves them.
    fn parse_number(&mut self) -> Result<Value, Unwind> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        match self.peek() {
            Some('0') => self.pos += 1,
            Some('1'..='9') => {
                while matches!(self.peek(), Some('0'..='9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.err("Expecting value", start)),
        }
        let mut is_float = false;
        if self.peek() == Some('.') && matches!(self.chars.get(self.pos + 1), Some('0'..='9')) {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some('0'..='9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let mut look = self.pos + 1;
            if matches!(self.chars.get(look), Some('+' | '-')) {
                look += 1;
            }
            if matches!(self.chars.get(look), Some('0'..='9')) {
                is_float = true;
                self.pos = look;
                while matches!(self.peek(), Some('0'..='9')) {
                    self.pos += 1;
                }
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            // Rust's parser saturates to ±inf on overflow, as `float()` does.
            Ok(Value::Float(
                text.parse::<f64>()
                    .map_err(|_| self.err("Expecting value", start))?,
            ))
        } else {
            Ok(Value::Int(VmInt::from(
                text.parse::<num_bigint::BigInt>()
                    .map_err(|_| self.err("Expecting value", start))?,
            )))
        }
    }
}

// ── Helper called by the interpreter for keyword-aware native calls ────────

/// Marker tag for the trailing tuple that carries keyword arguments through
/// the generic bound-method dispatcher (which has no kwargs slot of its own).
const KWARGS_MARKER: &str = "__typhon_kwargs_sentinel__";

/// Encode keyword arguments as a sentinel tuple appended to a method's args.
pub fn make_kwargs_sentinel(kwargs: &[(String, Value)]) -> Value {
    let mut m: DictMap = IndexMap::new();
    for (k, v) in kwargs {
        m.insert(HashKey::Str(Rc::new(k.clone())), v.clone());
    }
    Value::Tuple(Rc::new(vec![
        Value::Str(Rc::new(KWARGS_MARKER.to_owned())),
        Value::Dict(Rc::new(RefCell::new(m))),
    ]))
}

/// If `args` ends with a kwargs sentinel, split it into (positional, keywords).
/// [`split_kwargs`] for callers outside this module (the unbound
/// builtin-classmethod natives in `interp`).
pub(crate) fn split_kwargs_pub(args: &[Value]) -> (&[Value], Vec<(String, Value)>) {
    split_kwargs(args)
}

fn split_kwargs(args: &[Value]) -> (&[Value], Vec<(String, Value)>) {
    if let Some(Value::Tuple(t)) = args.last() {
        if t.len() == 2 {
            if let (Value::Str(s), Value::Dict(d)) = (&t[0], &t[1]) {
                if s.as_str() == KWARGS_MARKER {
                    let kw = d
                        .borrow()
                        .iter()
                        .filter_map(|(k, v)| match k {
                            HashKey::Str(s) => Some(((**s).clone(), v.clone())),
                            _ => None,
                        })
                        .collect();
                    return (&args[..args.len() - 1], kw);
                }
            }
        }
    }
    (args, Vec::new())
}

pub fn call_with_kwargs(
    interp: &mut Interpreter,
    n: &NativeFn,
    args: Vec<Value>,
    kwargs: &[(String, Value)],
) -> Result<Value, Unwind> {
    match n.name {
        // enumerate(iterable, start=N)
        "enumerate" => {
            let mut start: i64 = 0;
            for (k, v) in kwargs {
                if k == "start" {
                    start = v.to_int()?;
                } else {
                    return Err(type_error(format!(
                        "enumerate() got unexpected keyword: '{}'",
                        k
                    )));
                }
            }
            let iterable = args
                .into_iter()
                .next()
                .ok_or_else(|| type_error("enumerate() requires an iterable"))?;
            if let Value::Iter(it) = interp.make_iter(iterable)? {
                Ok(Value::Iter(Rc::new(RefCell::new(IterState::Enumerate {
                    inner: it,
                    index: start,
                }))))
            } else {
                unreachable!()
            }
        }
        // zip(*iterables, strict=True): error if lengths differ.
        "zip" => {
            let mut strict = false;
            for (k, v) in kwargs {
                if k == "strict" {
                    strict = interp.is_truthy(v)?;
                } else {
                    return Err(type_error(format!("zip() got unexpected keyword: '{}'", k)));
                }
            }
            if !strict {
                let mut inners = Vec::new();
                for a in args {
                    if let Value::Iter(it) = interp.make_iter(a)? {
                        inners.push(it);
                    }
                }
                return Ok(Value::Iter(Rc::new(RefCell::new(IterState::Zip {
                    inners,
                }))));
            }
            // Strict mode: materialise each iterable so we can check that
            // every column has the same length (CPython raises ValueError).
            let columns: Vec<Vec<Value>> = args
                .into_iter()
                .map(|a| -> Result<Vec<Value>, Unwind> {
                    let it = interp.make_iter(a)?;
                    let mut out = Vec::new();
                    while let Some(v) = interp.iter_next(&it)? {
                        out.push(v);
                    }
                    Ok(out)
                })
                .collect::<Result<_, _>>()?;
            // CPython names the offending argument: the first iterable to run
            // dry decides the verb — if it is argument 1, the first later
            // iterable that still has items is "longer"; otherwise the dry one
            // is "shorter than argument(s) 1[-i]".
            let len = columns.iter().map(|c| c.len()).min().unwrap_or(0);
            if columns.iter().any(|c| c.len() != len) {
                let dry = columns.iter().position(|c| c.len() == len).unwrap_or(0);
                let (verb, arg) = if dry == 0 {
                    (
                        "longer",
                        columns.iter().position(|c| c.len() > len).unwrap_or(1),
                    )
                } else {
                    ("shorter", dry)
                };
                let others = if arg == 1 {
                    " ".to_string()
                } else {
                    "s 1-".to_string()
                };
                return Err(value_error(format!(
                    "zip() argument {} is {verb} than argument{others}{arg}",
                    arg + 1
                )));
            }
            let mut rows: Vec<Value> = Vec::with_capacity(len);
            for row in 0..len {
                let tup: Vec<Value> = columns.iter().map(|c| c[row].clone()).collect();
                rows.push(Value::Tuple(Rc::new(tup)));
            }
            Ok(Value::Iter(Rc::new(RefCell::new(IterState::List {
                items: Rc::new(RefCell::new(rows)),
                index: 0,
            }))))
        }
        "min" | "max" => {
            let want_min = n.name == "min";
            // Gather candidates: a single iterable, or multiple positional args.
            let candidates: Vec<Value> = if args.len() == 1 {
                let it = interp.make_iter(args.into_iter().next().unwrap())?;
                let mut out = Vec::new();
                while let Some(v) = interp.iter_next(&it)? {
                    out.push(v);
                }
                out
            } else {
                args
            };
            let mut key_fn: Option<Value> = None;
            let mut default: Option<Value> = None;
            for (k, v) in kwargs {
                match k.as_str() {
                    "key" => key_fn = Some(v.clone()),
                    "default" => default = Some(v.clone()),
                    _ => {
                        return Err(type_error(format!(
                            "{}() got unexpected keyword: '{}'",
                            n.name, k
                        )))
                    }
                }
            }
            if candidates.is_empty() {
                return default.ok_or_else(|| {
                    value_error(format!("{}() iterable argument is empty", n.name))
                });
            }
            let mut best = candidates[0].clone();
            let mut best_key = match &key_fn {
                Some(f) => interp.call_value(f.clone(), vec![best.clone()], &[])?,
                None => best.clone(),
            };
            for v in candidates.into_iter().skip(1) {
                let vk = match &key_fn {
                    Some(f) => interp.call_value(f.clone(), vec![v.clone()], &[])?,
                    None => v.clone(),
                };
                // `value_cmp` honours a user `__lt__` on the (keyed) operands.
                let cmp = interp.value_cmp(&vk, &best_key)?;
                if (want_min && cmp == std::cmp::Ordering::Less)
                    || (!want_min && cmp == std::cmp::Ordering::Greater)
                {
                    best = v;
                    best_key = vk;
                }
            }
            Ok(best)
        }
        "sorted" => {
            let mut out = Vec::new();
            let it = interp.make_iter(
                args.into_iter()
                    .next()
                    .ok_or_else(|| type_error("sorted() requires an iterable"))?,
            )?;
            while let Some(v) = interp.iter_next(&it)? {
                out.push(v);
            }
            let mut reverse = false;
            let mut key_fn: Option<Value> = None;
            for (k, v) in kwargs {
                match k.as_str() {
                    "reverse" => reverse = v.truthy(),
                    "key" => key_fn = Some(v.clone()),
                    _ => {
                        return Err(type_error(format!(
                            "sorted() got unexpected keyword: '{}'",
                            k
                        )))
                    }
                }
            }
            // `value_cmp` honours a user `__lt__`; the dunder-blind `py_cmp`
            // left instance lists unsorted. `sort_by` can't return `Result`, so
            // the first comparison error is captured and surfaced afterwards.
            let mut sort_error: Option<Unwind> = None;
            if let Some(key) = key_fn {
                let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(out.len());
                for v in out {
                    let k = interp.call_value(key.clone(), vec![v.clone()], &[])?;
                    keyed.push((k, v));
                }
                keyed.sort_by(|a, b| {
                    if sort_error.is_some() {
                        return std::cmp::Ordering::Equal;
                    }
                    // Stable reverse: flip the comparator, not the list.
                    match interp.value_cmp(&a.0, &b.0) {
                        Ok(o) => {
                            if reverse {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                        Err(e) => {
                            sort_error = Some(e);
                            std::cmp::Ordering::Equal
                        }
                    }
                });
                if let Some(e) = sort_error {
                    return Err(e);
                }
                Ok(Value::List(Rc::new(RefCell::new(
                    keyed.into_iter().map(|(_, v)| v).collect(),
                ))))
            } else {
                out.sort_by(|a, b| {
                    if sort_error.is_some() {
                        return std::cmp::Ordering::Equal;
                    }
                    match interp.value_cmp(a, b) {
                        Ok(o) => {
                            if reverse {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                        Err(e) => {
                            sort_error = Some(e);
                            std::cmp::Ordering::Equal
                        }
                    }
                });
                if let Some(e) = sort_error {
                    return Err(e);
                }
                Ok(Value::List(Rc::new(RefCell::new(out))))
            }
        }
        // `dataclasses.field(default=…, default_factory=…)`. The desugar
        // pass rewrites bare-mutable defaults like `tags: list[str] = []`
        // into `dataclasses.field(default_factory=list)`. We return a
        // tagged tuple sentinel that the instance constructor recognises
        // and invokes per-instance, so each instance gets its own fresh
        // mutable container instead of sharing one across all instances.
        "field" => {
            let mut default: Option<Value> = args.into_iter().next();
            for (k, v) in kwargs {
                match k.as_str() {
                    "default" => default = Some(v.clone()),
                    "default_factory" => {
                        // Sentinel: ("__typhon_field_factory__", callable).
                        return Ok(Value::Tuple(Rc::new(vec![
                            Value::Str(Rc::new("__typhon_field_factory__".to_owned())),
                            v.clone(),
                        ])));
                    }
                    // `repr`, `hash`, `init`, `compare`, `metadata`, `kw_only`
                    // — silently accepted (VM doesn't model them) so the
                    // emitted dataclass `__init__` doesn't crash.
                    _ => {}
                }
            }
            Ok(default.unwrap_or(Value::None))
        }
        // `dataclasses.replace(obj, field=value, …)`.
        "dataclasses.replace" => dataclass_replace(interp, args, kwargs),
        // `json.dumps(obj, indent=…, sort_keys=…, ensure_ascii=…,
        // separators=…, allow_nan=…)` — the encoder options CPython honours.
        "dumps" => {
            let obj = args
                .first()
                .ok_or_else(|| type_error("dumps() missing argument"))?;
            let opts = json_dump_opts_from_kwargs(interp, kwargs)?;
            Ok(Value::Str(Rc::new(json_dumps_with(obj, &opts)?)))
        }
        // `json.dump(obj, fp, **same options)`.
        "dump" => {
            if args.len() < 2 {
                return Err(type_error("dump() requires (obj, fp)"));
            }
            let opts = json_dump_opts_from_kwargs(interp, kwargs)?;
            let serialised = json_dumps_with(&args[0], &opts)?;
            let write = interp.get_attr(&args[1], "write")?;
            interp.call_value(write, vec![Value::Str(Rc::new(serialised))], &[])?;
            Ok(Value::None)
        }
        // `dict(a=1, b=2)` and `dict(other, c=3)` — keyword pairs become entries.
        "dict" => {
            let base = (n.func)(interp, args)?;
            if let Value::Dict(d) = &base {
                let mut m = d.borrow_mut();
                for (k, v) in kwargs {
                    m.insert(HashKey::Str(Rc::new(k.clone())), v.clone());
                }
            }
            Ok(base)
        }
        // Native ctors / shims that intentionally accept any kwargs and
        // discard them. Used by stdlib stubs that exist purely so user
        // code that calls them at import time doesn't crash.
        "ConfigDict" | "dataclass" => (n.func)(interp, args),
        // `itertools.groupby(iterable, key=…)` — fold the `key` keyword into
        // the second positional slot the native already understands.
        "groupby" => {
            let mut args = args;
            for (k, v) in kwargs {
                match k.as_str() {
                    "key" => {
                        if args.len() < 2 {
                            args.push(v.clone());
                        } else {
                            args[1] = v.clone();
                        }
                    }
                    "iterable" => {
                        if args.is_empty() {
                            args.push(v.clone());
                        } else {
                            args[0] = v.clone();
                        }
                    }
                    _ => {
                        return Err(type_error(format!(
                            "groupby() got unexpected keyword: '{}'",
                            k
                        )))
                    }
                }
            }
            (n.func)(interp, args)
        }
        // print(*values, sep=' ', end='\n', file=..., flush=...)
        "print" => {
            let mut sep = " ".to_owned();
            let mut end = "\n".to_owned();
            let mut to_stderr = false;
            let mut file_sink: Option<Value> = None;
            for (k, v) in kwargs {
                match k.as_str() {
                    "sep" => match v {
                        Value::Str(s) => sep = (**s).clone(),
                        Value::None => {}
                        _ => {
                            return Err(type_error(format!(
                                "sep must be None or a string, not {}",
                                v.type_name()
                            )))
                        }
                    },
                    "end" => match v {
                        Value::Str(s) => end = (**s).clone(),
                        Value::None => {}
                        _ => {
                            return Err(type_error(format!(
                                "end must be None or a string, not {}",
                                v.type_name()
                            )))
                        }
                    },
                    // `file=` distinguishes the two std streams; arbitrary
                    // file objects aren't writable sinks in the VM. `flush=`
                    // is accepted and ignored — the VM writes through.
                    "file" => match v {
                        Value::Module(m) if m.name == "sys.stderr" => to_stderr = true,
                        Value::Module(m) if m.name == "sys.stdout" => to_stderr = false,
                        Value::None => {}
                        // Any other sink: `file.write(text)`, as CPython does.
                        other => file_sink = Some(other.clone()),
                    },
                    "flush" => {}
                    _ => {
                        return Err(type_error(format!(
                            "print() got unexpected keyword: '{}'",
                            k
                        )))
                    }
                }
            }
            let mut out = String::new();
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(&sep);
                }
                out.push_str(&interp.str_of(a)?);
            }
            out.push_str(&end);
            let file_sink = file_sink.or_else(|| redirected_std_stream(interp, to_stderr));
            if let Some(sink) = file_sink {
                let write = interp.get_attr(&sink, "write")?;
                interp.call_value(write, vec![Value::Str(Rc::new(out))], &[])?;
                return Ok(Value::None);
            }
            if to_stderr {
                eprint!("{out}");
            } else {
                vm_write_stdout(&out);
                if !end.ends_with('\n') {
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
            }
            Ok(Value::None)
        }
        // sum(iterable, start=X) — keyword form (positional start already
        // works through the plain native).
        "sum" => {
            let mut acc = Value::Int(VmInt::from(0));
            for (k, v) in kwargs {
                if k == "start" {
                    acc = v.clone();
                } else {
                    return Err(type_error(format!("sum() got unexpected keyword: '{}'", k)));
                }
            }
            let mut args = args.into_iter();
            let iterable = args
                .next()
                .ok_or_else(|| type_error("sum() requires an iterable"))?;
            // `sum(xs, start)` — CPython accepts `start` positionally as well
            // as by keyword. Only the keyword form was read, so the positional
            // one was silently discarded and the call returned a total short
            // by exactly `start`, with no error.
            if let Some(start) = args.next() {
                acc = start;
            }
            if args.next().is_some() {
                return Err(type_error("sum() takes at most 2 arguments"));
            }
            let it = interp.make_iter(iterable)?;
            while let Some(v) = interp.iter_next(&it)? {
                acc = interp.binop(&acc, ruff_python_ast::Operator::Add, &v)?;
            }
            Ok(acc)
        }
        // asyncio.gather(..., return_exceptions=True): force each
        // argument, catching exceptions into the result list.
        "gather" => {
            let return_exceptions = kwargs
                .iter()
                .find(|(k, _)| k == "return_exceptions")
                .map(|(_, v)| v.truthy())
                .unwrap_or(false);
            let mut out: Vec<Value> = Vec::with_capacity(args.len());
            for coro in args {
                match interp.force_awaitable(coro) {
                    Ok(v) => out.push(v),
                    Err(Unwind::Exception(e)) if return_exceptions => {
                        let v = e.value.clone().unwrap_or(Value::Exception {
                            kind: Rc::new(e.kind.clone()),
                            message: Rc::new(e.message.clone()),
                            args: Rc::new(vec![Value::Str(Rc::new(e.message.clone()))]),
                            chain: None,
                        });
                        out.push(v);
                    }
                    Err(other) => return Err(other),
                }
            }
            Ok(Value::List(Rc::new(RefCell::new(out))))
        }
        "Queue" => Ok(make_asyncio_queue(&args, kwargs)),
        // Instance-field natives that parse their own kwargs via the
        // sentinel (`split_kwargs`): forward and let the body peel it.
        "mkdir"
        | "makedirs"
        | "contextmanager_factory"
        | "namedtuple"
        | "open"
        | "str"
        | "bytes"
        | "bytearray"
        | "from_bytes"
        | "partial"
        | "partial_call" => {
            let mut args = args;
            args.push(make_kwargs_sentinel(kwargs));
            (n.func)(interp, args)
        }
        // `pow(base, exp, mod=None)` — CPython names all three parameters, so
        // `pow(2, exp=3)` and `functools.partial(pow, exp=2)` are ordinary
        // calls there.
        "pow" => {
            let mut positional = args;
            for name in ["base", "exp", "mod"] {
                if let Some((_, v)) = kwargs.iter().find(|(k, _)| k == name) {
                    positional.push(v.clone());
                }
            }
            if let Some((k, _)) = kwargs
                .iter()
                .find(|(k, _)| !matches!(k.as_str(), "base" | "exp" | "mod"))
            {
                return Err(type_error(format!(
                    "pow() got an unexpected keyword argument '{k}'"
                )));
            }
            (n.func)(interp, positional)
        }
        // `Ok` / `Err` are natives in the VM but frozen *dataclasses* in the
        // emitted `typhon_runtime`, where the field names are part of the
        // public API: `Ok(value=v)` and `Err(error=e)` are ordinary calls
        // under CPython. Accept the same keyword forms so a program does not
        // run under `tyc build` and fail under `tyc run`.
        "Ok" | "Err" => {
            let field = if n.name == "Ok" { "value" } else { "error" };
            let mut args = args;
            for (k, v) in kwargs {
                if k == field && args.is_empty() {
                    args.push(v.clone());
                } else {
                    return Err(type_error(format!(
                        "{}() got an unexpected keyword argument '{k}'",
                        n.name
                    )));
                }
            }
            (n.func)(interp, args)
        }
        // Bound builtin methods (the "method" native) never reach here — they
        // are intercepted in `call_value`, which forwards kwargs via the
        // tuple sentinel (`make_kwargs_sentinel` / `split_kwargs`).
        _ => {
            if kwargs.is_empty() {
                (n.func)(interp, args)
            } else {
                Err(type_error(format!(
                    "{}() does not accept keyword arguments",
                    n.name
                )))
            }
        }
    }
}

thread_local! {
    /// One canonical `Class` object per builtin type name, so repeated
    /// `type(x)` calls return the *same* object — `type(5) == type(6)` then
    /// holds by identity (Class equality is identity-based; see `py_eq`).
    static BUILTIN_TYPE_CACHE: RefCell<HashMap<String, Rc<crate::value::Class>>> =
        RefCell::new(HashMap::new());
}

/// Whether `c` is the cached stand-in class `type(x)` hands back for a
/// builtin — as opposed to a user class that happens to be named `int`.
/// A type-keyed registry has to see the stand-in and the constructor native
/// of the same name as one key.
pub(crate) fn is_builtin_type_class(c: &Rc<crate::value::Class>) -> bool {
    BUILTIN_TYPE_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&c.name)
            .is_some_and(|cached| Rc::ptr_eq(cached, c))
    })
}

/// A lightweight type object for a built-in type (`int`, `str`, …) — an empty
/// `Class` whose only meaningful attribute is its `name`. Used by `type(x)`
/// so type comparisons and `.__name__` work uniformly with user classes.
pub fn make_builtin_type(name: &str) -> Value {
    BUILTIN_TYPE_CACHE.with(|c| {
        let cls = c
            .borrow_mut()
            .entry(name.to_owned())
            .or_insert_with(|| {
                Rc::new(crate::value::Class {
                    name: name.to_owned(),
                    methods: std::cell::RefCell::new(HashMap::new()),
                    fields: vec![],
                    class_attrs: std::cell::RefCell::new(HashMap::new()),
                    bases: vec![],
                    properties: std::cell::RefCell::new(std::collections::HashSet::new()),
                    classmethods: std::cell::RefCell::new(std::collections::HashSet::new()),
                    is_exception: false,
                    is_protocol: false,
                })
            })
            .clone();
        Value::Class(cls)
    })
}

/// Full Unicode case folding, byte-exact with CPython's `str.casefold()`.
///
/// Rust's std offers no case-folding, and the two obvious stand-ins are both
/// wrong: `to_lowercase` leaves `ß` as `ß` (fold is `ss`) and maps Cherokee
/// *away* from its folded form, while an uppercase-then-lowercase round-trip
/// collapses distinctions the fold preserves (dotless `ı` → `I` → `i`). The
/// authoritative C+F mappings from the Unicode Character Database are embedded
/// in [`crate::casefold_data`] instead: each scalar is looked up (identity when
/// absent), expanding to one or more scalars.
fn casefold_str(s: &str) -> String {
    use crate::casefold_data::{CASEFOLD_MULTI, CASEFOLD_SINGLE};
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        if let Ok(i) = CASEFOLD_SINGLE.binary_search_by(|&(k, _)| k.cmp(&cp)) {
            // A table value is a fold target from the UCD, always a valid scalar.
            out.push(char::from_u32(CASEFOLD_SINGLE[i].1).unwrap_or(c));
        } else if let Ok(i) = CASEFOLD_MULTI.binary_search_by(|&(k, _)| k.cmp(&cp)) {
            out.push_str(CASEFOLD_MULTI[i].1);
        } else {
            out.push(c);
        }
    }
    out
}

fn is_title_case(s: &str) -> bool {
    let mut saw_cased = false;
    let mut prev_cased = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            let upper = c.is_uppercase();
            if prev_cased {
                if upper {
                    return false;
                }
            } else if !upper {
                return false;
            }
            saw_cased = true;
            prev_cased = true;
        } else {
            prev_cased = false;
        }
    }
    saw_cased
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut new_word = true;
    for c in s.chars() {
        if c.is_alphabetic() {
            if new_word {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            new_word = false;
        } else {
            out.push(c);
            new_word = true;
        }
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut it = s.chars();
    match it.next() {
        Some(c) => {
            let mut out = c.to_uppercase().collect::<String>();
            out.push_str(&it.as_str().to_lowercase());
            out
        }
        None => String::new(),
    }
}
