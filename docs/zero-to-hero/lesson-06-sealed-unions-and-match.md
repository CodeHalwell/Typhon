# Lesson 6 — Sealed unions and `match`

*Zero to Hero · Lesson 6 of 10*

This lesson covers Rule 6: a `match` over a closed set of variants must be exhaustive. Sealed unions are how you model "one of these N shapes" — and they're often a better fit than inheritance.

## Declaring a sealed union

A `type` alias over `|`-separated classes declares the union; each variant is an ordinary class. From `examples/08-sealed-unions-match/sealed_unions.ty`:

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

## Exhaustive `match`

You consume a union by matching its variants. The match is **statically checked exhaustive** — no `case _:` needed:

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

Here's the payoff. Add a `Pentagon` variant to the alias:

```python
type Shape = Circle | Rectangle | Triangle | Pentagon
```

…and *every* `match` over `Shape` immediately fails with `tyc::non_exhaustive_match` until you handle the new case. "I forgot a case" becomes a compile error instead of a 2am page. This is the single biggest reason to prefer sealed unions over class hierarchies for closed sets.

## Patterns

You can destructure positionally (`case Circle(radius)`), by keyword (`case Login(user_id=uid)`), and combine variants with `|`. For a variant with no fields, declare it `class Nil frozen: pass` and match `case Nil():` (two empty parens — `case Nil(_)` is a positional capture that never matches a fieldless class).

`match` also works on enums (Lesson 4) and built-in class patterns (`case str() as s:`).

## Distributing methods over a union

`impl` on the union alias attaches a method to *every* variant at once:

```python
impl Shape:
    def is_round(self) -> bool:
        match self:
            case Circle(_):       return True
            case Rectangle(_, _): return False
            case Triangle(_, _):  return False
```

## Sealed unions vs inheritance

| | Sealed union | Inheritance |
|---|---|---|
| Variant set | **Closed** — fixed, known at compile time | Open — anyone can subclass |
| Exhaustiveness | Checked by the compiler | Not possible |
| Best for | A fixed set of cases you switch on | Open-ended extension points |

Reach for a union when the set of cases is closed and you `match` on it; reach for inheritance when you want others to extend your type.

## Common mistakes

**A missing case:**

```python
def area(s: Shape) -> float:
    match s:
        case Circle(radius):  return 3.14159 * radius * radius
        case Rectangle(w, h): return w * h
        # ❌ tyc::non_exhaustive_match — Triangle uncovered
```

Fix: add the missing `case` (or, only if you truly mean it, `case _:`).

**Fieldless variant matched with `_`:**

```python
case Nil(_):     # never matches — Nil has no positional fields
case Nil():      # ✅
```

## Try it

1. Model a calculator command: `type Cmd = Push | Pop | Add`, with `class Push: value: float` and fieldless `Pop`/`Add`.
2. Write `def step(stack: list[float], cmd: Cmd) -> list[float]:` that `match`es each command.
3. Add a `Mul` variant to `Cmd` and watch `tyc check` point you at every match that needs updating.

## What you learned

- A `type Name = A | B | C` alias declares a closed sealed union.
- `match` over a union is exhaustive-checked — adding a variant flags every site.
- `impl Union:` distributes a method to every variant; prefer unions over inheritance for closed sets.

**Next:** [Lesson 7 — Generics and interfaces](lesson-07-generics-and-interfaces.md).
