//! Denial-of-service regressions: inputs a few hundred bytes to a megabyte
//! long that made the checker take exponential or quadratic time or memory.
//! Each test simply has to finish; before its fix it ran for minutes, ran
//! out of memory, or overflowed the stack.

use super::*;
use tyc_resolve::resolve_module;
use tyc_syntax::preprocess::{expand_sugar, preprocess};

/// The CLI and the language server check on a 256 MiB stack; the default
/// 2 MiB test stack is too small for the deeper probes in a debug build.
fn on_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(scope, f)
            .expect("spawn the test worker")
            .join()
            .expect("the test worker panicked")
    })
}

fn check(src: &str) -> Diagnostics {
    let expanded = expand_sugar(src, false);
    let prep = preprocess(&expanded);
    let module = tyc_syntax::parse_module(&prep.python_source)
        .unwrap()
        .into_syntax();
    let (resolved, _) = resolve_module("<test>".to_owned(), &prep.python_source, &module);
    check_module_with(
        "<test>",
        &prep.python_source,
        &resolved,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
        &prep.impl_distributed_lines,
    )
}

fn messages(d: &Diagnostics) -> Vec<String> {
    d.errors().iter().map(|e| e.to_string()).collect()
}

/// `x1 = (x0, x0)`, `x2 = (x1, x1)`, … doubles the type of each binding.
/// Forty lines used to mean a 2^41-node type (the checker was OOM-killed at
/// 22).
#[test]
fn doubling_tuple_bindings_stay_bounded() {
    let mut src = String::from("x0 = (1, 2)\n");
    for i in 0..40 {
        src.push_str(&format!("x{} = (x{i}, x{i})\n", i + 1));
    }
    src.push_str("print(x40 == x40)\n");
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
}

/// The same growth through a generic call: `pair(pair(pair(…)))`.
#[test]
fn doubling_generic_instantiations_stay_bounded() {
    let n = 40;
    let src = format!(
        "def pair[T](x: T) -> tuple[T, T]:\n    return (x, x)\n\ny = {}1{}\nprint(y == y)\n",
        "pair(".repeat(n),
        ")".repeat(n)
    );
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
}

/// The cap itself: small types pass untouched, and both an oversized and an
/// over-deep type are caught without walking all of them.
#[test]
fn expr_type_size_cap() {
    let pair = |t: Type| Type::Generic("tuple".into(), vec![t.clone(), t]);
    let mut ty = Type::Int;
    for _ in 0..10 {
        ty = pair(ty);
    }
    assert!(!expr_type_too_large(&ty), "2^11 nodes is an ordinary type");
    let union = Type::Union((0..5000).map(|i| Type::LitStr(format!("v{i}"))).collect());
    assert!(!expr_type_too_large(&union), "a 5,000-member literal union");
    for _ in 0..7 {
        ty = pair(ty);
    }
    assert!(expr_type_too_large(&ty), "2^18 nodes");
    let mut deep = Type::Int;
    for _ in 0..(MAX_EXPR_TYPE_DEPTH + 2) {
        deep = Type::Generic("list".into(), vec![deep]);
    }
    assert!(expr_type_too_large(&deep));
    // Dropping `deep` recurses once per level; fine at this depth.
}

/// `'a'.s().s().s()…` on an `extend` method inferred the receiver twice per
/// call: 2^n. Twenty-four calls took 6 s; two hundred would never finish.
#[test]
fn method_chain_on_an_extended_builtin_is_not_exponential() {
    let src = format!(
        "extend str:\n    def s(self) -> str:\n        return self\n\nx: str = 'a'{}\n",
        ".s()".repeat(150)
    );
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
}

/// A nullable receiver in such a chain is still reported, once.
#[test]
fn method_chain_still_checks_a_nullable_receiver() {
    let src = "\
class A:
    def next(self) -> \"A?\":
        return None

def f(a: A) -> None:
    print(a.next().next())
";
    let d = check(src);
    let all: Vec<String> = d
        .errors()
        .iter()
        .chain(d.warnings().iter())
        .filter(|e| matches!(e, TycError::NullableUse { .. }))
        .map(|e| e.to_string())
        .collect();
    assert_eq!(all.len(), 1, "{all:?}");
}

/// `a.b.b.b…`: the deprecated-property lookup re-inferred the whole receiver
/// chain at every link, and the property lookup walked the module — the
/// chain included — each time. 2,000 links took 11 s.
#[test]
fn long_attribute_chain_is_linear() {
    let src = format!(
        "class A:\n    b: \"A\"\n\ndef main(a: A) -> None:\n    print(a{})\n",
        ".b".repeat(6000)
    );
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
}

