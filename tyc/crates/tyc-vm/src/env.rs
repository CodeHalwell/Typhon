//! Lexical scope for the interpreter.
//!
//! Each `Env` has its own bindings plus a parent pointer. Function calls push
//! a fresh `Env` whose parent is the function's closure scope. `let` / `mut` /
//! plain assignments all share a single binding kind here — the source-level
//! distinction is enforced by `tyc check`, not the VM.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::value::Value;

pub type EnvRef = Rc<Env>;

pub struct Env {
    bindings: RefCell<HashMap<String, Value>>,
    /// Names declared `global NAME` in this function — assigns reach to module scope.
    globals: RefCell<HashSet<String>>,
    /// Names declared `nonlocal NAME` — assigns reach to the nearest enclosing function scope.
    nonlocals: RefCell<HashSet<String>>,
    parent: Option<EnvRef>,
    /// The module-global scope. The root env points to itself.
    module: RefCell<Option<EnvRef>>,
}

impl Env {
    pub fn new_root() -> EnvRef {
        let env = Rc::new(Env {
            bindings: RefCell::new(HashMap::new()),
            globals: RefCell::new(HashSet::new()),
            nonlocals: RefCell::new(HashSet::new()),
            parent: None,
            module: RefCell::new(None),
        });
        *env.module.borrow_mut() = Some(env.clone());
        env
    }

    pub fn new_child(parent: &EnvRef) -> EnvRef {
        Rc::new(Env {
            bindings: RefCell::new(HashMap::new()),
            globals: RefCell::new(HashSet::new()),
            nonlocals: RefCell::new(HashSet::new()),
            parent: Some(parent.clone()),
            module: RefCell::new(parent.module.borrow().clone()),
        })
    }

    pub fn module_scope(&self) -> EnvRef {
        self.module
            .borrow()
            .clone()
            .expect("env without module scope")
    }

    pub fn declare_global(&self, name: &str) {
        self.globals.borrow_mut().insert(name.to_string());
    }

    pub fn declare_nonlocal(&self, name: &str) {
        self.nonlocals.borrow_mut().insert(name.to_string());
    }

    /// Look up `name` walking parent links until the module scope.
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.bindings.borrow().get(name) {
            return Some(v.clone());
        }
        if let Some(parent) = &self.parent {
            return parent.get(name);
        }
        None
    }

    /// Bind `name = value` in this scope, honouring `global`/`nonlocal`
    /// declarations.
    pub fn set(&self, name: &str, value: Value) {
        if self.globals.borrow().contains(name) {
            self.module_scope()
                .bindings
                .borrow_mut()
                .insert(name.into(), value);
            return;
        }
        if self.nonlocals.borrow().contains(name) {
            // Walk up to the nearest enclosing scope that already binds it.
            let mut cur = self.parent.clone();
            while let Some(env) = cur {
                if env.bindings.borrow().contains_key(name) {
                    env.bindings.borrow_mut().insert(name.into(), value);
                    return;
                }
                cur = env.parent.clone();
            }
            // Fall through — Python would have errored at compile time.
        }
        self.bindings.borrow_mut().insert(name.into(), value);
    }

    /// Set `name = value` rewriting in the nearest scope that already binds
    /// `name`. Falls back to setting in the current scope. This matches
    /// Python's assignment-creates-local semantics for already-bound names
    /// in for-loop targets and walrus expressions.
    pub fn assign_or_create(&self, name: &str, value: Value) {
        // Honour explicit declarations first.
        if self.globals.borrow().contains(name) || self.nonlocals.borrow().contains(name) {
            self.set(name, value);
            return;
        }
        if self.bindings.borrow().contains_key(name) {
            self.bindings.borrow_mut().insert(name.into(), value);
            return;
        }
        self.bindings.borrow_mut().insert(name.into(), value);
    }

    pub fn delete(&self, name: &str) -> bool {
        self.bindings.borrow_mut().remove(name).is_some()
    }

    /// Iterate over all (name, value) pairs in this scope only.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        self.bindings
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
