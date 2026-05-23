# tyc::arg_count

Fires when a function, method, or class constructor is called with the
wrong number of arguments for its declared signature.

## Functions

```ty
def add(a: int, b: int) -> int:
    return a + b

def main() -> None:
    add(1, 2, 3)  # error: expected 2, got 3
```

## Class constructors

The auto-generated `__init__` of `class X:` (and `model X:`) requires every
field without an `= default` to be supplied — positionally or by keyword.

```ty
class ApiClient:
    api_key: str
    base_url: str

def main() -> None:
    let c: ApiClient = ApiClient(base_url="https://api.example.com")
    # error: wrong number of arguments to `ApiClient`: expected 2, got 1
```

Fix by passing every required field:

```ty
def main() -> None:
    let c: ApiClient = ApiClient(api_key="…", base_url="https://api.example.com")
```

A field of type `T?` is still required unless you write `= None` explicitly —
Typhon does not auto-inject the default, so the emitted `@dataclass` would
otherwise crash with `TypeError: missing 1 required positional argument` at
runtime.

```ty
class Foo:
    name: str?            # required at construction (no auto-default)
    label: str? = None    # optional
```

## Methods

`impl` methods are checked the same way as free functions. The implicit
`self` / `cls` receiver is excluded from the arity surface.

```ty
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

def main() -> None:
    let u: User = User(name="Ada")
    u.greet()          # error: wrong number of arguments to `greet`: expected 1, got 0
```

## Why

Typhon's checker computes the expected arity from the function / method's
parameters (or the class's field list) — including `*args`/`**kwargs` and
defaults where applicable — and rejects call sites whose argument shape
can't be matched. Catching the mismatch at check time avoids `TypeError`
at runtime.

## Cross-module imports

Both import shapes flow through the same arity check:

```ty
# src/clients.ty
class ApiClient:
    api_key: str
    base_url: str

# src/main.ty
from clients import ApiClient

def main() -> None:
    let c: ApiClient = ApiClient(base_url="x")
    # error: wrong number of arguments to `ApiClient`: expected 2, got 1
```

The bare-import dotted form is also checked; the diagnostic name is
module-qualified so two imports exporting the same class name remain
disambiguated:

```ty
import clients

def main() -> None:
    let c = clients.ApiClient(base_url="x")
    # error: wrong number of arguments to `clients.ApiClient`: expected 2, got 1
```

`.dty` stubs participate on equal footing with `.ty` source; stubs win
on name collisions.

## Inheritance

Subclass constructors accept inherited fields end-to-end —
`class Dog(Animal):` exposes `Dog(name="Rex", breed="Husky")` where
`name` is declared on `Animal`. The check walks the recorded base
chain via `effective_class_shape`, so a grandchild
`class C(B):` (with `B(A):`) sees fields from `A`, `B`, and `C` at
its construction site. A child override of a defaulted parent field
correctly re-marks the field as required.

## Limitations

Dotted-attribute *annotations* (`let c: f.Cls = …`) now resolve to
the foreign class shape end-to-end: a `Class("{module}.{attr}")` is
produced by `type_from_annotation`, which unifies with the call site's
`f.Cls(...)` inference. Downstream method-arity / kwarg checks on `c`
fire correctly. Only multi-segment paths (`a.b.Cls`) still fall back
to `Type::Unknown` because they need full module-registry resolution
that is out of scope for the annotation pass.

See https://typhon.dev/lang/diagnostics/arg_count
