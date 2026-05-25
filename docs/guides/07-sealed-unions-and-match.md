# 7. Sealed unions and `match`

A sealed union is a *closed* set of variants — exactly the cases listed, no more. When you match on one, the checker forces you to handle every case. Adding a new variant lights up every match site that doesn't cover it. This is, in plain code-correctness terms, the single biggest static-safety win Typhon offers over typed Python.

## Declaring a sealed union

Use `type` to alias a union of classes:

```python
type Shape = Circle | Rectangle | Triangle

class Circle:
    radius: float

class Rectangle:
    width: float
    height: float

class Triangle:
    base: float
    height: float
```

`Shape` is now a closed type with exactly three inhabitants. Nothing outside this file can extend the union — that's what "sealed" means.

Variants are normal classes. They can have fields, defaults, and `impl` blocks, like anything else.

## Pattern-matching with `match`

```python
def area(s: Shape) -> float:
    match s:
        case Circle(radius):
            return 3.14159 * radius * radius
        case Rectangle(width, height):
            return width * height
        case Triangle(base, height):
            return 0.5 * base * height
```

Three things to notice:

- Each `case` names one variant and destructures its fields. The names (`radius`, `width`, etc.) are new bindings, narrowed to the variant's field types.
- There's **no wildcard** — and we didn't need one. The checker has verified that the three cases cover the whole union.
- Add a fourth variant (`Square`?) to the `type Shape = ...` line and this `match` goes red. The diagnostic tells you exactly which variant is missing.

### What missing cases look like

```python
def area(s: Shape) -> float:
    match s:
        case Circle(radius):
            return 3.14159 * radius * radius
        case Rectangle(width, height):
            return width * height
        # forgot Triangle
```

```
error[tyc::non_exhaustive_match]: match does not cover variant `Triangle`
 ┌─ src/shapes.ty:2:5
 │
2 │     match s:
 │     ^^^^^^^^ add `case Triangle(base, height): ...` or use `case _:`
```

This is what the design doc calls "the single biggest static-safety win over current Python" — and it pays off every time the union grows.

## Exhaustiveness is configurable

By default, `[strictness] exhaustive-match = "error"` in `typhon.toml`. You can lower it to `"warn"` or `"off"` during refactors, but the default — and the recommendation — is to keep it as an error.

## Wildcards and guards

When a wildcard *is* the intent (you genuinely want a catch-all), use `case _:`:

```python
def describe(s: Shape) -> str:
    match s:
        case Circle(_):
            return "circle"
        case _:
            return "polygon"
```

`case _:` opts out of exhaustiveness for the rest of the union. The checker won't complain when a new variant lands, so use it sparingly — the safety story relies on most matches being exhaustive.

Patterns can include guards (`if` clauses):

```python
def classify(s: Shape) -> str:
    match s:
        case Circle(radius) if radius > 100:
            return "huge circle"
        case Circle(_):
            return "circle"
        case Rectangle(w, h) if w == h:
            return "square"
        case Rectangle(_, _):
            return "rectangle"
        case Triangle(_, _):
            return "triangle"
```

Guards do not relax exhaustiveness — you still need a case for every variant that *isn't* covered by a guard.

### Positional vs. keyword patterns

Positional patterns must match the declared field count exactly:

```python
case Triangle(base, height): ...    # ✅ Triangle has 2 fields, 2 patterns
case Triangle(base, _, height): ... # ❌ tyc::arg_count
```

This is sometimes noisy: when a dataclass grows a new field, every `case Foo(a, b, c)` site has to add an underscore. Use **keyword patterns** instead — they bind only the fields you name, in any order:

```python
case TaskStarted(task_id=tid, worker=w):     # ✅ ignores `attempt` and `at`
    return f"task {tid} on {w}"
case TaskStarted(task_id=tid):               # ✅ binds just `task_id`
    return f"task {tid}"
```

Python's `match` supports the keyword form natively (no `__match_args__` workaround required), and Typhon parses and type-checks it directly. Prefer the keyword form for any variant with more than two or three fields — it survives field additions without churn at every match site.

## Variants with shared methods

`impl` blocks attach to individual variants:

```python
type Shape = Circle | Rectangle | Triangle

class Circle:
    radius: float

impl Circle:
    def area() -> float:
        return 3.14159 * radius * radius

class Rectangle:
    width: float
    height: float

impl Rectangle:
    def area() -> float:
        return width * height

class Triangle:
    base: float
    height: float

impl Triangle:
    def area() -> float:
        return 0.5 * base * height
```

…or you can keep methods on the union by writing a free function that pattern-matches (often clearer for behaviour that depends on multiple variants at once).

## Real-world example: a parser

Sealed unions shine when you have a small fixed alphabet of inputs or outputs:

```python
type Token = Number | Ident | Plus | Minus | LParen | RParen | EOF

class Number:
    value: float
class Ident:
    name: str
class Plus frozen: pass
class Minus frozen: pass
class LParen frozen: pass
class RParen frozen: pass
class EOF frozen: pass

def display(t: Token) -> str:
    match t:
        case Number(v):
            return f"<num {v}>"
        case Ident(n):
            return f"<id {n}>"
        case Plus():
            return "+"
        case Minus():
            return "-"
        case LParen():
            return "("
        case RParen():
            return ")"
        case EOF():
            return "<eof>"
```

