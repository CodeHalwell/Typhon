# Typhon Diagnostics — Exhaustive Catalog

Every Typhon compiler error or warning carries a stable code of the form `tyc::<short_code>`. The URL pattern `https://typhon.dev/lang/diagnostics/<short_code>` resolves to the canonical doc page. `tyc explain <code>` prints the catalog entry offline. `tyc explain --list` prints every code.

This file is the field guide. Where a diagnostic's severity is configurable, the controlling `[strictness]` key is named in parentheses.

---

## 1. Binding & scope (Rule 2 — `let`/`mut`)

### `tyc::missing_binding_kind` — error

A local assignment inside a function body introduces a new name without `let` or `mut`. Module-scope assignments default to `let` and don't need the keyword; only function/method bodies trigger this.

```ty
def main() -> None:
    count = 0          # error: missing `let` or `mut`
```

**Fix:** Prefix with `let` (immutable) or `mut` (rebindable).

### `tyc::immutable_assign` — error

Re-assignment to a binding declared with `let`. `let` is single-assignment for the binding's lifetime in scope.

```ty
let count: int = 0
count = count + 1     # error: cannot assign to immutable binding
```

**Fix:** Change the declaration to `mut`.

### `tyc::missing_initialiser` — error

`let NAME: T` (or `mut NAME: T`) written without `= <expr>` AND the binding is never assigned anywhere. The `let NAME: T` declare-then-assign form is allowed (v0.7.0) provided the definite-assignment pass accepts the control flow.

```ty
let x: int            # error: missing initialiser (never assigned)
```

**Fix:** `let x: int = 1`, or add an assignment that the definite-assignment pass accepts.

### `tyc::use_of_uninitialised` — error (v0.7.0)

A `let NAME: T` declare-only binding is read on a control-flow path that didn't assign it. The DA pass intersects assigned sets across non-diverging `if`/`match` arms; `return`/`raise`/`continue`/`break` arms are excluded. Loops do **not** propagate body assignments. `match` over a sealed-union or `Result` counts as exhaustive without `case _:` when every variant is a class pattern.

```ty
def bad(cond: bool) -> int:
    let x: int
    if cond:
        x = 5
    return x          # error: not assigned on the else path
```

**Fix:** Initialise inline (`let x: int = 0`), or assign in every non-diverging arm.

### `tyc::no_block_shadow` — error

A second `let`/`mut` would shadow an outer function-scoped binding of the same name. Python has no block scope, so the "inner" binding would actually rebind the outer one. Sibling `if`/`elif` and `case` arms each get fresh-binding behaviour (v0.6.0+/v0.7.0+); only true shadow situations fire.

```ty
let x: int = 1
if True:
    let x: int = 2    # error: cannot shadow outer `x`
```

**Fix:** Rename the inner binding. **Do not** use `let` again on the same name — re-bind with `mut` or pick a fresh name.

### `tyc::pattern_shadows_outer` — error (firing site added in v0.8.0)

A `case` pattern captures a name that already exists as an immutable `let` in an enclosing scope. Python `case` captures are rebindings that outlive the `match`, so this would clash with Rule 2.

```ty
let value: int = 99
match b:
    case Wrap(value):   # error: pattern_shadows_outer
        print(value)
```

**Fix:** Rename the capture (`case Wrap(inner):`). Bare names in class patterns are always fresh bindings in Python's `match`.

### `tyc::unknown_name` — error

An identifier is referenced but no enclosing scope defines it. If the name is literally `self`, see `tyc::self_outside_impl`.

```ty
def main() -> None:
    print(greting)    # error: cannot find 'greting'
```

**Fix:** Declare the name, fix the typo, or add the import.

---

## 2. Types & nullability (Rule 1)

### `tyc::missing_annotation` — error

A function parameter or return type lacks an explicit annotation. Every parameter and return type is annotated; functions that return nothing must spell `-> None`. Since v0.9.0 `*args` / `**kwargs` are also enforced — the canonical idiom for genuinely variadic functions is `*args: object` / `**kwargs: object`. The v0.9.0 diagnostic text also drops the double-backtick wrapping the previous renderer produced (was `` `parameter `x`` ``).

```ty
def greet(name):       # error: missing annotation on `name`
    return f"hi {name}"

def trace(f, *args, **kwargs):    # error since v0.9.0
    ...
```

**Fix:** `def greet(name: str) -> str: …`; `def trace[R](f: Callable[..., R], *args: object, **kwargs: object) -> R: …`

### `tyc::implicit_any` — error (controlled by `[strictness] no-implicit-any`)

Bare collection annotation (`list`, `dict`, `tuple`, `set`, `frozenset`) without element-type parameters. `no-implicit-any = true` is the default; the flag is parsed for forward compat but today the check is always on.

```ty
def keys(d: dict) -> list:   # error
    return list(d.keys())
```

**Fix:** Spell parameters: `dict[str, int] -> list[str]`.

### `tyc::empty_collection_no_annotation` — warning (v0.8.0)

