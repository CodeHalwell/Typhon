//! Value representation for the Typhon VM.
//!
//! Values are reference-counted (single-threaded `Rc`) so cheap clones don't
//! deep-copy containers — matching Python semantics where `a = b` aliases
//! mutable containers.
//!
//! Numeric ints use `i64` for v1. Overflow falls through to `i64::checked_*`
//! and surfaces as an `OverflowError`. A future `num-bigint` upgrade is
//! straightforward — most call sites already go through `Value::int_add` etc.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::error::{type_error, value_error, Unwind};
use ruff_python_ast::{Parameters, Stmt};

/// Reference-counted, interior-mutable list. Cloning a `Value::List` aliases
/// the same storage.
pub type RcList = Rc<RefCell<Vec<Value>>>;
pub type RcDict = Rc<RefCell<HashMap<HashKey, Value>>>;
pub type RcSet = Rc<RefCell<std::collections::HashSet<HashKey>>>;
pub type RcStr = Rc<String>;

/// Hashable wrapper around a subset of `Value`s. Used as dict keys and set
/// elements. Floats are stored bitwise so `NaN != NaN` (matching Python).
#[derive(Debug, Clone)]
pub enum HashKey {
    None,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(RcStr),
    Tuple(Rc<Vec<HashKey>>),
}

impl HashKey {
    pub fn into_value(self) -> Value {
        match self {
            HashKey::None => Value::None,
            HashKey::Bool(b) => Value::Bool(b),
            HashKey::Int(i) => Value::Int(i),
            HashKey::Float(bits) => Value::Float(f64::from_bits(bits)),
            HashKey::Str(s) => Value::Str(s),
            HashKey::Tuple(items) => Value::Tuple(Rc::new(
                items.iter().cloned().map(HashKey::into_value).collect(),
            )),
        }
    }
}

impl PartialEq for HashKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (HashKey::None, HashKey::None) => true,
            (HashKey::Bool(a), HashKey::Bool(b)) => a == b,
            // Python: bool ↔ int comparison shares hash slot.
            (HashKey::Bool(a), HashKey::Int(b)) | (HashKey::Int(b), HashKey::Bool(a)) => {
                (*a as i64) == *b
            }
            (HashKey::Int(a), HashKey::Int(b)) => a == b,
            (HashKey::Float(a), HashKey::Float(b)) => a == b,
            (HashKey::Str(a), HashKey::Str(b)) => a == b,
            (HashKey::Tuple(a), HashKey::Tuple(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for HashKey {}

impl std::hash::Hash for HashKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            HashKey::None => 0u8.hash(state),
            // bool/int collide intentionally — Python's `hash(True) == hash(1)`.
            HashKey::Bool(b) => (*b as i64).hash(state),
            HashKey::Int(i) => i.hash(state),
            HashKey::Float(bits) => bits.hash(state),
            HashKey::Str(s) => s.hash(state),
            HashKey::Tuple(items) => items.hash(state),
        }
    }
}

#[derive(Clone)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(RcStr),
    Bytes(Rc<Vec<u8>>),
    List(RcList),
    Tuple(Rc<Vec<Value>>),
    Dict(RcDict),
    Set(RcSet),
    Range {
        start: i64,
        stop: i64,
        step: i64,
    },
    /// Native (Rust) callable. Receives positional args; keyword args handled
    /// by the call-site builtin if applicable.
    Native(Rc<NativeFn>),
    Function(Rc<Function>),
    BoundMethod {
        receiver: Box<Value>,
        function: Rc<Function>,
    },
    Class(Rc<Class>),
    Instance(Rc<Instance>),
    /// `typhon_runtime.Ok(value)` — native ADT variant for the `?` operator.
    ResultOk(Box<Value>),
    /// `typhon_runtime.Err(error)`.
    ResultErr(Box<Value>),
    /// A module — a namespace dictionary.
    Module(Rc<Module>),
    /// An exception instance — held when a Python-style `except X as e` binds it.
    Exception {
        kind: RcStr,
        message: RcStr,
    },
    /// Iterator state — opaque to the AST walker; consumed by `next`.
    Iter(Rc<RefCell<IterState>>),
}

pub type NativeFnImpl =
    dyn Fn(&mut crate::interp::Interpreter, Vec<Value>) -> Result<Value, Unwind>;

