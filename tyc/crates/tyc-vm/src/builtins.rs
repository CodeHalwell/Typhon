//! Native builtins and the small stdlib the VM understands directly.
//!
//! Everything here is implemented in Rust rather than dispatched to CPython.
//! Modules that aren't supported produce a clear `ImportError`-style message.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::error::{
    attribute_error, index_error, key_error, stop_iteration, type_error, value_error, Unwind,
};
use crate::interp::{normalize_index, Interpreter};
use crate::value::{DictMap, HashKey, IterState, Module, NativeFn, Value};
use num_traits::ToPrimitive as _;

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
        println!("{}", out);
        Ok(Value::None)
    });

    native!("len", |interp, args| {
        use num_traits::Signed;
        let v = single(&args, "len")?;
        if let Value::Instance(_) = v {
            if let Some(r) = interp.call_dunder0(v, "__len__")? {
                // `__len__` must return a non-negative int (CPython raises
                // TypeError for non-int, ValueError for negative).
                let n = match r {
                    Value::Int(i) => i,
                    Value::Bool(b) => num_bigint::BigInt::from(b as i64),
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
        Ok(Value::Int(num_bigint::BigInt::from(value_len(v)? as i64)))
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
        Ok(Value::Str(Rc::new(match args.first() {
            Some(v) => interp.str_of(v)?,
            None => String::new(),
        })))
    });

    native!("int", |_i, args| {
        // `int()` with no argument is 0 (matches CPython; used by
        // `defaultdict(int)` as a zero-factory).
        if args.is_empty() {
            return Ok(Value::Int(num_bigint::BigInt::from(0)));
        }
        let v = single(&args, "int")?;
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
                Some(n) => Ok(Value::Int(if neg { -n } else { n })),
                None => Err(value_error(format!(
                    "invalid literal for int() with base {}: '{}'",
                    base, s
                ))),
            };
        }
        Ok(Value::Int(v.to_bigint()?))
    });

    native!("divmod", |_i, args| {
        use num_integer::Integer;
        use num_traits::Zero;
        let a = args
            .first()
            .ok_or_else(|| type_error("divmod expected 2 arguments"))?;
        let b = args
            .get(1)
            .ok_or_else(|| type_error("divmod expected 2 arguments"))?;
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
                let q = (xf / yf).floor();
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Float(q),
                    Value::Float(xf - q * yf),
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
                use num_traits::{Signed, Zero};
                if modv.is_zero() {
                    return Err(value_error("pow() 3rd argument cannot be 0"));
                }
                if exp.is_negative() {
                    return Err(value_error(
                        "pow() 2nd argument cannot be negative when 3rd argument specified",
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
        let spec = args.get(1).map(|s| s.py_str()).unwrap_or_default();
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

    native!("float", |_i, args| {
        let v = single(&args, "float")?;
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
                if inst.fields.borrow_mut().remove(name.as_str()).is_none() {
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
        match args.into_iter().next() {
            None => Ok(Value::Bytes(Rc::new(Vec::new()))),
            Some(Value::Bytes(b)) => Ok(Value::Bytes(b)),
            // bytes(int) -> that many zero bytes.
            Some(Value::Int(n)) => {
                let n = num_traits::ToPrimitive::to_usize(&n)
                    .ok_or_else(|| value_error("negative count"))?;
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

    native!("set", |i, args| {
        let mut out = HashSet::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(x) = i.iter_next(&it)? {
                out.insert(x.to_hash_key()?);
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
                        map.insert(k.to_hash_key()?, val);
                    }
                    return Ok(Value::Dict(Rc::new(RefCell::new(map))));
                }
            }
            let it = i.make_iter(v)?;
            while let Some(pair) = i.iter_next(&it)? {
                match pair {
                    Value::Tuple(t) if t.len() == 2 => {
                        map.insert(t[0].to_hash_key()?, t[1].clone());
                    }
                    _ => return Err(type_error("dict update expected a sequence of pairs")),
                }
            }
        }
        Ok(Value::Dict(Rc::new(RefCell::new(map))))
    });

    native!("frozenset", |i, args| {
        let mut out = HashSet::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(x) = i.iter_next(&it)? {
                out.insert(x.to_hash_key()?);
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
        Ok(Value::Bool(is_instance_of(val, &cls)))
    });

    native!("abs", |i, args| match single(&args, "abs")? {
        Value::Int(n) => Ok(Value::Int(num_traits::Signed::abs(n))),
        Value::Float(x) => Ok(Value::Float(x.abs())),
        Value::Bool(b) => Ok(Value::Int(num_bigint::BigInt::from(*b as i64))),
        // `abs(complex)` is the Euclidean magnitude (a float), matching CPython.
        Value::Complex(re, im) => Ok(Value::Float((re * re + im * im).sqrt())),
        v @ Value::Instance(_) => {
            // User `__abs__` dunder.
            if let Some(r) = i.call_dunder0(v, "__abs__")? {
                Ok(r)
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
                Value::Int(n) => Ok(crate::value::bigint_to_f64(n)),
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
        let it = i.make_iter(
            args.into_iter()
                .next()
                .ok_or_else(|| type_error("sum() requires an iterable"))?,
        )?;
        let mut acc = Value::Int(num_bigint::BigInt::from(0));
        while let Some(v) = i.iter_next(&it)? {
            acc = i.binop(&acc, ruff_python_ast::Operator::Add, &v)?;
        }
        Ok(acc)
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
        let it = i.make_iter(
            args.into_iter()
                .next()
                .ok_or_else(|| type_error("reversed() requires an iterable"))?,
        )?;
        let mut out: Vec<Value> = Vec::new();
        while let Some(v) = i.iter_next(&it)? {
            out.push(v);
        }
        out.reverse();
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });

    native!("enumerate", |i, args| {
        let iterable = args
            .into_iter()
            .next()
            .ok_or_else(|| type_error("enumerate() requires an iterable"))?;
        let inner = i.make_iter(iterable)?;
        if let Value::Iter(it) = inner {
            Ok(Value::Iter(Rc::new(RefCell::new(IterState::Enumerate {
                inner: it,
                index: 0,
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
        let it = args
            .into_iter()
            .next()
            .ok_or_else(|| type_error("next() requires an iterator"))?;
        match i.iter_next(&it)? {
            Some(v) => Ok(v),
            None => Err(stop_iteration()),
        }
    });

    native!("iter", |i, args| {
        i.make_iter(
            args.into_iter()
                .next()
                .ok_or_else(|| type_error("iter() requires an iterable"))?,
        )
    });

    native!("hex", |_i, args| Ok(Value::Str(Rc::new(format!(
        "0x{:x}",
        single(&args, "hex")?.to_int()?
    )))));
    native!("bin", |_i, args| Ok(Value::Str(Rc::new(format!(
        "0b{:b}",
        single(&args, "bin")?.to_int()?
    )))));
    native!("oct", |_i, args| Ok(Value::Str(Rc::new(format!(
        "0o{:o}",
        single(&args, "oct")?.to_int()?
    )))));

    native!("chr", |_i, args| {
        let n = single(&args, "chr")?.to_int()?;
        let c = char::from_u32(n as u32)
            .ok_or_else(|| value_error(format!("chr() arg not in range: {n}")))?;
        Ok(Value::Str(Rc::new(c.to_string())))
    });
    native!("ord", |_i, args| {
        let v = single(&args, "ord")?;
        match v {
            Value::Str(s) => {
                let mut it = s.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => Ok(Value::Int(num_bigint::BigInt::from(c as i64))),
                    _ => Err(type_error("ord() expected a single-character string")),
                }
            }
            _ => Err(type_error("ord() expected a string")),
        }
    });

    native!("round", |_i, args| match args.first() {
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
                        Ok(Value::Int(num_bigint::BigInt::zero()))
                    } else {
                        let p = num_bigint::BigInt::from(10).pow((-nd) as u32);
                        // Floor-divide so the remainder is always in [0, p).
                        let q = i.div_floor(&p);
                        let r = i - &q * &p;
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
                        Ok(Value::Int(rounded * &p))
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
                _ => {
                    let r = round_half_even(x);
                    Ok(Value::Int(
                        <num_bigint::BigInt as num_traits::FromPrimitive>::from_f64(r)
                            .unwrap_or_else(|| num_bigint::BigInt::from(r as i64)),
                    ))
                }
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
        std::io::stdin().read_line(&mut s).map_err(|e| {
            crate::error::Unwind::Exception(crate::error::VmException::new(
                "OSError",
                format!("{e}"),
            ))
        })?;
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
        let v = single(&args, "hash")?;
        // A user-defined `__hash__` wins over the structural hash key.
        if let Value::Instance(inst) = v {
            if let Some(m) = i.find_method(&inst.class, "__hash__") {
                let r = i.call_value(
                    Value::BoundMethod {
                        receiver: Box::new(v.clone()),
                        function: m,
                    },
                    vec![],
                    &[],
                )?;
                return match r {
                    Value::Int(_) => Ok(r),
                    other => Err(type_error(format!(
                        "__hash__ method should return an integer, not {}",
                        other.type_name()
                    ))),
                };
            }
        }
        let key = v.to_hash_key()?;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        Ok(Value::Int(num_bigint::BigInt::from(h.finish() as i64)))
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
        Ok(Value::Int(num_bigint::BigInt::from(addr as i64)))
    });

    native!("callable", |_i, args| {
        let v = single(&args, "callable")?;
        Ok(Value::Bool(matches!(
            v,
            Value::Function(_) | Value::Native(_) | Value::BoundMethod { .. } | Value::Class(_)
        )))
    });

    native!("open", |_i, args| {
        let path = args
            .first()
            .ok_or_else(|| type_error("open() requires a path"))?
            .py_str();
        let mode = args
            .get(1)
            .map(|v| v.py_str())
            .unwrap_or_else(|| "r".into());
        crate::ffi::open_file(&path, &mode)
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
        })))
    });

    // Constants.
    root.set("True", Value::Bool(true));
    root.set("False", Value::Bool(false));
    root.set("None", Value::None);

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
        let ctor = NativeFn::new(Box::leak(n.clone().into_boxed_str()), move |_i, args| {
            let msg = args.first().map(|v| v.py_str()).unwrap_or_default();
            Ok(Value::Exception {
                kind: Rc::new(n.clone()),
                message: Rc::new(msg),
                args: Rc::new(args),
            })
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
    // checked_cast as __typhon_checked_cast__`. The VM treats the cast as an
    // identity passthrough: the static type is already `TYPE` (the checker
    // enforced that), and the recursive structural runtime check lives in the
    // generated `typhon_runtime/cast.py`, so the authoritative enforcement is
    // on the `tyc build && python` path. Returning the value unchanged keeps
    // `tyc run` working without needing to interpret the type descriptor.
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

fn single<'a>(args: &'a [Value], name: &str) -> Result<&'a Value, Unwind> {
    args.first()
        .ok_or_else(|| type_error(format!("{}() requires an argument", name)))
}

fn value_len(v: &Value) -> Result<usize, Unwind> {
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

fn is_instance_of(val: &Value, cls: &Value) -> bool {
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
        ("int", Value::Int(_)) => true,
        ("float", Value::Float(_)) => true,
        ("bool", Value::Bool(_)) => true,
        ("str", Value::Str(_)) => true,
        ("bytes", Value::Bytes(_)) => true,
        ("list", Value::List(_)) => true,
        ("tuple", Value::Tuple(_)) => true,
        ("dict", Value::Dict(_)) => true,
        ("set", Value::Set(_)) => true,
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
            "{}() arg is an empty sequence",
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
fn compile_helpers(interp: &mut Interpreter, source: &str) -> Result<Vec<(String, Value)>, Unwind> {
    use tyc_syntax::preprocess;
    let expanded = preprocess::expand_question_ops(&preprocess::expand_pipes(
        &preprocess::expand_with_chains(&preprocess::expand_go_calls(
            &preprocess::expand_gather_blocks(&preprocess::expand_multiline_guards(
                &preprocess::expand_typed_let_unpack(&preprocess::expand_lazy_lets(source)),
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
    let desugar_out = tyc_desugar::desugar_module(&module);
    module = desugar_out.module;

    let env = crate::env::Env::new_child(&interp.root);
    interp.exec_block(&module.body, &env)?;
    Ok(env.snapshot())
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
        return self._factory()
    def __contains__(self, key):
        return key in self._data
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
"#;

fn defaultdict_class(interp: &mut Interpreter) -> Result<Value, Unwind> {
    cached_helper_class(
        interp,
        "__shim_defaultdict__",
        DEFAULTDICT_SRC,
        "_DefaultDict",
    )
}

/// Source for the native `datetime` module: `date`, `datetime`, `timedelta`.
/// Date math uses a proleptic-Gregorian ordinal (`_ordinal`) so `date - date`
/// and `datetime + timedelta(days=…)` are exact. Arithmetic flows through the
/// `__add__` / `__sub__` dunders the foundation dispatches on instances.
const DATETIME_SRC: &str = r#"
def _is_leap(y):
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)

def _days_in_month(y, m):
    if m == 2:
        if _is_leap(y):
            return 29
        return 28
    if m == 1 or m == 3 or m == 5 or m == 7 or m == 8 or m == 10 or m == 12:
        return 31
    return 30

def _to_ordinal(y, m, d):
    n = 0
    yy = 1
    while yy < y:
        if _is_leap(yy):
            n = n + 366
        else:
            n = n + 365
        yy = yy + 1
    mm = 1
    while mm < m:
        n = n + _days_in_month(y, mm)
        mm = mm + 1
    return n + d

def _from_ordinal(n):
    y = 1
    while True:
        if _is_leap(y):
            ydays = 366
        else:
            ydays = 365
        if n > ydays:
            n = n - ydays
            y = y + 1
        else:
            break
    m = 1
    while n > _days_in_month(y, m):
        n = n - _days_in_month(y, m)
        m = m + 1
    return (y, m, n)

def _pad2(n):
    s = str(n)
    if len(s) < 2:
        return "0" + s
    return s

class timedelta:
    def __init__(self, days=0, seconds=0, minutes=0, hours=0, weeks=0):
        self.days = days + weeks * 7
        self.seconds = seconds + minutes * 60 + hours * 3600
    def total_seconds(self):
        return self.days * 86400 + self.seconds
    def __repr__(self):
        return "datetime.timedelta(days=" + str(self.days) + ")"

class date:
    def __init__(self, year, month, day):
        self.year = year
        self.month = month
        self.day = day
    def _ordinal(self):
        return _to_ordinal(self.year, self.month, self.day)
    def isoformat(self):
        return str(self.year) + "-" + _pad2(self.month) + "-" + _pad2(self.day)
    def __sub__(self, other):
        return timedelta(days=self._ordinal() - other._ordinal())
    def __add__(self, other):
        ymd = _from_ordinal(self._ordinal() + other.days)
        return date(ymd[0], ymd[1], ymd[2])
    def __repr__(self):
        return "datetime.date(" + str(self.year) + ", " + str(self.month) + ", " + str(self.day) + ")"
    def __str__(self):
        return self.isoformat()

class datetime:
    def __init__(self, year, month, day, hour=0, minute=0, second=0, microsecond=0):
        self.year = year
        self.month = month
        self.day = day
        self.hour = hour
        self.minute = minute
        self.second = second
        self.microsecond = microsecond
    def date(self):
        return date(self.year, self.month, self.day)
    def _ordinal(self):
        return _to_ordinal(self.year, self.month, self.day)
    def isoformat(self):
        return (str(self.year) + "-" + _pad2(self.month) + "-" + _pad2(self.day)
                + "T" + _pad2(self.hour) + ":" + _pad2(self.minute) + ":" + _pad2(self.second))
    def __add__(self, other):
        total_secs = self.hour * 3600 + self.minute * 60 + self.second + other.seconds
        extra_days = total_secs // 86400
        rem = total_secs % 86400
        ymd = _from_ordinal(self._ordinal() + other.days + extra_days)
        return datetime(ymd[0], ymd[1], ymd[2], rem // 3600, (rem % 3600) // 60, rem % 60)
    def __sub__(self, other):
        d = self._ordinal() - other._ordinal()
        s = (self.hour * 3600 + self.minute * 60 + self.second
             - other.hour * 3600 - other.minute * 60 - other.second)
        return timedelta(days=d, seconds=s)
    def __repr__(self):
        return ("datetime.datetime(" + str(self.year) + ", " + str(self.month) + ", "
                + str(self.day) + ", " + str(self.hour) + ", " + str(self.minute) + ")")
"#;

fn make_datetime_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    let members = compile_helpers(interp, DATETIME_SRC)?;
    let wanted = ["date", "datetime", "timedelta"];
    let entries: Vec<(&str, Value)> = wanted
        .iter()
        .filter_map(|&n| {
            members
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| (n, v.clone()))
        })
        .collect();
    Ok(make_module("datetime", entries))
}

// ── Module resolution ──────────────────────────────────────────────────────

pub fn resolve_module(interp: &mut Interpreter, name: &str) -> Result<Value, Unwind> {
    match name {
        "typhon_runtime" => Ok(make_typhon_runtime_module(interp)),
        "math" => Ok(make_math_module()),
        "os" | "os.path" => Ok(make_os_module()),
        "sys" => Ok(make_sys_module(interp)),
        "json" => Ok(make_json_module()),
        "time" => Ok(make_time_module()),
        "random" => Ok(make_random_module()),
        "typing" => Ok(make_typing_module()),
        "re" => Ok(make_re_module()),
        "collections" => Ok(make_collections_module()),
        // `from collections.abc import Callable / Iterator / ...` — the
        // canonical home for the abstract container types. Annotation-only
        // at runtime, so identity natives (mirroring the `typing` shim)
        // are all the VM needs.
        "collections.abc" => Ok(make_collections_abc_module()),
        // Cooperative (sequential) asyncio: coroutines are thunks forced at
        // await points, tasks complete at creation, and Queue.get on an
        // empty queue fails loudly instead of deadlocking. Programs whose
        // CORRECTNESS depends on interleaving need `tyc run --compile`.
        "asyncio" => Ok(make_asyncio_module()),
        "functools" => Ok(make_functools_module()),
        "itertools" => Ok(make_itertools_module()),
        "dataclasses" => Ok(make_dataclasses_module()),
        "pathlib" => make_pathlib_module(interp),
        "datetime" => make_datetime_module(interp),
        "heapq" => Ok(make_heapq_module()),
        "contextlib" => Ok(make_contextlib_module()),
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
        _ => Err(crate::error::Unwind::Exception(
            crate::error::VmException::new(
                "ImportError",
                format!(
                    "tyc-vm cannot import '{name}': only a small native stdlib is available. \
                     Run with `tyc run --compile` to use the full Python interpreter."
                ),
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
                    let f = args.into_iter().next().unwrap_or(Value::None);
                    i.call_value(f, vec![], &[])
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
            Value::Int(i) => Ok(i.clone()),
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
                    Ok(Value::Float(single(&args, "sqrt")?.to_float()?.sqrt()))
                }),
            ),
            (
                "floor",
                nf("floor", |_i, args| {
                    Ok(Value::Int(num_bigint::BigInt::from(
                        single(&args, "floor")?.to_float()?.floor() as i64,
                    )))
                }),
            ),
            (
                "ceil",
                nf("ceil", |_i, args| {
                    Ok(Value::Int(num_bigint::BigInt::from(
                        single(&args, "ceil")?.to_float()?.ceil() as i64,
                    )))
                }),
            ),
            (
                "trunc",
                nf("trunc", |_i, args| {
                    // Returns an int, consistent with CPython math.trunc.
                    Ok(Value::Int(num_bigint::BigInt::from(
                        single(&args, "trunc")?.to_float()?.trunc() as i64,
                    )))
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
                    let mut acc = Value::Int(num_bigint::BigInt::from(1));
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
                    let x = args
                        .first()
                        .ok_or_else(|| type_error("log() needs an arg"))?
                        .to_float()?;
                    let base = match args.get(1) {
                        Some(b) => b.to_float()?,
                        None => std::f64::consts::E,
                    };
                    Ok(Value::Float(x.log(base)))
                }),
            ),
            (
                "log2",
                nf("log2", |_i, args| {
                    Ok(Value::Float(single(&args, "log2")?.to_float()?.log2()))
                }),
            ),
            (
                "log10",
                nf("log10", |_i, args| {
                    Ok(Value::Float(single(&args, "log10")?.to_float()?.log10()))
                }),
            ),
            (
                "expm1",
                nf("expm1", |_i, args| {
                    // exp(x) - 1 with better precision near 0, matching
                    // CPython math.expm1.
                    Ok(Value::Float(single(&args, "expm1")?.to_float()?.exp_m1()))
                }),
            ),
            (
                "log1p",
                nf("log1p", |_i, args| {
                    // log(1 + x) with better precision near 0, matching
                    // CPython math.log1p.
                    Ok(Value::Float(single(&args, "log1p")?.to_float()?.ln_1p()))
                }),
            ),
            (
                "exp",
                nf("exp", |_i, args| {
                    Ok(Value::Float(single(&args, "exp")?.to_float()?.exp()))
                }),
            ),
            // ── trig ──────────────────────────────────────────────────────────
            (
                "sin",
                nf("sin", |_i, args| {
                    Ok(Value::Float(single(&args, "sin")?.to_float()?.sin()))
                }),
            ),
            (
                "cos",
                nf("cos", |_i, args| {
                    Ok(Value::Float(single(&args, "cos")?.to_float()?.cos()))
                }),
            ),
            (
                "tan",
                nf("tan", |_i, args| {
                    Ok(Value::Float(single(&args, "tan")?.to_float()?.tan()))
                }),
            ),
            (
                "asin",
                nf("asin", |_i, args| {
                    Ok(Value::Float(single(&args, "asin")?.to_float()?.asin()))
                }),
            ),
            (
                "acos",
                nf("acos", |_i, args| {
                    Ok(Value::Float(single(&args, "acos")?.to_float()?.acos()))
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
                    let a = args
                        .first()
                        .ok_or_else(|| type_error("pow() needs args"))?
                        .to_float()?;
                    let b = args
                        .get(1)
                        .ok_or_else(|| type_error("pow() needs args"))?
                        .to_float()?;
                    Ok(Value::Float(a.powf(b)))
                }),
            ),
            // ── integer-domain (return int) ───────────────────────────────────
            (
                "gcd",
                nf("gcd", |_i, args| {
                    if args.is_empty() {
                        return Ok(Value::Int(num_bigint::BigInt::from(0)));
                    }
                    let mut acc = require_int(&args[0], "gcd")?;
                    for v in &args[1..] {
                        acc = bigint_gcd(acc, require_int(v, "gcd")?);
                    }
                    Ok(Value::Int(acc))
                }),
            ),
            (
                "lcm",
                nf("lcm", |_i, args| {
                    use num_traits::{Signed, Zero};
                    if args.is_empty() {
                        return Ok(Value::Int(num_bigint::BigInt::from(1)));
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
                    Ok(Value::Int(acc.abs()))
                }),
            ),
            (
                "factorial",
                nf("factorial", |_i, args| {
                    let n = require_int(single(&args, "factorial")?, "factorial")?;
                    Ok(Value::Int(bigint_factorial(&n)?))
                }),
            ),
            (
                "isqrt",
                nf("isqrt", |_i, args| {
                    let n = require_int(single(&args, "isqrt")?, "isqrt")?;
                    Ok(Value::Int(bigint_isqrt(&n)?))
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
                    Ok(Value::Int(bigint_comb_full(n, k)?))
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
                    Ok(Value::Int(bigint_perm(n, k)?))
                }),
            ),
        ],
    )
}

fn make_os_module() -> Value {
    let environ = nf("environ", |_i, _args| Ok(Value::None));
    let _ = environ;
    let env_dict = {
        let mut m: DictMap = IndexMap::new();
        for (k, v) in std::env::vars() {
            m.insert(HashKey::Str(Rc::new(k)), Value::Str(Rc::new(v)));
        }
        Value::Dict(Rc::new(RefCell::new(m)))
    };
    make_module(
        "os",
        vec![
            (
                "getenv",
                nf("getenv", |_i, args| {
                    let key = single(&args, "getenv")?.py_str();
                    Ok(std::env::var(&key)
                        .map(|v| Value::Str(Rc::new(v)))
                        .unwrap_or_else(|_| args.get(1).cloned().unwrap_or(Value::None)))
                }),
            ),
            ("environ", env_dict),
            (
                "path",
                make_module(
                    "os.path",
                    vec![
                        (
                            "exists",
                            nf("exists", |_i, args| {
                                let path = single(&args, "exists")?.py_str();
                                Ok(Value::Bool(std::path::Path::new(&path).exists()))
                            }),
                        ),
                        (
                            "isfile",
                            nf("isfile", |_i, args| {
                                let path = single(&args, "isfile")?.py_str();
                                Ok(Value::Bool(std::path::Path::new(&path).is_file()))
                            }),
                        ),
                        (
                            "isdir",
                            nf("isdir", |_i, args| {
                                let path = single(&args, "isdir")?.py_str();
                                Ok(Value::Bool(std::path::Path::new(&path).is_dir()))
                            }),
                        ),
                        (
                            "join",
                            nf("join", |_i, args| {
                                let mut buf = std::path::PathBuf::new();
                                for a in &args {
                                    buf.push(a.py_str());
                                }
                                Ok(Value::Str(Rc::new(buf.to_string_lossy().into_owned())))
                            }),
                        ),
                        (
                            "basename",
                            nf("basename", |_i, args| {
                                let path = single(&args, "basename")?.py_str();
                                let name = std::path::Path::new(&path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                Ok(Value::Str(Rc::new(name)))
                            }),
                        ),
                        (
                            "dirname",
                            nf("dirname", |_i, args| {
                                let path = single(&args, "dirname")?.py_str();
                                let dir = std::path::Path::new(&path)
                                    .parent()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                Ok(Value::Str(Rc::new(dir)))
                            }),
                        ),
                    ],
                ),
            ),
            (
                "getcwd",
                nf("getcwd", |_i, _args| {
                    let cwd = std::env::current_dir()
                        .map_err(|e| os_error(format!("getcwd failed: {e}")))?;
                    Ok(Value::Str(Rc::new(cwd.to_string_lossy().into_owned())))
                }),
            ),
            (
                "remove",
                nf("remove", |_i, args| {
                    let path = single(&args, "remove")?.py_str();
                    std::fs::remove_file(&path).map_err(|e| fs_unwind(&path, e))?;
                    Ok(Value::None)
                }),
            ),
            (
                "unlink",
                nf("unlink", |_i, args| {
                    let path = single(&args, "unlink")?.py_str();
                    std::fs::remove_file(&path).map_err(|e| fs_unwind(&path, e))?;
                    Ok(Value::None)
                }),
            ),
            (
                "rmdir",
                nf("rmdir", |_i, args| {
                    let path = single(&args, "rmdir")?.py_str();
                    std::fs::remove_dir(&path).map_err(|e| fs_unwind(&path, e))?;
                    Ok(Value::None)
                }),
            ),
            (
                "mkdir",
                nf("mkdir", |_i, args| {
                    let path = single(&args, "mkdir")?.py_str();
                    std::fs::create_dir(&path).map_err(|e| fs_unwind(&path, e))?;
                    Ok(Value::None)
                }),
            ),
            (
                "makedirs",
                nf("makedirs", |_i, args| {
                    // Kwargs arrive via the sentinel forwarded by
                    // `call_with_kwargs` ("makedirs" arm).
                    let (pos, kw) = split_kwargs(&args);
                    let path = pos
                        .first()
                        .ok_or_else(|| type_error("makedirs() needs a path"))?
                        .py_str();
                    let exist_ok = kw
                        .iter()
                        .find(|(k, _)| k == "exist_ok")
                        .map(|(_, v)| v.truthy())
                        // Positional form: makedirs(path, mode, exist_ok).
                        .or_else(|| pos.get(2).map(|v| v.truthy()))
                        .unwrap_or(false);
                    // CPython raises FileExistsError for an existing leaf
                    // unless exist_ok=True.
                    if !exist_ok && std::path::Path::new(&path).exists() {
                        return Err(fs_unwind(
                            &path,
                            std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                        ));
                    }
                    std::fs::create_dir_all(&path).map_err(|e| fs_unwind(&path, e))?;
                    Ok(Value::None)
                }),
            ),
            (
                "rename",
                nf("rename", |_i, args| {
                    let src = args
                        .first()
                        .ok_or_else(|| type_error("rename() needs src and dst"))?
                        .py_str();
                    let dst = args
                        .get(1)
                        .ok_or_else(|| type_error("rename() needs src and dst"))?
                        .py_str();
                    std::fs::rename(&src, &dst).map_err(|e| fs_unwind(&src, e))?;
                    Ok(Value::None)
                }),
            ),
            (
                "listdir",
                nf("listdir", |_i, args| {
                    let path = args
                        .first()
                        .map(|v| v.py_str())
                        .unwrap_or_else(|| ".".to_owned());
                    let mut names: Vec<Value> = Vec::new();
                    for entry in std::fs::read_dir(&path).map_err(|e| fs_unwind(&path, e))? {
                        let entry = entry.map_err(|e| fs_unwind(&path, e))?;
                        names.push(Value::Str(Rc::new(
                            entry.file_name().to_string_lossy().into_owned(),
                        )));
                    }
                    Ok(Value::List(Rc::new(RefCell::new(names))))
                }),
            ),
        ],
    )
}

/// Map an `std::io::Error` from a filesystem builtin onto the matching
/// Python exception type (`FileNotFoundError` / `PermissionError` /
/// `FileExistsError` / generic `OSError`) so user `except` arms narrow the
/// same way they do under CPython.
fn fs_unwind(path: &str, e: std::io::Error) -> Unwind {
    use std::io::ErrorKind;
    let kind = match e.kind() {
        ErrorKind::NotFound => "FileNotFoundError",
        ErrorKind::PermissionDenied => "PermissionError",
        ErrorKind::AlreadyExists => "FileExistsError",
        _ => "OSError",
    };
    Unwind::Exception(crate::error::VmException::new(
        kind,
        format!("{e}: '{path}'"),
    ))
}

fn os_error(msg: String) -> Unwind {
    Unwind::Exception(crate::error::VmException::new("OSError", msg))
}

/// fnmatch-style single-component wildcard match: `*` (any run) and `?`
/// (single char). Backs the VM `Path.glob` shim.
fn glob_match(pat: &str, name: &str) -> bool {
    fn inner(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => {
                // `*` matches empty, or consumes one char of the name.
                inner(&p[1..], n) || (!n.is_empty() && inner(p, &n[1..]))
            }
            (Some('?'), Some(_)) => inner(&p[1..], &n[1..]),
            (Some(pc), Some(nc)) if pc == nc => inner(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    inner(&p, &n)
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
                nf("exit", |_i, args| {
                    let code = args.first().map(|v| v.to_int().unwrap_or(0)).unwrap_or(0);
                    std::process::exit(code as i32);
                }),
            ),
            ("stdout", make_std_stream("sys.stdout", false)),
            ("stderr", make_std_stream("sys.stderr", true)),
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
                    Ok(Value::Int(num_bigint::BigInt::from(
                        text.chars().count() as i64
                    )))
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
                    Ok(Value::Str(Rc::new(json_dumps(
                        single(&args, "dumps")?,
                        false,
                    ))))
                }),
            ),
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
                    let serialised = json_dumps(&args[0], false);
                    let fp = args[1].clone();
                    let write = interp.get_attr(&fp, "write")?;
                    interp.call_value(write, vec![Value::Str(Rc::new(serialised))], &[])?;
                    Ok(Value::None)
                }),
            ),
        ],
    )
}

fn make_time_module() -> Value {
    make_module(
        "time",
        vec![
            (
                "time",
                nf("time", |_i, _args| {
                    let t = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);
                    Ok(Value::Float(t))
                }),
            ),
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
    )
}

/// Seconds since the first call (a fixed reference point), so `monotonic`,
/// `perf_counter`, and `process_time` return increasing values across calls.
fn monotonic_secs() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn make_random_module() -> Value {
    use std::cell::RefCell;
    // CPython-compatible MT19937 so seeded programs produce IDENTICAL
    // sequences under `tyc run` and `tyc build && python` — random(),
    // getrandbits/_randbelow (which back randint / randrange / choice /
    // shuffle / sample), uniform, and gauss all follow CPython's
    // random.py / _randommodule.c to the letter.
    struct Mt19937 {
        mt: [u32; 624],
        index: usize,
        gauss_next: Option<f64>,
    }
    impl Mt19937 {
        fn new() -> Self {
            let mut s = Self {
                mt: [0u32; 624],
                index: 625,
                gauss_next: None,
            };
            // CPython seeds from urandom by default; any fixed default is
            // fine for unseeded use — reproducibility only matters after
            // an explicit seed().
            s.init_genrand(5489);
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
        /// `getrandbits(k)` for k <= 64 (covers every stdlib consumer the
        /// VM models; Python's small-int fast path uses the same word
        /// order).
        fn getrandbits(&mut self, k: u32) -> u64 {
            if k == 0 {
                return 0;
            }
            if k <= 32 {
                return (self.genrand_u32() >> (32 - k)) as u64;
            }
            // Little-endian words, last word truncated — matches
            // _random.Random.getrandbits for multi-word sizes.
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
            let mut r = self.getrandbits(k);
            while r >= n {
                r = self.getrandbits(k);
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
            self.gauss_next = None;
        }
    }
    thread_local! {
        static MT: RefCell<Mt19937> = RefCell::new(Mt19937::new());
    }
    fn with_mt<R>(f: impl FnOnce(&mut Mt19937) -> R) -> R {
        MT.with(|m| f(&mut m.borrow_mut()))
    }
    make_module(
        "random",
        vec![
            (
                "random",
                nf("random", |_i, _args| {
                    Ok(Value::Float(with_mt(|m| m.random())))
                }),
            ),
            (
                "seed",
                nf("seed", |_i, args| {
                    match args.first() {
                        Some(Value::Int(n)) => with_mt(|m| m.seed_int(n)),
                        Some(Value::Bool(b)) => {
                            with_mt(|m| m.seed_int(&num_bigint::BigInt::from(*b as i64)))
                        }
                        // CPython's no-arg / None form seeds from OS
                        // entropy — non-deterministic by design. A wall-
                        // clock-derived seed preserves that property.
                        Some(Value::None) | None => {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_nanos() as u64)
                                .unwrap_or(0);
                            with_mt(|m| m.seed_int(&num_bigint::BigInt::from(now)));
                        }
                        // str / bytes / float seeds hash through SHA-512
                        // in CPython — silently mapping them to a fixed
                        // seed would LOOK deterministic while diverging.
                        // Fail loudly instead.
                        Some(other) => {
                            return Err(type_error(format!(
                                "VM random.seed() supports int seeds only (got {}) — \
                                 use `tyc run --compile` for str/bytes/float seeding",
                                other.type_name()
                            )))
                        }
                    }
                    Ok(Value::None)
                }),
            ),
            (
                "getrandbits",
                nf("getrandbits", |_i, args| {
                    let k = args
                        .first()
                        .ok_or_else(|| type_error("getrandbits() needs k"))?
                        .to_int()? as u32;
                    if k > 64 {
                        return Err(value_error(
                            "VM getrandbits() supports k <= 64 — use `tyc run --compile`",
                        ));
                    }
                    Ok(Value::Int(num_bigint::BigInt::from(with_mt(|m| {
                        m.getrandbits(k)
                    }))))
                }),
            ),
            (
                "randint",
                nf("randint", |_i, args| {
                    let a = args
                        .first()
                        .ok_or_else(|| type_error("randint() needs args"))?
                        .to_int()?;
                    let b = args
                        .get(1)
                        .ok_or_else(|| type_error("randint() needs args"))?
                        .to_int()?;
                    if b < a {
                        return Err(value_error("randint(a, b): b must be >= a"));
                    }
                    let span = (b - a + 1) as u64;
                    let pick = with_mt(|m| m.randbelow(span)) as i64;
                    Ok(Value::Int(num_bigint::BigInt::from(a + pick)))
                }),
            ),
            (
                "randrange",
                nf("randrange", |_i, args| {
                    let (start, stop, step) = match args.len() {
                        1 => (0, args[0].to_int()?, 1),
                        2 => (args[0].to_int()?, args[1].to_int()?, 1),
                        3 => (args[0].to_int()?, args[1].to_int()?, args[2].to_int()?),
                        _ => return Err(type_error("randrange() takes 1-3 arguments")),
                    };
                    if step == 0 {
                        return Err(value_error("zero step for randrange()"));
                    }
                    // CPython supports descending ranges (negative step).
                    let width = stop - start;
                    let n = if step > 0 {
                        if width <= 0 {
                            0
                        } else {
                            (width + step - 1) / step
                        }
                    } else if width >= 0 {
                        0
                    } else {
                        (width + step + 1) / step
                    };
                    if n <= 0 {
                        return Err(value_error("empty range for randrange()"));
                    }
                    let pick = with_mt(|m| m.randbelow(n as u64)) as i64;
                    Ok(Value::Int(num_bigint::BigInt::from(start + pick * step)))
                }),
            ),
            (
                "uniform",
                nf("uniform", |_i, args| {
                    let a = args
                        .first()
                        .ok_or_else(|| type_error("uniform() needs 2 args"))?
                        .to_float()?;
                    let b = args
                        .get(1)
                        .ok_or_else(|| type_error("uniform() needs 2 args"))?
                        .to_float()?;
                    let t = with_mt(|m| m.random());
                    Ok(Value::Float(a + t * (b - a)))
                }),
            ),
            (
                "gauss",
                nf("gauss", |_i, args| {
                    // Exactly CPython's random.py gauss(): a cached
                    // sin/cos pair per two draws.
                    let mu = args
                        .first()
                        .map(|v| v.to_float())
                        .transpose()?
                        .unwrap_or(0.0);
                    let sigma = args
                        .get(1)
                        .map(|v| v.to_float())
                        .transpose()?
                        .unwrap_or(1.0);
                    let z = with_mt(|m| {
                        if let Some(z) = m.gauss_next.take() {
                            z
                        } else {
                            let x2pi = m.random() * std::f64::consts::TAU;
                            let g2rad = (-2.0 * (1.0 - m.random()).ln()).sqrt();
                            let z = x2pi.cos() * g2rad;
                            m.gauss_next = Some(x2pi.sin() * g2rad);
                            z
                        }
                    });
                    Ok(Value::Float(mu + z * sigma))
                }),
            ),
            (
                "choice",
                nf("choice", |interp, args| {
                    let seq = args
                        .first()
                        .ok_or_else(|| type_error("choice() needs a sequence"))?;
                    let items: Vec<Value> = {
                        let it = interp.make_iter(seq.clone())?;
                        let mut out = Vec::new();
                        while let Some(v) = interp.iter_next(&it)? {
                            out.push(v);
                        }
                        out
                    };
                    if items.is_empty() {
                        return Err(Unwind::Exception(crate::error::VmException::new(
                            "IndexError",
                            "Cannot choose from an empty sequence".to_owned(),
                        )));
                    }
                    let idx = with_mt(|m| m.randbelow(items.len() as u64)) as usize;
                    Ok(items[idx].clone())
                }),
            ),
            (
                "shuffle",
                nf("shuffle", |_i, args| {
                    let lst = match args.first() {
                        Some(Value::List(l)) => l.clone(),
                        _ => return Err(type_error("shuffle() needs a list")),
                    };
                    let mut items = lst.borrow_mut();
                    // CPython: for i in reversed(range(1, len(x))).
                    let n = items.len();
                    for i in (1..n).rev() {
                        let j = with_mt(|m| m.randbelow(i as u64 + 1)) as usize;
                        items.swap(i, j);
                    }
                    Ok(Value::None)
                }),
            ),
            (
                "sample",
                nf("sample", |interp, args| {
                    let seq = args
                        .first()
                        .ok_or_else(|| type_error("sample() needs a sequence"))?;
                    let k_raw = args
                        .get(1)
                        .ok_or_else(|| type_error("sample() needs k"))?
                        .to_int()?;
                    if k_raw < 0 {
                        return Err(value_error("Sample larger than population or is negative"));
                    }
                    let k = k_raw as usize;
                    let population: Vec<Value> = {
                        let it = interp.make_iter(seq.clone())?;
                        let mut out = Vec::new();
                        while let Some(v) = interp.iter_next(&it)? {
                            out.push(v);
                        }
                        out
                    };
                    let n = population.len();
                    if k > n {
                        return Err(value_error("Sample larger than population or is negative"));
                    }
                    // CPython's selection-set vs pool heuristic, verbatim,
                    // so seeded sequences match exactly.
                    let mut result: Vec<Value> = Vec::with_capacity(k);
                    let mut setsize: usize = 21;
                    if k > 5 {
                        // CPython: `setsize += 4 ** _ceil(_log(k * 3, 4))`
                        // — i.e. log base 4 of (3*k), not log base 3 of k.
                        // Getting this exact keeps the pool-vs-selection-set
                        // branch (and thus the MT19937 draw sequence) aligned
                        // with CPython for seeded `random.sample`.
                        setsize += 4.0f64.powf(((k * 3) as f64).log(4.0).ceil()) as usize;
                    }
                    if n <= setsize {
                        let mut pool = population.clone();
                        for i in 0..k {
                            let j = with_mt(|m| m.randbelow((n - i) as u64)) as usize;
                            result.push(pool[j].clone());
                            pool[j] = pool[n - i - 1].clone();
                        }
                    } else {
                        let mut selected: std::collections::HashSet<usize> =
                            std::collections::HashSet::new();
                        for _ in 0..k {
                            let mut j = with_mt(|m| m.randbelow(n as u64)) as usize;
                            while selected.contains(&j) {
                                j = with_mt(|m| m.randbelow(n as u64)) as usize;
                            }
                            selected.insert(j);
                            result.push(population[j].clone());
                        }
                    }
                    Ok(Value::List(Rc::new(RefCell::new(result))))
                }),
            ),
        ],
    )
}

// ── stdlib shims ───────────────────────────────────────────────────────────
//
// FINDINGS #25/#27. Most non-trivial Typhon programs reference the typing /
// collections / functools / itertools / pathlib / re modules even when only
// used as static annotations or for a handful of helpers. Compile mode
// resolves these through CPython, but VM mode never bridges to CPython, so
// every `from typing import …` line used to crash the VM with a hard
// ImportError. The shims below give the VM partial native coverage so the
// common-case program runs end-to-end. Each module documents what is
// implemented; anything not listed will still raise AttributeError on use.
//
// The shims are intentionally minimal — they cover the surface that
// users actually reach for in the v0.7.x stress sweep, not the full
// stdlib API.

/// Identity-callable. Used by typing shims (`Callable`, `Optional`, …)
/// where the only operation Typhon programs perform at runtime is
/// subscription (`Optional[int]`) and occasional invocation.
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
        let mut attrs: HashMap<String, Value> = HashMap::new();
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
                let hits: Vec<Value> = p3
                    .find_iter(&s)
                    .map(|m| Value::Str(Rc::new(m.as_str().to_owned())))
                    .collect();
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
                    Value::Int(num_bigint::BigInt::from(n as i64)),
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
        });
        Value::Instance(Rc::new(crate::value::Instance {
            class: cls,
            fields: RefCell::new(attrs),
        }))
    }
    // Build a match object from a `regex::Captures`. Group 0 is the whole
    // match; groups 1.. are the capture groups. Non-participating optional
    // groups are represented as `None`.
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
        let mut attrs: HashMap<String, Value> = HashMap::new();
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
                Ok(Value::Int(num_bigint::BigInt::from(start)))
            }))),
        );
        attrs.insert(
            "end".into(),
            Value::Native(Rc::new(NativeFn::new("end", move |_i, _args| {
                Ok(Value::Int(num_bigint::BigInt::from(end)))
            }))),
        );
        attrs.insert(
            "span".into(),
            Value::Native(Rc::new(NativeFn::new("span", move |_i, _args| {
                Ok(Value::Tuple(Rc::new(vec![
                    Value::Int(num_bigint::BigInt::from(start)),
                    Value::Int(num_bigint::BigInt::from(end)),
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
        });
        Value::Instance(Rc::new(crate::value::Instance {
            class: cls,
            fields: RefCell::new(attrs),
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
                        Value::Int(num_bigint::BigInt::from(n as i64)),
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
                    let hits: Vec<Value> = r
                        .find_iter(&s)
                        .map(|m| Value::Str(Rc::new(m.as_str().to_owned())))
                        .collect();
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
            ("IGNORECASE", Value::Int(num_bigint::BigInt::from(2))),
            ("MULTILINE", Value::Int(num_bigint::BigInt::from(8))),
            ("DOTALL", Value::Int(num_bigint::BigInt::from(16))),
            ("VERBOSE", Value::Int(num_bigint::BigInt::from(64))),
            ("ASCII", Value::Int(num_bigint::BigInt::from(256))),
        ],
    )
}

/// `collections` shim.
///
/// Implemented: `OrderedDict` (alias to `dict`), `defaultdict` (no auto-
/// default behaviour — same as plain `dict`), `Counter` (returns a dict
/// of counts), `namedtuple` (returns a callable that builds a tuple).
/// `deque` is not implemented; users hitting that case should fall back
/// to `tyc run --compile`.
/// A completed-task wrapper: `TaskGroup.create_task` / `spawn` force the
/// coroutine immediately (sequential semantics) and hand back this module
/// so `.result()` / `await task` recover the value.
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
                Value::Native(Rc::new(NativeFn::new("__aexit__", |_i, _args| {
                    Ok(Value::Bool(false))
                }))),
            );
            members.insert(
                "create_task".to_owned(),
                Value::Native(Rc::new(NativeFn::new("create_task", |i, args| {
                    let coro = args
                        .into_iter()
                        .next()
                        .ok_or_else(|| type_error("create_task() requires a coroutine"))?;
                    let result = i.force_awaitable(coro)?;
                    Ok(make_task_value(result))
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
    make_module(
        "asyncio",
        vec![
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
                Ok(Value::Int(
                    num_bigint::BigInt::from(b.borrow().len() as i64),
                ))
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

fn make_collections_module() -> Value {
    let counter = nf("Counter", |i, args| {
        let mut counts: DictMap = IndexMap::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(x) = i.iter_next(&it)? {
                let key = x.to_hash_key()?;
                let entry = counts
                    .entry(key)
                    .or_insert(Value::Int(num_bigint::BigInt::from(0)));
                *entry = match entry {
                    Value::Int(n) => Value::Int(&*n + 1),
                    _ => Value::Int(num_bigint::BigInt::from(1)),
                };
            }
        }
        Ok(Value::Dict(Rc::new(RefCell::new(counts))))
    });
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
    let ordered_dict = nf("OrderedDict", |i, args| {
        // CPython's `dict` preserves insertion order since 3.7, so this
        // is a true alias for the v1 shim (FINDINGS #18 — the VM itself
        // now backs dicts with IndexMap so iteration order matches).
        let mut m: DictMap = IndexMap::new();
        if let Some(v) = args.into_iter().next() {
            let it = i.make_iter(v)?;
            while let Some(pair) = i.iter_next(&it)? {
                if let Value::Tuple(t) = pair {
                    if t.len() == 2 {
                        m.insert(t[0].to_hash_key()?, t[1].clone());
                    }
                }
            }
        }
        Ok(Value::Dict(Rc::new(RefCell::new(m))))
    });
    // `namedtuple(name, fields)` returns a constructor that accepts the
    // field values positionally or by keyword and returns a plain tuple.
    // Attribute access on the result is not supported in this shim; the
    // tuple shape is enough for most arithmetic / unpacking use sites.
    let namedtuple = nf("namedtuple", |_i, args| {
        let _name = args
            .first()
            .ok_or_else(|| type_error("namedtuple() needs a name"))?;
        let fields = args
            .get(1)
            .ok_or_else(|| type_error("namedtuple() needs fields"))?
            .clone();
        let ctor = NativeFn::new("namedtuple_ctor", move |_i, mut call_args| {
            // Pull field count from the captured `fields` argument.
            let count = match &fields {
                Value::Str(s) => s
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|p| !p.is_empty())
                    .count(),
                Value::List(l) => l.borrow().len(),
                Value::Tuple(t) => t.len(),
                _ => 0,
            };
            // Pad with None up to the declared field count.
            while call_args.len() < count {
                call_args.push(Value::None);
            }
            Ok(Value::Tuple(Rc::new(call_args)))
        });
        Ok(Value::Native(Rc::new(ctor)))
    });
    // `deque([iterable])` — the VM exposes deques as plain lists, since
    // all the methods we shim (append, appendleft, pop, popleft, extend)
    // map cleanly onto list operations. This is `O(n)` for the *left*
    // variants instead of the `O(1)` CPython gives, but functional
    // equivalence is preserved.
    let deque = nf("deque", |i, args| {
        let mut out: Vec<Value> = Vec::new();
        if let Some(v) = args.into_iter().next() {
            if !matches!(v, Value::None) {
                let it = i.make_iter(v)?;
                while let Some(x) = i.iter_next(&it)? {
                    out.push(x);
                }
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    make_module(
        "collections",
        vec![
            ("OrderedDict", ordered_dict),
            ("abc", make_collections_abc_module()),
            ("defaultdict", defaultdict),
            ("Counter", counter),
            ("namedtuple", namedtuple),
            ("deque", deque),
        ],
    )
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
            (Value::Int(x), Value::Float(y)) => {
                num_traits::ToPrimitive::to_f64(x).is_some_and(|xf| xf < *y)
            }
            (Value::Float(x), Value::Int(y)) => {
                num_traits::ToPrimitive::to_f64(y).is_some_and(|yf| *x < yf)
            }
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
            let mut new_fields: HashMap<String, Value> = HashMap::new();
            for (k, val) in inst.fields.borrow().iter() {
                new_fields.insert(k.clone(), deep_freeze_value(val.clone())?);
            }
            Ok(Value::Instance(Rc::new(crate::value::Instance {
                class: inst.class.clone(),
                fields: RefCell::new(new_fields),
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
fn make_contextlib_module() -> Value {
    let identity = |name: &'static str| {
        nf(name, |_i, args| {
            Ok(args.into_iter().next().unwrap_or(Value::None))
        })
    };
    make_module(
        "contextlib",
        vec![
            ("contextmanager", identity("contextmanager")),
            ("asynccontextmanager", identity("asynccontextmanager")),
        ],
    )
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
fn make_functools_module() -> Value {
    fn make_cache(_i: &mut Interpreter, args: Vec<Value>) -> Result<Value, Unwind> {
        let inner = args.into_iter().next().unwrap_or(Value::None);
        let cache: Rc<RefCell<HashMap<HashKey, Value>>> = Rc::new(RefCell::new(HashMap::new()));
        Ok(Value::Native(Rc::new(NativeFn::new(
            "memo",
            move |interp, call_args| {
                let mut keys = Vec::with_capacity(call_args.len());
                for a in &call_args {
                    keys.push(a.to_hash_key()?);
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
    let partial = nf("partial", |_i, mut args| {
        if args.is_empty() {
            return Err(type_error("partial() needs a callable"));
        }
        let func = args.remove(0);
        let captured = args;
        Ok(Value::Native(Rc::new(NativeFn::new(
            "partial_call",
            move |i, call_args| {
                let mut all = captured.clone();
                all.extend(call_args);
                i.call_value(func.clone(), all, &[])
            },
        ))))
    });
    // `cached_property` in VM mode: identity wrapper. The wrapped method
    // stays a method (callers use `obj.x()`). Documented in FINDINGS #26.
    let cached_property = nf("cached_property", |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    let wraps = nf("wraps", |_i, _args| {
        // Returns a decorator that's an identity function.
        Ok(Value::Native(Rc::new(NativeFn::new(
            "wraps_inner",
            |_i, args| Ok(args.into_iter().next().unwrap_or(Value::None)),
        ))))
    });
    make_module(
        "functools",
        vec![
            ("cache", cache_fn),
            ("lru_cache", lru_cache),
            ("reduce", reduce),
            ("partial", partial),
            ("cached_property", cached_property),
            ("wraps", wraps),
        ],
    )
}

/// `itertools` shim.
///
/// Implemented: `chain`, `repeat`, `count`, `cycle`, `accumulate`,
/// `islice`, `takewhile`, `dropwhile`. The remaining helpers
/// (`combinations`, `permutations`, `product`, `groupby`) return their
/// results as eager lists because the VM doesn't yet expose a generator
/// protocol — large inputs will materialise everything in memory.
fn make_itertools_module() -> Value {
    fn drain(i: &mut Interpreter, v: Value) -> Result<Vec<Value>, Unwind> {
        let it = i.make_iter(v)?;
        let mut out = Vec::new();
        while let Some(x) = i.iter_next(&it)? {
            out.push(x);
        }
        Ok(out)
    }
    let chain = nf("chain", |i, args| {
        let mut out = Vec::new();
        for a in args {
            out.extend(drain(i, a)?);
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let repeat = nf("repeat", |_i, args| {
        let v = args
            .first()
            .ok_or_else(|| type_error("repeat() needs an arg"))?
            .clone();
        let times = args
            .get(1)
            .and_then(|x| x.to_int().ok())
            .unwrap_or(0)
            .max(0);
        let out: Vec<Value> = (0..times).map(|_| v.clone()).collect();
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let count = nf("count", |_i, args| {
        // Eagerly materialises 1024 elements (open-ended iterators
        // aren't safe to expand without a bound).
        let start = args.first().and_then(|x| x.to_int().ok()).unwrap_or(0);
        let step = args.get(1).and_then(|x| x.to_int().ok()).unwrap_or(1);
        let out: Vec<Value> = (0..1024)
            .map(|n| Value::Int(num_bigint::BigInt::from(start + n * step)))
            .collect();
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let cycle = nf("cycle", |i, args| {
        // Materialises a fixed-size repetition (8x) so the VM doesn't
        // hang on an infinite iterator. Users that need true cycling
        // should fall back to compile mode.
        let v = args
            .into_iter()
            .next()
            .ok_or_else(|| type_error("cycle() needs an iterable"))?;
        let elems = drain(i, v)?;
        let mut out = Vec::with_capacity(elems.len() * 8);
        for _ in 0..8 {
            out.extend(elems.iter().cloned());
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let accumulate = nf("accumulate", |i, mut args| {
        if args.is_empty() {
            return Err(type_error("accumulate() needs an iterable"));
        }
        let iterable = args.remove(0);
        let func = args.into_iter().next();
        let xs = drain(i, iterable)?;
        let mut out = Vec::with_capacity(xs.len());
        let mut iter = xs.into_iter();
        if let Some(first) = iter.next() {
            out.push(first.clone());
            let mut acc = first;
            for x in iter {
                acc = match &func {
                    Some(f) => i.call_value(f.clone(), vec![acc, x], &[])?,
                    None => i.binop(&acc, ruff_python_ast::Operator::Add, &x)?,
                };
                out.push(acc.clone());
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let islice = nf("islice", |i, mut args| {
        if args.is_empty() {
            return Err(type_error("islice() needs an iterable"));
        }
        let iterable = args.remove(0);
        let xs = drain(i, iterable)?;
        let (start, stop) = match args.len() {
            1 => (0usize, args[0].to_int().unwrap_or(0).max(0) as usize),
            2 => (
                args[0].to_int().unwrap_or(0).max(0) as usize,
                args[1].to_int().unwrap_or(0).max(0) as usize,
            ),
            _ => (0, xs.len()),
        };
        let stop = stop.min(xs.len());
        let start = start.min(stop);
        let out: Vec<Value> = xs[start..stop].to_vec();
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let takewhile = nf("takewhile", |i, mut args| {
        if args.len() < 2 {
            return Err(type_error("takewhile() needs predicate, iterable"));
        }
        let pred = args.remove(0);
        let xs = drain(i, args.remove(0))?;
        let mut out = Vec::new();
        for x in xs {
            let keep = i.call_value(pred.clone(), vec![x.clone()], &[])?;
            if !keep.truthy() {
                break;
            }
            out.push(x);
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let dropwhile = nf("dropwhile", |i, mut args| {
        if args.len() < 2 {
            return Err(type_error("dropwhile() needs predicate, iterable"));
        }
        let pred = args.remove(0);
        let xs = drain(i, args.remove(0))?;
        let mut out = Vec::new();
        let mut dropping = true;
        for x in xs {
            if dropping {
                let keep = i.call_value(pred.clone(), vec![x.clone()], &[])?;
                if keep.truthy() {
                    continue;
                }
                dropping = false;
            }
            out.push(x);
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let combinations = nf("combinations", |i, mut args| {
        if args.len() < 2 {
            return Err(type_error("combinations() needs iterable, r"));
        }
        let xs = drain(i, args.remove(0))?;
        let r = args[0].to_int().unwrap_or(0).max(0) as usize;
        let mut out = Vec::new();
        let n = xs.len();
        if r <= n {
            let mut idx: Vec<usize> = (0..r).collect();
            loop {
                let tup: Vec<Value> = idx.iter().map(|&i| xs[i].clone()).collect();
                out.push(Value::Tuple(Rc::new(tup)));
                let mut i_pos = r;
                let mut done = true;
                while i_pos > 0 {
                    i_pos -= 1;
                    if idx[i_pos] != i_pos + n - r {
                        idx[i_pos] += 1;
                        for j in (i_pos + 1)..r {
                            idx[j] = idx[j - 1] + 1;
                        }
                        done = false;
                        break;
                    }
                }
                if done {
                    break;
                }
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let permutations = nf("permutations", |i, mut args| {
        if args.is_empty() {
            return Err(type_error("permutations() needs an iterable"));
        }
        let xs = drain(i, args.remove(0))?;
        let r = args
            .first()
            .and_then(|x| x.to_int().ok())
            .map(|n| n.max(0) as usize)
            .unwrap_or(xs.len());
        let n = xs.len();
        let mut out = Vec::new();
        if r <= n {
            let mut indices: Vec<usize> = (0..n).collect();
            let mut cycles: Vec<usize> = (0..r).map(|i| n - i).collect();
            out.push(Value::Tuple(Rc::new(
                indices[..r].iter().map(|&i| xs[i].clone()).collect(),
            )));
            'outer: loop {
                let mut i_pos = r;
                while i_pos > 0 {
                    i_pos -= 1;
                    cycles[i_pos] -= 1;
                    if cycles[i_pos] == 0 {
                        let temp = indices.remove(i_pos);
                        indices.push(temp);
                        cycles[i_pos] = n - i_pos;
                    } else {
                        let j = cycles[i_pos];
                        let len = indices.len();
                        indices.swap(i_pos, len - j);
                        out.push(Value::Tuple(Rc::new(
                            indices[..r].iter().map(|&i| xs[i].clone()).collect(),
                        )));
                        continue 'outer;
                    }
                }
                break;
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    let product = nf("product", |i, args| {
        // `product(*iterables, repeat=1)` — ignore the repeat kwarg (not
        // wired into the call frame). Returns a list of tuples.
        let pools: Vec<Vec<Value>> = args
            .into_iter()
            .map(|a| drain(i, a))
            .collect::<Result<_, _>>()?;
        let mut out: Vec<Vec<Value>> = vec![vec![]];
        for pool in &pools {
            let mut next = Vec::with_capacity(out.len() * pool.len());
            for prefix in &out {
                for v in pool {
                    let mut new_p = prefix.clone();
                    new_p.push(v.clone());
                    next.push(new_p);
                }
            }
            out = next;
        }
        let tuples: Vec<Value> = out.into_iter().map(|v| Value::Tuple(Rc::new(v))).collect();
        Ok(Value::List(Rc::new(RefCell::new(tuples))))
    });
    let groupby = nf("groupby", |i, mut args| {
        if args.is_empty() {
            return Err(type_error("groupby() needs an iterable"));
        }
        let xs = drain(i, args.remove(0))?;
        let key = args.into_iter().next();
        // Returns a list of (key, list) tuples.
        let mut out: Vec<Value> = Vec::new();
        let mut current_key: Option<Value> = None;
        let mut current_group: Vec<Value> = Vec::new();
        for x in xs {
            let k = match &key {
                Some(f) => i.call_value(f.clone(), vec![x.clone()], &[])?,
                None => x.clone(),
            };
            if let Some(ck) = &current_key {
                if ck.py_eq(&k) {
                    current_group.push(x);
                    continue;
                }
                out.push(Value::Tuple(Rc::new(vec![
                    ck.clone(),
                    Value::List(Rc::new(RefCell::new(std::mem::take(&mut current_group)))),
                ])));
            }
            current_key = Some(k);
            current_group.push(x);
        }
        if let Some(ck) = current_key {
            out.push(Value::Tuple(Rc::new(vec![
                ck,
                Value::List(Rc::new(RefCell::new(current_group))),
            ])));
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    });
    make_module(
        "itertools",
        vec![
            ("chain", chain),
            ("repeat", repeat),
            ("count", count),
            ("cycle", cycle),
            ("accumulate", accumulate),
            ("islice", islice),
            ("takewhile", takewhile),
            ("dropwhile", dropwhile),
            ("combinations", combinations),
            ("permutations", permutations),
            ("product", product),
            ("groupby", groupby),
        ],
    )
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
        if let Value::Instance(inst) = v {
            let mut map: DictMap = IndexMap::new();
            for (k, val) in inst.fields.borrow().iter() {
                map.insert(HashKey::Str(Rc::new(k.clone())), val.clone());
            }
            Ok(Value::Dict(Rc::new(RefCell::new(map))))
        } else {
            Err(type_error("asdict() requires a dataclass instance"))
        }
    });
    make_module(
        "dataclasses",
        vec![
            ("dataclass", dataclass),
            ("field", field),
            ("asdict", asdict),
        ],
    )
}

/// `pathlib` shim.
///
/// Implements a `Path` callable that returns an instance with the
/// following surface: `__truediv__`, `read_text`, `write_text`,
/// `exists`, `is_file`, `is_dir`, `name`, `parent`, `suffix`, `suffixes`,
/// `stem`, and `__str__`. Internally a Path instance is an `Instance` value
/// whose `fields` map holds the resolved path string under `__path__`.
///
/// Source for the `pathlib.Path` dunder shim. Holds the path string in
/// `_path`; `__truediv__` defers the actual join to the native `_join` field
/// (so all the OS-aware part computation stays in Rust). `__str__` /
/// `__repr__` / `__fspath__` surface the string form.
const PATH_SRC: &str = r#"
class _Path:
    def __truediv__(self, other):
        return self._join(other)
    def __str__(self):
        return self._path
    def __repr__(self):
        return "PosixPath('" + self._path + "')"
    def __fspath__(self):
        return self._path
    def __eq__(self, other):
        return self._path == str(other)
    def __lt__(self, other):
        return self._path < str(other)
    def __le__(self, other):
        return self._path <= str(other)
    def __gt__(self, other):
        return self._path > str(other)
    def __ge__(self, other):
        return self._path >= str(other)
    def __hash__(self):
        return hash(self._path)
"#;

fn path_class(interp: &mut Interpreter) -> Result<Rc<crate::value::Class>, Unwind> {
    let cls = cached_helper_class(interp, "__shim_path__", PATH_SRC, "_Path")?;
    match cls {
        Value::Class(c) => Ok(c),
        _ => Err(type_error("internal: _Path shim is not a class")),
    }
}

fn make_pathlib_module(interp: &mut Interpreter) -> Result<Value, Unwind> {
    use crate::value::Instance;
    let cls = path_class(interp)?;
    fn make_path(cls: Rc<crate::value::Class>, s: String) -> Value {
        let mut fields: HashMap<String, Value> = HashMap::new();
        fields.insert("__path__".into(), Value::Str(Rc::new(s.clone())));
        // `_path` mirrors `__path__` for the compiled `_Path` dunder methods,
        // which reference `self._path`.
        fields.insert("_path".into(), Value::Str(Rc::new(s.clone())));
        // Cache derived parts as fields so attribute access (`p.name`,
        // `p.suffix`, `p.stem`, `p.parent`) works without descriptors.
        let p = std::path::Path::new(&s);
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_owned();
        let stem = p
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_owned();
        let suffix = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let parent = p.parent().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        fields.insert("name".into(), Value::Str(Rc::new(name)));
        fields.insert("stem".into(), Value::Str(Rc::new(stem)));
        fields.insert("suffix".into(), Value::Str(Rc::new(suffix)));
        // `parent` is exposed as a string here; users who need the
        // full Path API on the parent can wrap with `Path(p.parent)`.
        // This sidesteps an infinite recursion when `parent` of `/` is
        // itself `/`.
        fields.insert("parent".into(), Value::Str(Rc::new(parent)));
        // `.suffixes` — every dotted extension on the final component, e.g.
        // `Path("file.tar.gz").suffixes == ['.tar', '.gz']`. A leading dot
        // (dotfile like `.bashrc`) does NOT start a suffix, matching CPython.
        let suffixes: Vec<Value> = {
            let name = std::path::Path::new(&s)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            let trimmed = name.trim_start_matches('.');
            if trimmed.contains('.') {
                trimmed
                    .split('.')
                    .skip(1)
                    .map(|part| Value::Str(Rc::new(format!(".{part}"))))
                    .collect()
            } else {
                Vec::new()
            }
        };
        fields.insert(
            "suffixes".into(),
            Value::List(Rc::new(RefCell::new(suffixes))),
        );
        // `_join` backs `__truediv__`: append `other` as a path component and
        // return a fresh `_Path` instance.
        let s_for_join = s.clone();
        let cls_for_join = cls.clone();
        fields.insert(
            "_join".into(),
            Value::Native(Rc::new(NativeFn::new("_join", move |_i, args| {
                let other = single(&args, "_join")?.py_str();
                let joined = std::path::Path::new(&s_for_join)
                    .join(&other)
                    .to_str()
                    .unwrap_or("")
                    .to_owned();
                Ok(make_path(cls_for_join.clone(), joined))
            }))),
        );
        // Methods.
        let s_for_read = s.clone();
        fields.insert(
            "read_text".into(),
            Value::Native(Rc::new(NativeFn::new("read_text", move |_i, _args| {
                std::fs::read_to_string(&s_for_read)
                    .map(|t| Value::Str(Rc::new(t)))
                    .map_err(|e| {
                        crate::error::Unwind::Exception(crate::error::VmException::new(
                            "OSError",
                            format!("{e}"),
                        ))
                    })
            }))),
        );
        let s_for_write = s.clone();
        fields.insert(
            "write_text".into(),
            Value::Native(Rc::new(NativeFn::new("write_text", move |_i, args| {
                let text = single(&args, "write_text")?.py_str();
                std::fs::write(&s_for_write, text.as_bytes())
                    // `Path.write_text` returns the number of characters written.
                    .map(|_| Value::Int(num_bigint::BigInt::from(text.chars().count() as i64)))
                    .map_err(|e| {
                        crate::error::Unwind::Exception(crate::error::VmException::new(
                            "OSError",
                            format!("{e}"),
                        ))
                    })
            }))),
        );
        let s_for_exists = s.clone();
        fields.insert(
            "exists".into(),
            Value::Native(Rc::new(NativeFn::new("exists", move |_i, _args| {
                Ok(Value::Bool(std::path::Path::new(&s_for_exists).exists()))
            }))),
        );
        let s_for_is_file = s.clone();
        fields.insert(
            "is_file".into(),
            Value::Native(Rc::new(NativeFn::new("is_file", move |_i, _args| {
                Ok(Value::Bool(std::path::Path::new(&s_for_is_file).is_file()))
            }))),
        );
        let s_for_is_dir = s.clone();
        fields.insert(
            "is_dir".into(),
            Value::Native(Rc::new(NativeFn::new("is_dir", move |_i, _args| {
                Ok(Value::Bool(std::path::Path::new(&s_for_is_dir).is_dir()))
            }))),
        );
        let s_for_iter = s.clone();
        let cls_for_iter = cls.clone();
        fields.insert(
            "iterdir".into(),
            Value::Native(Rc::new(NativeFn::new("iterdir", move |_i, _args| {
                let mut entries: Vec<Value> = Vec::new();
                let rd = std::fs::read_dir(&s_for_iter).map_err(|e| {
                    crate::error::Unwind::Exception(crate::error::VmException::new(
                        if e.kind() == std::io::ErrorKind::NotFound {
                            "FileNotFoundError"
                        } else {
                            "OSError"
                        },
                        format!("{e}: '{s_for_iter}'"),
                    ))
                })?;
                for entry in rd.flatten() {
                    entries.push(make_path(
                        cls_for_iter.clone(),
                        entry.path().to_string_lossy().into_owned(),
                    ));
                }
                Ok(Value::List(Rc::new(RefCell::new(entries))))
            }))),
        );
        let s_for_glob = s.clone();
        let cls_for_glob = cls.clone();
        fields.insert(
            "glob".into(),
            Value::Native(Rc::new(NativeFn::new("glob", move |_i, args| {
                // Non-recursive single-component glob: `*`, `*.py`, `data*`.
                let pat = single(&args, "glob")?.py_str();
                if pat.contains("**") || pat.contains('/') {
                    return Err(type_error(
                        "VM glob() supports single-component patterns only — \
                         use `tyc run --compile` for recursive globs",
                    ));
                }
                let mut entries: Vec<Value> = Vec::new();
                if let Ok(rd) = std::fs::read_dir(&s_for_glob) {
                    for entry in rd.flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if glob_match(&pat, &name) {
                            entries.push(make_path(
                                cls_for_glob.clone(),
                                entry.path().to_string_lossy().into_owned(),
                            ));
                        }
                    }
                }
                Ok(Value::List(Rc::new(RefCell::new(entries))))
            }))),
        );
        let s_for_mkdir = s.clone();
        fields.insert(
            "mkdir".into(),
            Value::Native(Rc::new(NativeFn::new("mkdir", move |_i, args| {
                // Accept the common kwargs via sentinel: parents=, exist_ok=.
                let (_pos, kw_vec) = split_kwargs(&args);
                let kw: HashMap<String, Value> = kw_vec.into_iter().collect();
                let parents = kw.get("parents").map(|v| v.truthy()).unwrap_or(false);
                let exist_ok = kw.get("exist_ok").map(|v| v.truthy()).unwrap_or(false);
                let r = if parents {
                    std::fs::create_dir_all(&s_for_mkdir)
                } else {
                    std::fs::create_dir(&s_for_mkdir)
                };
                match r {
                    Ok(()) => Ok(Value::None),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && exist_ok => {
                        Ok(Value::None)
                    }
                    Err(e) => Err(fs_unwind(&s_for_mkdir, e)),
                }
            }))),
        );
        let s_for_unlink = s.clone();
        fields.insert(
            "unlink".into(),
            Value::Native(Rc::new(NativeFn::new("unlink", move |_i, _args| {
                std::fs::remove_file(&s_for_unlink).map_err(|e| fs_unwind(&s_for_unlink, e))?;
                Ok(Value::None)
            }))),
        );
        let s_for_read_b = s.clone();
        fields.insert(
            "read_bytes".into(),
            Value::Native(Rc::new(NativeFn::new("read_bytes", move |_i, _args| {
                std::fs::read(&s_for_read_b)
                    .map(|b| Value::Bytes(Rc::new(b)))
                    .map_err(|e| fs_unwind(&s_for_read_b, e))
            }))),
        );
        let s_for_write_b = s.clone();
        fields.insert(
            "write_bytes".into(),
            Value::Native(Rc::new(NativeFn::new("write_bytes", move |_i, args| {
                let data = match single(&args, "write_bytes")? {
                    Value::Bytes(b) => b.as_ref().clone(),
                    other => {
                        return Err(type_error(format!(
                            "write_bytes() expects bytes, not {}",
                            other.type_name()
                        )))
                    }
                };
                let n = data.len();
                std::fs::write(&s_for_write_b, data).map_err(|e| fs_unwind(&s_for_write_b, e))?;
                Ok(Value::Int(num_bigint::BigInt::from(n as i64)))
            }))),
        );
        Value::Instance(Rc::new(Instance {
            class: cls,
            fields: RefCell::new(fields),
        }))
    }
    let cls_for_ctor = cls.clone();
    let path = nf("Path", move |_i, args| {
        let s = args
            .first()
            .map(|v| v.py_str())
            .unwrap_or_else(|| ".".to_string());
        Ok(make_path(cls_for_ctor.clone(), s))
    });
    Ok(make_module("pathlib", vec![("Path", path)]))
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
    match (&receiver, name) {
        // ── str methods ────────────────────────────────────────────────────
        (Value::Str(s), m) => str_method(interp, s, m, rest, &kwargs),
        // ── bytes methods ──────────────────────────────────────────────────
        (Value::Bytes(b), m) => bytes_method(b, m, rest),
        // ── list methods ───────────────────────────────────────────────────
        (Value::List(l), m) => list_method(interp, l, m, rest),
        // ── dict methods ───────────────────────────────────────────────────
        (Value::Dict(d), m) => dict_method(interp, d, m, rest),
        // ── set methods ────────────────────────────────────────────────────
        (Value::Set(s), m) => set_method(s, m, rest),
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
        map.insert(k.to_hash_key()?, fill.clone());
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
    use num_bigint::BigInt;
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
                            (Some(c), None) => HashKey::Int(BigInt::from(c as u32)),
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
                    HashKey::Int(BigInt::from(fc as u32)),
                    Value::Int(BigInt::from(tc as u32)),
                );
            }
            if let Some(third) = args.get(2) {
                for dc in as_str(third)?.chars() {
                    map.insert(HashKey::Int(BigInt::from(dc as u32)), Value::None);
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
                let key = HashKey::Int(num_bigint::BigInt::from(ch as u32));
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
        "splitlines" => Value::List(Rc::new(RefCell::new(
            s.lines()
                .map(|l| Value::Str(Rc::new(l.to_owned())))
                .collect(),
        ))),
        "join" => {
            let iterable = args
                .first()
                .ok_or_else(|| type_error("str.join requires an iterable"))?
                .clone();
            let mut parts: Vec<String> = Vec::new();
            let it = interp.make_iter(iterable)?;
            while let Some(v) = interp.iter_next(&it)? {
                match v {
                    Value::Str(s) => parts.push((*s).clone()),
                    _ => return Err(type_error("sequence item: expected str")),
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
        "startswith" => Value::Bool(s.starts_with(&single(args, "startswith")?.py_str())),
        "endswith" => Value::Bool(s.ends_with(&single(args, "endswith")?.py_str())),
        "find" => {
            let needle = single(args, "find")?.py_str();
            Value::Int(num_bigint::BigInt::from(
                s.find(&needle).map(|i| i as i64).unwrap_or(-1),
            ))
        }
        "rfind" => {
            let needle = single(args, "rfind")?.py_str();
            Value::Int(num_bigint::BigInt::from(
                s.rfind(&needle).map(|i| i as i64).unwrap_or(-1),
            ))
        }
        "count" => {
            let needle = single(args, "count")?.py_str();
            if needle.is_empty() {
                Value::Int(num_bigint::BigInt::from(s.chars().count() as i64 + 1))
            } else {
                Value::Int(num_bigint::BigInt::from(s.matches(&needle).count() as i64))
            }
        }
        "isdigit" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())),
        "isalpha" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_alphabetic())),
        "isalnum" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_alphanumeric())),
        "isspace" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_whitespace())),
        "isupper" => Value::Bool(!s.is_empty() && s.chars().all(|c| !c.is_lowercase())),
        "islower" => Value::Bool(!s.is_empty() && s.chars().all(|c| !c.is_uppercase())),
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
        "index" => {
            let needle = single(args, "index")?.py_str();
            match s.find(&needle) {
                Some(i) => Value::Int(num_bigint::BigInt::from(i as i64)),
                None => return Err(value_error("substring not found")),
            }
        }
        "rindex" => {
            let needle = single(args, "rindex")?.py_str();
            match s.rfind(&needle) {
                Some(i) => Value::Int(num_bigint::BigInt::from(i as i64)),
                None => return Err(value_error("substring not found")),
            }
        }
        "isnumeric" | "isdecimal" => {
            Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_numeric()))
        }
        "istitle" => Value::Bool(is_title_case(s)),
        "casefold" => Value::Str(Rc::new(s.to_lowercase())),
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
                        let left = pad / 2;
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
        "encode" => Value::Bytes(Rc::new(s.as_bytes().to_vec())),
        _ => return Err(attribute_error(format!("str has no method '{}'", name))),
    })
}

/// Implementation of `str.format(...)`. Supports:
/// - `{}` auto-numbered positional fields
/// - `{0}`, `{1}` explicit positional fields
/// - `{name}` named fields
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

                // Split field_name from format_spec at the first ':'
                let (field_ref, spec) = if let Some(colon) = field.find(':') {
                    (&field[..colon], &field[colon + 1..])
                } else {
                    (field.as_str(), "")
                };

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

                // A user `__format__(self, spec)` controls its own
                // formatting for any spec (including the empty `{}` spec,
                // which CPython routes through `__format__("")`).
                let formatted = if let Some(custom) = interp.try_user_format(&value, spec)? {
                    custom
                } else {
                    // The default stringification honours a user `__str__`
                    // (via `str_of`), matching `print` / `str`.
                    let default = interp.str_of(&value)?;
                    if spec.is_empty() {
                        default
                    } else {
                        crate::interp::format_with_spec_pub(&value, &default, spec)?
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

fn bytes_method(b: &Rc<Vec<u8>>, name: &str, args: &[Value]) -> Result<Value, Unwind> {
    Ok(match name {
        // `.decode()` / `.decode("utf-8")` -> str. Only UTF-8/ASCII handled;
        // other encodings fall back to a lossy UTF-8 decode.
        "decode" => {
            let enc = args.first().map(|v| v.py_str()).unwrap_or_default();
            let enc_norm = enc.to_ascii_lowercase().replace(['-', '_'], "");
            match enc_norm.as_str() {
                "" | "utf8" | "ascii" => match std::str::from_utf8(b) {
                    Ok(s) => Value::Str(Rc::new(s.to_owned())),
                    Err(_) => return Err(value_error("'utf-8' codec can't decode byte sequence")),
                },
                "latin1" | "iso88591" => {
                    Value::Str(Rc::new(b.iter().map(|&c| c as char).collect::<String>()))
                }
                _ => Value::Str(Rc::new(String::from_utf8_lossy(b).into_owned())),
            }
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
            let needle = bytes_arg(single(args, name)?)?;
            match find_subslice(b, &needle) {
                Some(i) => Value::Int(num_bigint::BigInt::from(i as i64)),
                None if name == "find" => Value::Int(num_bigint::BigInt::from(-1)),
                None => return Err(value_error("subsection not found")),
            }
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
            let i = normalize_index(idx, l.len())
                .ok_or_else(|| index_error("pop index out of range"))?;
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
            let target = single(args, "index")?.clone();
            let items = l.borrow().clone();
            for (i, v) in items.iter().enumerate() {
                if interp.values_equal(v, &target)? {
                    return Ok(Value::Int(num_bigint::BigInt::from(i as i64)));
                }
            }
            Err(value_error("list.index(x): x not in list"))
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
            Ok(Value::Int(num_bigint::BigInt::from(n)))
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
            let k = single(args, "get")?.to_hash_key()?;
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
            let k = single(args, "pop")?.to_hash_key()?;
            let default = args.get(1).cloned();
            // `shift_remove` preserves the insertion order of remaining
            // keys (matches CPython `dict.pop` semantics).
            match d.borrow_mut().shift_remove(&k) {
                Some(v) => Ok(v),
                None => default.ok_or_else(|| key_error(format!("{:?}", k))),
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
                                d.borrow_mut().insert(t[0].to_hash_key()?, t[1].clone());
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
            let k = single(args, "setdefault")?.to_hash_key()?;
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
                .ok_or_else(|| type_error("move_to_end() requires a key"))?
                .to_hash_key()?;
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
                .map(|(k, count)| {
                    Value::Tuple(Rc::new(vec![
                        k,
                        Value::Int(num_bigint::BigInt::from(count)),
                    ]))
                })
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
            s.borrow_mut().insert(single(args, "add")?.to_hash_key()?);
            Ok(Value::None)
        }
        "remove" | "discard" => {
            let k = single(args, name)?.to_hash_key()?;
            let removed = s.borrow_mut().remove(&k);
            if name == "remove" && !removed {
                return Err(key_error(format!("{:?}", k)));
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
fn set_keys_no_sentinel(s: &Rc<RefCell<HashSet<HashKey>>>) -> HashSet<HashKey> {
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
            Ok(Value::Int(num_bigint::BigInt::from(
                t.iter().filter(|v| v.py_eq(target)).count() as i64,
            )))
        }
        "index" => {
            let target = single(args, "index")?;
            t.iter()
                .position(|v| v.py_eq(target))
                .map(|p| Value::Int(num_bigint::BigInt::from(p as i64)))
                .ok_or_else(|| value_error("tuple.index(x): x not in tuple"))
        }
        _ => Err(attribute_error(format!("tuple has no method '{}'", name))),
    }
}

fn num_method(v: &Value, name: &str, args: &[Value]) -> Result<Value, Unwind> {
    match (v, name) {
        (Value::Float(x), "is_integer") => Ok(Value::Bool(x.fract() == 0.0 && x.is_finite())),
        (Value::Int(i), "bit_length") => Ok(Value::Int(num_bigint::BigInt::from(i.bits() as i64))),
        // `(n).bit_count()` — number of set bits in the absolute value.
        (Value::Int(i), "bit_count") => {
            let (_, bytes) = i.to_bytes_be();
            let count: u32 = bytes.iter().map(|b| b.count_ones()).sum();
            Ok(Value::Int(num_bigint::BigInt::from(count)))
        }
        // `(n).to_bytes(length, byteorder="big")` — non-negative ints.
        (Value::Int(i), "to_bytes") => {
            use num_traits::Signed;
            if i.is_negative() {
                return Err(value_error("to_bytes: negative ints not supported"));
            }
            let length = match args.first() {
                Some(n) => n.to_int()?.max(0) as usize,
                None => 1,
            };
            let big_endian = match args.get(1) {
                Some(Value::Str(s)) => s.as_str() != "little",
                _ => true,
            };
            let (_, mut raw) = i.to_bytes_be();
            if raw.len() > length {
                return Err(value_error("int too big to convert"));
            }
            // Left-pad with zero bytes to the requested length.
            let mut out = vec![0u8; length - raw.len()];
            out.append(&mut raw);
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

/// `json.dumps(v, indent=n)` — pretty-printed with `n`-space indentation.
fn json_dumps_indent(v: &Value, indent: usize, level: usize, sort: bool) -> String {
    let pad = " ".repeat(indent * (level + 1));
    let close_pad = " ".repeat(indent * level);
    match v {
        Value::List(l) => {
            let items = l.borrow();
            if items.is_empty() {
                return "[]".into();
            }
            let body: Vec<String> = items
                .iter()
                .map(|x| format!("{}{}", pad, json_dumps_indent(x, indent, level + 1, sort)))
                .collect();
            format!("[\n{}\n{}]", body.join(",\n"), close_pad)
        }
        Value::Tuple(t) => {
            if t.is_empty() {
                return "[]".into();
            }
            let body: Vec<String> = t
                .iter()
                .map(|x| format!("{}{}", pad, json_dumps_indent(x, indent, level + 1, sort)))
                .collect();
            format!("[\n{}\n{}]", body.join(",\n"), close_pad)
        }
        Value::Dict(d) => {
            let d = d.borrow();
            let entries: Vec<(HashKey, Value)> = d
                .iter()
                .filter(|(k, _)| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if entries.is_empty() {
                return "{}".into();
            }
            let mut body: Vec<(String, String)> = entries
                .iter()
                .map(|(k, val)| {
                    (
                        json_dumps(&k.clone().into_value(), sort),
                        json_dumps_indent(val, indent, level + 1, sort),
                    )
                })
                .collect();
            if sort {
                body.sort_by(|a, b| a.0.cmp(&b.0));
            }
            let body: Vec<String> = body
                .iter()
                .map(|(k, val)| format!("{}{}: {}", pad, k, val))
                .collect();
            format!("{{\n{}\n{}}}", body.join(",\n"), close_pad)
        }
        other => json_dumps(other, sort),
    }
}

/// Public wrapper so `interp.rs` (`model_dump_json`) can reuse the serializer.
pub fn json_dumps_pub(v: &Value) -> String {
    json_dumps(v, false)
}

fn json_dumps(v: &Value, sort: bool) -> String {
    match v {
        Value::None => "null".into(),
        Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        Value::Int(i) => i.to_string(),
        Value::Float(x) => format!("{}", x),
        Value::Str(s) => json_string(s),
        Value::List(l) => {
            let items: Vec<String> = l.borrow().iter().map(|x| json_dumps(x, sort)).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Tuple(t) => {
            let items: Vec<String> = t.iter().map(|x| json_dumps(x, sort)).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Dict(d) => {
            // Filter the `__typhon_frozen__` sentinel a `freeze let`
            // inserts (review thread copilot on PR #147 — otherwise
            // `json.dump(frozen_dict, fp)` leaks the marker into the
            // emitted JSON).
            let mut pairs: Vec<(String, String)> = d
                .borrow()
                .iter()
                .filter(|(k, _)| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .map(|(k, v)| {
                    (
                        json_dumps(&k.clone().into_value(), sort),
                        json_dumps(v, sort),
                    )
                })
                .collect();
            if sort {
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
            }
            let items: Vec<String> = pairs.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
            format!("{{{}}}", items.join(", "))
        }
        other => json_string(&other.py_repr()),
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_loads(s: &str) -> Result<Value, Unwind> {
    let mut p = JsonParser {
        src: s.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.parse()?;
    p.skip_ws();
    if p.pos < p.src.len() {
        return Err(value_error("extra data in JSON input"));
    }
    Ok(v)
}

struct JsonParser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }
    fn parse(&mut self) -> Result<Value, Unwind> {
        self.skip_ws();
        if self.pos >= self.src.len() {
            return Err(value_error("unexpected end of JSON input"));
        }
        match self.src[self.pos] {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(|s| Value::Str(Rc::new(s))),
            b't' | b'f' => self.parse_bool(),
            b'n' => self.parse_null(),
            c if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            c => Err(value_error(format!(
                "unexpected char '{}' in JSON",
                c as char
            ))),
        }
    }
    fn parse_object(&mut self) -> Result<Value, Unwind> {
        self.pos += 1; // {
        let mut map: DictMap = IndexMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Dict(Rc::new(RefCell::new(map))));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let value = self.parse()?;
            map.insert(HashKey::Str(Rc::new(key)), value);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(value_error("expected ',' or '}' in JSON object")),
            }
        }
        Ok(Value::Dict(Rc::new(RefCell::new(map))))
    }
    fn parse_array(&mut self) -> Result<Value, Unwind> {
        self.pos += 1; // [
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::List(Rc::new(RefCell::new(out))));
        }
        loop {
            out.push(self.parse()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(value_error("expected ',' or ']' in JSON array")),
            }
        }
        Ok(Value::List(Rc::new(RefCell::new(out))))
    }
    fn parse_string(&mut self) -> Result<String, Unwind> {
        self.expect(b'"')?;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == b'"' {
                self.pos += 1;
                return Ok(out);
            }
            if c == b'\\' {
                self.pos += 1;
                match self.peek() {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(b'/') => out.push('/'),
                    Some(b'n') => out.push('\n'),
                    Some(b'r') => out.push('\r'),
                    Some(b't') => out.push('\t'),
                    _ => return Err(value_error("bad escape in JSON string")),
                }
                self.pos += 1;
                continue;
            }
            out.push(c as char);
            self.pos += 1;
        }
        Err(value_error("unterminated string"))
    }
    fn parse_bool(&mut self) -> Result<Value, Unwind> {
        if self.src[self.pos..].starts_with(b"true") {
            self.pos += 4;
            return Ok(Value::Bool(true));
        }
        if self.src[self.pos..].starts_with(b"false") {
            self.pos += 5;
            return Ok(Value::Bool(false));
        }
        Err(value_error("expected boolean"))
    }
    fn parse_null(&mut self) -> Result<Value, Unwind> {
        if self.src[self.pos..].starts_with(b"null") {
            self.pos += 4;
            return Ok(Value::None);
        }
        Err(value_error("expected null"))
    }
    fn parse_number(&mut self) -> Result<Value, Unwind> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let slice = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| value_error("invalid number"))?;
        if is_float {
            Ok(Value::Float(
                slice.parse().map_err(|_| value_error("bad number"))?,
            ))
        } else {
            Ok(Value::Int(
                slice
                    .parse::<num_bigint::BigInt>()
                    .map_err(|_| value_error("bad number"))?,
            ))
        }
    }
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }
    fn expect(&mut self, byte: u8) -> Result<(), Unwind> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(value_error(format!("expected '{}'", byte as char)))
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
            let len = columns.first().map(|c| c.len()).unwrap_or(0);
            if columns.iter().any(|c| c.len() != len) {
                return Err(value_error("zip() argument lengths differ (strict=True)"));
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
                return default
                    .ok_or_else(|| value_error(format!("{}() arg is an empty sequence", n.name)));
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
        // `json.dumps(obj, indent=n, sort_keys=...)` — honour the common
        // `indent` kwarg for pretty-printing (others are accepted/ignored).
        "dumps" => {
            let obj = args
                .first()
                .ok_or_else(|| type_error("dumps() missing argument"))?;
            let mut indent: Option<usize> = None;
            let mut sort_keys = false;
            for (k, v) in kwargs {
                if k == "indent" {
                    if let Value::Int(_) = v {
                        indent = v.to_int().ok().filter(|n| *n > 0).map(|n| n as usize);
                    }
                } else if k == "sort_keys" {
                    sort_keys = interp.is_truthy(v)?;
                }
            }
            match indent {
                Some(n) => Ok(Value::Str(Rc::new(json_dumps_indent(obj, n, 0, sort_keys)))),
                None => Ok(Value::Str(Rc::new(json_dumps(obj, sort_keys)))),
            }
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
        // Text-IO helpers accept an `encoding=` (and `errors=`) kwarg the VM
        // doesn't model (it is always UTF-8). Drop the kwargs and run.
        "write_text" | "read_text" | "open" => (n.func)(interp, args),
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
                        other => {
                            return Err(type_error(format!(
                                "print() file= must be sys.stdout or sys.stderr in the VM, \
                                 not {} — use `tyc run --compile` for arbitrary file sinks",
                                other.type_name()
                            )))
                        }
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
            if to_stderr {
                eprint!("{out}");
            } else {
                print!("{out}");
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
            let mut acc = Value::Int(num_bigint::BigInt::from(0));
            for (k, v) in kwargs {
                if k == "start" {
                    acc = v.clone();
                } else {
                    return Err(type_error(format!("sum() got unexpected keyword: '{}'", k)));
                }
            }
            let it = interp.make_iter(
                args.into_iter()
                    .next()
                    .ok_or_else(|| type_error("sum() requires an iterable"))?,
            )?;
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
        "mkdir" | "makedirs" => {
            let mut args = args;
            args.push(make_kwargs_sentinel(kwargs));
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

/// A lightweight type object for a built-in type (`int`, `str`, …) — an empty
/// `Class` whose only meaningful attribute is its `name`. Used by `type(x)`
/// so type comparisons and `.__name__` work uniformly with user classes.
pub fn make_builtin_type(name: &str) -> Value {
    // Cache one canonical `Class` object per builtin type name so repeated
    // `type(x)` calls return the *same* object — `type(5) == type(6)` then
    // holds by identity (Class equality is identity-based; see `py_eq`).
    thread_local! {
        static CACHE: RefCell<HashMap<String, Rc<crate::value::Class>>> =
            RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
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
                })
            })
            .clone();
        Value::Class(cls)
    })
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