An empty collection literal (`let xs = []`, `let d = {}`, `let s = set()`) without an annotation or expected type. The element type can't be inferred from the literal alone; downstream operations all see `list[Unknown]` and lose type safety.

```ty
let xs = []                 # warning
```

**Fix:** Annotate (`let xs: list[str] = []`) or seed with one element of the target type (`mut xs = [first_value]`).

### `tyc::typing_alias_in_annotation` — warning (v0.8.0)

A bare `typing` alias (`List[T]`, `Optional[T]`, `Dict[K, V]`, `Union[A, B]`, `Tuple[A, B]`, `Set[T]`, `FrozenSet[T]`) used in an annotation. Consistent with the existing import-level `tyc::typing_alias_deprecated`; this catches in-place uses that may have arrived via wildcard imports or `typing.*` qualified access.

```ty
def f(items: List[str]) -> Optional[int]: ...   # warning
```

**Fix:** Use lowercase built-ins / Typhon sugar: `list[str]`, `int?`, `dict[K, V]`, `A | B`.

### `tyc::type_mismatch` — error

An expression of one type used where another was expected (arguments, returns, assignments, container elements, `mut` rebinds). Also fires on cross-newtype assignment (`PostId` into `UserId` slot) and bare-base-to-newtype flow in some cases. Help text reads "change the value so it produces `<expected>`, or widen the annotation to `<expected> | <found>` if both are intended" (v0.6.0).

```ty
let result: int = double("3")   # error: expected `int`, found `str`
```

**(v0.12.0) Third-party argument types.** This is also the code you'll see when a wrong-*typed* argument is passed to a fully-typed third-party function **or constructor** — venv signature introspection (`tyc-venv`) now recovers parameter/return *annotations* (scalars, `Optional[X]` / `X | None`, parametric containers, fixed-arity tuples), so a dependency exposing `def fetch(url: str, …)` called as `fetch(12345)`, or constructing a `Client(host: str, port: int)` with `port="oops"`, is rejected at check time, not just on arity. Anything not confidently modelled degrades to a permissive `Unknown`, so the check only adds true positives. The dependency must ship **inline** annotations for this path — a stub-only library like `requests` (typed via typeshed's `types-requests`, not in its own source) degrades to `Unknown` here and is instead caught by the `ty`/typeshed pass (`[checker] external = "ty"`) or a `.dty` stub. A declared dependency that can't be introspected at all surfaces the separate `unintrospectable-dependency` warning (`[strictness] unintrospectable-dependency`, default `"warn"`) rather than silently skipping these checks.

**Fix:** Convert at the boundary, or correct the annotation.

### `tyc::nullable_use` — error

A value of type `T?` (= `T | None`) used where `T` is required. Render no longer shows `?` as the "expected" type (v0.6.0); the formatter substitutes the resolved bound.

```ty
def length_of(name: str?) -> int:
    return len(name)            # error
```

**Fix:** Narrow with `if name is not None:` first.

### `tyc::operator_type_mismatch` — error

Binary operator with clearly-incompatible operands (both types fully known, neither a user-defined class). Also fires on cross-newtype arithmetic where two distinct newtypes share a numeric base (`LogIndex + Term`).

```ty
let result: str = "x" + 1   # error: unsupported operand types for `+`
```

**Fix:** Convert one operand, or wrap explicitly at the newtype boundary.

### `tyc::attribute_not_found` — error

Attribute access on a value whose static type doesn't declare that attribute (and isn't `Any`). v0.8.0 widened the firing site from `TypeVar`-bounded parameters to also include direct class instances (`p: Point`) and generic-class receivers (`s: Stream[int]`). v0.8.1 narrowed it again: venv-introspected third-party classes now carry a `partial` shape marker on `InterfaceShape`, and `class_hierarchy_fully_known` returns `false` whenever any class in the chain is partial. The net effect: calls like `uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`, `fastapi.Request.body(...)` against third-party libraries do not false-positive. Skipped in `unsafe:` regions and on dunder / leading-underscore names.

```ty
let p: Point = Point(x=1, y=2)
print(p.z)            # error: attribute `z` not defined on `Point`
```

**Fix:** Correct the name, add the missing field, or wrap a genuinely dynamic call in `unsafe:`.

### `tyc::tuple_index_out_of_range` — error

Constant integer index out of range for a fixed-arity tuple (`tuple[A, B]`). Homogeneous tuples (`tuple[T, ...]`) are not checked.

```ty
let t: tuple[int, int] = (1, 2)
print(t[2])           # error
```

**Fix:** Use an in-range index, or change the type to homogeneous.

### `tyc::cyclic_type_alias` — error

`type` alias chain forms a cycle. The checker surfaces the cycle once then rewrites to `Any` so downstream type-checking continues (v0.3.0).

```ty
type A = B
type B = A            # error
```

**Fix:** Anchor one alias to a concrete type.

### `tyc::newtype_violation` — error

