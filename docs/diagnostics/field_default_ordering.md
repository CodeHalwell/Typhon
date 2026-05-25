# tyc::field_default_ordering

Fires when a `class` field without a default value is declared after a field
that has one. The synthesised `__init__` follows declaration order, and
Python's signature rules forbid a non-default positional parameter after a
default one — left unchecked, the class definition would blow up at *import*
time with a `TypeError: non-default argument 'X' follows default argument
'Y'`.

The parser already rejects this shape on free function parameters; this
diagnostic is the class-field twin, so the rule is consistent across both
surface forms (R3-11).

## Example

```ty
class Worker:
    name: str
    retries: int = 3
    queue_size: int       # ❌ tyc::field_default_ordering
```

## Fix

Move every field without a default above every field with one:

```ty
class Worker:
    name: str
    queue_size: int
    retries: int = 3
```

If a field genuinely has no sensible compile-time default but needs to follow
a defaulted field, lift it into a `keyword-only` initialiser instead — or
construct via a factory function:

```ty
def worker(name: str, queue_size: int, retries: int = 3) -> Worker:
    return Worker(name=name, retries=retries, queue_size=queue_size)
```

## Why

`@dataclass(slots=True)` (the default emit for `class`) generates
`__init__(self, name, retries=3, queue_size)` for the above declaration
order. Python's grammar then rejects the synthesised function:

```
TypeError: non-default argument 'queue_size' follows default argument 'retries'
```

Catching this at check time saves a confusing import-time failure that points
at the generated `.py` rather than the `.ty` source.

## Scope

- `class`: checked (emits `@dataclass(slots=True)`).
- `class frozen`: checked (emits `@dataclass(slots=True, frozen=True)`).
- `class!` (raw): checked (synthesises `__init__` calling `super().__init__()`).
- `plain class`: checked (the user is expected to provide an `__init__` and
  the declaration order signals their intent).
- `model` / Pydantic: skipped — Pydantic models have separate field-ordering
  semantics.
- `interface` / Protocol: skipped — interfaces never construct instances.
