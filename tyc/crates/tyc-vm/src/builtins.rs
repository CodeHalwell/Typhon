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

    native!("bool", |_i, args| Ok(Value::Bool(match args.first() {
        Some(v) => v.truthy(),
        None => false,
    })));

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
            other => make_builtin_type(other.type_name()),
        })
    });

    native!("isinstance", |_i, args| {
        if args.len() != 2 {
            return Err(type_error("isinstance() expected 2 arguments"));
        }
        let val = &args[0];
        let cls = &args[1];
        Ok(Value::Bool(is_instance_of(val, cls)))
    });

    native!("abs", |_i, args| match single(&args, "abs")? {
        Value::Int(i) => Ok(Value::Int(num_traits::Signed::abs(i))),
        Value::Float(x) => Ok(Value::Float(x.abs())),
        Value::Bool(b) => Ok(Value::Int(num_bigint::BigInt::from(*b as i64))),
        _ => Err(type_error("bad operand type for abs()")),
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
        out.sort_by(|a, b| a.py_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
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
        // round(int) and round(int, ndigits) return the int unchanged
        // (Python rounds to the given decimal place; for ints with
        // non-negative ndigits that's a no-op, and these are the only
        // cases the shim handles).
        Some(Value::Int(i)) => Ok(Value::Int(i.clone())),
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

    native!("hash", |_i, args| {
        let v = single(&args, "hash")?;
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
    // The `?` operator desugars to `isinstance(_, __typhon_Err__)`, so the
    // preprocessor's import alias must resolve to the VM's `Err` ctor.
    root.set(
        "__typhon_Err__",
        root.get("Err").expect("Err just registered"),
    );

    // A couple of exception types so user `raise ValueError(...)` works.
    for name in [
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
        "OSError",
        "FileNotFoundError",
        "NotImplementedError",
    ] {
        let n = name.to_owned();
        let ctor = NativeFn::new(Box::leak(n.clone().into_boxed_str()), move |_i, args| {
            let msg = args.first().map(|v| v.py_str()).unwrap_or_default();
            Ok(Value::Exception {
                kind: Rc::new(n.clone()),
                message: Rc::new(msg),
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
        // Exception kind match.
        (k, Value::Exception { kind, .. }) if k == kind.as_str() => true,
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
        let cmp = v.py_cmp(&best).unwrap_or(std::cmp::Ordering::Equal);
        if (want_min && cmp == std::cmp::Ordering::Less)
            || (!want_min && cmp == std::cmp::Ordering::Greater)
        {
            best = v;
        }
    }
    Ok(best)
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
        "functools" => Ok(make_functools_module()),
        "itertools" => Ok(make_itertools_module()),
        "dataclasses" => Ok(make_dataclasses_module()),
        "pathlib" => Ok(make_pathlib_module()),
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

fn make_typhon_runtime_module(interp: &Interpreter) -> Value {
    let ok = interp.root.get("Ok").unwrap();
    let err = interp.root.get("Err").unwrap();
    // Submodules.
    let tasks = make_module(
        "typhon_runtime.tasks",
        vec![(
            "spawn",
            nf("spawn", |i, args| {
                // Synchronous "spawn" — just call the value immediately.
                let f = args.into_iter().next().unwrap_or(Value::None);
                i.call_value(f, vec![], &[])
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
    // `Result` is exposed as a type marker — the type checker uses
    // `Result[T, E]` as a typing construct; at runtime the desugared
    // module only needs the name to be defined. An identity callable
    // suffices since user code never invokes `Result(...)` directly.
    let result_marker = nf("Result", |_i, args| {
        Ok(args.into_iter().next().unwrap_or(Value::None))
    });
    make_module(
        "typhon_runtime",
        vec![
            ("Ok", ok),
            ("Err", err),
            ("Result", result_marker),
            ("tasks", tasks),
            ("lazy", lazy),
            ("freeze", freeze),
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
                    ],
                ),
            ),
        ],
    )
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
                    Ok(Value::Str(Rc::new(json_dumps(single(&args, "dumps")?))))
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
                    let serialised = json_dumps(&args[0]);
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
    use std::cell::Cell;
    thread_local! {
        static SEED: Cell<u64> = const { Cell::new(0x_2545_F491_4F6C_DD1D) };
    }
    fn next_u64() -> u64 {
        SEED.with(|s| {
            let mut x = s.get();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.set(x);
            x
        })
    }
    make_module(
        "random",
        vec![
            (
                "random",
                nf("random", |_i, _args| {
                    Ok(Value::Float((next_u64() as f64) / (u64::MAX as f64)))
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
                    Ok(Value::Int(num_bigint::BigInt::from(
                        a + (next_u64() % span) as i64,
                    )))
                }),
            ),
            (
                "seed",
                nf("seed", |_i, args| {
                    let s = args.first().and_then(|v| v.to_int().ok()).unwrap_or(0);
                    SEED.with(|c| c.set(s as u64 ^ 0x_2545_F491_4F6C_DD1D));
                    Ok(Value::None)
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
                Ok(captures_to_value(caps))
            }))),
        );
        let p2 = p_rc.clone();
        attrs.insert(
            "search".into(),
            Value::Native(Rc::new(NativeFn::new("search", move |_i, args| {
                let s = single(&args, "search")?.py_str();
                Ok(captures_to_value(p2.captures(&s)))
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
            Value::Native(Rc::new(NativeFn::new("sub", move |_i, args| {
                let repl = args
                    .first()
                    .ok_or_else(|| type_error("sub() needs replacement"))?
                    .py_str();
                let s = args
                    .get(1)
                    .ok_or_else(|| type_error("sub() needs string"))?
                    .py_str();
                Ok(Value::Str(Rc::new(p4.replace_all(&s, repl).into_owned())))
            }))),
        );
        let p5 = p_rc.clone();
        attrs.insert(
            "split".into(),
            Value::Native(Rc::new(NativeFn::new("split", move |_i, args| {
                let s = single(&args, "split")?.py_str();
                let parts: Vec<Value> = p5
                    .split(&s)
                    .map(|p| Value::Str(Rc::new(p.to_owned())))
                    .collect();
                Ok(Value::List(Rc::new(RefCell::new(parts))))
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
        });
        Value::Instance(Rc::new(crate::value::Instance {
            class: cls,
            fields: RefCell::new(attrs),
        }))
    }
    // Build a match object from a `regex::Captures`. Group 0 is the whole
    // match; groups 1.. are the capture groups. Non-participating optional
    // groups are represented as `None`.
    fn captures_to_value(caps: Option<regex::Captures<'_>>) -> Value {
        let Some(caps) = caps else { return Value::None };
        let whole = caps.get(0).expect("group 0 always present");
        let start = whole.start() as i64;
        let end = whole.end() as i64;
        // Collect each group's optional captured text by index.
        let group_texts: Vec<Option<String>> = (0..caps.len())
            .map(|i| caps.get(i).map(|m| m.as_str().to_owned()))
            .collect();
        let mut attrs: HashMap<String, Value> = HashMap::new();
        // `.group()`/`.group(n)`/`.group(a, b, ...)`.
        let gt = group_texts.clone();
        attrs.insert(
            "group".into(),
            Value::Native(Rc::new(NativeFn::new("group", move |_i, args| {
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
                    return pick(args[0].to_int()? as usize);
                }
                let mut out = Vec::with_capacity(args.len());
                for a in &args {
                    out.push(pick(a.to_int()? as usize)?);
                }
                Ok(Value::Tuple(Rc::new(out)))
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
                    Ok(captures_to_value(caps))
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
                    Ok(captures_to_value(r.captures(&s)))
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
                    Ok(captures_to_value(r.captures(&s)))
                }),
            ),
            (
                "sub",
                nf("sub", move |_i, args| {
                    let p = args
                        .first()
                        .ok_or_else(|| type_error("sub() needs pattern"))?
                        .py_str();
                    let repl = args
                        .get(1)
                        .ok_or_else(|| type_error("sub() needs replacement"))?
                        .py_str();
                    let s = args
                        .get(2)
                        .ok_or_else(|| type_error("sub() needs string"))?
                        .py_str();
                    let r = compile_one(&p)?;
                    Ok(Value::Str(Rc::new(r.replace_all(&s, repl).into_owned())))
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
                    let r = compile_one(&p)?;
                    let parts: Vec<Value> = r
                        .split(&s)
                        .map(|p| Value::Str(Rc::new(p.to_owned())))
                        .collect();
                    Ok(Value::List(Rc::new(RefCell::new(parts))))
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
    let defaultdict = nf("defaultdict", |_i, _args| {
        // The factory argument is discarded — accessing a missing key
        // raises KeyError just like a plain dict in this shim. Users
        // that need the auto-default behaviour should fall back to
        // compile mode.
        Ok(Value::Dict(Rc::new(RefCell::new(IndexMap::new()))))
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
        Value::None
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
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
/// `exists`, `is_file`, `is_dir`, `name`, `parent`, `suffix`, `stem`,
/// and `__str__`. Internally a Path instance is an `Instance` value
/// whose `fields` map holds the resolved path string under `__path__`.
fn make_pathlib_module() -> Value {
    use crate::value::{Class, Instance};
    fn path_class() -> Rc<Class> {
        Rc::new(Class {
            name: "Path".into(),
            methods: RefCell::new(HashMap::new()),
            fields: vec![],
            class_attrs: RefCell::new(HashMap::new()),
            bases: vec![],
            properties: std::cell::RefCell::new(std::collections::HashSet::new()),
            classmethods: std::cell::RefCell::new(std::collections::HashSet::new()),
        })
    }
    fn make_path(s: String) -> Value {
        let cls = path_class();
        let mut fields: HashMap<String, Value> = HashMap::new();
        fields.insert("__path__".into(), Value::Str(Rc::new(s.clone())));
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
                    .map(|_| Value::Int(num_bigint::BigInt::from(text.len() as i64)))
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
        Value::Instance(Rc::new(Instance {
            class: cls,
            fields: RefCell::new(fields),
        }))
    }
    let path = nf("Path", |_i, args| {
        let s = args
            .first()
            .map(|v| v.py_str())
            .unwrap_or_else(|| ".".to_string());
        Ok(make_path(s))
    });
    make_module("pathlib", vec![("Path", path)])
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
            Value::Str(Rc::new(s.replace(&from, &to)))
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
        "rsplit" => {
            let maxsplit = args.get(1).and_then(|v| v.to_int().ok()).unwrap_or(-1);
            let parts: Vec<Value> = match args.first() {
                Some(v) => {
                    let sep = v.py_str();
                    let mut collected: Vec<String> = if maxsplit < 0 {
                        s.split(&sep).map(|p| p.to_owned()).collect()
                    } else {
                        s.rsplitn((maxsplit + 1) as usize, &sep)
                            .map(|p| p.to_owned())
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect()
                    };
                    collected
                        .drain(..)
                        .map(|p| Value::Str(Rc::new(p)))
                        .collect()
                }
                None => s
                    .split_whitespace()
                    .map(|p| Value::Str(Rc::new(p.to_owned())))
                    .collect(),
            };
            Value::List(Rc::new(RefCell::new(parts)))
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

                // Apply format spec if present. The default stringification
                // honours a user `__str__` (via `str_of`), matching `print` /
                // `str` and CPython's `"{}".format(obj)`.
                let default = interp.str_of(&value)?;
                let formatted = if spec.is_empty() {
                    default
                } else {
                    crate::interp::format_with_spec_pub(&value, &default, spec)?
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
                    Err(_) => {
                        return Err(value_error(
                            "'utf-8' codec can't decode byte sequence",
                        ))
                    }
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
        _ => return Err(attribute_error(format!("bytes has no method '{}'", name))),
    })
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
                if reverse {
                    keyed.reverse();
                }
                items = keyed.into_iter().map(|(_, v)| v).collect();
            } else {
                items.sort_by(|a, b| {
                    if sort_error.is_some() {
                        return std::cmp::Ordering::Equal;
                    }
                    match interp.value_cmp(a, b) {
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
                if reverse {
                    items.reverse();
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
    // Refuse mutations on a `freeze let`-tagged dict so the VM matches
    // the compile path's `MappingProxyType` semantics: CPython surfaces
    // missing-method calls as `AttributeError` (review thread copilot
    // on PR #147 — item assignment is a `TypeError`, handled in
    // `assign_target_subscript`). Read-only methods (`get`, `keys`,
    // `values`, `items`, `copy`) fall through.
    let is_mutator = matches!(
        name,
        "pop" | "update" | "setdefault" | "clear" | "popitem" | "__setitem__" | "__delitem__"
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
        "keys" => Ok(Value::List(Rc::new(RefCell::new(
            d.borrow()
                .keys()
                .filter(|k| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .cloned()
                .map(HashKey::into_value)
                .collect(),
        )))),
        "values" => Ok(Value::List(Rc::new(RefCell::new(
            d.borrow()
                .iter()
                .filter(|(k, _)| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .map(|(_, v)| v.clone())
                .collect(),
        )))),
        "items" => Ok(Value::List(Rc::new(RefCell::new(
            d.borrow()
                .iter()
                .filter(|(k, _)| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .map(|(k, v)| Value::Tuple(Rc::new(vec![k.clone().into_value(), v.clone()])))
                .collect(),
        )))),
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

fn num_method(v: &Value, name: &str, _args: &[Value]) -> Result<Value, Unwind> {
    match (v, name) {
        (Value::Float(x), "is_integer") => Ok(Value::Bool(x.fract() == 0.0 && x.is_finite())),
        (Value::Int(i), "bit_length") => Ok(Value::Int(num_bigint::BigInt::from(i.bits() as i64))),
        _ => Err(attribute_error(format!(
            "'{}' object has no method '{}'",
            v.type_name(),
            name
        ))),
    }
}

// ── JSON ───────────────────────────────────────────────────────────────────

/// `json.dumps(v, indent=n)` — pretty-printed with `n`-space indentation.
fn json_dumps_indent(v: &Value, indent: usize, level: usize) -> String {
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
                .map(|x| format!("{}{}", pad, json_dumps_indent(x, indent, level + 1)))
                .collect();
            format!("[\n{}\n{}]", body.join(",\n"), close_pad)
        }
        Value::Tuple(t) => {
            if t.is_empty() {
                return "[]".into();
            }
            let body: Vec<String> = t
                .iter()
                .map(|x| format!("{}{}", pad, json_dumps_indent(x, indent, level + 1)))
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
            let body: Vec<String> = entries
                .iter()
                .map(|(k, val)| {
                    format!(
                        "{}{}: {}",
                        pad,
                        json_dumps(&k.clone().into_value()),
                        json_dumps_indent(val, indent, level + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{}}}", body.join(",\n"), close_pad)
        }
        other => json_dumps(other),
    }
}

/// Public wrapper so `interp.rs` (`model_dump_json`) can reuse the serializer.
pub fn json_dumps_pub(v: &Value) -> String {
    json_dumps(v)
}

fn json_dumps(v: &Value) -> String {
    match v {
        Value::None => "null".into(),
        Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        Value::Int(i) => i.to_string(),
        Value::Float(x) => format!("{}", x),
        Value::Str(s) => json_string(s),
        Value::List(l) => {
            let items: Vec<String> = l.borrow().iter().map(json_dumps).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Tuple(t) => {
            let items: Vec<String> = t.iter().map(json_dumps).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Dict(d) => {
            // Filter the `__typhon_frozen__` sentinel a `freeze let`
            // inserts (review thread copilot on PR #147 — otherwise
            // `json.dump(frozen_dict, fp)` leaks the marker into the
            // emitted JSON).
            let items: Vec<String> = d
                .borrow()
                .iter()
                .filter(|(k, _)| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                .map(|(k, v)| format!("{}: {}", json_dumps(&k.clone().into_value()), json_dumps(v)))
                .collect();
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
                let cmp = vk.py_cmp(&best_key).unwrap_or(std::cmp::Ordering::Equal);
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
            if let Some(key) = key_fn {
                let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(out.len());
                for v in out {
                    let k = interp.call_value(key.clone(), vec![v.clone()], &[])?;
                    keyed.push((k, v));
                }
                keyed.sort_by(|a, b| a.0.py_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                if reverse {
                    keyed.reverse();
                }
                Ok(Value::List(Rc::new(RefCell::new(
                    keyed.into_iter().map(|(_, v)| v).collect(),
                ))))
            } else {
                out.sort_by(|a, b| a.py_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                if reverse {
                    out.reverse();
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
            for (k, v) in kwargs {
                if k == "indent" {
                    if let Value::Int(_) = v {
                        indent = v.to_int().ok().filter(|n| *n > 0).map(|n| n as usize);
                    }
                }
            }
            match indent {
                Some(n) => Ok(Value::Str(Rc::new(json_dumps_indent(obj, n, 0)))),
                None => Ok(Value::Str(Rc::new(json_dumps(obj)))),
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
                    _ => return Err(type_error(format!("groupby() got unexpected keyword: '{}'", k))),
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

/// A lightweight type object for a built-in type (`int`, `str`, …) — an empty
/// `Class` whose only meaningful attribute is its `name`. Used by `type(x)`
/// so type comparisons and `.__name__` work uniformly with user classes.
fn make_builtin_type(name: &str) -> Value {
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