pub struct NativeFn {
    pub name: &'static str,
    pub func: Box<NativeFnImpl>,
}

impl NativeFn {
    pub fn new<F>(name: &'static str, f: F) -> Self
    where
        F: Fn(&mut crate::interp::Interpreter, Vec<Value>) -> Result<Value, Unwind> + 'static,
    {
        NativeFn {
            name,
            func: Box::new(f),
        }
    }
}

pub struct Function {
    pub name: String,
    pub params: Box<Parameters>,
    pub body: Rc<Vec<Stmt>>,
    /// Default values for non-variadic params, evaluated at def-time and
    /// stored in source order matching `iter_non_variadic_params`.
    pub defaults: Vec<Option<Value>>,
    /// Closure scope captured at def-time.
    pub closure: crate::env::EnvRef,
    pub is_async: bool,
}

pub struct Class {
    pub name: String,
    /// Method table — looked up on instance attribute access.
    pub methods: RefCell<HashMap<String, Rc<Function>>>,
    /// Annotated field names, in source order. Used to synthesise `__init__`
    /// when none was defined.
    pub fields: Vec<ClassField>,
    /// Class-level attributes (constants, defaults pulled out of class body).
    pub class_attrs: RefCell<HashMap<String, Value>>,
    /// Base classes in MRO order (after head). For v1 we only walk the chain
    /// for method lookup; we don't compute C3 linearisation.
    pub bases: Vec<Rc<Class>>,
}

#[derive(Clone)]
pub struct ClassField {
    pub name: String,
    pub default: Option<Value>,
}

pub struct Instance {
    pub class: Rc<Class>,
    pub fields: RefCell<HashMap<String, Value>>,
}

pub struct Module {
    pub name: String,
    pub members: RefCell<HashMap<String, Value>>,
}

pub enum IterState {
    Range {
        current: i64,
        stop: i64,
        step: i64,
    },
    List {
        items: RcList,
        index: usize,
    },
    Tuple {
        items: Rc<Vec<Value>>,
        index: usize,
    },
    Str {
        chars: Vec<char>,
        index: usize,
    },
    Dict {
        keys: Vec<HashKey>,
        index: usize,
    },
    Set {
        keys: Vec<HashKey>,
        index: usize,
    },
    Enumerate {
        inner: Rc<RefCell<IterState>>,
        index: i64,
    },
    Zip {
        inners: Vec<Rc<RefCell<IterState>>>,
    },
    Map {
        func: Value,
        inner: Rc<RefCell<IterState>>,
    },
    Filter {
        func: Value,
        inner: Rc<RefCell<IterState>>,
    },
}

// ── Debug / display ────────────────────────────────────────────────────────

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::None => write!(f, "None"),
            Value::Bool(b) => write!(f, "{b:?}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Str(s) => write!(f, "{:?}", s.as_str()),
            Value::Bytes(b) => write!(f, "b{:?}", &b[..]),
            Value::List(l) => write!(f, "{:?}", l.borrow()),
            Value::Tuple(t) => write!(f, "{:?}", &t[..]),
            Value::Dict(d) => {
                write!(f, "{{")?;
                let d = d.borrow();
                for (i, (k, v)) in d.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:?}: {:?}", k.clone().into_value(), v)?;
                }
                write!(f, "}}")
            }
            Value::Set(_) => write!(f, "<set>"),
            Value::Range { start, stop, step } => {
                write!(f, "range({start}, {stop}, {step})")
            }
            Value::Native(n) => write!(f, "<built-in function {}>", n.name),
            Value::Function(func) => write!(f, "<function {}>", func.name),
            Value::BoundMethod { function, .. } => {
                write!(f, "<bound method {}>", function.name)
            }
            Value::Class(c) => write!(f, "<class {}>", c.name),
            Value::Instance(i) => write!(f, "<{} instance>", i.class.name),
            Value::ResultOk(v) => write!(f, "Ok({:?})", v),
            Value::ResultErr(v) => write!(f, "Err({:?})", v),
            Value::Module(m) => write!(f, "<module {}>", m.name),
            Value::Exception { kind, message } => {
                if message.is_empty() {
                    write!(f, "{kind}()")
                } else {
                    write!(f, "{kind}({:?})", message.as_str())
                }
            }
            Value::Iter(_) => write!(f, "<iterator>"),
        }
    }
}

