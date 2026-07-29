# Typhon — 30-second cheat sheet

Typhon is a statically-typed, stricter superset of Python that compiles to
clean CPython 3.13+. The CLI is `tyc`.

## Bindings

    let name: str = "Ada"      # immutable; function-scoped
    mut count: int = 0         # rebindable, same declared type
    count = count + 1

Module-level assignments default to `let` and don't need the keyword.
Inside a function body the keyword is mandatory (`tyc::missing_binding_kind`).

Tuple unpacking carries annotations per element:

    let (a: int, b: str) = func(x, y)         # both elements typed
    let (a: int, b)      = pair()             # b's type inferred from RHS
    let (a, b)           = pair()             # both inferred

## Annotations (Rule 1)

Every parameter and every return type carries an annotation:

    def greet(name: str) -> str:
        return f"hi {name}"

`T?` is shorthand for `T | None`. A function that returns nothing must say
`-> None` explicitly.

`*args` and `**kwargs` also need annotations — the idiomatic spelling is
`*args: object` / `**kwargs: object` when a function is genuinely variadic
(typically generic decorators):

    def trace(f: Callable[..., R], *args: object, **kwargs: object) -> R: ...

## Classes

    class Point:                # default = dataclass(slots=True, frozen=False)
        x: int
        y: int

    class Vec frozen:           # immutable, slotted dataclass
        dx: int
        dy: int

    # When combining `frozen` with inheritance the modifier comes BETWEEN
    # the class name and the base list (not after `(Base)`):
    class Square frozen(Shape):  # ✅ parses
        side: float
    # `class Square(Shape) frozen:` does NOT parse.

    plain class Bag:            # no decorator at all
        items: list[str]

    class! Counter:             # mutable, no slots — wide-open
        value: int

    model User:                 # pydantic.BaseModel
        name: str
        age: int

    enum Color:                 # class Color(enum.Enum): (v0.11.0)
        RED                     #   RED = enum.auto()
        GREEN                   #   GREEN = enum.auto()
        BLUE = 4                #   BLUE = 4  (explicit values preserved)

## Methods live in `impl` (Rule 4)

    class Point:
        x: int
        y: int

    impl Point:
        def length(self) -> float:
            return (self.x * self.x + self.y * self.y) ** 0.5

Methods inside the `class` body trigger `tyc::method_in_class_body`.

## Deep-immutable bindings (`freeze let`)

    freeze let TAGS: list[str] = ["a", "b"]
    freeze let CONFIG: dict[str, int] = {"port": 8080}

Recursively wraps `list → tuple`, `dict → MappingProxyType`,
`set → frozenset` at binding time so the value cannot be mutated
through any reference. Module-level only for v1.

## Public API (`pub`)

    pub def greet(name: str) -> str: ...
    pub class User: ...
    pub let API_VERSION: str = "v1"

Modules with at least one `pub` declaration emit a synthesised
`__all__ = [...]` listing every `pub` name in source order. Use to
distinguish public surface from internal helpers.

Place a single `pub *` line at the top of a package's `__init__.ty`
and the build pipeline re-exports every direct-sibling module's `pub`
names (and, transitively, every direct sub-package's effective
public surface) from the package facade. Colliding sibling names
fire `tyc::pub_name_collision`; `pub *` outside `__init__.ty` is a
no-op and triggers `tyc::pub_star_outside_init`.

## Newtype (nominal alias)

    newtype UserId = int
    newtype Email = str

    def greet(uid: UserId) -> str:
        return f"hi {uid}"

    let me: UserId = UserId(7)         # explicit construction
    let raw: int = me                  # escape upward is free

Bare `int` flowing into a `UserId` slot is rejected as
`tyc::type_mismatch`; passing the wrong-typed argument to the
constructor (`UserId("seven")`) fires `tyc::newtype_violation`. Use
`type` instead of `newtype` when you want a transparent,
bidirectional alias.

## Checked boundary cast (`as!`)

    let data = resp.json() as! dict[str, int]
    let uid  = row[0] as! int

The one-line, *sound* replacement for the `unsafe:`-block +
re-assertion dance at an untyped boundary. The checker types the
expression as the target (so the boundary value — which may be `Any` —
needs no `unsafe:` block), and at runtime the value's shape is checked
against the target, raising `TypeError` on a mismatch (recursing
through `list[…]` / `dict[…]` / `tuple[…]` / unions). Unlike a
static-only re-assertion or an unchecked `as`, an `as!` can only let
through values it can't prove wrong. v1: single-line value positions
(after `=` / `return` / `yield`, or a bare expression).

## Sealed unions + exhaustive `match`

    type Shape = Circle | Square      # sealed union = alias over the variants

    class Circle:
        radius: float
    class Square:
        side: float

    def area(s: Shape) -> float:
        match s:
            case Circle(radius):
                return 3.14159 * radius * radius
            case Square(side):
                return side * side

Missing variants without a wildcard arm: `tyc::non_exhaustive_match`.

## Result + `?`

    def parse_count(s: str) -> Result[int, str]:
        let n: int = int(s) rescue e: f"not an int: {s}"  # catch + map + forward
        return Ok(n)

    def doubled(s: str) -> Result[int, str]:
        let n: int = parse_count(s)?   # forwards Err to caller
        return Ok(n * 2)

`EXPR rescue NAME: ERR` lifts a throwing boundary call into a `Result` (sugar for
`try_result(lambda: EXPR, lambda NAME: ERR)?`) — no `try`/`except`, no lambdas.

## Concurrency

Both forms live inside an `async def`.

    gather:                   # asyncio.TaskGroup, run concurrently
        a = fetch(url1)       # no `let`/`mut` — `gather:` introduces the names
        b = fetch(url2)       # no `go` — the block already runs them together

    go send_welcome(user)     # fire-and-forget; the runtime holds a strong ref

## Lazy loading

Module level only, and the alias is required on 3.13 / 3.14 targets.

    lazy import pd = pandas   # deferred until first use

## Comptime

    comptime let API_URL: str = env("API_URL", "http://localhost:8080")

## Commands

    tyc init [NAME]           # scaffold a project
    tyc build                 # parse, check, desugar, emit, format
    tyc check                 # parse + type-check only (CI-friendly)
    tyc fmt                   # format .ty files in place
    tyc run                   # default: in-process VM; --compile for CPython interop
    tyc migrate <path.py>     # rewrite typed Python into Typhon
    tyc explain <code>        # describe a diagnostic (tyc::immutable_assign, ...)
    tyc lsp                   # speak LSP on stdio

See https://github.com/CodeHalwell/Typhon/tree/main/docs for the full reference.
