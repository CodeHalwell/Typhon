# `lazy let NAME = expr` — the proxy that defers `expr` to first use.
#
# The emitted runtime (`typhon_runtime/lazy.py`) has the same shape; this is
# its VM counterpart. Calling the factory eagerly, as the VM used to, ran the
# initialiser at *module import* order rather than at first use — so a
# module-level `lazy let X = helper()` failed with `NameError` when `helper`
# was defined further down the file, on a program `tyc build` runs fine.


class _Unset:
    pass


_UNSET = _Unset()


class _LazyValue:
    def __init__(self, factory):
        self._factory = factory
        self._value = _UNSET

    def _materialise(self):
        if isinstance(self._value, _Unset):
            self._value = self._factory()
        return self._value

    def __getattr__(self, name):
        return getattr(self._materialise(), name)

    def __call__(self, *args, **kwargs):
        return self._materialise()(*args, **kwargs)

    def __getitem__(self, key):
        return self._materialise()[key]

    def __setitem__(self, key, value):
        self._materialise()[key] = value

    def __contains__(self, item):
        return item in self._materialise()

    def __iter__(self):
        return iter(self._materialise())

    def __len__(self):
        return len(self._materialise())

    def __bool__(self):
        return bool(self._materialise())

    def __repr__(self):
        if isinstance(self._value, _Unset):
            return "<lazy: unmaterialised>"
        return repr(self._value)

    def __str__(self):
        return str(self._materialise())

    def __format__(self, spec):
        return format(self._materialise(), spec)

    def __hash__(self):
        return hash(self._materialise())

    def __eq__(self, other):
        return self._materialise() == other

    def __ne__(self, other):
        return self._materialise() != other

    def __lt__(self, other):
        return self._materialise() < other

    def __le__(self, other):
        return self._materialise() <= other

    def __gt__(self, other):
        return self._materialise() > other

    def __ge__(self, other):
        return self._materialise() >= other

    def __add__(self, other):
        return self._materialise() + other

    def __radd__(self, other):
        return other + self._materialise()

    def __sub__(self, other):
        return self._materialise() - other

    def __rsub__(self, other):
        return other - self._materialise()

    def __mul__(self, other):
        return self._materialise() * other

    def __rmul__(self, other):
        return other * self._materialise()

    def __truediv__(self, other):
        return self._materialise() / other

    def __rtruediv__(self, other):
        return other / self._materialise()

    def __floordiv__(self, other):
        return self._materialise() // other

    def __rfloordiv__(self, other):
        return other // self._materialise()

    def __mod__(self, other):
        return self._materialise() % other

    def __rmod__(self, other):
        return other % self._materialise()

    def __pow__(self, other):
        return self._materialise() ** other

    def __rpow__(self, other):
        return other ** self._materialise()

    def __neg__(self):
        return -self._materialise()

    def __pos__(self):
        return +self._materialise()

    def __abs__(self):
        return abs(self._materialise())

    def __invert__(self):
        return ~self._materialise()

    def __int__(self):
        return int(self._materialise())

    def __float__(self):
        return float(self._materialise())

    def __index__(self):
        return self._materialise().__index__()

    def __and__(self, other):
        return self._materialise() & other

    def __or__(self, other):
        return self._materialise() | other

    def __xor__(self, other):
        return self._materialise() ^ other

    def __lshift__(self, other):
        return self._materialise() << other

    def __rshift__(self, other):
        return self._materialise() >> other
