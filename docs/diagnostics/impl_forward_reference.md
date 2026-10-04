# tyc::impl_forward_reference

Fires when a method of an `impl` block stays inside its class body although
one of its decorators or parameter defaults reads a module-level name that is
bound only *after* the class.

## Example

```ty
class Greeter:
    name: str

let DEFAULT_GREETING: str = "hello"

impl Greeter:
    def __call__(self, word: str = DEFAULT_GREETING) -> str:   # error
        return f"{word}, {self.name}"
```

## Why

An `impl` method is compiled into its class: `class Greeter` gets the method
in its body. A method's decorators and parameter defaults are evaluated when
the `def` runs — that is, when the `class` statement runs, above the `impl`
block. `DEFAULT_GREETING` does not exist yet at that point, so importing the
module raises `NameError` on both `tyc run` and CPython.

Most such methods are instead defined where the `impl` block is and attached
to the class there (`Greeter.greet = …`), which evaluates the default after
`DEFAULT_GREETING` is bound. That is not possible when the class body itself
gives the method its meaning, and the diagnostic names which case applies:

- a special method (`__call__`, `__eq__`, `__post_init__`, …): the class and
  its dataclass machinery need it when the class is created;
- a `@property`, `@classmethod`, `@cached_property` or `@abstractmethod`;
- a method that uses a private `__name`, which Python mangles only inside a
  class body;
- a method the class may inherit from a base class, or a class with a
  subclass defined before the `impl` block (or one that overrides the
  method).

It does not fire when the name might be bound before the class after all: a
`from … import *` above the class, a `global NAME` declaration in any
function, or a module that writes its namespace through `globals()`, `exec`,
`setattr`, `sys.modules` or `builtins`.

## How to fix

Bind the name above the class:

```ty
let DEFAULT_GREETING: str = "hello"

class Greeter:
    name: str

impl Greeter:
    def __call__(self, word: str = DEFAULT_GREETING) -> str:
        return f"{word}, {self.name}"
```

or default the parameter to something that exists when the class is created
and resolve the late name inside the body, which runs at call time:

```ty
impl Greeter:
    def __call__(self, word: str? = None) -> str:
        return f"{word or DEFAULT_GREETING}, {self.name}"
```
