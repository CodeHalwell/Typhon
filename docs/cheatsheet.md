# Typhon — 30-second cheat sheet

Typhon is a statically-typed, stricter superset of Python that compiles to
clean CPython 3.13+. The CLI is `tyc`.

## Bindings

    let name: str = "Ada"      # immutable; function-scoped
    mut count: int = 0         # rebindable, same declared type
    count = count + 1

Module-level assignments default to `let` and don't need the keyword.
Inside a function body the keyword is mandatory (`tyc::missing_binding_kind`).

## Annotations (Rule 1)

Every parameter and every return type carries an annotation:

    def greet(name: str) -> str:
        return f"hi {name}"

`T?` is shorthand for `T | None`. A function that returns nothing must say
`-> None` explicitly.

## Classes

    class Point:                # default = dataclass(slots=True, frozen=False)
        x: int
        y: int

    class Vec frozen:           # immutable, slotted dataclass
        dx: int
        dy: int

    plain class Bag:            # no decorator at all
        items: list[str]

    class! Counter:             # mutable, no slots — wide-open
        value: int

    model User:                 # pydantic.BaseModel
        name: str
        age: int

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

## Newtype (nominal alias)

    newtype UserId = int
    newtype Email = str

    def greet(uid: UserId) -> str:
        return f"hi {uid}"

    let me: UserId = UserId(7)         # explicit construction
    let raw: int = me                  # escape upward is free

Bare `int` flowing into a `UserId` slot is rejected
(`tyc::newtype_violation`). Use `type` instead of `newtype` when you
want a transparent, bidirectional alias.

## Sealed unions + exhaustive `match`

    sealed union Shape:
        Circle(radius: float)
        Square(side: float)

    def area(s: Shape) -> float:
        match s:
            case Circle(radius):
                return 3.14159 * radius * radius
            case Square(side):
                return side * side

Missing variants without a wildcard arm: `tyc::non_exhaustive_match`.

## Result + `?`

    def parse_count(s: str) -> Result[int, str]:
        try:
            return Ok(int(s))
        except ValueError:
            return Err(f"not an int: {s}")

    def doubled(s: str) -> Result[int, str]:
        let n: int = parse_count(s)?   # forwards Err to caller
        return Ok(n * 2)

## Concurrency

    gather:                   # asyncio.TaskGroup, run concurrently
        let a: bytes = go fetch(url1)
        let b: bytes = go fetch(url2)

    lazy import pandas        # deferred until first use

## Comptime

    comptime let API_URL = env("API_URL", "http://localhost:8080")

## Commands

    tyc init [NAME]           # scaffold a project
    tyc build                 # parse, check, desugar, emit, format
    tyc check                 # parse + type-check only (CI-friendly)
    tyc fmt                   # format .ty files in place
    tyc run                   # default: in-process VM; --compile for CPython interop
    tyc migrate <path.py>     # rewrite typed Python into Typhon
    tyc explain <code>        # describe a diagnostic (tyc::immutable_assign, ...)
    tyc lsp                   # speak LSP on stdio

See https://typhon.dev/lang for the full reference.