Wrong-typed argument passed to a `newtype` constructor (`Email("alice")` where the base is `int`). Also fires on bare-primitive-into-newtype flow (v0.3.1 — `int → UserId` routes here instead of `tyc::type_mismatch`).

```ty
newtype UserId = int
let bad: UserId = UserId("seven")   # error: argument is `str`, base is `int`
fetch_user(42)                       # error if fetch_user expects UserId
```

**Fix:** Pass the right base value, or wrap explicitly at the boundary.

### `tyc::newtype_invalid_base` — error (v0.8.0)

A `newtype` RHS that isn't a type expression. Before v0.8.0 the base silently resolved to `Type::Unknown` and every downstream check accepted any value. Now the common non-type shapes (string / numeric / boolean / `None` literals, lambdas, generic call expressions other than `Result[…]`-style subscripts) are rejected at the declaration site.

```ty
newtype Bogus = "string literal"     # error
newtype X = 42                       # error
newtype Maker = lambda x: x          # error
```

**Fix:** Use a proper type expression — `newtype UserId = int`, `newtype Email = str`, `newtype Result = Result[T, E]`.

### `tyc::div_by_zero_literal` — error (v0.3.0)

`/`, `//`, or `%` with a literal `0`, `0.0`, `-0`, `-0.0`, or unary-negated zero on the right. Constant-fold only — no flow analysis.

```ty
return sum(values) / 0   # error
```

**Fix:** Use a non-zero literal, or guard at runtime. Wrap in `unsafe:` if the throw is the intended behaviour.

### `tyc::python_semantic_drift` — warning

Typhon's checker rejects something CPython accepts. This is a bug in Typhon, not user code — file an issue with the snippet. The warning never blocks the build.

---

## 3. Async / concurrency

### `tyc::async_without_await` — warning

`async def` body never uses `await`. Marking a function `async` adds caller ceremony but buys nothing if the body never yields.

```ty
async def fetch_count() -> int:  # warning
    return 42
```

**Fix:** Remove `async`, or add the missing `await`.

Contract exemption (v0.15.0): an awaitless `async def` is **not** warned when it is async only to honour a contract it can't opt out of — implementing an async `interface` method (structural conformance) or overriding an async base-class method. So a trivial `impl ConsoleSink: async def deliver(...)` satisfying `interface Sink: async def deliver(...)` checks clean, with no dead `await asyncio.sleep(0)` no-op. An `async` impl of a *sync* method, or any awaitless `async def` with no contract, still warns.

### `tyc::missing_await` — error

A sync function calls an `async def` without awaiting it. The result is a coroutine, not the declared return value. `loop.run_until_complete(coro())` and `asyncio.run(coro())` are whitelisted (v0.6.0).

```ty
async def fetch() -> int: return 42
def main() -> int:
    return fetch()       # error
```

**Fix:** `await` it inside an async caller, or `asyncio.run(...)` at the top level.

### `tyc::blocking_in_async` — warning (controlled by `[strictness] blocking-in-async`; v0.3.0)

Direct call to a known-blocking stdlib function inside an `async def` body. Suppressed inside `unsafe:`. Direct-call only — receiver-method calls (`sock.recv`) are not yet flagged.

Registry: `time.sleep`; `input`; `requests.{get,post,put,delete,patch,head,options,request}`; `urllib.request.urlopen`; `subprocess.{run,call,check_call,check_output}`.

```ty
async def f(url: str) -> str:
    time.sleep(1)        # warning: blocks the event loop
```

**Fix:** Use async equivalent (`asyncio.sleep`, `httpx.AsyncClient`), or `asyncio.to_thread(time.sleep, 1)`.

### `tyc::auto_gather_missed` — advice

Adjacent `await CALLEE(...)` statements look gatherable, but at least one same-module async callee isn't `@gatherable`. The auto-gather pass only rewrites runs where every callee opts in. Only fires when `[strictness] auto-gather = true`.

```ty
async def fetch_a() -> int: ...        # missing @gatherable
@gatherable
async def fetch_b() -> int: ...
```

**Fix:** Add `@gatherable` to every same-module async callee in the run.

Cross-module reach (v0.14.2): the auto-gather pass also folds runs whose callees are `@gatherable` async functions **imported from another project module**, so this advice fires for an imported callee that's missing the decorator too.

### `tyc::gather_opportunity` — advice (controlled by `[strictness] suggest-gather`, default `true`; v0.14.2)

A run of 2+ adjacent **independent** awaited calls inside an `async def` — they could run concurrently in an explicit `gather:` block instead of sequentially. Unlike `auto_gather_missed`, this is **callee-agnostic** and **on by default**: it fires for awaited **method calls on imported clients** (`await client.get_user(...)`), the shape `auto-gather` never touches, because the suggested fix works for any awaitable with no `@gatherable` decorator.

```ty
async def load(client: Client, uid: int) -> tuple[User, list[Post]]:
    let user = await client.get_user(uid)     # advice: these 2 awaits …
    let posts = await client.get_posts(uid)   # … could run concurrently
    return (user, posts)
```

