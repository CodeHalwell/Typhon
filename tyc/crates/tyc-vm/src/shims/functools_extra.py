# `functools` helpers that CPython also writes in Python: the comparison
# adapters and the single-dispatch generic function.


def cmp_to_key(mycmp):
    class K:
        __slots__ = ["obj"]

        def __init__(self, obj):
            self.obj = obj

        def __lt__(self, other):
            return mycmp(self.obj, other.obj) < 0

        def __gt__(self, other):
            return mycmp(self.obj, other.obj) > 0

        def __eq__(self, other):
            return mycmp(self.obj, other.obj) == 0

        def __le__(self, other):
            return mycmp(self.obj, other.obj) <= 0

        def __ge__(self, other):
            return mycmp(self.obj, other.obj) >= 0

        def __ne__(self, other):
            return mycmp(self.obj, other.obj) != 0

        def __hash__(self):
            raise TypeError("unhashable type: 'K'")

    return K


def total_ordering(cls):
    """Fill in the ordering methods the class did not define.

    CPython derives the missing three from whichever of `__lt__` / `__le__` /
    `__gt__` / `__ge__` the class provides; the rules below are the same
    table, written against the one root method found.
    """
    roots = [op for op in ["__lt__", "__le__", "__gt__", "__ge__"] if op in cls.__dict__]
    if not roots:
        raise ValueError("must define at least one ordering operation: < > <= >=")
    root = roots[0]
    op = getattr(cls, root)
    if root == "__lt__":
        def __gt__(self, other):
            return not (op(self, other) or self == other)

        def __le__(self, other):
            return op(self, other) or self == other

        def __ge__(self, other):
            return not op(self, other)
        fills = {"__gt__": __gt__, "__le__": __le__, "__ge__": __ge__}
    elif root == "__le__":
        def __ge__(self, other):
            return not op(self, other) or self == other

        def __lt__(self, other):
            return op(self, other) and self != other

        def __gt__(self, other):
            return not op(self, other)
        fills = {"__ge__": __ge__, "__lt__": __lt__, "__gt__": __gt__}
    elif root == "__gt__":
        def __lt__(self, other):
            return not (op(self, other) or self == other)

        def __ge__(self, other):
            return op(self, other) or self == other

        def __le__(self, other):
            return not op(self, other)
        fills = {"__lt__": __lt__, "__ge__": __ge__, "__le__": __le__}
    else:
        def __le__(self, other):
            return not op(self, other) or self == other

        def __gt__(self, other):
            return op(self, other) and self != other

        def __lt__(self, other):
            return not op(self, other)
        fills = {"__le__": __le__, "__gt__": __gt__, "__lt__": __lt__}
    for name in fills:
        if name not in cls.__dict__:
            setattr(cls, name, fills[name])
    return cls


def singledispatch(func):
    """Generic function dispatching on the first argument's type."""
    registry = {}

    def dispatch(cls):
        # Walk the MRO so a registered base handles its subclasses.
        for klass in getattr(cls, "__mro__", [cls]):
            if klass in registry:
                return registry[klass]
        return func

    def register(cls, method=None):
        if method is None:
            def decorator(fn):
                registry[cls] = fn
                return fn
            return decorator
        registry[cls] = method
        return method

    def wrapper(*args, **kwargs):
        if not args:
            raise TypeError("%s requires at least 1 positional argument" % getattr(func, "__name__", "function"))
        return dispatch(type(args[0]))(*args, **kwargs)

    wrapper.register = register
    wrapper.dispatch = dispatch
    wrapper.registry = registry
    wrapper.__wrapped__ = func
    return wrapper


# ── update_wrapper / partialmethod / singledispatchmethod ───────────────────

WRAPPER_ASSIGNMENTS = ('__module__', '__name__', '__qualname__', '__doc__',
                       '__annotations__', '__type_params__')
WRAPPER_UPDATES = ('__dict__',)


def update_wrapper(wrapper, wrapped, assigned=WRAPPER_ASSIGNMENTS, updated=WRAPPER_UPDATES):
    """Copy `wrapped`'s identity onto `wrapper` and record `__wrapped__`."""
    for attr in assigned:
        try:
            value = getattr(wrapped, attr)
        except AttributeError:
            pass
        else:
            setattr(wrapper, attr, value)
    for attr in updated:
        try:
            getattr(wrapper, attr).update(getattr(wrapped, attr, {}))
        except AttributeError:
            pass
    wrapper.__wrapped__ = wrapped
    return wrapper


class partialmethod:
    """`partial` as a method descriptor: the receiver is bound first, then
    the captured positional arguments, then the call's own."""

    def __init__(self, func, /, *args, **keywords):
        if not callable(func) and not hasattr(func, "__get__"):
            raise TypeError("{!r} is not callable or a descriptor".format(func))
        if isinstance(func, partialmethod):
            self.func = func.func
            self.args = func.args + args
            self.keywords = {**func.keywords, **keywords}
        else:
            self.func = func
            self.args = args
            self.keywords = keywords

    def __repr__(self):
        cls = type(self)
        parts = [repr(self.func)]
        parts.extend(repr(a) for a in self.args)
        parts.extend("{}={!r}".format(k, v) for k, v in self.keywords.items())
        return "{}.{}({})".format(cls.__module__, cls.__qualname__, ", ".join(parts))

    def _make_unbound_method(self):
        def _method(cls_or_self, /, *args, **keywords):
            keywords = {**self.keywords, **keywords}
            return self.func(cls_or_self, *self.args, *args, **keywords)
        _method.__isabstractmethod__ = self.__isabstractmethod__
        _method.__partialmethod__ = self
        return _method

    def __get__(self, obj, cls=None):
        if obj is None:
            return self._make_unbound_method()
        # `partial` is a native of the assembled module, not a name of this
        # shim's own namespace, so reach it through the module.
        from functools import partial
        return partial(self._make_unbound_method(), obj)

    @property
    def __isabstractmethod__(self):
        return getattr(self.func, "__isabstractmethod__", False)


class singledispatchmethod:
    """`singledispatch` on a method: dispatch on the first argument *after*
    the receiver."""

    def __init__(self, func):
        if not callable(func) and not hasattr(func, "__get__"):
            raise TypeError("{!r} is not callable or a descriptor".format(func))
        self.dispatcher = singledispatch(func)
        self.func = func

    def register(self, cls, method=None):
        return self.dispatcher.register(cls, method)

    def __get__(self, obj, cls=None):
        dispatcher = self.dispatcher

        def _method(*args, **kwargs):
            method = dispatcher.dispatch(type(args[0]))
            if obj is None:
                return method(*args, **kwargs)
            return method(obj, *args, **kwargs)

        _method.__isabstractmethod__ = self.__isabstractmethod__
        _method.register = self.register
        update_wrapper(_method, self.func)
        return _method

    @property
    def __isabstractmethod__(self):
        return getattr(self.func, "__isabstractmethod__", False)
