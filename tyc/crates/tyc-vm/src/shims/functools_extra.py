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
