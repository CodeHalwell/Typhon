"""PEP 695 / PEP 696 type-parameter objects.

`def f[T = int]()` and `class C[T: int]` bind real objects in CPython, and a
program can read them back off `__type_params__`. The VM erases the
parameters for *checking* — that is the compiler's job — but the objects
still have to exist, with the attributes and reprs CPython gives them.
"""


class _NoDefaultType:
    def __repr__(self):
        return "typing.NoDefault"


NoDefault = _NoDefaultType()


class TypeVar:
    def __init__(self, name, *constraints, bound=None, default=NoDefault,
                 covariant=False, contravariant=False, infer_variance=False):
        self.__name__ = name
        self.__bound__ = bound
        self.__constraints__ = tuple(constraints)
        self.__default__ = default
        self.__covariant__ = covariant
        self.__contravariant__ = contravariant
        self.__infer_variance__ = infer_variance

    def has_default(self):
        return self.__default__ is not NoDefault

    def __repr__(self):
        # A parameter whose variance is inferred (every PEP 695 one) prints
        # bare; an explicit variance prints its sign, and the rest a `~`.
        if self.__infer_variance__:
            return self.__name__
        if self.__covariant__:
            return "+" + self.__name__
        if self.__contravariant__:
            return "-" + self.__name__
        return "~" + self.__name__


class ParamSpec:
    def __init__(self, name, *, bound=None, default=NoDefault,
                 covariant=False, contravariant=False, infer_variance=False):
        self.__name__ = name
        self.__bound__ = bound
        self.__default__ = default
        self.__covariant__ = covariant
        self.__contravariant__ = contravariant
        self.__infer_variance__ = infer_variance

    def has_default(self):
        return self.__default__ is not NoDefault

    def __repr__(self):
        if self.__infer_variance__:
            return self.__name__
        if self.__covariant__:
            return "+" + self.__name__
        if self.__contravariant__:
            return "-" + self.__name__
        return "~" + self.__name__


class TypeVarTuple:
    def __init__(self, name, *, default=NoDefault):
        self.__name__ = name
        self.__default__ = default

    def has_default(self):
        return self.__default__ is not NoDefault

    def __repr__(self):
        return self.__name__
