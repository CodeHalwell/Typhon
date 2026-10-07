# `frozendict` (PEP 814) and `sentinel` (PEP 661) — builtins new in
# Python 3.15.
#
# The checker only accepts them on a `[python] target` of 3.15 or later, so
# under `tyc run` they behave as CPython 3.15's. `frozendict` keeps a plain
# dict and exposes the read-only mapping surface over it;
# `__typhon_builtin_bases__` makes it a `collections.abc.Mapping` for
# `isinstance`.


class frozendict:
    __typhon_builtin_bases__ = ("Mapping",)
    # Lets `freeze let` recognise the shim (and only it) as a frozendict.
    __typhon_frozendict__ = True

    def __init__(self, *args, **kwargs):
        if len(args) > 1:
            raise TypeError(f"frozendict expected at most 1 argument, got {len(args)}")
        data = {}
        if args:
            source = args[0]
            if isinstance(source, frozendict):
                data.update(source._data)
            else:
                data.update(source)
        data.update(kwargs)
        self._data = data

    @classmethod
    def fromkeys(cls, iterable, value=None):
        return cls(dict.fromkeys(iterable, value))

    # ── the read-only mapping surface ───────────────────────────────────

    def __getitem__(self, key):
        return self._data[key]

    def __contains__(self, key):
        return key in self._data

    def __iter__(self):
        return iter(self._data)

    def __reversed__(self):
        return reversed(list(self._data))

    def __len__(self):
        return len(self._data)

    def get(self, key, default=None):
        return self._data.get(key, default)

    def keys(self):
        return self._data.keys()

    def values(self):
        return self._data.values()

    def items(self):
        return self._data.items()

    def copy(self):
        return self

    # ── comparison, hashing, rendering ──────────────────────────────────

    def __eq__(self, other):
        if isinstance(other, frozendict):
            return self._data == other._data
        if isinstance(other, dict):
            return self._data == other
        return NotImplemented

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __hash__(self):
        return hash(frozenset(self._data.items()))

    def __repr__(self):
        if not self._data:
            return "frozendict()"
        return f"frozendict({self._data!r})"

    def __str__(self):
        return self.__repr__()

    # ── merging: `fd | m` stays a frozendict, `d | fd` is a dict ────────

    def __or__(self, other):
        if isinstance(other, frozendict):
            other = other._data
        if not isinstance(other, dict):
            return NotImplemented
        merged = dict(self._data)
        merged.update(other)
        return frozendict(merged)

    def __ror__(self, other):
        if not isinstance(other, dict):
            return NotImplemented
        merged = dict(other)
        merged.update(self._data)
        return merged

    # ── immutability ────────────────────────────────────────────────────

    def __setitem__(self, key, value):
        raise TypeError("'frozendict' object does not support item assignment")

    def __delitem__(self, key):
        raise TypeError("'frozendict' object does not support item deletion")


class sentinel:
    def __init__(self, name):
        if not isinstance(name, str):
            raise TypeError("sentinel name must be a string")
        self.__name__ = name

    def __repr__(self):
        return self.__name__

    def __copy__(self):
        return self

    def __deepcopy__(self, memo):
        return self
