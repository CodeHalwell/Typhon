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