Independence is static data flow: a run breaks the moment a later await references a name bound earlier — including through the callee's receiver (`b = await a.next()`), keyword args, comprehensions, walrus, slices, f-strings — and a single non-matching statement between two awaits ends the run. It **never rewrites** (concurrency is a behaviour change the author opts into) and is advice-level (never blocks a build; renders as an editor hint via the LSP). When `auto-gather` is also on, runs it folds are gone before this pass runs, so they aren't double-reported.

**Fix:** Wrap the independent awaits in a `gather:` block, or set `[strictness] suggest-gather = false` to silence the nudge project-wide.

### `tyc::generator_return_type` — error

Function body contains `yield`/`yield from` but the return type isn't iterator-shaped. Python silently switches `def` semantics on `yield`.

```ty
def counts() -> list[int]:   # error
    yield 1
```

**Fix:** Use `Iterator[T]` / `Generator[T, S, R]` (or async variants for `async def`).

---

## 4. Classes, methods, fields (Rule 4)

### `tyc::manual_init` — error

A `class` body declares `__init__` directly. Typhon synthesises the constructor from field annotations.

```ty
class Point:
    x: int
    def __init__(self, x: int) -> None:   # error
        self.x = x
```

**Fix:** Remove the manual `__init__`. Set field defaults, or use a free factory function.

### `tyc::method_in_class_body` — error (controlled by `[strictness] methods-in-class-body`, default `"warn"`)

`def` inside `class Name:` instead of a sibling `impl Name:` block. The class body declares data; `impl` declares behaviour.

```ty
class Point:
    x: int
    def length(self) -> float:   # warn/error per config
        return 0.0
```

**Fix:** Move the `def` into an `impl Point:` block.

### `tyc::impl_unknown_class` — error

`impl NAME:` targets a class not declared in the current module / project. Methods would lower into a `__typhon_impl_NAME` pseudo-class the merge pass silently drops.

```ty
impl Point:           # error: no class `Point`
    def length(self) -> float: return 0.0
```

**Fix:** Declare the class, or correct the name. If targeting a sealed-union alias, declare the alias before the `impl` block.

### `tyc::duplicate_class` — error

Same class name declared more than once at the same scope.

```ty
class Point: x: int
class Point: y: int   # error
```

**Fix:** Rename, or merge — additional methods belong in a sibling `impl`.

### `tyc::duplicate_method` — error (v0.3.1)

Two `impl`/`extend` blocks on the same class both define a method with the same name. The desugar pass merges multiple `impl`/`extend` blocks as a union; duplicates would silently lose one definition. Also fires when a method exists on both `impl Union:` and `impl Variant:` (v0.6.0).

```ty
impl Box:
    def get(self) -> int: return self.value
extend Box:
    def get(self) -> int: return self.value * 2   # error
```

**Fix:** Rename, delete the duplicate, or merge bodies.

### `tyc::extend_builtin` — error (v0.3.0)

`extend` targets a built-in with a parametric form (`list[int]`). Built-ins are extended by their bare name only.

```ty
extend list[int]:           # error
    def first(self) -> int: ...
```

**Fix:** Drop the brackets: `extend list:` is the supported form.

### `tyc::self_outside_impl` — error

`self` referenced outside an `impl` method body.

```ty
def length(self) -> float:   # error: `self` not available
    return 0.0
```

**Fix:** Move into an `impl` block, or use an explicit parameter.

### `tyc::class_attr_shadows_slot` — warning

A `class` body contains only annotated defaults with no methods or per-instance fields — reads like a constants namespace, but `@dataclass(slots=True)` makes each name a slot descriptor at runtime.

```ty
class Limits:
    MAX_RETRIES: int = 3   # warning
```

**Fix:** Annotate as `ClassVar[int]`. For nullary sealed-union variants, use `class Foo frozen: pass` and match with `case Foo():`.

Since v0.9.0 the warning no longer false-positives on classes whose only annotated defaults are mutable literals (`list[str] = []`, `dict[str, int] = {}`, `set[int] = set()`). Those defaults are rewritten at desugar time into `default_factory` calls — each instance gets its own value rather than sharing a single constant.

### `tyc::field_default_ordering` — error (v0.7.0)

A non-defaulted field declared after a defaulted one. The synthesised `__init__` would raise `TypeError` at import time. Checked for `class`, `class frozen`, `class!`, and `plain class`; skipped for `model`/Pydantic and `interface`/Protocol.

```ty
class Worker:
    name: str
    retries: int = 3
    queue_size: int     # error
```

**Fix:** Move every non-defaulted field above defaulted ones, or use a factory function.

### `tyc::frozen_assign` — error

Field assignment on a `frozen` class outside the constructor.

```ty
frozen class Identity: name: str
let id: Identity = Identity(name="Alice")
id.name = "Bob"        # error
```

**Fix:** Construct a new value instead of mutating.