/// The receiver hint that makes long chains linear must not change what a
/// chain resolves to: a missing attribute far down one is still reported,
/// and a deprecated property read still warns.
#[test]
fn long_attribute_chain_keeps_its_diagnostics() {
    let missing = format!(
        "class A:\n    b: \"A\"\n\ndef main(a: A) -> None:\n    print(a{}.nope)\n",
        ".b".repeat(200)
    );
    let d = on_big_stack(|| check(&missing));
    assert!(
        d.errors().iter().any(|e| matches!(
            e,
            TycError::AttributeNotFound { attr, recv_type, .. } if attr == "nope" && recv_type == "A"
        )),
        "{:?}",
        messages(&d)
    );
    let deprecated = format!(
        "from warnings import deprecated\n\nclass A:\n    b: \"A\"\n\n    @property\n    @deprecated(\"use b\")\n    def old(self) -> int:\n        return 1\n\ndef main(a: A) -> None:\n    print(a{}.old)\n",
        ".b".repeat(200)
    );
    let short = deprecated.replace(&".b".repeat(200), ".b");
    let count = |src: &str| {
        let d = on_big_stack(|| check(src));
        d.warnings()
            .iter()
            .filter(|w| w.to_string().contains("deprecated"))
            .count()
    };
    assert_eq!(count(&deprecated), count(&short));
}

/// Nested lambdas each snapshot an environment that grows with the nesting:
/// quadratic memory, 1.7 GB for 100,000. Past `MAX_CHECKED_LAMBDA_NESTING`
/// the bodies are no longer checked; an error in a shallow one still is.
#[test]
fn deeply_nested_lambdas_stay_bounded() {
    let src = format!("x = {}1\n", "lambda: ".repeat(9000));
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));

    let shallow = "\
class A:
    n: int

def f(a: A) -> None:
    g = lambda: lambda: lambda: a.missing
";
    let d = check(shallow);
    assert!(
        d.errors()
            .iter()
            .any(|e| matches!(e, TycError::AttributeNotFound { attr, .. } if attr == "missing")),
        "{:?}",
        messages(&d)
    );
}

/// The definite-assignment walk cloned every tracked name for every branch.
/// A branch that assigns nothing (a `return`, the lowered `?`) must share its
/// tables with the state before it, and so must the join.
#[test]
fn definite_assignment_branches_share_untouched_state() {
    let mut pre = DaState::new();
    for i in 0..1000 {
        pre.assign(&format!("a{i}"));
        pre.mark_unassigned(&format!("u{i}"));
    }
    let mut diverging = pre.clone();
    diverging.diverges = true;
    let fall_through = pre.clone();
    let joined = intersect_branches(&pre, &[diverging, fall_through]);
    assert!(Rc::ptr_eq(&joined.assigned, &pre.assigned));
    assert!(Rc::ptr_eq(&joined.unassigned, &pre.unassigned));
    assert!(Rc::ptr_eq(&joined.unbound_reason, &pre.unbound_reason));
    assert!(!joined.diverges);

    // No-op writes leave a shared table shared.
    let mut same = pre.clone();
    same.assign("a1");
    assert!(Rc::ptr_eq(&same.assigned, &pre.assigned));

    // And a real difference between the arms is still intersected.
    let mut then_arm = pre.clone();
    then_arm.assign("only_then");
    then_arm.assign("u0");
    let mut else_arm = pre.clone();
    else_arm.assign("u0");
    else_arm.unassign("a0", "was deleted");
    let joined = intersect_branches(&pre, &[then_arm, else_arm]);
    assert!(!joined.assigned.contains("only_then"));
    assert!(joined.assigned.contains("u0"));
    assert!(!joined.assigned.contains("a0"));
    assert!(!joined.unassigned.contains("u0"));
    assert!(!joined.unassigned.contains("a0"), "unbound on one arm only");
    assert!(joined.unbound_reason.is_empty());
    assert!(joined.assigned.contains("a1") && joined.unassigned.contains("u1"));
}

/// End to end: a function of a few hundred `?` statements checks cleanly.
#[test]
fn many_propagations_in_one_function() {
    let mut src = String::from(
        "def g() -> Result[int, str]:\n    return Ok(1)\n\ndef f() -> Result[int, str]:\n",
    );
    for i in 0..300 {
        src.push_str(&format!("    let v{i} = g()?\n"));
    }
    src.push_str("    return Ok(v0 + v299)\n");
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
}

