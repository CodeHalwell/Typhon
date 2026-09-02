# `bytearray` — CPython's mutable bytes, over a list of ints.
#
# The VM's `Value::Bytes` is immutable, so the mutable sibling is a class
# whose `__typhon_builtin_bases__` marks it as a `bytearray` for
# `isinstance`. Every read method funnels through `bytes(self)` so the
# existing (well-tested) `bytes` surface is the single implementation, and
# only the mutations are written here.


class bytearray:
    __typhon_builtin_bases__ = ("bytearray",)

    def __init__(self, source=b"", encoding=None, errors="strict"):
        if isinstance(source, str):
            if encoding is None:
                raise TypeError("string argument without an encoding")
            self._data = list(source.encode(encoding, errors))
        elif isinstance(source, int):
            if source < 0:
                raise ValueError("negative count")
            self._data = [0] * source
        elif isinstance(source, bytearray):
            self._data = list(source._data)
        else:
            self._data = list(source)
        for b in self._data:
            if not isinstance(b, int) or b < 0 or b > 255:
                raise ValueError("bytes must be in range(0, 256)")

    # ── conversion ──────────────────────────────────────────────────────
    def _as_bytes(self):
        return bytes(self._data)

    def __bytes__(self):
        return self._as_bytes()

    def __repr__(self):
        return "bytearray(%r)" % self._as_bytes()

    def __str__(self):
        return self.__repr__()

    # ── sequence protocol ───────────────────────────────────────────────
    def __len__(self):
        return len(self._data)

    def __iter__(self):
        return iter(self._data)

    def __contains__(self, item):
        if isinstance(item, int):
            return item in self._data
        return _coerce(item) in self._as_bytes()

    def __getitem__(self, index):
        part = self._data[index]
        if isinstance(index, slice):
            return bytearray(part)
        return part

    def __setitem__(self, index, value):
        if isinstance(index, slice):
            self._data[index] = _checked_bytes(value)
            return
        if not isinstance(value, int) or value < 0 or value > 255:
            raise ValueError("byte must be in range(0, 256)")
        self._data[index] = value

    def __delitem__(self, index):
        del self._data[index]

    # ── comparison ──────────────────────────────────────────────────────
    def __eq__(self, other):
        if isinstance(other, bytearray):
            return self._data == other._data
        if isinstance(other, bytes):
            return self._as_bytes() == other
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def __lt__(self, other):
        return self._as_bytes() < _coerce(other)

    def __le__(self, other):
        return self._as_bytes() <= _coerce(other)

    def __gt__(self, other):
        return self._as_bytes() > _coerce(other)

    def __ge__(self, other):
        return self._as_bytes() >= _coerce(other)

    # CPython: `bytearray` is unhashable, because it is mutable.
    def __hash__(self):
        raise TypeError("unhashable type: 'bytearray'")

    # ── arithmetic ──────────────────────────────────────────────────────
    def __add__(self, other):
        return bytearray(self._data + list(_iter_ints(other)))

    def __iadd__(self, other):
        self._data.extend(_iter_ints(other))
        return self

    def __mul__(self, n):
        return bytearray(self._data * n)

    def __rmul__(self, n):
        return bytearray(self._data * n)

    # ── mutation ────────────────────────────────────────────────────────
    def append(self, value):
        if not isinstance(value, int) or value < 0 or value > 255:
            raise ValueError("byte must be in range(0, 256)")
        self._data.append(value)

    def extend(self, iterable):
        self._data.extend(_checked_bytes(iterable))

    def insert(self, index, value):
        if not isinstance(value, int) or value < 0 or value > 255:
            raise ValueError("byte must be in range(0, 256)")
        self._data.insert(index, value)

    def pop(self, index=-1):
        if not self._data:
            raise IndexError("pop from empty bytearray")
        return self._data.pop(index)

    def remove(self, value):
        if value not in self._data:
            raise ValueError("value not found in bytearray")
        self._data.remove(value)

    def clear(self):
        self._data = []

    def reverse(self):
        self._data.reverse()

    def copy(self):
        return bytearray(self._data)

    # ── read methods, delegated to `bytes` ──────────────────────────────
    def decode(self, encoding="utf-8", errors="strict"):
        return self._as_bytes().decode(encoding, errors)

    def hex(self, *args):
        return self._as_bytes().hex(*args)

    def find(self, *args):
        return self._as_bytes().find(*[_coerce(a) for a in args])

    def rfind(self, *args):
        return self._as_bytes().rfind(*[_coerce(a) for a in args])

    def index(self, *args):
        return self._as_bytes().index(*[_coerce(a) for a in args])

    def count(self, *args):
        return self._as_bytes().count(*[_coerce(a) for a in args])

    def startswith(self, prefix, *rest):
        return self._as_bytes().startswith(_coerce(prefix), *rest)

    def endswith(self, suffix, *rest):
        return self._as_bytes().endswith(_coerce(suffix), *rest)

    def split(self, sep=None, maxsplit=-1):
        parts = self._as_bytes().split(sep if sep is None else _coerce(sep), maxsplit)
        return [bytearray(p) for p in parts]

    def rsplit(self, sep=None, maxsplit=-1):
        parts = self._as_bytes().rsplit(sep if sep is None else _coerce(sep), maxsplit)
        return [bytearray(p) for p in parts]

    def splitlines(self, keepends=False):
        return [bytearray(p) for p in self._as_bytes().splitlines(keepends)]

    def strip(self, chars=None):
        return bytearray(self._as_bytes().strip(chars if chars is None else _coerce(chars)))

    def lstrip(self, chars=None):
        return bytearray(self._as_bytes().lstrip(chars if chars is None else _coerce(chars)))

    def rstrip(self, chars=None):
        return bytearray(self._as_bytes().rstrip(chars if chars is None else _coerce(chars)))

    def upper(self):
        return bytearray(self._as_bytes().upper())

    def lower(self):
        return bytearray(self._as_bytes().lower())

    def replace(self, old, new, count=-1):
        return bytearray(self._as_bytes().replace(_coerce(old), _coerce(new), count))

    def join(self, parts):
        return bytearray(self._as_bytes().join([_coerce(p) for p in parts]))

    @classmethod
    def fromhex(cls, text):
        return cls(bytes.fromhex(text))


def _coerce(value):
    """A `bytes` view of anything the bytes methods accept."""
    if isinstance(value, bytearray):
        return value._as_bytes()
    return value


def _iter_ints(value):
    if isinstance(value, bytearray):
        return list(value._data)
    if isinstance(value, int):
        raise TypeError("can't concat int to bytearray")
    return list(value)


def _checked_bytes(value):
    """`_iter_ints`, with the 0-255 invariant every mutator has to keep."""
    out = _iter_ints(value)
    for b in out:
        if not isinstance(b, int) or b < 0 or b > 255:
            raise ValueError("byte must be in range(0, 256)")
    return out
