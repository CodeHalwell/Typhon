# `property(fget, fset, fdel, doc)` called as a function — the decorator
# form `@property` is handled natively by the class builder. A data
# descriptor: the VM's attribute protocol calls `__get__` on read through an
# instance (and with `obj=None` through the class), and `__set__` /
# `__delete__` on assignment / deletion through an instance.


class property:
    def __init__(self, fget=None, fset=None, fdel=None, doc=None):
        self.fget = fget
        self.fset = fset
        self.fdel = fdel
        if doc is None and fget is not None:
            doc = getattr(fget, "__doc__", None)
        self.__doc__ = doc
        self.__name__ = getattr(fget, "__name__", None) if fget is not None else None

    def __set_name__(self, owner, name):
        self.__name__ = name

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        if self.fget is None:
            raise AttributeError(
                f"property {self.__name__!r} of {type(obj).__name__!r} object has no getter"
            )
        return self.fget(obj)

    def __set__(self, obj, value):
        if self.fset is None:
            raise AttributeError(
                f"property {self.__name__!r} of {type(obj).__name__!r} object has no setter"
            )
        self.fset(obj, value)

    def __delete__(self, obj):
        if self.fdel is None:
            raise AttributeError(
                f"property {self.__name__!r} of {type(obj).__name__!r} object has no deleter"
            )
        self.fdel(obj)

    def getter(self, fget):
        return type(self)(fget, self.fset, self.fdel, self.__doc__)

    def setter(self, fset):
        return type(self)(self.fget, fset, self.fdel, self.__doc__)

    def deleter(self, fdel):
        return type(self)(self.fget, self.fset, fdel, self.__doc__)


class NewType:
    def __init__(self, name, tp):
        self.__name__ = name
        self.__qualname__ = name
        self.__supertype__ = tp
        self.__module__ = "__main__"

    def __call__(self, x):
        return x

    def __repr__(self):
        return self.__module__ + "." + self.__qualname__


def _type_repr(t):
    if t is None:
        return "None"
    r = repr(t)
    if r == "<class 'NoneType'>":
        return "None"
    if r == "Ellipsis":
        return "..."
    if r.startswith("<class '") and r.endswith("'>"):
        return r[8:-2]
    return r


class UnionType:
    def __init__(self, args, typing_form=False):
        self.__args__ = args
        self._typing_form = typing_form

    def __repr__(self):
        if not self._typing_form:
            return " | ".join([_type_repr(a) for a in self.__args__])
        # A union with a `typing` generic in it is `typing.Union`.
        if len(self.__args__) == 2 and None in self.__args__:
            other = [a for a in self.__args__ if a is not None][0]
            return "typing.Optional[" + _type_repr(other) + "]"
        parts = ["NoneType" if a is None else _type_repr(a) for a in self.__args__]
        return "typing.Union[" + ", ".join(parts) + "]"

    def __or__(self, other):
        return _union(self, other)

    def __ror__(self, other):
        return _union(other, self)

    def __eq__(self, other):
        if not isinstance(other, UnionType):
            return NotImplemented
        return set(self.__args__) == set(other.__args__)

    def __hash__(self):
        return hash(frozenset(self.__args__))


def _union(a, b):
    # `X | typing.Optional[int]`: a `typing` form makes it `typing.Union`.
    if isinstance(a, _TypingAlias) or isinstance(b, _TypingAlias):
        return _TypingAlias._union_of((a, b))
    args = []
    typing_form = False
    for t in (a, b):
        parts = t.__args__ if isinstance(t, UnionType) else (t,)
        if isinstance(t, UnionType) and t._typing_form:
            typing_form = True
        for p in parts:
            # A user generic class's alias is `typing`'s (the builtin
            # `list[int]` is a `types.GenericAlias`, which `|` keeps plain).
            if isinstance(p, _GenericAlias) and type(p.__origin__).__name__ == "type":
                typing_form = True
            if p not in args:
                args.append(p)
    if len(args) == 1:
        return args[0]
    return UnionType(tuple(args), typing_form)


class _GenericAlias:
    def __init__(self, origin, args):
        self.__origin__ = origin
        self.__args__ = args

    def __call__(self, *args, **kwargs):
        return self.__origin__(*args, **kwargs)

    def __repr__(self):
        return _type_repr(self.__origin__) + "[" + ", ".join([_type_repr(a) for a in self.__args__]) + "]"

    def __eq__(self, other):
        if not isinstance(other, _GenericAlias):
            return NotImplemented
        return self.__origin__ is other.__origin__ and self.__args__ == other.__args__

    def __hash__(self):
        return hash((self.__origin__, self.__args__))

    def __or__(self, other):
        return _union(self, other)

    def __ror__(self, other):
        return _union(other, self)


# `typing.List[int]`, `typing.Optional[str]`, `typing.Callable[[int], str]`:
# a subscripted `typing` form. The VM's bare forms are inert natives the
# interpreter recognises by name; subscripting one builds this alias, which
# prints, compares and answers `get_origin` / `get_args` as CPython's
# `typing._GenericAlias` does. `_form` is the bare form, `__origin__` what
# CPython reports as the origin (`list` for `List`, the form itself for
# `Union` / `Literal`), and `_union_form` (`typing.Union`) is set on the
# class when the `typing` module is built.
def _literal_keys(args):
    # CPython compares `Literal` aliases by unordered (value, type) pairs.
    return frozenset([(a, type(a)) for a in args])