> **Nullary variant idiom (R3-13):** the `class Foo frozen: pass` form is
> the recommended shape for nullary sealed-union variants — it emits as
> `@dataclass(slots=True, frozen=True)` so each instance is hashable,
> comparable, and immutable. Match these variants with `case Foo():` —
> two empty parens, no subpatterns — and Typhon's exhaustiveness check
> recognises the arm. **Do not write `case Foo(_):`** — the single
> underscore is a *positional capture* and the dataclass has no positional
> fields, so the arm never matches and the union's `Foo` variant looks
> uncovered.

Add a `Star` variant for multiplication and every `match` on `Token` fails to compile until you handle it. The compiler becomes your TODO list.

## Sealed unions vs inheritance

You can model "shape" with inheritance — `class Shape:` plus `class Circle(Shape): ...`. Two reasons to prefer a sealed union:

- **Exhaustive matching.** Subclassing has no mechanism for "every subtype". A sealed union does.
- **No hidden behaviour.** A subclass can override a method silently. A sealed union forces every behaviour into the `match`, where you can see what each variant does in one place.

When in doubt: if the set of variants is fixed and known at design time, reach for a sealed union; if you genuinely expect external extension, use a base class or an `interface` (next guide).

## How it desugars

Each variant is emitted as a dataclass; the union itself emits as a typing union:

```python
# Typhon
type Shape = Circle | Rectangle | Triangle

# Emitted Python
from dataclasses import dataclass

@dataclass(slots=True)
class Circle:
    radius: float

@dataclass(slots=True)
class Rectangle:
    width: float
    height: float

@dataclass(slots=True)
class Triangle:
    base: float
    height: float

Shape = Circle | Rectangle | Triangle
```

`match` lowers to a Python 3.10+ `match` statement essentially unchanged — Typhon's check is at compile time; runtime semantics are vanilla `match`.

## Putting it together

A small expression evaluator using sealed unions throughout — both for the AST and for evaluation errors:

```python
type Expr = Lit | Add | Sub | Mul | Div

class Lit:
    value: float
class Add:
    lhs: Expr
    rhs: Expr
class Sub:
    lhs: Expr
    rhs: Expr
class Mul:
    lhs: Expr
    rhs: Expr
class Div:
    lhs: Expr
    rhs: Expr

type EvalError = DivByZero | Overflow

class DivByZero: pass
class Overflow:
    value: float

def eval(e: Expr) -> Result[float, EvalError]:
    match e:
        case Lit(v):
            return Ok(v)
        case Add(l, r):
            let a: float = eval(l)?
            let b: float = eval(r)?
            return Ok(a + b)
        case Sub(l, r):
            let a: float = eval(l)?
            let b: float = eval(r)?
            return Ok(a - b)
        case Mul(l, r):
            let a: float = eval(l)?
            let b: float = eval(r)?
            return Ok(a * b)
        case Div(l, r):
            let a: float = eval(l)?
            let b: float = eval(r)?
            if b == 0.0:
                return Err(DivByZero())
            return Ok(a / b)
```

What this shows:

- `Expr` is sealed; every operator variant is listed. Adding `Mod` to the union breaks the match until handled.
- `EvalError` is also sealed; the `match` in a caller forces you to think about `DivByZero` vs `Overflow` distinctly.
- `?` composes happily with `match` — both work because the function returns `Result[float, EvalError]`.

## Common mistakes

**Forgetting `type` and just listing classes:**

```python
class Circle: ...
class Rectangle: ...

def area(s: Circle | Rectangle) -> float:    # ⚠️ works but doesn't seal
    ...
```

This makes `Circle | Rectangle` an *ad-hoc* union, not a sealed one. Exhaustiveness on `match` will still work for *this* union, but there's no named type that catches drift in other files.

Fix: `type Shape = Circle | Rectangle`, then use `Shape` everywhere.

**Adding a variant and not updating matches:**

```python
type Shape = Circle | Rectangle | Triangle | Square    # added Square
```

The checker now flags every `match s:` that didn't have `case Square(...):`. That's the feature — fix each one and move on.

**Mixing `case _:` and exhaustiveness:**

```python
def area(s: Shape) -> float:
    match s:
        case Circle(r):
            return 3.14159 * r * r
        case _:                  # catches every other variant
            return 0.0
```

This compiles, but you've opted out of exhaustiveness. New variants will silently fall into the wildcard. Only use `_:` when that's genuinely what you want.

## What you've learned

- `type X = A | B | C` declares a sealed union.
- `match` on a sealed union must cover every variant — exhaustiveness is checked at compile time.
- Variants are normal classes; methods live in `impl` blocks or free functions.
- Use sealed unions over inheritance whenever the variant set is fixed at design time.

Next: [Generics and interfaces](08-generics-and-interfaces.md) — type parameters and structural typing.
