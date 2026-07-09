//! Slot-resolved locals — VM performance Tier 1b.
//!
//! For *eligible* functions the interpreter binds and resolves every
//! function-local name through a per-frame `Vec<Option<Value>>` indexed by a
//! precomputed slot table, instead of the classic per-call
//! `HashMap<String, Value>`. This removes the per-call map allocation, the
//! per-write `String` key allocation, and — via a node-index cache stamped into
//! the (VM-private) AST clone — the per-read name hash.
//!
//! A function is **slot-eligible** iff its body contains none of: `global` /
//! `nonlocal` declarations (they bind an outer scope); a nested `def` /
//! `lambda` (a closure could capture our frame); a comprehension (the VM
//! evaluates list/set/dict/generator comprehensions in a child `Env` that
//! reaches our locals through the parent chain); or a nested `class` (its
//! body/methods form a nested scope). Anything read but never bound is a *free
//! variable* and resolves through the closure / module / builtins chain exactly
//! as before.
//!
//! Correctness is preserved by construction. Ineligible functions keep the
//! exact `Env`-HashMap path, byte-for-byte. The slot table lives *inside* the
//! frame `Env`, so every existing binding site (`Env::set` / `assign_or_create`
//! / `delete`, used by imports, `except` aliases, `with` / `match` captures,
//! walrus, …) routes to slots transparently — a binding the analysis fails to
//! collect simply stays in the `Env` HashMap and still resolves correctly, so
//! the slot table only ever *accelerates*, never changes name resolution. An
//! unbound slot read falls through to the enclosing scope, exactly reproducing
//! the VM's current read-before-assign behaviour (the VM does *not* raise
//! `UnboundLocalError` — see the crate tests).

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{
    ExceptHandler, Expr, ExprContext, ExprName, NodeIndex, Parameters, Pattern, Stmt,
};

/// Per-function slot layout + eligibility. Computed once when the `Function`
/// value is built and shared immutably (behind an `Rc`) across every call.
#[derive(Debug)]
pub struct SlotInfo {
    /// Whether this function may use the slot-resolved fast path at all.
    pub eligible: bool,
    /// Slot index → local name (parameters first, then other locals in
    /// discovery order). Empty when ineligible.
    pub slots: Vec<String>,
    /// Local name → slot index.
    pub name_to_slot: HashMap<String, u32>,
}

impl SlotInfo {
    /// An ineligible function: the interpreter uses the classic `Env` path.
    pub fn ineligible() -> Self {
        SlotInfo {
            eligible: false,
            slots: Vec::new(),
            name_to_slot: HashMap::new(),
        }
    }

    /// Number of slots a frame for this function must allocate.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Resolve a `Name` node from *this* function's body to its slot index, if
    /// it names a function-local. The stamped node index gives an O(1) hit with
    /// no hashing; a miss (an un-stamped or synthesised node) falls back to the
    /// name map, so resolution is always correct — only sometimes slower.
    #[inline]
    pub fn slot_of_node(&self, n: &ExprName) -> Option<u32> {
        if let Some(k) = n.node_index.load().as_u32() {
            // Slot indices are the only values ever stamped onto a body Name,
            // and always within range; a stray index falls back to the map.
            if (k as usize) < self.slots.len() {
                return Some(k);
            }
        }
        self.name_to_slot.get(n.id.as_str()).copied()
    }

    /// Resolve a slot purely by name — used by the `Env` binding methods, which
    /// don't carry the AST node (imports, `except` / `match` captures, walrus).
    #[inline]
    pub fn slot_of_name(&self, name: &str) -> Option<u32> {
        self.name_to_slot.get(name).copied()
    }

    /// Analyse a function's parameters + body. When eligible, stamps each
    /// slot-local `Name` node's index with its slot (interior mutability on the
    /// VM-private body clone) so eval-time resolution is a single atomic load.
    ///
    /// `body` must be the exact `Rc`'d slice the interpreter will later walk —
    /// the stamped node indices are read back from those same nodes at eval
    /// time.
    pub fn analyze(params: &Parameters, body: &[Stmt]) -> Self {
        let mut collector = Collector::new();
        // Parameters (including `*args` / `**kwargs`) are always the first slots.
        for p in params.iter() {
            collector.add(p.name().as_str());
        }
        for stmt in body {
            collector.visit_stmt(stmt);
        }
        if !collector.eligible {
            return SlotInfo::ineligible();
        }
        let slots = collector.names;
        let mut name_to_slot = HashMap::with_capacity(slots.len());
        for (i, name) in slots.iter().enumerate() {
            name_to_slot.insert(name.clone(), i as u32);
        }
        // Stamp node indices for the O(1) read/write fast path.
        let mut indexer = Indexer { map: &name_to_slot };
        for stmt in body {
            indexer.visit_stmt(stmt);
        }
        SlotInfo {
            eligible: true,
            slots,
            name_to_slot,
        }
    }
}

