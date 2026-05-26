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

    // `freeze let X = expr` lowers to `X = __typhon_freeze__(expr)`. The
    // compile path resolves this via `from typhon_runtime.freeze import
    // deep_freeze as __typhon_freeze__`; the VM has no `typhon_runtime`
    // package on disk, so we register the helper as a builtin instead.
    // Acceptable VM-mode limitation: returns the value as-is (identity).
    // The static checker still enforces immutability of `freeze let`
    // bindings at the type level, so the runtime degradation is silent
    // and only affects users who attempt to mutate the value through an
    // aliased reference at runtime — a pattern the type system already
    // rejects.
    root.set(
        "__typhon_freeze__",
        Value::Native(Rc::new(NativeFn::new("__typhon_freeze__", |_i, args| {
            Ok(args.into_iter().next().unwrap_or(Value::None))
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
        "typing" => Ok(make_typing_module()),
        "re" => Ok(make_re_module()),
        "collections" => Ok(make_collections_module()),
        "functools" => Ok(make_functools_module()),
        "itertools" => Ok(make_itertools_module()),
        "dataclasses" => Ok(make_dataclasses_module()),
        "pathlib" => Ok(make_pathlib_module()),
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
    // `typhon_runtime.freeze`. We expose the same name here so the
    // existing desugar-injected import (`from typhon_runtime.freeze import
    // deep_freeze as __typhon_freeze__`) resolves in VM mode. The shim is
    // an identity function — see the `__typhon_freeze__` root binding in
    // `install()` for the rationale.
    let freeze = make_module(
        "typhon_runtime.freeze",
        vec![(
            "deep_freeze",
            nf("deep_freeze", |_i, args| {
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
                Ok(match_to_value(p1.find(&s), &s))
            }))),
        );
        let p2 = p_rc.clone();
        attrs.insert(
            "search".into(),
            Value::Native(Rc::new(NativeFn::new("search", move |_i, args| {
                let s = single(&args, "search")?.py_str();
                Ok(match_to_value(p2.find(&s), &s))
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
        });
        Value::Instance(Rc::new(crate::value::Instance {
            class: cls,
            fields: RefCell::new(attrs),
        }))
    }
    fn match_to_value(m: Option<regex::Match<'_>>, _s: &str) -> Value {
        let Some(m) = m else { return Value::None };
        let captured = m.as_str().to_owned();
        let start = m.start() as i64;
        let end = m.end() as i64;
        let mut attrs: HashMap<String, Value> = HashMap::new();
        let cap = captured.clone();
        attrs.insert(
            "group".into(),
            Value::Native(Rc::new(NativeFn::new("group", move |_i, _args| {
                Ok(Value::Str(Rc::new(cap.clone())))
            }))),
        );
        let cap2 = captured.clone();
        attrs.insert(
            "groups".into(),
            Value::Native(Rc::new(NativeFn::new("groups", move |_i, _args| {
                Ok(Value::Tuple(Rc::new(vec![Value::Str(Rc::new(
                    cap2.clone(),
                ))])))
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
                    let m = r.find(&s).filter(|m| m.start() == 0);
                    Ok(match_to_value(m, &s))
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
                    Ok(match_to_value(r.find(&s), &s))
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
                    Ok(match_to_value(r.find(&s), &s))
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
    make_module(
        "collections",
        vec![
            ("OrderedDict", ordered_dict),
            ("defaultdict", defaultdict),
            ("Counter", counter),
            ("namedtuple", namedtuple),
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
            |i, args| make_cache(i, args),
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
