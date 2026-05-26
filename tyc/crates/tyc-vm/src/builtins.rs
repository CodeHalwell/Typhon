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
    attribute_error, index_error, key_error, not_implemented, stop_iteration, type_error,
    value_error, Unwind,
};
use crate::interp::{normalize_index, Interpreter};
use crate::value::{DictMap, HashKey, IterState, Module, NativeFn, Value};

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
            out.push_str(&a.py_str());
        }
        println!("{}", out);
        let _ = interp;
        Ok(Value::None)
    });

    native!("len", |_i, args| {
        let v = single(&args, "len")?;
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

    native!("str", |_i, args| {
        Ok(Value::Str(Rc::new(match args.first() {
            Some(v) => v.py_str(),
            None => String::new(),
        })))
    });

    native!("int", |_i, args| {
        let v = single(&args, "int")?;
        Ok(Value::Int(v.to_bigint()?))
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
        Ok(Value::Set(Rc::new(RefCell::new(out))))
    });

    native!("repr", |_i, args| Ok(Value::Str(Rc::new(
        single(&args, "repr")?.py_repr()
    ))));

    native!("type", |_i, args| {
        let v = single(&args, "type")?;
        Ok(Value::Str(Rc::new(v.type_name().to_owned())))
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
        Some(Value::Int(i)) => Ok(Value::Int(i.clone())),
        Some(Value::Float(x)) => match args.get(1) {
            Some(n) => {
                let n = n.to_int()? as i32;
                let p = 10f64.powi(n);
                Ok(Value::Float((x * p).round() / p))
            }
            None => Ok(Value::Int(num_bigint::BigInt::from(x.round() as i64))),
        },
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
        Value::Dict(d) => d.borrow().len(),
        Value::Set(s) => s.borrow().len(),
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
        // Typhon-runtime submodules — same content reachable as attributes.
        n if n.starts_with("typhon_runtime.") => Ok(make_typhon_runtime_module(interp)),
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
    make_module(
        "typhon_runtime",
        vec![("Ok", ok), ("Err", err), ("tasks", tasks), ("lazy", lazy)],
    )
}

fn make_math_module() -> Value {
    make_module(
        "math",
        vec![
            ("pi", Value::Float(std::f64::consts::PI)),
            ("e", Value::Float(std::f64::consts::E)),
            ("inf", Value::Float(f64::INFINITY)),
            ("nan", Value::Float(f64::NAN)),
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
                "exp",
                nf("exp", |_i, args| {
                    Ok(Value::Float(single(&args, "exp")?.to_float()?.exp()))
                }),
            ),
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
            (
                "fabs",
                nf("fabs", |_i, args| {
                    Ok(Value::Float(single(&args, "fabs")?.to_float()?.abs()))
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
                nf("monotonic", |_i, _args| {
                    let t = std::time::Instant::now().elapsed().as_secs_f64();
                    Ok(Value::Float(t))
                }),
            ),
        ],
    )
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

// ── Method dispatch on built-in types ──────────────────────────────────────

pub fn method_for(_value: &Value, _attr: &str) -> Option<()> {
    // Stub — actual dispatch happens through `dispatch_method`.
    None
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
    match (&receiver, name) {
        // ── str methods ────────────────────────────────────────────────────
        (Value::Str(s), m) => str_method(interp, s, m, &args[1..]),
        // ── list methods ───────────────────────────────────────────────────
        (Value::List(l), m) => list_method(interp, l, m, &args[1..]),
        // ── dict methods ───────────────────────────────────────────────────
        (Value::Dict(d), m) => dict_method(interp, d, m, &args[1..]),
        // ── set methods ────────────────────────────────────────────────────
        (Value::Set(s), m) => set_method(s, m, &args[1..]),
        // ── tuple methods ──────────────────────────────────────────────────
        (Value::Tuple(t), m) => tuple_method(t, m, &args[1..]),
        // ── int/float/bool method calls ────────────────────────────────────
        (Value::Int(_) | Value::Float(_) | Value::Bool(_), m) => {
            num_method(&receiver, m, &args[1..])
        }
        _ => Err(attribute_error(format!(
            "'{}' object has no method '{}'",
            receiver.type_name(),
            name
        ))),
    }
}

fn str_method(
    _interp: &mut Interpreter,
    s: &Rc<String>,
    name: &str,
    args: &[Value],
) -> Result<Value, Unwind> {
    Ok(match name {
        "upper" => Value::Str(Rc::new(s.to_uppercase())),
        "lower" => Value::Str(Rc::new(s.to_lowercase())),
        "strip" => Value::Str(Rc::new(s.trim().to_owned())),
        "lstrip" => Value::Str(Rc::new(s.trim_start().to_owned())),
        "rstrip" => Value::Str(Rc::new(s.trim_end().to_owned())),
        "split" => {
            let parts: Vec<Value> = match args.first() {
                Some(v) => {
                    let sep = v.py_str();
                    s.split(&sep)
                        .map(|p| Value::Str(Rc::new(p.to_owned())))
                        .collect()
                }
                None => s
                    .split_whitespace()
                    .map(|p| Value::Str(Rc::new(p.to_owned())))
                    .collect(),
            };
            Value::List(Rc::new(RefCell::new(parts)))
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
            let it = _interp.make_iter(iterable)?;
            while let Some(v) = _interp.iter_next(&it)? {
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
        "format" => return Err(not_implemented("str.format (use f-strings)")),
        "encode" => Value::Bytes(Rc::new(s.as_bytes().to_vec())),
        _ => return Err(attribute_error(format!("str has no method '{}'", name))),
    })
}

fn list_method(
    interp: &mut Interpreter,
    l: &Rc<RefCell<Vec<Value>>>,
    name: &str,
    args: &[Value],
) -> Result<Value, Unwind> {
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
            let mut l = l.borrow_mut();
            let pos = l
                .iter()
                .position(|v| v.py_eq(&target))
                .ok_or_else(|| value_error("list.remove(x): x not in list"))?;
            l.remove(pos);
            Ok(Value::None)
        }
        "index" => {
            let target = single(args, "index")?.clone();
            let l = l.borrow();
            let pos = l
                .iter()
                .position(|v| v.py_eq(&target))
                .ok_or_else(|| value_error("list.index(x): x not in list"))?;
            Ok(Value::Int(num_bigint::BigInt::from(pos as i64)))
        }
        "count" => {
            let target = single(args, "count")?.clone();
            let l = l.borrow();
            Ok(Value::Int(num_bigint::BigInt::from(
                l.iter().filter(|v| v.py_eq(&target)).count() as i64,
            )))
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
            l.borrow_mut()
                .sort_by(|a, b| a.py_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Ok(Value::None)
        }
        "copy" => Ok(Value::List(Rc::new(RefCell::new(l.borrow().clone())))),
        _ => Err(attribute_error(format!("list has no method '{}'", name))),
    }
}

fn dict_method(
    interp: &mut Interpreter,
    d: &Rc<RefCell<DictMap>>,
    name: &str,
    args: &[Value],
) -> Result<Value, Unwind> {
    match name {
        "get" => {
            let k = single(args, "get")?.to_hash_key()?;
            let default = args.get(1).cloned().unwrap_or(Value::None);
            Ok(d.borrow().get(&k).cloned().unwrap_or(default))
        }
        "keys" => Ok(Value::List(Rc::new(RefCell::new(
            d.borrow()
                .keys()
                .cloned()
                .map(HashKey::into_value)
                .collect(),
        )))),
        "values" => Ok(Value::List(Rc::new(RefCell::new(
            d.borrow().values().cloned().collect(),
        )))),
        "items" => Ok(Value::List(Rc::new(RefCell::new(
            d.borrow()
                .iter()
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
        _ => Err(attribute_error(format!("dict has no method '{}'", name))),
    }
}

fn set_method(
    s: &Rc<RefCell<HashSet<HashKey>>>,
    name: &str,
    args: &[Value],
) -> Result<Value, Unwind> {
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
        "copy" => Ok(Value::Set(Rc::new(RefCell::new(s.borrow().clone())))),
        _ => Err(attribute_error(format!("set has no method '{}'", name))),
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
            let items: Vec<String> = d
                .borrow()
                .iter()
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

pub fn call_with_kwargs(
    interp: &mut Interpreter,
    n: &NativeFn,
    args: Vec<Value>,
    kwargs: &[(String, Value)],
) -> Result<Value, Unwind> {
    match n.name {
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