### `tyc::freeze_not_freezable` — error (v0.9.0)

`freeze let X = <expr>` where the RHS constructs a non-`frozen` user class. Before v0.9.0 the failure surfaced as a runtime `TypeError` at first import; v0.9.0 pre-validates the RHS at check time.

```ty
class Counter:                       # not frozen
    value: int

freeze let CFG = Counter(value=0)    # ❌ tyc::freeze_not_freezable
```

**Fix:** Mark the class `frozen`, switch to a built-in container (list/dict/set get wrapped automatically), or use a plain `let` if mutability is wanted.

```ty
class Counter frozen:
    value: int

freeze let CFG = Counter(value=0)    # ✓
```

### `tyc::missing_field_init` — error

An instance constructed via `X.__new__(X)` or `object.__new__(X)` (bypassing `__init__`) escapes the function without every required field assigned. "Escape" = `return`, pass as call argument, or annotated assignment into a container/outer binding. Tracking is dropped on `setattr`, method calls on the binding, reassignment, or `unsafe:`. Trivial factory-helper shapes are recognised (v0.5.0): `def make(): return X.__new__(X)` propagates the uninit set to call sites.

```ty
def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.base_url = "https://api.example.com"
    return c                # error: missing `api_key`
```

**Fix:** Use `ApiClient(api_key=..., base_url=...)` — the regular constructor is checked by `tyc::arg_count`.

### `tyc::interface_isinstance` — error

`isinstance(x, Interface)` without `@runtime_checkable`. Structural interfaces describe a shape; `isinstance` only checks attribute presence, weakening the guarantee.

```ty
if isinstance(x, Writer): ...   # error
```

**Fix:** Decorate the interface `@runtime_checkable`, or rely on static narrowing.

### `tyc::interface_not_conforming` — error

A concrete type is used where an interface is required and is missing members or has incompatible signatures. v0.8.0 added position-by-position (contravariant) parameter-type checking on top of arity. Since v0.9.0 the arity diagnostic reads "got N non-self parameter(s), expected M" instead of the ambiguous "arity N; expected M".

```ty
emit(Sink(), "hi")    # error: Sink lacks `write`
```

**Fix:** Add the missing method, or pass a conforming value.

### `tyc::frozen_inheritance_conflict` — error (v1.0.0-alpha.2)

A `frozen` dataclass inherits from a non-`frozen` one (or vice versa) across an
in-module base. The combination type-checked clean but the emitted module
crashed on import with CPython's `TypeError: cannot inherit frozen dataclass
from a non-frozen one` (both directions). Only in-module dataclass bases are
compared, so external / non-dataclass bases are unaffected.

**Fix:** Make both classes `frozen`, or both non-`frozen`.

---

## 5. Call-site argument checking

### `tyc::arg_count` — error

Function, method, or class constructor called with the wrong number of arguments. Reported when the checker can't name specific missing params (too many positional, conflicting positional+kwarg, etc.). Class constructors check the synthesised `__init__` — every non-defaulted field is required. A `T?` field is still required unless `= None` is written explicitly. Inheritance is walked end-to-end. Cross-module / dotted-attribute / `.dty` stubs all flow through the same check; stubs win on collisions.

```ty
def add(a: int, b: int) -> int: return a + b
add(1, 2, 3)          # error: expected 2, got 3
```

**Fix:** Pass the correct number of arguments.

### `tyc::missing_argument` — error

The named form of `arg_count`: surfaces *which* required parameters were not supplied.

```ty
Agent(name="X", tools=[], description="…", instructions="…")
# error: missing required argument to `Agent`: `client`
```

**Fix:** Supply the named missing arguments. Works equally for venv-introspected third-party classes.

### `tyc::unknown_kwarg` — error

Keyword argument doesn't match any parameter and the callee has no `**kwargs`. Help suggests the closest name.

```ty
connect(host="localhost", prot=80)   # error: unknown keyword `prot`
```

**Fix:** Correct the spelling.

### `tyc::not_callable` — error

A non-callable value is called.

```ty
let n: int = 42
n()                   # error: `int` is not callable
```

**Fix:** Drop the parentheses or call the right name.

---

## 6. Error handling & `Result`

### `tyc::invalid_question_op` — error

`?` outside a `Result`-returning function, or inside a comprehension (v0.3.1). `?` only makes sense where an `Err` can be forwarded. Since v0.9.0 the help text mentions both causes explicitly — the diagnostic surfaces in two situations:

1. The enclosing function's return type can't hold the `Err`.
2. The `?` appears inside a `for ... in ...` comprehension or generator expression — comprehensions lower to nested loops in Python, so the surrounding function frame `?` would short-circuit out of is not the comprehension's frame.

```ty
def main() -> int:
    return try_parse()?                          # error: returns `int`, not `Result`

def collect(xs: list[str]) -> Result[list[int], str]:
    return Ok([parse(x)? for x in xs])           # error: `?` inside a comprehension