/// A snapshot shares every bucket of a scope, and a write after it copies
/// the bucket list and the one bucket written — not the scope. With one map
/// per scope, 5,000 `let v = g()?` lines deep-copied 5,000 bindings 5,000
/// times (26 s).
#[test]
fn scope_writes_after_a_snapshot_copy_one_bucket() {
    let binding = |name: String| TypeBinding {
        name,
        declared: Type::optional(Type::Int),
        narrowed: Type::optional(Type::Int),
        span: (0, 0),
        from_unsafe: false,
    };
    let mut env = TypeEnv::default();
    env.enter();
    for i in 0..10_000 {
        env.declare(binding(format!("v{i}")));
    }
    let snap = env.snapshot();
    assert!(Scope::ptr_eq(&env.scopes[0], &snap.scopes[0]));

    env.narrow("v42", Type::Int);
    assert!(!Scope::ptr_eq(&env.scopes[0], &snap.scopes[0]));
    let copied = env.scopes[0]
        .buckets
        .iter()
        .zip(snap.scopes[0].buckets.iter())
        .filter(|(live, saved)| !Rc::ptr_eq(live, saved))
        .count();
    assert_eq!(copied, 1, "one bucket un-shared by one narrowing");
    assert_eq!(env.lookup("v42").map(|b| &b.narrowed), Some(&Type::Int));
    assert_eq!(
        snap.lookup("v42").map(|b| &b.narrowed),
        Some(&Type::optional(Type::Int)),
        "the snapshot is untouched"
    );

    env.declare(binding("fresh".to_owned()));
    assert!(env.lookup("fresh").is_some() && snap.lookup("fresh").is_none());
    assert_eq!(env.scopes[0].iter().count(), 10_001);
    assert_eq!(snap.scopes[0].iter().count(), 10_000);

    // A whole-scope rewrite that changes nothing un-shares nothing, and one
    // that changes a binding restores exactly that binding.
    let mut restored = env.clone();
    restored.reset_narrowings_to(&env);
    assert!(Scope::ptr_eq(&restored.scopes[0], &env.scopes[0]));
    let mut restored = env.clone();
    restored.reset_narrowings_to(&snap);
    assert_eq!(
        restored.lookup("v42").map(|b| &b.narrowed),
        Some(&Type::optional(Type::Int))
    );
    assert!(
        restored.lookup("fresh").is_some(),
        "the binding set is kept"
    );
}

/// Hundreds of statements that each declare and narrow (`guard`, `?`) in
/// one function, end to end.
#[test]
fn many_guards_in_one_function() {
    let mut src = String::from("def f(a: int?) -> int:\n");
    for i in 0..400 {
        src.push_str(&format!("    guard v{i} = a else: return {i}\n"));
    }
    src.push_str("    return v0 + v399\n");
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
}

/// `a | None | None | …` that is *not* a type union asked "is this a type
/// union?" of every prefix of the chain, each answer walking that prefix.
/// The answers for all prefixes now come from one walk, and they are the
/// same answers.
#[test]
fn value_union_chain_is_linear_and_unchanged() {
    let src = format!("import typing\nx = typing.Any{}\n", " | None".repeat(4000));
    let _ = on_big_stack(|| check(&src));

    // A type union of any shape is still one, however it is grouped …
    let clean = "\
class A:
    pass

t1 = int | None | str | bytes
t2 = (int | None) | (str | A)
t3 = list[int] | None | A
";
    let d = check(clean);
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
    // … and a chain with a value in it is still checked as arithmetic, at
    // every link.
    let count = |src: &str| {
        let d = check(src);
        d.errors().len() + d.warnings().len()
    };
    let chain = "def f(a: int) -> None:\n    x = a | None | None | None\n";
    let grouped = "def f(a: int) -> None:\n    x = ((a | None) | None) | None\n";
    assert!(count(chain) > 0);
    assert_eq!(count(chain), count(grouped));
}

/// A long method chain on a class: every call asked `infer_expr_readonly`
/// for its receiver, which walked all the calls below it.
#[test]
fn long_method_chain_is_linear() {
    let src = format!(
        "class A:\n    def next(self) -> \"A\":\n        return self\n\ndef f(a: A) -> None:\n    print(a{})\n",
        ".next()".repeat(3000)
    );
    let d = on_big_stack(|| check(&src));
    assert!(d.errors().is_empty(), "{:?}", messages(&d));

    // What the chain resolves to is unchanged by the shortcut: a method
    // missing far down a long chain is still reported, on the right class.
    let missing = format!(
        "class A:\n    def next(self) -> \"A\":\n        return self\n\ndef f(a: A) -> None:\n    print(a{}.nope())\n",
        ".next()".repeat(200)
    );
    let d = on_big_stack(|| check(&missing));
    assert!(
        d.errors().iter().any(|e| matches!(
            e,
            TycError::AttributeNotFound { attr, recv_type, .. } if attr == "nope" && recv_type == "A"
        )),
        "{:?}",
        messages(&d)
    );
}

/// The binding lookup behind `callables::origin` used to walk the module
/// for every call; it must still find a callable's `def`, and follow an
/// alias to it, wherever they are declared.
#[test]
fn callable_origin_lookup_still_resolves_definitions() {
    let src = "\
def scale(x: int, factor: int = 2) -> int:
    return x * factor

alias = scale

def outer() -> int:
    def inner(a: int, b: int) -> int:
        return a + b
    local = inner
    return local(1, 2) + alias(3) + alias(3, 4) + scale(5)
";
    let d = check(src);
    assert!(d.errors().is_empty(), "{:?}", messages(&d));
    let bad = src.replace("alias(3, 4)", "alias(3, 4, 5)");
    let d = check(&bad);
    assert!(
        !d.errors().is_empty(),
        "too many arguments through an alias"
    );
}
