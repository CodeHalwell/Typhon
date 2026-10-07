//! Lexical scope for the interpreter.
//!
//! Each `Env` has its own bindings plus a parent pointer. Function calls push
//! a fresh `Env` whose parent is the function's closure scope. `let` / `mut` /
//! plain assignments all share a single binding kind here — the source-level
//! distinction is enforced by `tyc check`, not the VM.
//!
//! ## Slot-resolved frames (VM performance Tier 1b)
//!
//! An *eligible* function's call frame carries a [`SlotInfo`] and a parallel
//! `Vec<Option<Value>>`: its function-local names live in the slot vector
//! (indexed, no hashing, no per-call `HashMap`) instead of `bindings`. The slot
//! table is consulted first by every binding / lookup method, so all existing
//! call sites (imports, `except` aliases, `with` / `match` captures, walrus)
//! route through it transparently. A frame's `bindings` map stays empty in the
//! common case; a name the slot analysis didn't collect simply lands there and
//! still resolves — slots only ever accelerate, never change resolution.
//!
//! An **unbound** slot (a `None` entry, e.g. read before its first assignment)
//! falls through to the enclosing scope, exactly reproducing the VM's existing
//! read-before-assign behaviour (it reads an outer binding of the same name; it
//! does *not* raise `UnboundLocalError`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ruff_python_ast::ExprName;

use crate::slots::SlotInfo;
use crate::value::Value;

pub type EnvRef = Rc<Env>;

/// See [`Env::delete_scope`].
pub enum DeleteScope {
    Global,
    Local,
    Free,
}

pub struct Env {
    bindings: RefCell<HashMap<String, Value>>,
    /// Names declared `global NAME` in this function — assigns reach to module scope.
    globals: RefCell<HashSet<String>>,
    /// Names declared `nonlocal NAME` — assigns reach to the nearest enclosing function scope.
    nonlocals: RefCell<HashSet<String>>,
    /// Plain-binding names `del` removed from this scope. They stay owned
    /// here, so a later `nonlocal` write or `del` from an inner function
    /// stops at this scope instead of reaching further out.
    deleted: RefCell<HashSet<String>>,
    /// A class body's namespace: `del` of an absent name there raises
    /// `NameError`, not `UnboundLocalError`.
    class_namespace: std::cell::Cell<bool>,
    parent: Option<EnvRef>,
    /// The module-global scope. The root env points to itself.
    module: RefCell<Option<EnvRef>>,
    /// Slot layout for an eligible function frame; `None` for module / class /
    /// comprehension / ineligible scopes (which use `bindings`).
    slot_info: Option<Rc<SlotInfo>>,
    /// Slot storage, parallel to `slot_info.slots`. Empty when `slot_info` is `None`.
    slots: RefCell<Vec<Option<Value>>>,
}

impl Env {
    pub fn new_root() -> EnvRef {
        let env = Rc::new(Env {
            bindings: RefCell::new(HashMap::new()),
            globals: RefCell::new(HashSet::new()),
            nonlocals: RefCell::new(HashSet::new()),
            deleted: RefCell::new(HashSet::new()),
            class_namespace: std::cell::Cell::new(false),
            parent: None,
            module: RefCell::new(None),
            slot_info: None,
            slots: RefCell::new(Vec::new()),
        });
        *env.module.borrow_mut() = Some(env.clone());
        env
    }

    /// A module namespace under `parent` (the root): `global NAME` inside
    /// functions defined here binds in *this* scope, not in the root — the
    /// namespace a loaded sibling module or a stdlib shim owns.
    pub fn new_module(parent: &EnvRef) -> EnvRef {
        let env = Self::new_child(parent);
        *env.module.borrow_mut() = Some(env.clone());
        env
    }

    pub fn new_child(parent: &EnvRef) -> EnvRef {
        Rc::new(Env {
            bindings: RefCell::new(HashMap::new()),
            globals: RefCell::new(HashSet::new()),
            nonlocals: RefCell::new(HashSet::new()),
            deleted: RefCell::new(HashSet::new()),
            class_namespace: std::cell::Cell::new(false),
            parent: Some(parent.clone()),
            module: RefCell::new(parent.module.borrow().clone()),
            slot_info: None,
            slots: RefCell::new(Vec::new()),
        })
    }