/// First pass: decide eligibility and collect every binding-position local
/// name (parameters are added by the caller before the walk).
struct Collector {
    eligible: bool,
    names: Vec<String>,
    seen: HashSet<String>,
}

impl Collector {
    fn new() -> Self {
        Collector {
            eligible: true,
            names: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn add(&mut self, name: &str) {
        if self.seen.insert(name.to_string()) {
            self.names.push(name.to_string());
        }
    }
}

impl<'a> Visitor<'a> for Collector {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            // Constructs that introduce a nested scope or reach an outer scope
            // disqualify the whole function. Don't descend — eligibility is
            // already lost and the nested scope's names are not ours.
            Stmt::Global(_) | Stmt::Nonlocal(_) | Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {
                self.eligible = false;
            }
            Stmt::Import(im) => {
                for alias in &im.names {
                    // `import a` / `import a.b` binds root `a`; `... as c` binds `c`.
                    let bound = match &alias.asname {
                        Some(n) => n.as_str().to_string(),
                        None => alias
                            .name
                            .as_str()
                            .split('.')
                            .next()
                            .unwrap_or("")
                            .to_string(),
                    };
                    if !bound.is_empty() {
                        self.add(&bound);
                    }
                }
            }
            Stmt::ImportFrom(im) => {
                for alias in &im.names {
                    let bound = alias
                        .asname
                        .as_ref()
                        .map(|n| n.as_str())
                        .unwrap_or_else(|| alias.name.as_str());
                    self.add(bound);
                }
            }
            // `del NAME` unbinds; it doesn't introduce a new local (the target
            // carries `ExprContext::Del`, not `Store`).
            _ => visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Lambda(_)
            | Expr::ListComp(_)
            | Expr::SetComp(_)
            | Expr::DictComp(_)
            | Expr::Generator(_) => {
                self.eligible = false;
            }
            Expr::Name(n) => {
                // A store-context bare name is a binding position (assignment,
                // aug/annotated assign, `for` / `with` target, tuple unpack,
                // walrus target).
                if matches!(n.ctx, ExprContext::Store) {
                    self.add(n.id.as_str());
                }
            }
            _ => visitor::walk_expr(self, expr),
        }
    }

    fn visit_except_handler(&mut self, handler: &'a ExceptHandler) {
        let ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.add(name.as_str());
        }
        visitor::walk_except_handler(self, handler);
    }

    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        match pattern {
            Pattern::MatchAs(a) => {
                if let Some(name) = &a.name {
                    self.add(name.as_str());
                }
            }
            Pattern::MatchStar(s) => {
                if let Some(name) = &s.name {
                    self.add(name.as_str());
                }
            }
            Pattern::MatchMapping(m) => {
                if let Some(rest) = &m.rest {
                    self.add(rest.as_str());
                }
            }
            _ => {}
        }
        visitor::walk_pattern(self, pattern);
    }
}

/// Second pass: stamp each slot-local `Name` node with its slot index so that
/// eval-time resolution is a single atomic load. Runs only on eligible
/// functions, which by construction contain no nested scopes — but the nested
/// guards are kept as defence in depth.
struct Indexer<'m> {
    map: &'m HashMap<String, u32>,
}