```

**Fix:** Change the return type to `Result[T, E]`, pre-extract with an explicit loop, or handle with `match`:

```ty
def collect(xs: list[str]) -> Result[list[int], str]:
    mut out: list[int] = []
    for x in xs:
        out.append(parse(x)?)
    return Ok(out)
```

### `tyc::result_error_mismatch` — error

`?` forwards an `Err` whose type doesn't match the enclosing `Result`.

```ty
def step() -> Result[int, ParseErr]: ...
def main() -> Result[int, IOErr]:
    let n: int = step()?   # error: ParseErr into IOErr
```

**Fix:** Convert at the boundary with `match` or `.map_err(...)`, or align error types.

### `tyc::missing_return` — error

A function with non-`None` return type has at least one path that reaches the end without `return`/`raise`. Exhaustive `match` over a sealed union or `Result` satisfies the analysis (v0.6.0+, including `match` on subject expressions like `match self.field:` or `match get_state():`).

```ty
def classify(n: int) -> str:
    if n > 0: return "positive"
    if n < 0: return "negative"
    # falls off the end                  ← error
```

**Fix:** Cover the missing path, or widen the return type.

### `tyc::raise_non_exception` — error (v1.0.0-alpha.2)

`raise 42` or `raise Problem(...)` where `Problem` is a plain dataclass (not a
`BaseException` subclass). These type-checked clean and then crashed at runtime
with CPython's `TypeError: exceptions must derive from BaseException`. The check
is **conservative** — only literals and locally-defined classes with a
fully-known non-exception ancestry fire; builtin / imported / venv-introspected
exceptions and `Exception` subclasses stay permissive.

**Fix:** Raise a `BaseException` subclass (give `Problem` an `Exception` base, or
raise a real exception type).

---

## 7. Match / sealed unions

### `tyc::non_exhaustive_match` — error (controlled by `[strictness] exhaustive-match`, default `"error"`)

`match` on a sealed union misses variants without a wildcard. Keyword patterns (`case Foo(field=x):`) satisfy exhaustiveness (v0.6.0).

```ty
type Shape = Circle | Square | Triangle
match s:
    case Circle(r): ...
    case Square(s): ...   # error: missing Triangle
```

**Fix:** Handle every variant, or add `case _:`.

---

## 8. Comptime / lazy

### `tyc::comptime` — error

A `comptime` binding's RHS cannot be evaluated at build time — unsupported operation or missing input (e.g. unset `env(...)`).

```ty
comptime let PORT: int = int(env("PORT"))   # error if unset
comptime let NOW: float = time.time()        # error: time forbidden
```

**Fix:** Supply the env var, or move the computation to runtime.

### `tyc::contains_secret_literal` — warning

A `comptime let` binding's name matches a secret-suffix heuristic: `*KEY`, `*TOKEN`, `*PASSWORD`, `*SECRET`, `*PASS`, `*PWD`. The build artifact would contain the resolved env-var value as a string literal.

```ty
comptime let API_KEY: str = env("MY_API_KEY")   # warning
```

**Fix:** Read at runtime via `os.environ[...]`.

### `tyc::lazy_usage` — error

A `lazy` form other than `lazy import name = module` or `lazy let NAME: T = expr`.

```ty
lazy from heavy import Thing   # error
lazy let X: T:                  # error — colon-block form does not parse
```

**Fix:** Use a supported form — `lazy import heavy` then `heavy.Thing`, or `lazy let X: T = expr`.

---

## 9. Pure functions / decorators

### `tyc::impure_pure_fn` — error

A function decorated `@pure` violates one of six purity rules:

1. **Synchronous** (no `async`/`await`/generators).
2. **Hashable parameters** (primitives, frozen dataclasses, tuples thereof).
3. **No I/O** — no `open`/socket/subprocess/print/logger/DB drivers in transitive call graph.
4. **No entropy/clock reads** (`random`, `secrets`, `time.*`, `datetime.now`, `os.urandom`, `uuid.*`).
5. **No reads/writes of mutable module-level state.** `comptime let` reads are fine.
6. **No exceptions raised** — pure functions express failure via `Result[T, E]`.

```ty
@pure
def now_plus(n: int) -> float:
    return time.time() + n      # error: clock read
```

**Fix:** Drop `@pure`, or restructure so impure work happens at the caller.

---

## 10. Resources / blocking / safety

### `tyc::resource_not_managed` — warning (controlled by `[strictness] require-with`; v0.3.0)

A call to a known resource-returning function bound without `with`. Registry: `open`, `socket.socket`, `sqlite3.connect`, `tempfile.NamedTemporaryFile`, `tempfile.TemporaryDirectory`, `tempfile.TemporaryFile`. `@contextmanager` / `@asynccontextmanager` factory bodies are exempt (v0.6.0).

```ty
let f = open(path)    # warning
return f.read()
```

**Fix:** `with open(path) as f: ...`. If the handle must outlive the call site, wrap in `unsafe:`.

### `tyc::unsafe_value_leak` — error (v0.3.0)

A binding introduced inside an `unsafe:` block is returned from a function with a concrete declared return type, without being re-asserted at the boundary. Rule 5: unsafe values carry `Unsafe[T]`.

```ty
def parse(raw: object) -> int:
    unsafe:
        let value = raw.maybe_int()   # Unsafe[T]
    return value                       # error
