# Typhon Adversarial Audit — 50-agent deep sweep (2026-06-28)

Generated from a 50-agent Workflow run (49 agents, ~2.9M tokens) plus the round-3 manual agents. 
Each finding was grounded by its agent in real `tyc check` + `tyc build`/`run` output. 
Counts: 30 CRITICAL, 44 HIGH, 8 MEDIUM, 2 LOW (84 deduped).

Status legend: ✅ fixed · ⏳ pending · 🔁 dup of an already-fixed item.


## CRITICAL

### [13] (emit / s_newtype) isinstance() against a transparent `type` alias (or `comptime let T: type`) type-checks clean but the emitted CPython 3.13 raises TypeError at runtime
- **Status:** ⏳ pending
- **Root cause:** tyc-types isinstance-narrowing path (crates/tyc-types/src/lib.rs around line 12669 `isinstance(x, T)` handling and refine_isinstance_target) accepts a transparent `type` alias / `comptime let T: type` value as the class argument and narrows with it, while tyc-desugar/tyc-emit pass the alias name through verbatim. Because a PEP 695 `type X = ...` lowers to a typing.TypeAliasType (not a runtime clas

### [14] (emit / s_crossmod) Cross-module `extend BUILTIN:` via `from mod import method` checks clean but emits a dead import that crashes with ImportError at module load
- **Status:** ⏳ pending
- **Root cause:** Two-part: (1) tyc-resolve/tyc-types treats `shout` in `from strext import shout` as an ordinary imported value (it even flags it `unused_import`, proving the checker never connects it to the `title.shout(...)` call) and never validates that `strext` actually exports a value `shout` — it does not, because `extend str: def shout` lowers to a free function `__typhon_ext_str__shout`. (2) tyc-desugar/t

### [19] (emit / fp_class) Mixed frozen/non-frozen dataclass inheritance type-checks clean but emits import-crashing Python (and the VM silently runs it — parity divergence)
- **Status:** ✅ fixed — frozen_inheritance_conflict diagnostic (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types class checking has no rule requiring a `frozen` class's bases to all be frozen (and a non-frozen class's bases to all be non-frozen); the only frozen diagnostic is `tyc::frozen_assign`. tyc-desugar/tyc-emit then stamps `@dataclass(slots=True, frozen=True)` onto a class whose base lacks `frozen`, producing the dataclasses TypeError at import. The tyc-vm class path ignores the frozen modif

### [21] (emit / e_async) `await` inside a `gather:` binding emits `create_task(await f())`, type-checks clean, crashes compiled CPython AND silently diverges from the VM
- **Status:** ✅ fixed — gather/go await stripping (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** The gather lowering in tyc-desugar/tyc-emit textually wraps each binding's RHS in `_tg.create_task(<rhs>)` without (a) the type checker (tyc-types) verifying the RHS is a coroutine-returning call, or (b) the desugarer stripping/rejecting an `await` already present on the RHS. A `gather:` binding RHS is awaited by the TaskGroup itself, so an explicit `await` (a plausible author mistake) double-eval

### [22] (emit / e_class) class! with an arg-taking base: checker accepts inherited fields as constructor kwargs, but synthesized __init__ drops them and calls super().__init__() with no args → TypeError at runtime
- **Status:** ✅ fixed — class! inherited-field __init__ (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-desugar/src/lib.rs synthesise_raw_class_init (called near line 2195): the synthesized class! __init__ always emits `super().__init__()` with no arguments and assigns only the class's OWN annotated fields, ignoring the base class's required constructor parameters. Meanwhile tyc-types computes the constructor signature including inherited base fields, so `tyc check` accepts inherited kwargs the 

### [23] (emit / e_freeze) Module-level `lazy let` of a primitive type emits a `_LazyValue` proxy that crashes on every operator/index/arithmetic use (clean check, VM/CPython divergence)
- **Status:** ✅ fixed — _LazyValue operator forwarding (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** typhon_runtime/lazy.py `_LazyValue` (emitted by tyc-emit / tyc-desugar for module-level `lazy let`). The proxy only forwards a small set of dunders (__getattr__, __call__, __getitem__, __iter__, __bool__, __eq__, __hash__, __len__, __str__, __repr__). It is missing every numeric dunder (__add__/__radd__/__sub__/__mul__/__truediv__/__floordiv__/__mod__/__pow__ ...), all rich-comparison dunders (__l

### [0] (soundness / s_dunder) Calling an instance with a user-defined __call__ is typed as the class, not __call__'s return type (soundness hole + false positive)
- **Status:** ⏳ pending
- **Root cause:** tyc-types call-expression typing: a call where the callee expression has a user-defined class type is resolved as if it were a constructor (yielding the class type) rather than dispatching to the class's __call__ method and using its declared return type. Note: __add__ / __enter__ / __len__ return types ARE honored, so the defect is isolated to the __call__ dispatch path at a direct call site. Lik

### [1] (soundness / s_closure) Flow-narrowing of a captured local leaks into a nested function/closure body, suppressing nullable_use and causing runtime TypeError
- **Status:** ⏳ pending
- **Root cause:** tyc-types `check_function` (tyc/crates/tyc-types/src/lib.rs:10475). On entering a nested function body it calls `c.env.enter()` (push a fresh frame) but never resets the `narrowed` field of *enclosing* local frames. Only module-global narrowings are reset (`reset_global_narrowings`, line 2364 / 10210). A name lookup inside the closure (`Env::lookup`, line 2316) returns the enclosing binding with i

### [2] (soundness / s_async) Missing `await` on an async call type-checks clean: coroutine call expression is typed as its return type T, not Coroutine[...,T]
- **Status:** ⏳ pending
- **Root cause:** tyc-types call-expression typing: a call to an `async def` is typed as the function's return type T rather than `Coroutine[Any, Any, T]`. `await` unwrapping is therefore a no-op in the type lattice, and the `async_without_await` lint only inspects the callee body, not the call site, so it cannot catch a forgotten await. Likely in the call/await expression typing in tyc-types (await handling + asyn

### [3] (soundness / s_async) `go f() -> task` task-result type is not propagated: `await task` binding accepts any annotation
- **Status:** ⏳ pending
- **Root cause:** tyc-types/tyc-analyse handling of `go EXPR -> task`: the synthesized `task` binding is typed without recording the spawned coroutine's result type, so `await task` is not checked against (and does not constrain) the LHS annotation. The task handle's awaited result type should be the callee's return type (str here), conflicting with `let n: int`.

### [4] ✅ (soundness / s_augassign) Augmented assignment to a typed dict/list subscript ignores both value-widening and element type — `dict[str,int]` value silently becomes float, then crashes downstream
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs `check_stmt` Stmt::AugAssign arm (~line 10213). The aug-assign handler only runs `operator_operands_compatible` and ONLY when the *target's inferred type* is a primitive scalar (`scalar_target` gate, lib.rs:10224). A `Subscript` target like `bad["a"]` is not gated as a scalar local, so no operator check runs at all; and even for scalars the code never checks that the result ty

### [5] (soundness / s_return) Nested `def`/`lambda` is typed as Any: assignment to any annotation (return, let, arg) is silently unchecked, defeating Callable assignability and producing runtime AttributeError
- **Status:** ⏳ pending
- **Root cause:** tyc-types: locally-defined functions (nested `def`) and `lambda` expressions are assigned an unchecked/`Any` type rather than a concrete `Callable[...]` signature. Demonstrated: `let n: int = inner` where `inner` is a nested `def(x:int)->int` is accepted with NO error (probe.ty), while the identical top-level function correctly fires `tyc::type_mismatch` (probe2.ty). Because the nested-def type is

### [6] (soundness / s_variance) Mutating methods (.append, __setitem__) allowed on covariant read-only views Sequence/Mapping, letting a wrong element be written into a list[Sub]/dict[_,Sub] — silent unsafe
- **Status:** ⏳ pending
- **Root cause:** tyc-types: member-access/attribute resolution treats the abstract typing collection heads (Sequence, Mapping, Iterable, Collection, etc. — the names listed around tyc/crates/tyc-types/src/lib.rs:1360-1379) as lenient receivers: a method call on a `Sequence`/`Mapping`-typed value is not constrained to that type's actual (read-only) method set. Confirmed independently: `s.append(5)` AND `s.nonexiste

### [7] (soundness / s_interface) Generic interface type-args checked covariantly regardless of declared variance — Consumer[int] silently accepted where Consumer[object] required, runtime TypeError
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs, Typer::is_assignable, the same-head generic-interface arm at lines 3144-3161 (the `(Type::Generic(exp_name, exp_args), Type::Generic(act_name, act_args))` case). It accepts the assignment when `is_interface_name(exp_name) && class_conforms_to_generic_interface(act_name, exp_name, exp_args) && exp_args.len()==act_args.len() && exp_args.iter().zip(act_args).all(|(e,a)| self.is_

### [8] (soundness / s_result) .map_err(f)? erases the error type to Unknown, letting ? propagate an error type that mismatches the enclosing function's declared E (soundness hole)
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs, `result_combinator_member_type` (the `("Result", "map_err", [t, _e])` arm at ~line 13159, plus the `Ok`/`Err` arms at 13110/13133). map_err's return type is hardcoded to `Result[t, Unknown]` — the mapper argument's actual return type is discarded (`_e` is ignored and never replaced with the mapper return). The `?` operator then sees E = Unknown, which is assignable to any enc

### [9] (soundness / s_except) `except E as e` types the bound exception as `Any`, dropping all type-checking of its fields/methods (wrong-typed values escape silently)
- **Status:** ⏳ pending
- **Root cause:** tyc-types: the `except <type> as <name>` handler binding is given type `Any` (or `Unknown`) instead of the annotated exception type `<type>` from the handler's exception expression. Because the binding is `Any`, attribute access (`e.code`), arithmetic (`e + 1`), and method calls (`e.nonexistent_method()`) all type as `Any`, so the declared field types of `class!`/exception classes never participat

### [10] (soundness / s_literals) Tuple-unpack assignment to existing/declared targets skips per-slot type checking (incl. for-loop targets) — silent type confusion
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs, check_stmt's `Stmt::Assign` handler (~line 9610-9713) and the `Stmt::For` handler (~line 10164): the per-target loop only type-checks/narrows when `target` is `Expr::Name` (re-bind path at 9633 `is_assignable`) or `Expr::Attribute`. When `target` is `Expr::Tuple` (an unpack assignment, including nested unpack and the for-loop variable), the loop performs no per-slot compariso

### [11] (soundness / s_forloop) For-loop (and let) tuple-unpacking targets are untyped (Any): dict.items()/enumerate/zip/list[tuple] element types lost, allowing silent type confusion and runtime TypeError
- **Status:** ⏳ pending
- **Root cause:** tyc-types: the for-statement / assignment target binder does not destructure a tuple element type onto a tuple unpacking target list. When the for-target (or LHS of `let a, b = t`) is a tuple pattern, each element binding is left as Any instead of being projected from the iterable's element type. The single-target path is correctly typed (a single-target control case `for x in list[int]: let s: st

### [12] (soundness / s_match) match OR-pattern capture is typed from the FIRST alternative only, ignoring the other arms — value matching a later arm gets a wrong concrete capture type (silent unsoundness)
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs, fn bind_pattern_names, Pattern::MatchOr arm (lines ~16934-16938): it only recurses into `o.patterns.first()`, binding each name with the FIRST alternative's type. The correct behavior is to bind each name to the UNION of its type across all alternatives (Python requires every alternative to bind the same names). Because only the first arm's type is used, a value that actually

### [15] (soundness / s_defaults) Function/method parameter default values are never type-checked against the parameter annotation (documented tyc::default_mismatch is absent) — silent runtime TypeError
- **Status:** ✅ fixed — parameter default value type-check (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types signature collection (tyc/crates/tyc-types/src/lib.rs, around the FunctionSignature/param-default machinery near lines 2387, 2620-2776) records parameter defaults only for arity purposes (required_arity, field_defaults) and never compares the default expression's inferred type to the parameter's declared annotation. Contrast: dataclass FIELD defaults ARE checked (`class C: n: int = "zero

### [16] (soundness / s_unsafe) as! checked cast silently accepts wrong element types for collections.abc parametric containers (Sequence/Mapping/Iterable/etc.), allowing mistyped data into typed slots
- **Status:** ⏳ pending
- **Root cause:** tyc/crates/tyc/src/commands/build.rs, TYPHON_RUNTIME_CAST_PY -> _matches(): the parametric-container handling only special-cases origins list/set/frozenset/dict/tuple/Union. Every other parameterised origin (collections.abc.Sequence/Mapping/Iterable/Iterator/MutableSequence/...) falls to the final `if isinstance(origin, type): return isinstance(value, origin)` branch, which erases the type argumen

### [17] (soundness / s_unsafe) as! cast to a parametric user-defined generic class (Box[int]) ignores the type argument, letting Box[str] pass as Box[int]
- **Status:** ⏳ pending
- **Root cause:** Same _matches() gap as the abc-container finding: get_origin(Box[int]) is the user class Box, which is not in {list,set,frozenset,dict,tuple,Union}, so it hits the trailing `isinstance(origin, type) -> isinstance(value, origin)` branch and the PEP 695 type argument is discarded. tyc/crates/tyc/src/commands/build.rs TYPHON_RUNTIME_CAST_PY.

### [18] (soundness / s_numeric) int ** int is typed `int` even for negative exponents, which return float at runtime (soundness hole)
- **Status:** ⏳ pending
- **Root cause:** The binary-operator type rule in tyc-types models `int ** int -> int` unconditionally. Python's `int.__pow__` returns `float` when the exponent is negative (e.g. `2 ** -1 == 0.5`). The checker has no special case for a statically-known negative literal exponent (where pyright/mypy DO narrow the result to `float`), nor does it widen `int ** int` to `int | float` for an unknown-sign exponent. So a f

### [20] ✅ (soundness / e_question) Inline `?` is hoisted out of conditionally-evaluated positions (ternary branch, `and`/`or` RHS), breaking short-circuit evaluation — runs the fallible call unconditionally and returns a spurious `Err` (VM crashes)
- **Status:** ⏳ pending
- **Root cause:** The inline-`?` desugaring pass (tyc-desugar, the `?`-hoisting/statement-tail lowering that produces `__typhon_qi_N__` temporaries) lifts the fallible operand to the nearest enclosing statement unconditionally. It does not account for operands that sit in conditionally-evaluated sub-expressions (ternary `then`/`else` arms, RHS of `and`/`or`). Postfix `?` must be evaluated only on the control-flow p

### [25] ✅ (tooling / t_comptime) comptime str(float) drops the trailing '.0', inlining a wrong string constant that diverges from CPython and from the f-string path
- **Status:** ⏳ pending
- **Root cause:** tyc-analyse/src/lib.rs, eval_call "str" arm (~line 1047): uses Rust's f.to_string() for ComptimeValue::Float, which prints whole-valued floats without a decimal point (2.0 -> "2") and large floats in full decimal form (1e21 -> "100...0"), neither matching CPython's float __str__/repr. By contrast the f-string path uses comptime_str() (~line 791) which special-cases finite whole floats with format!

### [26] ✅ (tooling / t_comptime) comptime numeric comparison coerces i64 to f64, folding large int-vs-float comparisons to the wrong boolean constant
- **Status:** ⏳ pending
- **Root cause:** tyc-analyse/src/lib.rs, eval_cmpop (~line 836): the as_f64 helper coerces ComptimeValue::Int(i64) via `*n as f64` for mixed int/float ordering, discarding precision beyond 2^53. CPython's int/float comparison is exact. The Lt/LtE/Gt/GtE mixed-numeric case should compare without lossy f64 coercion.

### [27] (tooling / t_migrate) tyc migrate rewrites typing-alias names (List/Dict/Tuple/Set/Type/FrozenSet) inside string literals and docstrings, silently corrupting program data
- **Status:** ⏳ pending
- **Root cause:** tyc/crates/tyc/src/commands/migrate.rs: rewrite_typing_aliases() (line ~2050) does a raw whole-line substring search/replace for needles `List[`/`Dict[`/`Tuple[`/`Set[`/`FrozenSet[`/`Type[` (and their `typing.` qualified forms), replacing the capitalized prefix with the lowercase builtin. It only guards against a preceding identifier char (to avoid `MyList[`); it has NO string-literal / docstring 

### [24] (vm_parity / r_preprocess) VM mis-lowers `?` propagation on a semicolon-joined statement line: silently binds the unwrapped value to the wrong target (clean check, correct compiled output, VM crash/wrong-output)
- **Status:** ⏳ pending
- **Root cause:** tyc-syntax/src/preprocess.rs `expand_question_ops_inner` (~line 5556) processes one PHYSICAL line at a time: it requires `?` to be the last non-whitespace code char (`content.ends_with('?')`) and splits the line on its FIRST assignment `=` via `find_assignment_eq`. It is not semicolon-aware, so a `let A; let B = expr?` line mis-attributes the unwrap to A (and a `let B = expr?; let A` line is skipp

### [28] (vm_parity / v_vmstdlib) VM bin()/hex()/oct() of negative ints emit 64-bit two's-complement (wrong) and OverflowError on bigints; CPython emits sign-magnitude
- **Status:** ⏳ pending
- **Root cause:** tyc/crates/tyc-vm/src/builtins.rs ~lines 934-945: the `hex`/`bin`/`oct` natives do `format!("0x{:x}", single(&args).to_int()?)` etc. `.to_int()` narrows the BigInt to i64 and Rust's `{:b}/{:x}/{:o}` format negatives as two's-complement (and `.to_int()` overflows for |value| > i64). CPython formats sign-magnitude. Fix: format the BigInt directly with a leading `-` for negatives (e.g. value.magnitud

### [29] (vm_parity / v_vmsem) VM treats non-frozen dataclass instances as hashable; CPython raises TypeError: unhashable type — VM gives wrong output where compiled path crashes
- **Status:** ⏳ pending
- **Root cause:** tyc-vm value.rs / builtins.rs: the VM makes every user Instance hashable (HashKey::Instance, class-identity + sorted fields) regardless of whether the synthesized dataclass would have __hash__ set. CPython's @dataclass(eq=True, frozen=False) — the default emitted by tyc-desugar/tyc-emit — sets __hash__=None, making instances unhashable. The VM should mirror this: only frozen (or explicitly __hash_


## HIGH

### [68] (docs / t_docs) Canonical docs (language.md + guide 05) claim optional fields `T?` get an implicit `= None` default and emit `field: T | None = None`; the compiler does the opposite — `T?` is a REQUIRED field, so the foundational `class User` example fails to compile (tyc::field_default_ordering) and the documented constructor calls / emit are wrong
- **Status:** ⏳ pending
- **Root cause:** Documentation defect in docs/language.md (~L127-142) and docs/guides/05-classes-and-models.md (first class example). The docs assume `T?` fields receive an implicit `= None` dataclass/pydantic default; the desugarer (tyc-desugar) deliberately does NOT inject a default for optional fields, so they remain required and trigger tyc::field_default_ordering when placed after a defaulted field. Either th

### [54] (emit / e_async) Non-awaitable RHS in a `gather:` binding (`b = 99`) type-checks clean and emits `create_task(99)`, crashing compiled CPython while the VM returns the wrong value
- **Status:** ⏳ pending
- **Root cause:** Same root cause as the await-in-gather bug: gather binding RHS expressions are never checked for awaitability in tyc-types before tyc-emit wraps them in create_task(...). Any non-coroutine RHS (literal, sync-function call, attribute) emits invalid create_task() calls. The compiler should require each gather binding RHS to be a coroutine-returning call.

### [55] (emit / e_async) `go` spawns a non-coroutine target with no check: `go sync_fn(x)` / `go await f()` type-check clean, emit `spawn(...)`, and crash compiled CPython
- **Status:** ⏳ pending
- **Root cause:** `go EXPR` lowering in tyc-desugar emits `typhon_runtime.tasks.spawn(EXPR)` without tyc-types verifying EXPR is a call to an async def / coroutine-returning callable, and without rejecting an `await` on the target. spawn() internally calls asyncio.create_task which requires a coroutine, so any sync-call or already-awaited target produces a runtime TypeError that the type checker should have caught.

### [56] (emit / e_compre) Unparenthesized `as!` in a comprehension/generator `for`/`if` clause mis-lowers: the backward left-operand scan swallows the comprehension head into checked_cast(...), emitting invalid Python
- **Status:** ⏳ pending
- **Root cause:** tyc-syntax/src/preprocess.rs — `find_cast_expr_start` (line 3990). The backward left-operand scan for `as!` only breaks on brackets, top-level `,`/`;`/`:`, an assignment `=`, or a newline. It does NOT break on comprehension/genexpr clause keywords (`for`, `if`, `async for`) within the same bracket group, so from an `as!` inside an `if`-clause it walks all the way back to the opening `[`/`(`, captu

### [59] (emit / e_sourcemap) Source map (.py.map) is systematically misaligned whenever the desugar pass injects an extra runtime import (?-operator, freeze let, etc.), so tyc trace remaps every traceback frame to the wrong (often nonexistent) .ty line
- **Status:** ⏳ pending
- **Root cause:** tyc-emit/src/printer.rs line-offset table generation. writeln/newline push self.current_input_offset once per emitted physical line (printer.rs:141-150). The `from __future__ import annotations` line is emitted via writeln, and the desugar pass (tyc-desugar/src/lib.rs Result/freeze import injection) prepends extra import statements carrying TextRange::default() (zero range), so emit_stmt (printer.

### [33] (false_positive / s_dunder) Correct use of __call__ result is rejected, and a no-arg __call__ triggers a spurious missing_argument (treated as a constructor)
- **Status:** ⏳ pending
- **Root cause:** Same root cause as the soundness finding: tyc-types routes a call on an instance to the nominal-construction path. When __call__ has only `self`, the call is checked against the class's synthesized constructor signature (requiring field `n`), producing missing_argument; when __call__ takes extra args, the result is mis-typed as the class. Both stem from not dispatching instance calls through __cal

### [39] (false_positive / s_generics) Dataclass subclass of a generic base drops inherited fields from the constructor (`unknown_kwarg` false positive)
- **Status:** ⏳ pending
- **Root cause:** tyc-types/tyc-resolve constructor-shape synthesis: the inherited-field merge that works for plain base classes is skipped (or the base shape is not resolved) when the base class carries type parameters, so the subclass's synthesized __init__/kwarg set contains only its own fields. Same root area as the soundness finding above (generic-base inheritance flattening).

### [46] (false_positive / s_crossmod) Cross-module `extend BUILTIN:` via `import mod` (the documented idiomatic form) is a false positive: `tyc check` rejects `recv.method()` with attribute_not_found, blocking build, though the VM runs it correctly
- **Status:** ⏳ pending
- **Root cause:** tyc-types attribute resolution on a `str` receiver does not consult the project-wide cross-module `extend BUILTIN:` registry when the declaring module is brought in via `import mod` (only the desugar/VM paths do). The consumer's checker has no entry for `str.shout`, so it emits attribute_not_found. Same underlying gap as the CRITICAL finding: the extension registry is wired into desugar/codegen/VM

### [50] (false_positive / fp_narrow) isinstance(x.attr, T) does not narrow the attribute, rejecting valid code (only Name targets narrow; Attribute targets ignored)
- **Status:** ✅ fixed — isinstance attribute narrowing (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types/src/lib.rs, narrowing collector ~line 12672: the `isinstance(x, T)` branch matches only `if let Expr::Name(target) = &pos_args[0]` and emits a name-keyed Narrowing. It never handles `Expr::Attribute`, unlike the adjacent `is None` comparison branch (~line 12640) which calls `attr_path_of(cmp.left)` and emits an `attr_path` Narrowing. So `isinstance(b.v, T)` (Attribute first arg) yields n

### [51] (false_positive / fp_generic) Read-only ABC subtyping lattice missing: Sequence/Collection/Iterator/genexpr not assignable to Iterable[T] (and the broader abc hierarchy)
- **Status:** ⏳ pending
- **Root cause:** tyc-types `assignable()` in /home/user/Typhon/tyc/crates/tyc-types/src/lib.rs (~lines 320-366). The `READ_VIEW_HEADS` covariance rule (Sequence/Iterable/Iterator/Collection/Container/Reversible) only fires when the ACTUAL type is a concrete builtin container head (`list`|`tuple`|`set`|`frozenset`, line ~338). There is no rule encoding the abc subtyping lattice itself — Iterator<:Iterable, Sequence

### [52] (false_positive / fp_match) Exhaustive match with an or-pattern over a string-literal (or bool) union falsely fires tyc::missing_return — or-pattern arms aren't recognized as covering LitStr/bool inhabitants
- **Status:** ✅ fixed — or-pattern exhaustiveness (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types/src/lib.rs `cases_cover_type`. The LitStr coverage loop (lines 12000-12012) calls `pattern_str_value(&case.pattern)` to learn which literal an arm covers, but `pattern_str_value` (fn at line 12465) only handles `Pattern::MatchValue` and `Pattern::MatchAs` — it returns None for `Pattern::MatchOr`. So `case "red" | "green":` marks neither variant covered, and the union-recursion at line 11

### [53] (false_positive / fp_stdlib) Bare `Final` / `ClassVar` annotation (type inferred from value, PEP 591) spuriously rejected with tyc::type_mismatch
- **Status:** ✅ fixed — bare Final/ClassVar inference (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types/src/lib.rs, `type_from_annotation_with_params`. The bare-Name arm (around lines 1468-1498) has NO case for `Final`/`ClassVar`, so a bare imported `Final`/`ClassVar` falls through to the catch-all `other => Type::Class(other.to_owned())` (line ~1497) and becomes `Type::Class("Final")` / `Type::Class("ClassVar")`. The annotated-assign assignability check then compares the value type (e.g. 

### [58] (false_positive / e_strings) bytes %-formatting (PEP 461) rejected as operator_type_mismatch; valid Python blocked at check time (and unsupported in VM)
- **Status:** ✅ fixed — bytes %-format (checker + VM) (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types/src/lib.rs line 13848: the early-return that makes `%` yield a string is gated on `matches!(l_stripped, Type::Str | Type::LitStr(_))` and omits `Type::Bytes`. As a result a `bytes` LHS falls through to operator_operands_compatible / the numeric `%` compatibility check (line 13851-13856) and is flagged. Fix: add `Type::Bytes` to that match arm, returning `Type::Bytes` for bytes %-formatti

### [61] (false_positive / r_crash) Bare enum member with a non-ASCII identifier (accented Latin, Cyrillic, Greek, CJK) is silently dropped — `enum.auto()` lowering is gated on `is_ascii_*`, so the member never exists; `tyc check` rejects valid code with a misleading `unknown_name`
- **Status:** ⏳ pending
- **Root cause:** tyc-syntax/src/preprocess.rs, the enum bare-member rewrite (lines ~367-375). `is_bare_member` requires `body.chars().next() == is_ascii_alphabetic()||'_'` AND `body.chars().all(is_ascii_alphanumeric()||'_')`. A member name containing any non-ASCII letter fails this gate, so the ` = enum.auto()` suffix is never appended (line 383). The line `GRÉEN` survives verbatim into the class body, where Pytho

### [62] (robustness / r_emptyodd) Exponential-time blowup in `assignable()` on deeply-nested invariant containers (`list[list[...]]`) — algorithmic DoS that hangs `tyc check`/`build`/`run`
- **Status:** ⏳ pending
- **Root cause:** tyc-types crate, `assignable()` in tyc/crates/tyc-types/src/lib.rs. The structural-generic arm (around lines 364-393) dispatches each type argument by variance; for invariant parameters — which `list`/`set`/`dict` use (line ~384 names `list` explicitly) — line 389-391 does `Variance::Invariant => assignable(formal, actual_arg) && assignable(actual_arg, formal)`. For `list[X]` this makes TWO recurs

### [30] (soundness / s_compre) dict-comprehension KEY expression type is unchecked — wrong key type silently accepted against dict[K, V] annotation
- **Status:** ⏳ pending
- **Root cause:** tyc-types comprehension typing: the DictComp key node is not checked against the annotated/expected K. Boundary confirmed sharp: a PLAIN dict literal {"a": 1} against dict[int,int] correctly fires tyc::type_mismatch on BOTH key and value, but the dict-comprehension form {str(x): x for x in r} skips key (and value) assignability. Likely the comprehension element-type inference path (the same code t

### [31] (soundness / s_slice) Slice read on list/str/tuple is typed Any (or unchecked), letting a slice be assigned to an incompatible type and crash at runtime
- **Status:** ⏳ pending
- **Root cause:** tyc-types subscript/index inference: the slice-subscript case (ExprSubscript with a Slice index) is not given the container type (list[T]->list[T], str->str, tuple->tuple) — it falls back to Any — whereas the scalar-index case correctly yields the element type T. Likely in the binary/subscript expression type rule in tyc-types.

### [32] (soundness / s_slice) Slice-assignment (and scalar indexed-assignment) into list[T] is not checked against element type T — silently corrupts the container invariant
- **Status:** ⏳ pending
- **Root cause:** tyc-types assignment checker does not validate the RHS against the element type of a subscript assignment target (ExprSubscript on the LHS), for both Slice and scalar index. The element-type compatibility check applied to e.g. `list.append`/literal elements is not applied to `container[idx] = value` / `container[a:b] = seq`. Distinct from the slice-READ hole above (write side vs read side).

### [34] (soundness / s_async) `gather:` (TaskGroup) block bindings are untyped: downstream misuse of a gather result is unchecked
- **Status:** ⏳ pending
- **Root cause:** tyc-analyse/tyc-types `gather:` lowering: the single-assignment names introduced by a `gather:` block are bound without the producing call's return type (treated as Any/unknown), so any subsequent operation on them is unchecked. The success bindings should carry the callee return types (str, int).

### [35] (soundness / s_async) `await f(...)` where f: Callable[..., T] returns a NON-awaitable T type-checks clean (await-Callable-unwrap over-accepts plain T)
- **Status:** ⏳ pending
- **Root cause:** tyc-types await handling for the v0.7.0 async-callable-await feature: when awaiting a call whose callee type is `Callable[..., R]`, the checker unwraps to R unconditionally instead of only when R is `Awaitable[T]`/`Coroutine[Y,S,T]`. A non-awaitable return (plain int) should produce an await-on-non-awaitable error.

### [36] ✅ (soundness / s_augassign) Augmented assignment to a typed class field silently widens it (int field becomes float) with a clean check
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs Stmt::AugAssign arm (~10213). The `scalar_target` gate (10224) only matches when `infer_expr(target)` is a bare primitive; an attribute target `self.n` does not trigger the operator-compat check, and there is no check that the BinOp result type (float) is assignable to the field's declared annotation (int). Distinct from the known local-scalar `int += float` issue because attr

### [37] (soundness / s_augassign) `list[int] += list[str]` injects str elements into a list[int] with no diagnostic — element type of the RHS iterable is never checked
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs Stmt::AugAssign arm. The comment at lib.rs:10219-10221 deliberately stays permissive for mutable-container targets ('list += any_iterable') to avoid FPs on valid `list[int] += range(...)`. But it makes NO element-type check: it never verifies that the RHS iterable's element type is assignable to the list's element type. `list[int] += list[str]` is therefore accepted, mutating 

### [38] (soundness / s_generics) Inherited field from a generic base with a fixed type argument is typed `Any` (silent type confusion, soundness hole)
- **Status:** ⏳ pending
- **Root cause:** tyc-types class-shape / inheritance flattening: when a subclass extends a generic base with a FIXED type argument (`Container[int]`), the inherited field's declared type is not substituted (T->int) into the subclass shape and degrades to Any, so `let s: str = b.item` (where b.item is int) is accepted. Boundary: when the subclass instead keeps the parameter (`SubBox[T](Container[T])`), the field ty

### [40] (soundness / s_result) try_result(thunk, on_err)? (the lowering target of postfix/block `rescue`) does not check the on_err mapper's return type against the enclosing function's E
- **Status:** ⏳ pending
- **Root cause:** tyc-types: the type of `try_result(thunk, on_err)` does not infer its E from the second argument's (on_err) return type; the subsequent `?` therefore propagates an E (here `str`) that is never checked against the enclosing function's declared error type (NetErr). Same family as the map_err hole — the error type produced by a fallible-mapping combinator is not back-checked at the `?` site. Since `r

### [41] (soundness / s_except) `raise <non-exception>` (plain int, or instance of a non-BaseException class) type-checks clean but is a guaranteed runtime TypeError
- **Status:** ✅ fixed — raise non-exception diagnostic (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types / tyc-analyse: the `raise EXPR` statement does not verify that EXPR's type is a subtype of BaseException. Any expression (int literal, or an instance of an ordinary `class`/dataclass that does not inherit Exception) is accepted. CPython enforces `exceptions must derive from BaseException` at runtime, so this is a checkable static error the compiler is missing. The class-shape information

### [42] (soundness / s_operators) int ** int with a negative exponent is typed `int` but yields `float` at runtime (unsound; crashing emit)
- **Status:** ⏳ pending
- **Root cause:** tyc-types `infer_expr` BinOp arm, /home/user/Typhon/tyc/crates/tyc-types/src/lib.rs ~line 14026: the conservative numeric rule `(Type::Int | Type::Bool, Type::Int | Type::Bool) => Type::Int` is applied to every non-Div operator, including `Operator::Pow`. Python's `int ** int` is `int` only when the exponent is non-negative; a negative exponent returns `float`. Pow on int operands should widen to 

### [43] (soundness / s_match) Sequence-pattern star capture `*rest` is typed as Unknown/Any — arbitrary misuse type-checks clean
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs, fn bind_pattern_names, Pattern::MatchStar arm (lines ~16887-16899): declares the binding as Type::Unknown with comment 'element type is hard to pin precisely here, so stay permissive'. For a list[T]/tuple subject the star capture is list[T] and can be typed precisely; binding Unknown lets any misuse slip through.

### [44] (soundness / s_match) Mapping-pattern double-star capture `**rest` is typed as Unknown/Any — arbitrary misuse type-checks clean
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs, fn bind_pattern_names, Pattern::MatchMapping arm (lines ~16900-16909): the `m.rest` binding is declared Type::Unknown. For a dict[K, V] subject the rest binding is dict[K, V] and should be typed as such; Unknown allows arbitrary misuse.

### [45] (soundness / s_fieldinit) ClassVar fields are wrongly counted as constructor parameters: ClassVar names accepted as kwargs and as positional slots, producing clean-check programs that crash with TypeError at runtime
- **Status:** ✅ fixed — ClassVar excluded from constructor (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-types/src/lib.rs: `class_constructor_arity_for` (lines 8703-8733) builds `param_names`, `max_positional`, `required_positional`, and `param_types` directly from `shape.field_order` / `shape.fields` with no ClassVar filtering. The dataclass `InterfaceShape.field_order` includes ClassVar-annotated names, so they become accepted constructor kwargs and positional slots. Note the codebase already h

### [47] (soundness / s_with) with/async with perform NO context-manager protocol check: any non-CM expression (plain class, list, int) type-checks clean then crashes at runtime with TypeError
- **Status:** ⏳ pending
- **Root cause:** tyc-types `with`/`async with` handling: REFERENCE.md 6.5 says the as-target reads its type from @contextmanager yield-type or concrete-class __enter__ return type, but when the head expression is NOT a context manager the checker silently falls back to typing the as-target as the head type instead of rejecting it. There is no validation that the with-subject implements __enter__/__exit__ (sync) be

### [48] (soundness / s_with) async with accepts a sync-only context manager (has __enter__/__exit__ but no __aenter__/__aexit__): clean check, runtime TypeError
- **Status:** ⏳ pending
- **Root cause:** tyc-types does not distinguish the sync vs async context-manager protocol when checking `async with`. A class providing only __enter__/__exit__ is accepted under `async with`, which at runtime requires __aenter__/__aexit__. The async-with path should require __aenter__/__aexit__ (or @asynccontextmanager) and reject a sync-only CM. Same missing-validation root as finding 1 but specifically the sync

### [49] (soundness / s_unsafe) as! cast to Callable[[A], R] only checks callable-ness, not the signature, so a wrongly-typed lambda passes and crashes when called per its static contract
- **Status:** ⏳ pending
- **Root cause:** _matches(): get_origin(Callable[[int],int]) is collections.abc.Callable (a type), so it reaches the trailing `isinstance(origin, type) -> isinstance(value, origin)` branch which only verifies the value is callable; parameter/return types are never (and largely cannot be) checked. The static checker still pins the slot to the full Callable signature. tyc/crates/tyc/src/commands/build.rs TYPHON_RUNT

### [63] ✅ (tooling / t_comptime) comptime equality treats bool as distinct from int, folding `True == 1` to a wrong False constant
- **Status:** ⏳ pending
- **Root cause:** tyc-analyse/src/lib.rs, values_equal (~line 932): there is no (Bool, Int)/(Int, Bool) arm, so any Bool-vs-Int (or Bool-vs-Float) equality falls through to the `_ => false` catch-all. CPython treats True as 1 and False as 0 for ==. Add Bool<->Int and Bool<->Float equality arms coercing bool to its integer value.

### [64] (tooling / t_dty) Emitted .pyi strips @dataclass, so consumers cannot construct any stubbed class — every .dty class produces an unfaithful stub that type-checkers reject for correct keyword construction
- **Status:** ✅ fixed — .pyi keeps @dataclass (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-emit/src/stub.rs:72 — `new_c.decorator_list.retain(|d| !is_dataclass_decorator(d));` deliberately strips `@dataclasses.dataclass(...)` from the emitted .pyi (comment: "an implementation choice that doesn't belong on the stub surface"). For a dataclass the decorator IS the surface: it synthesises __init__/__eq__/__match_args__. Stripping it makes the .pyi advertise a no-arg constructor, so ever

### [65] (tooling / t_config) typhon.toml accepts unknown/typo'd keys silently (no deny_unknown_fields) — a mistyped strictness key downgrades a CI gate and exits 0
- **Status:** ✅ fixed — typhon.toml deny_unknown_fields (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc/crates/tyc/src/config.rs: TyphonConfig and every nested config struct lack `#[serde(deny_unknown_fields)]`. serde's default behaviour is to ignore unknown fields. Fix: add deny_unknown_fields to each #[serde(default, rename_all="kebab-case")] struct (compatible with serde default) and/or post-parse known-key validation.

### [66] (tooling / t_config) Typo'd [python] target key silently produces a 3.13 build instead of the documented hard rejection — same unknown-key root cause, defeats the runtime-version floor
- **Status:** ✅ fixed — typhon.toml deny_unknown_fields (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc/crates/tyc/src/config.rs PythonConfig (#[serde(default, rename_all="kebab-case")], no deny_unknown_fields). Unknown key `targett` dropped; `target` keeps default "3.13"; TyphonConfig::validate() never sees the user's intended 3.10.

### [67] (tooling / t_cli) tyc check diagnostics are mislocated (wrong line numbers + blank/desugared source snippet) for any file using the `?` propagation or `as!` cast operators
- **Status:** ⏳ pending
- **Root cause:** tyc-syntax/src/preprocess.rs: expand_checked_casts (line ~3842) and expand_question_ops both call prepend_typhon_runtime_alias_import (line 6120), which physically inserts an import line into the source body, and the `?` rewrite also expands a single statement into a multi-line isinstance-Err ladder. The type-check/diagnostic-rendering path in `tyc check` computes diagnostic spans against this pre

### [57] (vm_parity / e_pub) Relative import ascending beyond the source root (`from ...x` / `from ..x`) passes `tyc check` clean but crashes at runtime with ImportError, even when the target module does not exist
- **Status:** ✅ fixed — over-deep relative-import off-by-one (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc-resolve/src/lib.rs `check_unknown_modules` (ImportFrom arm, ~line 590) explicitly skips all relative imports (`if imp.level > 0 { /* we don't model relative path resolution at this layer */ skip }`), so dot-depth is never validated against the project root. The build module-graph resolver (tyc/src/commands/build.rs) likewise resolves `..`/`...` relative to src/ as if src/ had a parent package,

### [60] (vm_parity / r_preprocess) Postfix `rescue` on a semicolon-joined statement line fails to apply the generated `?` unwrap (spurious `type_mismatch` on compiled path; VM crash)
- **Status:** ⏳ pending
- **Root cause:** Same as finding 1: tyc-syntax/src/preprocess.rs `expand_question_ops_inner` / `expand_question_ops` are not semicolon-aware. The `rescue` lowering correctly emits `try_result(...)?` (see `try_rewrite_rescue_at`, ~line 4212), but the subsequent `?` pass cannot place the unwrap when the statement is not the sole statement on its physical line.

### [69] (vm_parity / v_vmstdlib) VM: collections.Counter is a plain dict — arithmetic (+,-,&,|) raises TypeError and repr lacks the Counter(...) wrapper; compiled path matches CPython
- **Status:** ⏳ pending
- **Root cause:** VM Counter shim in tyc-vm (collections shim) returns a plain Value::Dict rather than a Counter-tagged value, so __repr__ falls through to dict repr and __add__/__sub__/__and__/__or__ are not implemented. Needs a Counter value kind (or dict subtype tag) with multiset arithmetic and `Counter({...})` repr.

### [70] (vm_parity / v_vmstdlib) VM: dict | dict and dict |= dict (PEP 584 merge) raise TypeError; compiled path / CPython work
- **Status:** ⏳ pending
- **Root cause:** tyc-vm binary-op dispatch (interp.rs): BitOr / BitOrAssign handling does not special-case Value::Dict operands; it falls through to the generic 'unsupported operand' TypeError. Needs dict-merge semantics for `|` and `|=` (and analogously confirm set `|` still works).

### [71] (vm_parity / v_vmstdlib) VM: datetime/date missing strftime(), weekday(), isoweekday() — AttributeError; compiled path matches CPython
- **Status:** ⏳ pending
- **Root cause:** tyc-vm datetime shim (referenced in interp.rs / builtins.rs): the date/datetime native object only binds a subset of methods (isoformat present). strftime/weekday/isoweekday are not registered. Add them (strftime via a format-spec translation, weekday()=Mon=0, isoweekday()=Mon=1).

### [72] (vm_parity / v_vmsem) VM eagerly materializes finite generators, so side effects run at call time (all of them) instead of lazily per-pull — early break / interleaved side effects produce different output than CPython
- **Status:** ⏳ pending
- **Root cause:** tyc-vm: generators are materialized eagerly (run to completion at call() time, buffering yielded values, returning an iterator over the collected list — capped at GENERATOR_CAP=1_000_000). docs/vm.md only warns about INFINITE generators and send()/throw; it does not flag that FINITE generators with observable side effects (mutation, I/O, early break) silently diverge from CPython laziness. This is

### [73] (vm_parity / v_vmcontrol) VM stores repr-string of the missing key in KeyError.args instead of the original key object (tyc run vs CPython divergence)
- **Status:** ⏳ pending
- **Root cause:** tyc/crates/tyc-vm/src/interp.rs:3535 (and 3488 for __getitem__ on subscript, 3716 for del): the dict-miss path raises `key_error(key.py_repr())`, passing the key's repr STRING as the exception message. tyc-vm/src/error.rs:95 `key_error` builds `VmException::new("KeyError", msg)` with that string, so the stored args[0] is the repr-string rather than the original key Value. CPython constructs `KeyEr


## MEDIUM

### [76] (docs / e_pub) Documented `pub *` + package-level `pub` override pattern is rejected by `tyc::pub_name_collision` — PACKAGING.md §4 promises the package-level name wins, but the compiler errors
- **Status:** ⏳ pending
- **Root cause:** The `pub *` aggregation / collision detector in tyc/src/commands/build.rs (and the parallel check-path collision pass) treats the package's own `pub` names in `__init__.ty` as participants in the collision set under the synthetic key `<package>` instead of giving them precedence over sibling-aggregated names. Per PACKAGING.md the package-level declaration should win and suppress the sibling's same

### [75] (false_positive / fp_narrow) Dependent `and`-chain attribute narrowing rejected: left conjunct's narrowing not visible to right conjunct (o.inner is not None and o.inner.val is not None)
- **Status:** ⏳ pending
- **Root cause:** tyc-types/src/lib.rs narrowing collector for BoolOp(And): narrowings produced by an earlier conjunct are not threaded into the environment used to infer/narrow later conjuncts. When the right conjunct `o.inner.val is not None` is processed, `o.inner` is still typed `Inner | None`, so `attr_path_of`/the nullable-attr narrowing for `o.inner.val` is computed against a still-nullable base and the deep

### [74] (soundness / s_compre) set-comprehension element expression type is unchecked against set[T] annotation
- **Status:** ⏳ pending
- **Root cause:** Same comprehension-typing gap as the dict-comp key, applied to SetComp element. The set-comp element-expression type is not unified with the expected set[T] element type. Mirrors the known-open list-comp element-expression gap but for the set surface; included to characterize the full boundary (list/set/dict comprehensions all skip element/key/value assignability while plain literals are checked).

### [77] (tooling / t_dty) tyc check --stubs misses decorator drift (e.g. @staticmethod present in impl, absent in .dty) — silent false negative
- **Status:** ⏳ pending
- **Root cause:** tyc-emit/src/stubtest.rs `function_shape` records only param names, param annotations and the return annotation — it never inspects `decorator_list`. So @staticmethod / @classmethod / @property presence is invisible to compare_modules / diff_stub_against_impl (tyc/src/commands/check.rs:1284). Decorator parity (at least the receiver-affecting staticmethod/classmethod/property) should be part of the

### [78] (vm_parity / v_vmstdlib) VM: math.isclose is missing — AttributeError; present in CPython and compiled path
- **Status:** ⏳ pending
- **Root cause:** tyc/crates/tyc-vm/src/builtins.rs math-module table (~lines 2073-2441): no `isclose` entry. Add `isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0)` matching CPython's algorithm: abs(a-b) <= max(rel_tol*max(abs(a),abs(b)), abs_tol), with inf/nan handling.

### [79] (vm_parity / v_vmstdlib) VM: %-format with a mapping (e.g. "%(name)s" % {...}) raises ValueError: unsupported format character '('; compiled path / CPython work
- **Status:** ⏳ pending
- **Root cause:** tyc-vm string %-format implementation: the format-spec parser does not handle the `(key)` mapping syntax after `%`; it treats `(` as a conversion character and errors. Needs to parse `%(key)conv` and look up `key` in the RHS mapping.

### [80] (vm_parity / v_vmsem) VM ignores reflected-operator subclass priority: for a+b where type(b) subclasses type(a) and overrides __radd__, CPython calls b.__radd__(a) first, VM calls a.__add__(b) first
- **Status:** ⏳ pending
- **Root cause:** tyc-vm binary-operator dunder dispatch: it always tries the left operand's __add__ before the right operand's reflected __radd__. CPython's data-model rule gives the reflected method priority when the right operand's type is a proper subclass of the left operand's type and overrides the reflected dunder. The VM's dispatch order needs the subclass-priority special case.

### [81] (vm_parity / v_vmcontrol) VM does not populate exception __cause__ (raise ... from) or __context__ (implicit chaining); both stay None under tyc run
- **Status:** ⏳ pending
- **Root cause:** tyc-vm exception raising (tyc/crates/tyc-vm/src/interp.rs raise-statement handling and the VmException model in value.rs) does not implement PEP 3134 chaining: the `from` clause is not wired to set __cause__, and the implicit __context__ (currently-handled exception) is not threaded onto a newly raised VmException. VmException needs cause/context fields populated by the raise handler.


## LOW

### [82] (docs / fp_resultasync) Docs claim postfix `rescue` works after `if`/`while`/`assert`, but those forms fail to parse with a misleading error
- **Status:** ⏳ pending
- **Root cause:** REFERENCE.md §5.4 states postfix `rescue` 'works after `return`/`if`/`while`/`assert`'. Only `return`/`let`-RHS tail positions are actually implemented in the preprocessor/parser (tyc-syntax rescue lowering). The `if`/`while`/`assert` cases are not handled — the rescue `:` collides with the statement's own `:`. The feature is tagged '(Unreleased)' in the docs, so the parse rejection is arguably ex

### [83] (tooling / t_config) [checker] external accepts arbitrary unknown values (e.g. "mypy") and silently no-ops instead of validating against the {none, ty} allow-list
- **Status:** ✅ fixed — [checker] external allow-list (branch claude/typhon-adversarial-review-31ep54)
- **Root cause:** tyc/crates/tyc/src/config.rs TyphonConfig::validate() validates class-default, model-extra and strictness severities but has no allow-list check for self.checker.external. Add an ALLOWED_EXTERNAL_CHECKERS = ["none","ty"] check mirroring the existing InvalidSeverity pattern.