impl<'a, 'm> Visitor<'a> for Indexer<'m> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            _ => visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Lambda(_)
            | Expr::ListComp(_)
            | Expr::SetComp(_)
            | Expr::DictComp(_)
            | Expr::Generator(_) => {}
            Expr::Name(n) => {
                if let Some(&k) = self.map.get(n.id.as_str()) {
                    n.node_index.set(NodeIndex::from(k));
                }
            }
            _ => visitor::walk_expr(self, expr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Analyse the first top-level function definition in `src`.
    fn analyze_first_fn(src: &str) -> SlotInfo {
        let parsed = tyc_syntax::parse_module(src).expect("parse");
        let module = parsed.into_syntax();
        for stmt in &module.body {
            if let Stmt::FunctionDef(f) = stmt {
                return SlotInfo::analyze(&f.parameters, &f.body);
            }
        }
        panic!("no function definition found");
    }

    fn has_slot(info: &SlotInfo, name: &str) -> bool {
        info.name_to_slot.contains_key(name)
    }

    #[test]
    fn params_and_locals_are_slots() {
        let info = analyze_first_fn(
            "def f(a, b=1, *args, **kwargs):\n    x = a\n    y = b\n    return x + y\n",
        );
        assert!(info.eligible);
        for n in ["a", "b", "args", "kwargs", "x", "y"] {
            assert!(has_slot(&info, n), "missing slot {n}");
        }
    }

    #[test]
    fn global_is_ineligible() {
        let info = analyze_first_fn("def f():\n    global g\n    g = 1\n");
        assert!(!info.eligible);
    }

    #[test]
    fn nonlocal_is_ineligible() {
        let info = analyze_first_fn("def f():\n    nonlocal n\n    n = 1\n");
        assert!(!info.eligible);
    }

    #[test]
    fn nested_def_is_ineligible() {
        let info = analyze_first_fn("def f():\n    def g():\n        return 1\n    return g\n");
        assert!(!info.eligible);
    }

    #[test]
    fn lambda_is_ineligible() {
        let info = analyze_first_fn("def f():\n    h = lambda z: z + 1\n    return h(1)\n");
        assert!(!info.eligible);
    }

    #[test]
    fn comprehension_is_ineligible() {
        for body in [
            "def f():\n    return [i for i in range(3)]\n",
            "def f():\n    return {i for i in range(3)}\n",
            "def f():\n    return {i: i for i in range(3)}\n",
            "def f():\n    return sum(i for i in range(3))\n",
        ] {
            assert!(
                !analyze_first_fn(body).eligible,
                "should be ineligible: {body}"
            );
        }
    }

    #[test]
    fn nested_class_is_ineligible() {
        let info = analyze_first_fn("def f():\n    class C:\n        x = 1\n    return C\n");
        assert!(!info.eligible);
    }

    #[test]
    fn binding_positions_are_collected() {
        // for / with / except / walrus / tuple-unpack / import all bind locals.
        let src = "def f(seq):\n    total = 0\n    for k in seq:\n        total = total + k\n    with open('x') as fh:\n        total = total + 1\n    try:\n        total = total + 1\n    except ValueError as err:\n        total = total + 1\n    (p, q) = (1, 2)\n    if (w := total) > 0:\n        total = w\n    import math\n    return total\n";
        let info = analyze_first_fn(src);
        assert!(info.eligible);
        for n in ["seq", "total", "k", "fh", "err", "p", "q", "w", "math"] {
            assert!(has_slot(&info, n), "missing slot {n}");
        }
    }

    #[test]
    fn match_captures_are_collected() {
        let src = "def f(v):\n    match v:\n        case [a, *rest]:\n            return a\n        case {'k': val, **others}:\n            return val\n        case int() as num:\n            return num\n    return 0\n";
        let info = analyze_first_fn(src);
        assert!(info.eligible);
        for n in ["v", "a", "rest", "val", "others", "num"] {
            assert!(has_slot(&info, n), "missing slot {n}");
        }
    }

    #[test]
    fn node_index_stamp_resolves_to_slot() {
        // After analysis, a load-position `Name` node in the body must resolve
        // to its slot via the stamped node index (fast path), matching the
        // name-keyed answer.
        let parsed = tyc_syntax::parse_module("def f(a):\n    b = a\n    return b\n").unwrap();
        let module = parsed.into_syntax();
        let Stmt::FunctionDef(func) = &module.body[0] else {
            panic!("expected fn");
        };
        let info = SlotInfo::analyze(&func.parameters, &func.body);
        // Statement 1: `b = a` — the value `a` is a load-position Name.
        let Stmt::Assign(assign) = &func.body[0] else {
            panic!("expected assign");
        };
        let Expr::Name(a_ref) = assign.value.as_ref() else {
            panic!("expected name value");
        };
        let by_node = info.slot_of_node(a_ref);
        let by_name = info.slot_of_name("a");
        assert_eq!(by_node, by_name);
        assert!(by_node.is_some());
    }
}