```

**Fix:** Annotate inside `unsafe:` (`let value: int = ...`), or re-bind with an annotation outside (`let checked: int = value`).

### `tyc::not_a_context_manager` — error (v1.0.0-alpha.2)

A `with` / `async with` whose subject is a **local** class that lacks the
context-manager protocol (`__enter__` / `__exit__`, or `__aenter__` /
`__aexit__` for `async with`). Previously this crashed the compiler; it is now
rejected at check time. Stdlib / third-party context managers and
`@contextmanager` / `@asynccontextmanager` factories stay permissive.

**Fix:** Add the protocol methods to the class, or use a `@contextmanager`
factory.

---

## 11. Imports / modules / visibility

### `tyc::unknown_module` — error

`import …` names a module not in stdlib, project, `typhon_runtime`, or `typhon.toml` `[dependencies]`.

```ty
import flask          # error if not declared
```

**Fix:** Spell correctly, add the dep and run `tyc sync`, or create the file.

### `tyc::unused_import` — warning (controlled by `[strictness] unused-import`, default `"warn"` since v0.8.0)

An import is never referenced in the module.

```ty
import os; import json
def main() -> None: print(json.dumps({"ok": True}))   # `os` unused
```

**Fix:** Remove the import, or rename with a leading underscore for side-effect-only imports.

### `tyc::orphan_py_import` — warning

A relative `.py` import resolves outside `src/`. `tyc build` only copies files under `src/` into the output, so the emitted Python would crash with `ModuleNotFoundError`.

```ty
from .helper import do_thing   # warning: helper.py outside src/
```

**Fix:** Move under `src/`, or use an absolute import that names a packaged module.

### `tyc::stdlib_module_shadow` — warning (v0.6.0, refined v0.7.0)

A project `.ty` file's stem matches a Python 3.13 stdlib top-level module name (`types`, `ast`, `string`, `io`, `json`, `dataclasses`, `logging`, `random`, `time`, …). The emitted `build/<name>.py` would land on `sys.path` and intercept transitive stdlib imports. Only fires for files at the top of the configured source directory (v0.7.0); nested files like `src/indexer/tokenize.ty` are exempt because they lower to `build/indexer/tokenize.py` which is NOT on `sys.path`.

```ty
# src/types.ty     ← warning
```

**Fix:** Rename (e.g. `lang_types.ty`, `records.ty`).

### `tyc::typevar_import_rejected` — error

`from typing import TypeVar`. Use PEP 695 syntax (`def f[T](x: T) -> T:`).

```ty
from typing import TypeVar    # error
```

**Fix:** Declare type parameters on the function/class with `[T]`.

### `tyc::typing_alias_deprecated` — error

Imports of deprecated capitalised aliases from `typing` (`List`, `Dict`, `Tuple`, `Set`, `FrozenSet`, `Type`).

```ty
from typing import List       # error
```

**Fix:** Use the lowercase built-ins: `list[int]`, etc.

### `tyc::pub_name_collision` — error (v0.7.0)

A `pub *` aggregation in `__init__.ty` finds two siblings exporting the same `pub` name.

```ty
# a.ty: pub def hello() -> str: …
# b.ty: pub def hello() -> str: …
# __init__.ty: pub *           ← error
```

**Fix:** Rename one, drop `pub` on one, or replace `pub *` with explicit re-exports.

### `tyc::pub_star_outside_init` — advice (v0.7.0)

`pub *` appears in a regular `.ty` module. The wildcard only has meaning in `__init__.ty`.

```ty
# src/mypkg/handlers.ty
pub *                # advice (no-op)
```

**Fix:** Move to `__init__.ty`, or remove.

---

## 12. Generics / TypeVar

### `tyc::typevar_bound` — error

An inferred type argument doesn't satisfy its TypeVar's declared bound.

```ty
def min[T: Comparable](a: T, b: T) -> T: ...
min(1, 2)             # error: `int` does not satisfy `Comparable`
```

**Fix:** Pass values whose type satisfies the bound, or drop the bound if unused.

### `tyc::kind_mismatch` — error

A higher-kinded type-constructor variable (the `F` in `class Functor[F[_]]:`) is
applied with the wrong arity, or bound to two different constructors in one call.
Landed in v1.0.0-alpha with HKT unification.

```ty
interface Functor[F[_]]:
    def map[A, B](self, fa: F[A], f: Callable[[A], B]) -> F[B]: ...