    /// Create a slot-resolved call frame: a child of `closure` whose
    /// function-local names are backed by an indexed slot vector rather than
    /// the `bindings` HashMap. Used only for slot-eligible functions.
    pub fn new_frame(closure: &EnvRef, slot_info: Rc<SlotInfo>) -> EnvRef {
        let n = slot_info.slot_count();
        Rc::new(Env {
            bindings: RefCell::new(HashMap::new()),
            globals: RefCell::new(HashSet::new()),
            nonlocals: RefCell::new(HashSet::new()),
            deleted: RefCell::new(HashSet::new()),
            class_namespace: std::cell::Cell::new(false),
            parent: Some(closure.clone()),
            module: RefCell::new(closure.module.borrow().clone()),
            slot_info: Some(slot_info),
            slots: RefCell::new(vec![None; n]),
        })
    }

    /// Mark this env as a class body's namespace.
    pub fn mark_class_namespace(&self) {
        self.class_namespace.set(true);
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

    /// Whether `name` is declared `global` or `nonlocal` in this scope.
    pub fn declared_outer(&self, name: &str) -> bool {
        self.globals.borrow().contains(name) || self.nonlocals.borrow().contains(name)
    }

    /// Look up `name` in this scope only (no parent walk).
    pub fn get_own(&self, name: &str) -> Option<Value> {
        if let Some(info) = &self.slot_info {
            if let Some(k) = info.slot_of_name(name) {
                return self.slots.borrow()[k as usize].clone();
            }
        }
        self.bindings.borrow().get(name).cloned()
    }

    /// Look up `name` walking parent links until the module scope.
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(info) = &self.slot_info {
            if let Some(k) = info.slot_of_name(name) {
                if let Some(v) = &self.slots.borrow()[k as usize] {
                    return Some(v.clone());
                }
                // A local unbound by `del` stays unbound: CPython raises
                // `UnboundLocalError` rather than reading an outer name.
                if self.deleted_local(name) {
                    return None;
                }
                // Unbound slot (read-before-assign) → consult enclosing scopes,
                // matching the VM's existing fall-through behaviour.
                return self.parent.as_ref().and_then(|p| p.get(name));
            }
            // Free variable: `bindings` is normally empty for a frame, but a
            // name the analysis missed lands there, so it must still be checked.
        }
        if let Some(v) = self.bindings.borrow().get(name) {
            return Some(v.clone());
        }
        if self.deleted_local(name) {
            return None;
        }
        if let Some(parent) = &self.parent {
            return parent.get(name);
        }
        None
    }

    /// Whether `name` is a function local of this scope that a `del` has
    /// unbound (and nothing has rebound since). A module or class scope's
    /// deleted name still falls through to the builtins, as in CPython.
    pub fn deleted_local(&self, name: &str) -> bool {
        let deleted = self.deleted.borrow();
        !deleted.is_empty()
            && deleted.contains(name)
            && !self.bindings.borrow().contains_key(name)
            && matches!(self.delete_scope(name), DeleteScope::Local)
    }

    /// Whether reading `name` from this scope hits a local unbound by `del`
    /// — the read raises `UnboundLocalError` instead of `NameError`.
    pub fn reads_deleted_local(&self, name: &str) -> bool {
        let mut cur: Option<&Env> = Some(self);
        while let Some(env) = cur {
            if env.get_own(name).is_some() {
                return false;
            }
            if env.deleted_local(name) {
                return true;
            }
            cur = env.parent.as_deref();
        }
        false
    }

    /// Read a `Name` node — the hot expression-lookup path. Uses the node-index
    /// slot cache to avoid hashing when this env is the owning frame; otherwise
    /// identical to [`Env::get`].
    pub fn get_name_node(&self, n: &ExprName) -> Option<Value> {
        if let Some(info) = &self.slot_info {
            if let Some(k) = info.slot_of_node(n) {
                if let Some(v) = &self.slots.borrow()[k as usize] {
                    return Some(v.clone());
                }
                if self.deleted_local(n.id.as_str()) {
                    return None;
                }
                return self.parent.as_ref().and_then(|p| p.get(n.id.as_str()));
            }
            let name = n.id.as_str();
            if !self.bindings.borrow().is_empty() {
                if let Some(v) = self.bindings.borrow().get(name) {
                    return Some(v.clone());
                }
            }
            if self.deleted_local(name) {
                return None;
            }
            return self.parent.as_ref().and_then(|p| p.get(name));
        }
        self.get(n.id.as_str())
    }