class _TypingAlias:
    def __init__(self, name, form, origin, args, callable_params=None):
        self._name = name
        self._form = form
        self.__origin__ = origin
        self.__args__ = args
        self._callable_params = callable_params
        self.__metadata__ = ()

    @classmethod
    def _union_of(cls, params):
        flat = []
        for p in params:
            if isinstance(p, _TypingAlias) and p._name == "Union":
                items = p.__args__
            elif isinstance(p, UnionType):
                items = p.__args__
            else:
                items = (p,)
            for it in items:
                if it is None:
                    it = type(None)
                if it not in flat:
                    flat.append(it)
        if len(flat) == 1:
            return flat[0]
        form = cls._union_form[0]
        return cls("Union", form, form, tuple(flat))

    def __repr__(self):
        name = "typing." + self._name
        if self._name == "Union":
            nonetype = type(None)
            if len(self.__args__) == 2 and nonetype in self.__args__:
                other = [a for a in self.__args__ if a is not nonetype][0]
                return "typing.Optional[" + _type_repr(other) + "]"
            parts = ["NoneType" if a is nonetype else _type_repr(a) for a in self.__args__]
            return name + "[" + ", ".join(parts) + "]"
        if self._name == "Callable" and self._callable_params is not None:
            params = "[" + ", ".join([_type_repr(a) for a in self._callable_params]) + "]"
            return name + "[" + params + ", " + _type_repr(self.__args__[-1]) + "]"
        if self._name == "Annotated":
            parts = [_type_repr(self.__origin__)] + [repr(m) for m in self.__metadata__]
            return name + "[" + ", ".join(parts) + "]"
        if not self.__args__:
            return name + "[()]"
        return name + "[" + ", ".join([_type_repr(a) for a in self.__args__]) + "]"

    def __eq__(self, other):
        if not isinstance(other, _TypingAlias):
            return NotImplemented
        if self._name == "Union" and other._name == "Union":
            return set(self.__args__) == set(other.__args__)
        if self._name == "Literal" and other._name == "Literal":
            return _literal_keys(self.__args__) == _literal_keys(other.__args__)
        return (self._name == other._name and self.__origin__ == other.__origin__
                and self.__args__ == other.__args__ and self.__metadata__ == other.__metadata__)

    def __hash__(self):
        if self._name == "Union":
            return hash(frozenset(self.__args__))
        if self._name == "Literal":
            return hash(_literal_keys(self.__args__))
        return hash((self._name, self.__args__))

    def __or__(self, other):
        return _TypingAlias._union_of((self, other))

    def __ror__(self, other):
        return _TypingAlias._union_of((other, self))

    def __call__(self, *args, **kwargs):
        if self._name in ("Union", "Literal"):
            raise TypeError("Cannot instantiate typing." + self._name)
        if self._name in ("List", "Dict", "Set", "FrozenSet", "Tuple", "Type"):
            raise TypeError(
                "Type " + self._name + " cannot be instantiated; use " + self._name.lower() + "() instead"
            )
        return self.__origin__(*args, **kwargs)


def _typing_subscript(name, form, origin, params):
    if not isinstance(params, tuple):
        params = (params,)
    if name == "Optional":
        if len(params) != 1:
            raise TypeError("typing.Optional requires a single type.")
        return _TypingAlias._union_of((params[0], None))
    if name == "Union":
        if not params:
            raise TypeError("Cannot take a Union of no types.")
        return _TypingAlias._union_of(params)
    if name == "Annotated":
        if len(params) < 2:
            raise TypeError("Annotated[...] should be used with at least two arguments (a type and an annotation).")
        origin, metadata = params[0], tuple(params[1:])
        # `Annotated[Annotated[int, "a"], "b"]` flattens to one alias.
        if isinstance(origin, _TypingAlias) and origin._name == "Annotated":
            metadata = origin.__metadata__ + metadata
            origin = origin.__origin__
        alias = _TypingAlias(name, form, origin, (origin,))
        alias.__metadata__ = metadata
        return alias
    if name == "Callable" and len(params) == 2 and isinstance(params[0], list):
        args = tuple(params[0]) + (params[1],)
        return _TypingAlias(name, form, origin, args, list(params[0]))
    if name == "Literal":
        # Deduplicate by (value, type), so `Literal[0, False]` keeps both.
        args = []
        seen = []
        for p in params:
            key = (p, type(p))
            if key not in seen:
                seen.append(key)
                args.append(p)
        return _TypingAlias(name, form, origin, tuple(args))
    if name == "Tuple" and params == ((),):
        return _TypingAlias(name, form, origin, ())
    args = tuple(type(None) if p is None else p for p in params)
    return _TypingAlias(name, form, origin, args)


def _typing_get_origin(tp):
    if isinstance(tp, _TypingAlias):
        if tp._name in ("Annotated", "Union"):
            return tp._form
        return tp.__origin__
    if isinstance(tp, _GenericAlias):
        return tp.__origin__
    if isinstance(tp, UnionType):
        return UnionType
    return None


def _typing_get_args(tp):
    if isinstance(tp, _TypingAlias):
        if tp._name == "Annotated":
            return (tp.__origin__,) + tp.__metadata__
        if tp._name == "Callable" and tp._callable_params is not None:
            return (list(tp._callable_params), tp.__args__[-1])
        return tp.__args__
    if isinstance(tp, _GenericAlias):
        if tp.__args__ == ((),):
            return ()
        return tp.__args__
    if isinstance(tp, UnionType):
        return tuple(type(None) if a is None else a for a in tp.__args__)
    return ()