# error: `F` expects 1 type argument, applied with 2
# error: `F` bound to both `list` and `set` in one call
```

**Fix:** Apply the constructor variable with the arity its kind declares
(`F[_]` takes exactly one argument), and keep a single call's `F` bound to one
concrete constructor. Function-level HKT params (`def f[F[_]]`) are not yet
supported — see `TYPE_SYSTEM_FRONTIER.md`.

---

## 13. Parse / I/O

### `tyc::parse` — error

The source file cannot be parsed. Every later pass assumes a well-formed AST.

```ty
def main() -> None
    print("missing colon")   # error
```

**Fix:** Fix the syntax. Most parse errors are missing punctuation, indentation problems, or stray keywords.

### `tyc::io` — error

The compiler cannot read a source file (missing, unreadable, OS I/O error).

```text
tyc check missing.ty
# error: could not read file 'missing.ty': No such file or directory
```

**Fix:** Verify the path/permissions; correct imports if files were moved.

### `tyc::generic` — error (variant)

Catch-all early-phase diagnostic. The message describes the specific problem; there's no separate language rule attached. Variants get promoted to dedicated codes as compiler passes mature.

---

## 14. Config / tooling

### `tyc::invalid_config_value` — error

`typhon.toml` declares a value outside the allowed enumeration for a key. Failed eagerly at config-load time.

```toml
[emit]
class-default = "plain"   # error: expected `dataclass` | `pydantic`
```

**Fix:** Use one of the allowed values listed in the error message.

### `tyc::stub_mismatch` — error (controlled by `[strictness] stub-check`)

`tyc check --stubs` finds a mismatch between a `.dty` stub and its implementation module.

```text
# helper.dty: def add(a: int, b: int) -> int
# helper.ty:  def add(a: int, b: float) -> int: …     ← error
```

**Fix:** Sync stub and implementation, or make the symbol private (`_name`).

### `tyc::main_not_called` — advice

A module declares top-level `def main()` but never calls it.

```ty
def main() -> None: print("hello")   # advice
```

**Fix:** Add the standard entry pattern:

```ty
if __name__ == "__main__":
    main()
```

---

## Configurable strictness keys

| Diagnostic | Key | Default |
|---|---|---|
| `tyc::implicit_any` / `tyc::missing_annotation` | `[strictness] no-implicit-any` | `true` (= error; always on today) |
| `tyc::blocking_in_async` | `[strictness] blocking-in-async` | `"warn"` |
| `tyc::resource_not_managed` | `[strictness] require-with` | `"warn"` |
| `tyc::unused_import` | `[strictness] unused-import` | `"error"` |
| `tyc::non_exhaustive_match` | `[strictness] exhaustive-match` | `"error"` |
| `tyc::method_in_class_body` | `[strictness] methods-in-class-body` | `"warn"` |
| `tyc::stub_mismatch` | `[strictness] stub-check` | `"error"` |

Standard values for severity keys are `"off"`, `"warn"`, `"error"`.

---

## Severity-only summary

**Errors** (block the build by default): `arg_count`, `attribute_not_found`, `comptime`, `cyclic_type_alias`, `div_by_zero_literal`, `duplicate_class`, `duplicate_method`, `extend_builtin`, `field_default_ordering`, `frozen_assign`, `generator_return_type`, `generic`, `immutable_assign`, `impl_unknown_class`, `implicit_any`, `impure_pure_fn`, `interface_isinstance`, `interface_not_conforming`, `invalid_config_value`, `invalid_question_op`, `io`, `lazy_usage`, `manual_init`, `method_in_class_body` (default warn but commonly bumped), `missing_annotation`, `missing_argument`, `missing_await`, `missing_binding_kind`, `missing_field_init`, `missing_initialiser`, `missing_return`, `newtype_violation`, `no_block_shadow`, `non_exhaustive_match`, `not_callable`, `nullable_use`, `operator_type_mismatch`, `parse`, `pattern_shadows_outer`, `pub_name_collision`, `result_error_mismatch`, `self_outside_impl`, `stub_mismatch`, `tuple_index_out_of_range`, `type_mismatch`, `typevar_bound`, `typevar_import_rejected`, `typing_alias_deprecated`, `unknown_kwarg`, `unknown_module`, `unknown_name`, `unsafe_value_leak`, `unused_import`, `use_of_uninitialised`.

**Warnings**: `async_without_await`, `class_attr_shadows_slot`, `blocking_in_async`, `contains_secret_literal`, `orphan_py_import`, `python_semantic_drift`, `resource_not_managed`, `stdlib_module_shadow`.

**Advice**: `auto_gather_missed`, `gather_opportunity`, `main_not_called`, `pub_star_outside_init`.

---

## Quick lookup

When in doubt about a diagnostic, prefer `tyc explain <code>` over guessing — it prints the catalog entry directly from the binary. `tyc explain --list` enumerates every code. The docs site mirror is at `https://typhon.dev/lang/diagnostics/<code>`.

To find every site that emits a given code in the Rust source:

```bash
rg "TYC_CODE_NAME" tyc/crates
```

Every code is registered once in `tyc-diagnostics`.