    /// Bind `name = value` in this scope, honouring `global` / `nonlocal`
    /// declarations and the slot table.
    pub fn set(&self, name: &str, value: Value) {
        if self.globals.borrow().contains(name) {
            self.module_scope()
                .bindings
                .borrow_mut()
                .insert(name.into(), value);
            return;
        }
        if self.nonlocals.borrow().contains(name) {
            // Walk up to the nearest enclosing scope that already binds it —
            // as a slot or as a plain binding.
            let mut cur = self.parent.clone();
            while let Some(env) = cur {
                if let Some(info) = &env.slot_info {
                    if let Some(k) = info.slot_of_name(name) {
                        env.slots.borrow_mut()[k as usize] = Some(value);
                        return;
                    }
                }
                if env.owns_binding(name) {
                    env.bind_here(name, value);
                    return;
                }
                cur = env.parent.clone();
            }
            // Fall through — Python would have errored at compile time.
        }
        if let Some(info) = &self.slot_info {
            if let Some(k) = info.slot_of_name(name) {
                self.slots.borrow_mut()[k as usize] = Some(value);
                return;
            }
        }
        self.bind_here(name, value);
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
        if let Some(info) = &self.slot_info {
            if let Some(k) = info.slot_of_name(name) {
                self.slots.borrow_mut()[k as usize] = Some(value);
                return;
            }
        }
        self.bind_here(name, value);
    }

    /// Store into a `Name` node target — the hot assignment path. Uses the
    /// node-index slot cache to avoid hashing when this env is the owning
    /// frame; otherwise identical to [`Env::assign_or_create`].
    pub fn store_name_node(&self, n: &ExprName, value: Value) {
        if let Some(info) = &self.slot_info {
            if let Some(k) = info.slot_of_node(n) {
                self.slots.borrow_mut()[k as usize] = Some(value);
                return;
            }
        }
        self.assign_or_create(n.id.as_str(), value);
    }

    /// Unbind `name` (`del name`), honouring `global` / `nonlocal`
    /// declarations the way [`Env::set`] does. Returns whether it was bound.
    pub fn delete(&self, name: &str) -> bool {
        if self.globals.borrow().contains(name) {
            return self.module_scope().delete_here(name);
        }
        if self.nonlocals.borrow().contains(name) {
            // The enclosing scope `nonlocal` names is the nearest one that
            // owns the name — as a slot even when it is currently unbound,
            // so a second `del` fails there instead of reaching further out.
            let mut cur = self.parent.clone();
            while let Some(env) = cur {
                let owns_slot = env
                    .slot_info
                    .as_ref()
                    .is_some_and(|info| info.slot_of_name(name).is_some());
                if owns_slot || env.owns_binding(name) {
                    return env.delete_here(name);
                }
                cur = env.parent.clone();
            }
            return false;
        }
        self.delete_here(name)
    }

    /// Which kind of scope a `del` of `name` resolves in, for the error an
    /// unbound name raises: a declared global or a module-level name gives
    /// `NameError`, a function local `UnboundLocalError`, a `nonlocal` the
    /// free-variable `NameError`.
    pub fn delete_scope(&self, name: &str) -> DeleteScope {
        if self.nonlocals.borrow().contains(name) {
            DeleteScope::Free
        } else if self.globals.borrow().contains(name)
            || self.class_namespace.get()
            || std::ptr::eq(self, Rc::as_ptr(&self.module_scope()))
        {
            DeleteScope::Global
        } else {
            DeleteScope::Local
        }
    }

    fn delete_here(&self, name: &str) -> bool {
        if let Some(info) = &self.slot_info {
            if let Some(k) = info.slot_of_name(name) {
                let existed = {
                    let mut slots = self.slots.borrow_mut();
                    let existed = slots[k as usize].is_some();
                    slots[k as usize] = None;
                    existed
                };
                // Remembered so a later read does not fall through to an
                // enclosing scope's binding of the same name.
                if existed {
                    self.deleted.borrow_mut().insert(name.to_string());
                }
                return existed;
            }
        }
        let existed = self.bindings.borrow_mut().remove(name).is_some();
        if existed {
            self.deleted.borrow_mut().insert(name.to_string());
        }
        existed
    }

    /// Whether `name` is a plain binding of this scope — bound now, or
    /// unbound by a `del` since.
    fn owns_binding(&self, name: &str) -> bool {
        self.bindings.borrow().contains_key(name) || self.deleted.borrow().contains(name)
    }

    /// Insert a plain binding, clearing any `del` tombstone for it.
    fn bind_here(&self, name: &str, value: Value) {
        {
            let mut deleted = self.deleted.borrow_mut();
            if !deleted.is_empty() {
                deleted.remove(name);
            }
        }
        self.bindings.borrow_mut().insert(name.into(), value);
    }

    /// Iterate over all (name, value) pairs in this scope only — both plain
    /// bindings and bound slots.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        let mut out: Vec<(String, Value)> = self
            .bindings
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(info) = &self.slot_info {
            let slots = self.slots.borrow();
            for (i, name) in info.slots.iter().enumerate() {
                if let Some(v) = &slots[i] {
                    out.push((name.clone(), v.clone()));
                }
            }
        }
        out
    }
}