// ── Conversion / introspection helpers ────────────────────────────────────

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::None => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::Bytes(_) => "bytes",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Dict(_) => "dict",
            Value::Set(_) => "set",
            Value::Range { .. } => "range",
            Value::Native(_) | Value::Function(_) | Value::BoundMethod { .. } => "function",
            Value::Class(_) => "type",
            // Don't leak the class name into a `'static str`. Callers that
            // need the specific class name read `instance.class.name`
            // directly; everywhere else `"instance"` is descriptive enough
            // for an error message.
            Value::Instance(_) => "instance",
            Value::ResultOk(_) => "Ok",
            Value::ResultErr(_) => "Err",
            Value::Module(_) => "module",
            Value::Exception { .. } => "Exception",
            Value::Iter(_) => "iterator",
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(x) => *x != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::List(l) => !l.borrow().is_empty(),
            Value::Tuple(t) => !t.is_empty(),
            Value::Dict(d) => !d.borrow().is_empty(),
            Value::Set(s) => !s.borrow().is_empty(),
            Value::Range { start, stop, step } => {
                if *step > 0 {
                    stop > start
                } else if *step < 0 {
                    stop < start
                } else {
                    false
                }
            }
            _ => true,
        }
    }

    /// Convert to a `HashKey`, failing for unhashable values (lists, dicts,
    /// sets, instances without `__hash__`).
    pub fn to_hash_key(&self) -> Result<HashKey, Unwind> {
        match self {
            Value::None => Ok(HashKey::None),
            Value::Bool(b) => Ok(HashKey::Bool(*b)),
            Value::Int(i) => Ok(HashKey::Int(*i)),
            Value::Float(x) => Ok(HashKey::Float(x.to_bits())),
            Value::Str(s) => Ok(HashKey::Str(s.clone())),
            Value::Tuple(items) => {
                let mut keys = Vec::with_capacity(items.len());
                for v in items.iter() {
                    keys.push(v.to_hash_key()?);
                }
                Ok(HashKey::Tuple(Rc::new(keys)))
            }
            other => Err(type_error(format!(
                "unhashable type: '{}'",
                other.type_name()
            ))),
        }
    }

    /// Python-style equality. Unlike `PartialEq` we cross between `int` and
    /// `float`, and between `bool` and the numeric types.
    pub fn py_eq(&self, other: &Value) -> bool {
        use Value::*;
        match (self, other) {
            (None, None) => true,
            (Bool(a), Bool(b)) => a == b,
            (Bool(a), Int(b)) | (Int(b), Bool(a)) => (*a as i64) == *b,
            (Bool(a), Float(b)) | (Float(b), Bool(a)) => (*a as i64 as f64) == *b,
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Int(a), Float(b)) | (Float(b), Int(a)) => (*a as f64) == *b,
            (Str(a), Str(b)) => a == b,
            (Bytes(a), Bytes(b)) => a == b,
            (List(a), List(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.py_eq(y))
            }
            (Tuple(a), Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.py_eq(y))
            }
            (Dict(a), Dict(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                if a.len() != b.len() {
                    return false;
                }
                a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.py_eq(w)))
            }
            (ResultOk(a), ResultOk(b)) => a.py_eq(b),
            (ResultErr(a), ResultErr(b)) => a.py_eq(b),
            _ => false,
        }
    }

    /// Python-style ordering. Returns None for incomparable types.
    pub fn py_cmp(&self, other: &Value) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering::*;
        use Value::*;
        match (self, other) {
            (Int(a), Int(b)) => a.partial_cmp(b),
            (Float(a), Float(b)) => a.partial_cmp(b),
            (Int(a), Float(b)) => (*a as f64).partial_cmp(b),
            (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)),
            (Bool(a), Bool(b)) => a.partial_cmp(b),
            (Bool(a), Int(b)) => (*a as i64).partial_cmp(b),
            (Int(a), Bool(b)) => a.partial_cmp(&(*b as i64)),
            (Str(a), Str(b)) => a.partial_cmp(b),
            (Tuple(a), Tuple(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    match x.py_cmp(y)? {
                        Equal => continue,
                        ord => return Some(ord),
                    }
                }
                a.len().partial_cmp(&b.len())
            }
            (List(a), List(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                for (x, y) in a.iter().zip(b.iter()) {
                    match x.py_cmp(y)? {
                        Equal => continue,
                        ord => return Some(ord),
                    }
                }
                a.len().partial_cmp(&b.len())
            }
            _ => Option::None,
        }
    }

    pub fn to_int(&self) -> Result<i64, Unwind> {
        match self {
            Value::Int(i) => Ok(*i),
            Value::Bool(b) => Ok(*b as i64),
            Value::Float(x) => Ok(*x as i64),
            Value::Str(s) => s
                .trim()
                .parse::<i64>()
                .map_err(|_| value_error(format!("invalid literal for int(): {:?}", s.as_str()))),
            _ => Err(type_error(format!(
                "int() argument must be a string or a number, not '{}'",
                self.type_name()
            ))),
        }
    }

    pub fn to_float(&self) -> Result<f64, Unwind> {
        match self {
            Value::Float(x) => Ok(*x),
            Value::Int(i) => Ok(*i as f64),
            Value::Bool(b) => Ok(*b as i64 as f64),
            Value::Str(s) => s.trim().parse::<f64>().map_err(|_| {
                value_error(format!(
                    "could not convert string to float: {:?}",
                    s.as_str()
                ))
            }),
            _ => Err(type_error(format!(
                "float() argument must be a string or a number, not '{}'",
                self.type_name()
            ))),
        }
    }

    /// Python-style `str(x)` — readable representation.
    pub fn py_str(&self) -> String {
        match self {
            Value::None => "None".into(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            Value::Int(i) => i.to_string(),
            Value::Float(x) => format_float(*x),
            Value::Str(s) => (**s).clone(),
            Value::Bytes(b) => format!("b{:?}", String::from_utf8_lossy(b)),
            Value::List(l) => {
                let l = l.borrow();
                let mut s = String::from("[");
                for (i, v) in l.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&v.py_repr());
                }
                s.push(']');
                s
            }
            Value::Tuple(t) => {
                let mut s = String::from("(");
                for (i, v) in t.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&v.py_repr());
                }
                if t.len() == 1 {
                    s.push(',');
                }
                s.push(')');
                s
            }
            Value::Dict(d) => {
                let d = d.borrow();
                let mut s = String::from("{");
                for (i, (k, v)) in d.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&k.clone().into_value().py_repr());
                    s.push_str(": ");
                    s.push_str(&v.py_repr());
                }
                s.push('}');
                s
            }
            Value::Set(set) => {
                let s = set.borrow();
                if s.is_empty() {
                    return "set()".into();
                }
                let mut out = String::from("{");
                for (i, k) in s.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&k.clone().into_value().py_repr());
                }
                out.push('}');
                out
            }
            Value::Range { start, stop, step } => {
                if *step == 1 {
                    format!("range({}, {})", start, stop)
                } else {
                    format!("range({}, {}, {})", start, stop, step)
                }
            }
            Value::Native(n) => format!("<built-in function {}>", n.name),
            Value::Function(func) => format!("<function {}>", func.name),
            Value::BoundMethod { function, .. } => format!("<bound method {}>", function.name),
            Value::Class(c) => format!("<class '{}'>", c.name),
            Value::Instance(i) => format!("<{} instance>", i.class.name),
            Value::ResultOk(v) => format!("Ok({})", v.py_repr()),
            Value::ResultErr(v) => format!("Err({})", v.py_repr()),
            Value::Module(m) => format!("<module '{}'>", m.name),
            Value::Exception { kind, message } => {
                if message.is_empty() {
                    format!("{kind}()")
                } else {
                    (**message).clone()
                }
            }
            Value::Iter(_) => "<iterator>".into(),
        }
    }

    /// Python-style `repr(x)`. Differs from `py_str` for strings — adds quotes.
    pub fn py_repr(&self) -> String {
        match self {
            Value::Str(s) => format!("{:?}", s.as_str()),
            other => other.py_str(),
        }
    }
}

fn format_float(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == x.trunc() && x.abs() < 1e16 {
        // Python prints integral floats with a trailing `.0`.
        return format!("{:.1}", x);
    }
    let s = format!("{}", x);
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{}.0", s)
    }
}
