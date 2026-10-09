# VM `collections`: Counter / deque / OrderedDict / ChainMap / namedtuple in
# the dialect the tree-walking VM interprets. Each mapping type keeps its
# entries in a plain dict (`_data`) and speaks the mapping protocol through
# dunders, so `c[k]`, `k in c`, `for k in c`, `len(c)`, `dict(c)` and the
# `most_common` / `elements` / `rotate` / `move_to_end` APIs all behave as in
# CPython. Validated against the real module (see validate_collections.py).


class _MappingBase:
    # `copy.copy` hook: CPython rebuilds these through `__reduce__`, which
    # hands the copy its own storage rather than sharing `_data`.
    def _typhon_copy(self):
        return type(self)(self._data)

    def __getitem__(self, key):
        if key in self._data:
            return self._data[key]
        return self.__missing__(key)

    def __missing__(self, key):
        raise KeyError(key)

    def __setitem__(self, key, value):
        self._data[key] = value

    def __delitem__(self, key):
        if key not in self._data:
            raise KeyError(key)
        del self._data[key]

    def __contains__(self, key):
        return key in self._data

    def __len__(self):
        return len(self._data)

    def __iter__(self):
        return iter(list(self._data.keys()))

    def __bool__(self):
        return len(self._data) > 0

    def keys(self):
        return self._data.keys()

    def values(self):
        return self._data.values()

    def items(self):
        return self._data.items()

    def get(self, key, default=None):
        if key in self._data:
            return self._data[key]
        return default

    def pop(self, key, *default):
        if key in self._data:
            v = self._data[key]
            del self._data[key]
            return v
        if default:
            return default[0]
        raise KeyError(key)

    def popitem(self):
        if not self._data:
            raise KeyError("popitem(): dictionary is empty")
        k, v = self._data.popitem()
        return (k, v)

    def setdefault(self, key, default=None):
        if key not in self._data:
            self._data[key] = default
        return self._data[key]

    def clear(self):
        self._data.clear()

    def update(self, *args, **kwargs):
        for arg in args:
            if hasattr(arg, "keys"):
                for k in list(arg.keys()):
                    self._data[k] = arg[k]
            else:
                for k, v in arg:
                    self._data[k] = v
        for k in kwargs:
            self._data[k] = kwargs[k]

    def __eq__(self, other):
        if isinstance(other, _MappingBase):
            return self._data == other._data
        if isinstance(other, dict):
            return self._data == other
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        raise TypeError("unhashable type: '%s'" % type(self).__name__)

    def copy(self):
        return type(self)(self._data)

    def __or__(self, other):
        if isinstance(other, _MappingBase):
            other = other._data
        if not isinstance(other, dict):
            raise TypeError("unsupported operand type(s) for |")
        new = type(self)(self._data)
        new.update(other)
        return new

    def __ior__(self, other):
        self.update(other)
        return self


class Counter(_MappingBase):
    __typhon_builtin_bases__ = ("dict",)
    def __init__(self, iterable=None, **kwargs):
        self._data = {}
        self._count(iterable, kwargs)

    def _count(self, iterable, kwargs):
        if iterable is not None:
            if isinstance(iterable, _MappingBase):
                for k in list(iterable._data.keys()):
                    self._data[k] = self._data.get(k, 0) + iterable._data[k]
            elif isinstance(iterable, dict):
                for k in list(iterable.keys()):
                    self._data[k] = self._data.get(k, 0) + iterable[k]
            else:
                for x in iterable:
                    self._data[x] = self._data.get(x, 0) + 1
        for k in kwargs:
            self._data[k] = self._data.get(k, 0) + kwargs[k]

    def __missing__(self, key):
        return 0

    def __delitem__(self, key):
        if key in self._data:
            del self._data[key]

    def update(self, iterable=None, **kwargs):
        self._count(iterable, kwargs)

    def subtract(self, iterable=None, **kwargs):
        if iterable is not None:
            if isinstance(iterable, _MappingBase) or isinstance(iterable, dict):
                src = iterable._data if isinstance(iterable, _MappingBase) else iterable
                for k in list(src.keys()):
                    self._data[k] = self._data.get(k, 0) - src[k]
            else:
                for x in iterable:
                    self._data[x] = self._data.get(x, 0) - 1
        for k in kwargs:
            self._data[k] = self._data.get(k, 0) - kwargs[k]

    def total(self):
        return sum(self._data.values())

    def most_common(self, n=None):
        items = list(self._data.items())
        items.sort(key=lambda kv: -kv[1])
        if n is None:
            return items
        return items[:n]

    def elements(self):
        for k in list(self._data.keys()):
            c = self._data[k]
            i = 0
            while i < c:
                yield k
                i += 1

    def copy(self):
        return Counter(self._data)

    def __repr__(self):
        if not self._data:
            return "Counter()"
        items = self.most_common()
        return "Counter({%s})" % ", ".join("%r: %r" % (k, v) for k, v in items)

    def __eq__(self, other):
        if isinstance(other, Counter):
            return all(self[e] == other[e] for e in set(self._data) | set(other._data))
        if isinstance(other, dict):
            return self._data == other
        return False

    def __le__(self, other):
        if not isinstance(other, Counter):
            raise TypeError("'<=' not supported between instances of 'Counter' and '%s'" % type(other).__name__)
        return all(self[e] <= other[e] for e in set(self._data) | set(other._data))

    def __lt__(self, other):
        return self <= other and self != other

    def __ge__(self, other):
        if not isinstance(other, Counter):
            raise TypeError("'>=' not supported between instances of 'Counter' and '%s'" % type(other).__name__)
        return all(self[e] >= other[e] for e in set(self._data) | set(other._data))

    def __gt__(self, other):
        return self >= other and self != other

    def _keep_positive(self):
        for k in list(self._data.keys()):
            if self._data[k] <= 0:
                del self._data[k]
        return self

    def __add__(self, other):
        if not isinstance(other, Counter):
            raise TypeError("unsupported operand type(s) for +: 'Counter' and '%s'" % type(other).__name__)
        result = Counter()
        for k in list(self._data.keys()):
            newcount = self._data[k] + other[k]
            if newcount > 0:
                result._data[k] = newcount
        for k in list(other._data.keys()):
            if k not in self._data and other._data[k] > 0:
                result._data[k] = other._data[k]
        return result

    def __sub__(self, other):
        if not isinstance(other, Counter):
            raise TypeError("unsupported operand type(s) for -: 'Counter' and '%s'" % type(other).__name__)
        result = Counter()
        for k in list(self._data.keys()):
            newcount = self._data[k] - other[k]
            if newcount > 0:
                result._data[k] = newcount
        for k in list(other._data.keys()):
            if k not in self._data and other._data[k] < 0:
                result._data[k] = 0 - other._data[k]
        return result

    def __or__(self, other):
        if not isinstance(other, Counter):
            raise TypeError("unsupported operand type(s) for |: 'Counter' and '%s'" % type(other).__name__)
        result = Counter()
        for k in list(self._data.keys()):
            other_count = other[k]
            count = self._data[k]
            newcount = other_count if count < other_count else count
            if newcount > 0:
                result._data[k] = newcount
        for k in list(other._data.keys()):
            if k not in self._data and other._data[k] > 0:
                result._data[k] = other._data[k]
        return result

    def __and__(self, other):
        if not isinstance(other, Counter):
            raise TypeError("unsupported operand type(s) for &: 'Counter' and '%s'" % type(other).__name__)
        result = Counter()
        for k in list(self._data.keys()):
            other_count = other[k]
            count = self._data[k]
            newcount = count if count < other_count else other_count
            if newcount > 0:
                result._data[k] = newcount
        return result

    def __pos__(self):
        result = Counter()
        for k in list(self._data.keys()):
            if self._data[k] > 0:
                result._data[k] = self._data[k]
        return result

    def __neg__(self):
        result = Counter()
        for k in list(self._data.keys()):
            if self._data[k] < 0:
                result._data[k] = 0 - self._data[k]
        return result

    def __iadd__(self, other):
        for k in list(other._data.keys()):
            self._data[k] = self._data.get(k, 0) + other._data[k]
        return self._keep_positive()

    def __isub__(self, other):
        for k in list(other._data.keys()):
            self._data[k] = self._data.get(k, 0) - other._data[k]
        return self._keep_positive()

    def __ior__(self, other):
        for k in list(other._data.keys()):
            other_count = other._data[k]
            if other_count > self[k]:
                self._data[k] = other_count
        return self._keep_positive()

    def __iand__(self, other):
        for k in list(self._data.keys()):
            other_count = other[k]
            if other_count < self._data[k]:
                self._data[k] = other_count
        return self._keep_positive()


class OrderedDict(_MappingBase):
    __typhon_builtin_bases__ = ("dict",)
    def __init__(self, *args, **kwargs):
        self._data = {}
        self.update(*args, **kwargs)

    # `_data` is a VM dict, whose native `move_to_end` / `popitem(last=)`
    # work in place (taking from the front is O(1)).
    def move_to_end(self, key, last=True):
        if key not in self._data:
            raise KeyError(key)
        self._data.move_to_end(key, last=last)

    def popitem(self, last=True):
        if not self._data:
            raise KeyError("dictionary is empty")
        return self._data.popitem(last=last)

    def __eq__(self, other):
        if isinstance(other, OrderedDict):
            return list(self._data.items()) == list(other._data.items())
        if isinstance(other, _MappingBase):
            return self._data == other._data
        if isinstance(other, dict):
            return self._data == other
        return False

    def __reversed__(self):
        return iter(list(self._data.keys())[::-1])

    def __repr__(self):
        if not self._data:
            return "OrderedDict()"
        return "OrderedDict(%r)" % self._data

    def copy(self):
        return OrderedDict(self._data)

    @classmethod
    def fromkeys(cls, iterable, value=None):
        d = cls()
        for k in iterable:
            d._data[k] = value
        return d


class ChainMap(_MappingBase):
    def __init__(self, *maps):
        self.maps = list(maps) if maps else [{}]

    def _lookup_map(self, key):
        for m in self.maps:
            if key in m:
                return m
        return None

    def __getitem__(self, key):
        m = self._lookup_map(key)
        if m is None:
            raise KeyError(key)
        return m[key]

    def __setitem__(self, key, value):
        self.maps[0][key] = value

    def __delitem__(self, key):
        if key not in self.maps[0]:
            raise KeyError("Key not found in the first mapping: %r" % key)
        del self.maps[0][key]

    def __contains__(self, key):
        return self._lookup_map(key) is not None

    def _flat(self):
        d = {}
        for m in self.maps[::-1]:
            for k in m:
                d[k] = m[k]
        return d

    def __len__(self):
        return len(self._flat())

    def __iter__(self):
        return iter(list(self._flat().keys()))

    def __bool__(self):
        return any(len(m) > 0 for m in self.maps)

    def keys(self):
        return self._flat().keys()

    def values(self):
        return self._flat().values()

    def items(self):
        return self._flat().items()

    def get(self, key, default=None):
        m = self._lookup_map(key)
        if m is None:
            return default
        return m[key]

    def new_child(self, m=None):
        if m is None:
            m = {}
        return ChainMap(m, *self.maps)

    @property
    def parents(self):
        return ChainMap(*self.maps[1:])

    def __repr__(self):
        return "ChainMap(%s)" % ", ".join(repr(m) for m in self.maps)

    def __eq__(self, other):
        if isinstance(other, ChainMap):
            return self._flat() == other._flat()
        if isinstance(other, dict):
            return self._flat() == other
        return False

    def copy(self):
        return ChainMap(dict(self.maps[0]), *self.maps[1:])

    def __copy__(self):
        return self.copy()

    # `_MappingBase`'s mutators all reach for `self._data`, which a ChainMap
    # does not have: every one of them has to work on the first mapping.
    def pop(self, key, *default):
        if key not in self.maps[0]:
            if default:
                return default[0]
            raise KeyError("Key not found in the first mapping: %r" % (key,))
        value = self.maps[0][key]
        del self.maps[0][key]
        return value

    def popitem(self):
        try:
            key = next(iter(self.maps[0]))
        except StopIteration:
            raise KeyError("No keys found in the first mapping.")
        value = self.maps[0][key]
        del self.maps[0][key]
        return (key, value)

    def setdefault(self, key, default=None):
        m = self._lookup_map(key)
        if m is not None:
            return m[key]
        self.maps[0][key] = default
        return default

    def clear(self):
        self.maps[0].clear()

    def update(self, other=None, **kwargs):
        if other is not None:
            if hasattr(other, "keys"):
                for k in other.keys():
                    self.maps[0][k] = other[k]
            else:
                for k, v in other:
                    self.maps[0][k] = v
        for k in kwargs:
            self.maps[0][k] = kwargs[k]

    def __or__(self, other):
        merged = self._flat()
        if hasattr(other, "keys"):
            for k in other.keys():
                merged[k] = other[k]
        else:
            return NotImplemented
        return ChainMap(merged, *self.maps[1:])

    def __ior__(self, other):
        self.update(other)
        return self


class deque:
    # The items are `_data[_head:]`. Taking from the left advances `_head`
    # (compacting once the dead prefix is half the list) and adding on the
    # left fills spare room kept before `_head`, so both ends are amortised
    # O(1) as in CPython — a plain list paid O(n) per `popleft`.
    def __init__(self, iterable=None, maxlen=None):
        if maxlen is not None:
            if not isinstance(maxlen, int):
                raise TypeError("an integer is required")
            if maxlen < 0:
                raise ValueError("maxlen must be non-negative")
        self.maxlen = maxlen
        self._data = []
        self._head = 0
        if iterable is not None:
            for x in iterable:
                self.append(x)

    def _items(self):
        if self._head:
            self._data = self._data[self._head:]
            self._head = 0
        return self._data

    def _drop_left(self):
        v = self._data[self._head]
        self._data[self._head] = None
        self._head += 1
        if self._head == len(self._data):
            self._data = []
            self._head = 0
        elif self._head > 16 and self._head * 2 > len(self._data):
            self._data = self._data[self._head:]
            self._head = 0
        return v

    def append(self, x):
        self._data.append(x)
        if self.maxlen is not None and len(self._data) - self._head > self.maxlen:
            self._drop_left()

    def appendleft(self, x):
        if self._head == 0:
            room = max(8, len(self._data))
            self._data = [None] * room + self._data
            self._head = room
        self._head -= 1
        self._data[self._head] = x
        if self.maxlen is not None and len(self._data) - self._head > self.maxlen:
            self._data.pop()

    def pop(self):
        if len(self._data) == self._head:
            raise IndexError("pop from an empty deque")
        v = self._data.pop()
        if len(self._data) == self._head:
            self._data = []
            self._head = 0
        return v

    def popleft(self):
        if len(self._data) == self._head:
            raise IndexError("pop from an empty deque")
        return self._drop_left()

    def extend(self, iterable):
        for x in list(iterable):
            self.append(x)

    def extendleft(self, iterable):
        for x in list(iterable):
            self.appendleft(x)

    def clear(self):
        self._data = []
        self._head = 0

    def copy(self):
        return deque(self._data[self._head:], self.maxlen)

    def count(self, x):
        return self._items().count(x)

    def index(self, x, *args):
        return self._items().index(x, *args)

    def insert(self, i, x):
        if self.maxlen is not None and len(self) >= self.maxlen:
            raise IndexError("deque already at its maximum size")
        self._items().insert(i, x)

    def remove(self, x):
        items = self._items()
        if x not in items:
            raise ValueError("%r is not in deque" % (x,))
        items.remove(x)

    def reverse(self):
        self._items().reverse()

    def rotate(self, n=1):
        items = self._items()
        length = len(items)
        if length == 0:
            return
        n = n % length
        if n:
            self._data = items[-n:] + items[:-n]

    def __len__(self):
        return len(self._data) - self._head

    def __bool__(self):
        return len(self._data) > self._head

    def __iter__(self):
        return iter(self._data[self._head:])

    def __reversed__(self):
        return iter(self._data[self._head:][::-1])

    def __contains__(self, x):
        return x in self._data[self._head:]

    def _slot(self, i):
        n = len(self._data) - self._head
        if i >= n or i < -n:
            raise IndexError("deque index out of range")
        return self._head + (i if i >= 0 else i + n)

    def __getitem__(self, i):
        if not isinstance(i, int):
            raise TypeError("sequence index must be integer, not '%s'" % type(i).__name__)
        return self._data[self._slot(i)]

    def __setitem__(self, i, v):
        self._data[self._slot(i)] = v

    def __delitem__(self, i):
        del self._data[self._slot(i)]

    def __eq__(self, other):
        if isinstance(other, deque):
            return self._data[self._head:] == other._data[other._head:]
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def __lt__(self, other):
        return self._data[self._head:] < other._data[other._head:]

    def __le__(self, other):
        return self._data[self._head:] <= other._data[other._head:]

    def __gt__(self, other):
        return self._data[self._head:] > other._data[other._head:]

    def __ge__(self, other):
        return self._data[self._head:] >= other._data[other._head:]

    def __add__(self, other):
        if not isinstance(other, deque):
            raise TypeError("can only concatenate deque (not \"%s\") to deque" % type(other).__name__)
        return deque(self._data[self._head:] + other._data[other._head:], self.maxlen)

    def __iadd__(self, other):
        self.extend(other)
        return self

    def __mul__(self, n):
        return deque(self._data[self._head:] * n, self.maxlen)

    def __rmul__(self, n):
        return deque(self._data[self._head:] * n, self.maxlen)

    def __hash__(self):
        raise TypeError("unhashable type: 'collections.deque'")

    def __repr__(self):
        if self.maxlen is None:
            return "deque(%r)" % self._data[self._head:]
        return "deque(%r, maxlen=%d)" % (self._data[self._head:], self.maxlen)


class _NamedTupleBase:
    __typhon_builtin_bases__ = ("tuple",)
    # `_fields` is a class attribute set on each generated class; the VM builds
    # the concrete class by copying these methods under the requested name.
    def __init__(self, *args, **kwargs):
        fields = type(self)._fields
        defaults = type(self)._field_defaults
        values = list(args)
        if len(values) > len(fields):
            raise TypeError("%s.__new__() takes %d positional arguments but %d were given" % (type(self).__name__, len(fields) + 1, len(values) + 1))
        for name in fields[len(values):]:
            if name in kwargs:
                values.append(kwargs.pop(name))
            elif name in defaults:
                values.append(defaults[name])
            else:
                missing = [f for f in fields[len(values):] if f not in kwargs and f not in defaults]
                raise TypeError("%s.__new__() missing %d required positional argument%s: %s" % (type(self).__name__, len(missing), "" if len(missing) == 1 else "s", " and ".join(["'%s'" % m for m in missing]) if len(missing) <= 2 else ", ".join(["'%s'" % m for m in missing[:-1]]) + ", and '%s'" % missing[-1]))
        for k in kwargs:
            raise TypeError("%s.__new__() got an unexpected keyword argument '%s'" % (type(self).__name__, k))
        self._values = tuple(values)
        i = 0
        for name in fields:
            object.__setattr__(self, name, values[i])
            i += 1

    def __setattr__(self, name, value):
        if name == "_values":
            object.__setattr__(self, name, value)
            return
        raise AttributeError("can't set attribute")

    def __getitem__(self, i):
        return self._values[i]

    def __iter__(self):
        return iter(self._values)

    def __len__(self):
        return len(self._values)

    def __contains__(self, x):
        return x in self._values

    def __eq__(self, other):
        if isinstance(other, _NamedTupleBase):
            return self._values == other._values
        if isinstance(other, tuple):
            return self._values == other
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def __lt__(self, other):
        return self._values < (other._values if isinstance(other, _NamedTupleBase) else other)

    def __le__(self, other):
        return self._values <= (other._values if isinstance(other, _NamedTupleBase) else other)

    def __gt__(self, other):
        return self._values > (other._values if isinstance(other, _NamedTupleBase) else other)

    def __ge__(self, other):
        return self._values >= (other._values if isinstance(other, _NamedTupleBase) else other)

    def __hash__(self):
        return hash(self._values)

    def __add__(self, other):
        return self._values + (other._values if isinstance(other, _NamedTupleBase) else other)

    def __repr__(self):
        parts = []
        i = 0
        for name in type(self)._fields:
            parts.append("%s=%r" % (name, self._values[i]))
            i += 1
        return "%s(%s)" % (type(self).__name__, ", ".join(parts))

    def _asdict(self):
        d = {}
        i = 0
        for name in type(self)._fields:
            d[name] = self._values[i]
            i += 1
        return d

    def _replace(self, **kwargs):
        d = self._asdict()
        for k in kwargs:
            if k not in d:
                raise TypeError("Got unexpected field names: %r" % [k])
            d[k] = kwargs[k]
        return type(self)(**d)

    @classmethod
    def _make(cls, iterable):
        return cls(*iterable)

    def count(self, x):
        return self._values.count(x)

    def index(self, x):
        return self._values.index(x)


def _namedtuple_fields(field_names):
    if isinstance(field_names, str):
        field_names = field_names.replace(",", " ").split()
    return list(map(str, field_names))


# ── UserDict / UserList / UserString ────────────────────────────────────────
# The wrapper classes, as CPython writes them: each keeps its payload in
# `self.data` and forwards the container protocol to it.


class UserDict:
    def __init__(self, dict=None, /, **kwargs):
        self.data = {}
        if dict is not None:
            self.update(dict)
        if kwargs:
            self.update(kwargs)

    def __len__(self):
        return len(self.data)

    def __getitem__(self, key):
        if key in self.data:
            return self.data[key]
        if hasattr(self.__class__, "__missing__"):
            return self.__class__.__missing__(self, key)
        raise KeyError(key)

    def __setitem__(self, key, item):
        self.data[key] = item

    def __delitem__(self, key):
        del self.data[key]

    def __iter__(self):
        return iter(self.data)

    def __contains__(self, key):
        return key in self.data

    def __repr__(self):
        return repr(self.data)

    def __eq__(self, other):
        if isinstance(other, UserDict):
            return self.data == other.data
        if isinstance(other, dict):
            return self.data == other
        return NotImplemented

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __or__(self, other):
        if isinstance(other, UserDict):
            return self.__class__({**self.data, **other.data})
        if isinstance(other, dict):
            return self.__class__({**self.data, **other})
        return NotImplemented

    def __ror__(self, other):
        if isinstance(other, UserDict):
            return self.__class__({**other.data, **self.data})
        if isinstance(other, dict):
            return self.__class__({**other, **self.data})
        return NotImplemented

    def __ior__(self, other):
        if isinstance(other, UserDict):
            self.data.update(other.data)
        else:
            self.data.update(other)
        return self

    # The MutableMapping mixins, routed through the item protocol so a
    # subclass overriding `__setitem__` / `__getitem__` sees every access.
    def keys(self):
        return self.data.keys()

    def items(self):
        return self.data.items()

    def values(self):
        return self.data.values()

    def get(self, key, default=None):
        if key in self:
            return self[key]
        return default

    def pop(self, key, *default):
        if len(default) > 1:
            raise TypeError(
                f"UserDict.pop() takes from 2 to 3 positional arguments but {len(default) + 2} were given"
            )
        if key in self:
            value = self[key]
            del self[key]
            return value
        if default:
            return default[0]
        raise KeyError(key)

    def popitem(self):
        if not self.data:
            raise KeyError("popitem(): dictionary is empty")
        key = next(reversed(list(self.data.keys())))
        value = self[key]
        del self[key]
        return key, value

    def clear(self):
        self.data.clear()

    def update(self, other=(), /, **kwds):
        if isinstance(other, UserDict):
            for key in other.data:
                self[key] = other.data[key]
        elif hasattr(other, "keys"):
            for key in other.keys():
                self[key] = other[key]
        else:
            for key, value in other:
                self[key] = value
        for key in kwds:
            self[key] = kwds[key]

    def setdefault(self, key, default=None):
        if key in self:
            return self[key]
        self[key] = default
        return default

    def copy(self):
        return self.__class__(self.data.copy())

    def __copy__(self):
        # CPython: the whole instance state, with a copy of `data`.
        inst = object.__new__(type(self))
        for k, v in vars(self).items():
            object.__setattr__(inst, k, v)
        inst.data = self.data.copy()
        return inst

    @classmethod
    def fromkeys(cls, iterable, value=None):
        d = cls()
        for key in iterable:
            d[key] = value
        return d

class UserList:
    def __init__(self, initlist=None):
        self.data = []
        if initlist is not None:
            if type(initlist) is list:
                self.data[:] = initlist
            elif isinstance(initlist, UserList):
                self.data[:] = initlist.data[:]
            else:
                self.data = list(initlist)

    def __repr__(self):
        return repr(self.data)

    def __lt__(self, other):
        return self.data < self.__cast(other)

    def __le__(self, other):
        return self.data <= self.__cast(other)

    def __eq__(self, other):
        return self.data == self.__cast(other)

    def __gt__(self, other):
        return self.data > self.__cast(other)

    def __ge__(self, other):
        return self.data >= self.__cast(other)

    def __cast(self, other):
        return other.data if isinstance(other, UserList) else other

    def __contains__(self, item):
        return item in self.data

    def __len__(self):
        return len(self.data)

    def __iter__(self):
        return iter(self.data)

    def __getitem__(self, i):
        if isinstance(i, slice):
            return self.__class__(self.data[i])
        return self.data[i]

    def __setitem__(self, i, item):
        self.data[i] = item

    def __delitem__(self, i):
        del self.data[i]

    def __add__(self, other):
        if isinstance(other, UserList):
            return self.__class__(self.data + other.data)
        if isinstance(other, list):
            return self.__class__(self.data + other)
        return self.__class__(self.data + list(other))

    def __radd__(self, other):
        if isinstance(other, UserList):
            return self.__class__(other.data + self.data)
        if isinstance(other, list):
            return self.__class__(other + self.data)
        return self.__class__(list(other) + self.data)

    def __iadd__(self, other):
        if isinstance(other, UserList):
            self.data += other.data
        elif isinstance(other, list):
            self.data += other
        else:
            self.data += list(other)
        return self

    def __mul__(self, n):
        return self.__class__(self.data * n)

    def __rmul__(self, n):
        return self.__class__(self.data * n)

    def __imul__(self, n):
        self.data *= n
        return self

    def append(self, item):
        self.data.append(item)

    def insert(self, i, item):
        self.data.insert(i, item)

    def pop(self, i=-1):
        return self.data.pop(i)

    def remove(self, item):
        self.data.remove(item)

    def clear(self):
        self.data.clear()

    def copy(self):
        return self.__class__(self)

    def __copy__(self):
        # CPython: the whole instance state, with a copy of `data`.
        inst = object.__new__(type(self))
        for k, v in vars(self).items():
            object.__setattr__(inst, k, v)
        inst.data = self.data[:]
        return inst

    def count(self, item):
        return self.data.count(item)

    def index(self, item, *args):
        return self.data.index(item, *args)

    def reverse(self):
        self.data.reverse()

    def sort(self, /, *args, **kwds):
        self.data.sort(*args, **kwds)

    def extend(self, other):
        if isinstance(other, UserList):
            self.data.extend(other.data)
        else:
            self.data.extend(other)


class UserString:
    def __init__(self, seq):
        if isinstance(seq, str):
            self.data = seq
        elif isinstance(seq, UserString):
            self.data = seq.data[:]
        else:
            self.data = str(seq)

    def __str__(self):
        return str(self.data)

    def __repr__(self):
        return repr(self.data)

    def __int__(self):
        return int(self.data)

    def __float__(self):
        return float(self.data)

    def __complex__(self):
        return complex(self.data)

    def __hash__(self):
        return hash(self.data)

    def __getnewargs__(self):
        return (self.data[:],)

    def __eq__(self, string):
        if isinstance(string, UserString):
            return self.data == string.data
        return self.data == string

    def __lt__(self, string):
        if isinstance(string, UserString):
            return self.data < string.data
        return self.data < string

    def __le__(self, string):
        if isinstance(string, UserString):
            return self.data <= string.data
        return self.data <= string

    def __gt__(self, string):
        if isinstance(string, UserString):
            return self.data > string.data
        return self.data > string

    def __ge__(self, string):
        if isinstance(string, UserString):
            return self.data >= string.data
        return self.data >= string

    def __contains__(self, char):
        if isinstance(char, UserString):
            char = char.data
        return char in self.data

    def __len__(self):
        return len(self.data)

    def __getitem__(self, index):
        return self.__class__(self.data[index])

    def __iter__(self):
        return iter(self.data)

    def __add__(self, other):
        if isinstance(other, UserString):
            return self.__class__(self.data + other.data)
        elif isinstance(other, str):
            return self.__class__(self.data + other)
        return self.__class__(self.data + str(other))

    def __radd__(self, other):
        if isinstance(other, str):
            return self.__class__(other + self.data)
        return self.__class__(str(other) + self.data)

    def __mul__(self, n):
        return self.__class__(self.data * n)

    def __rmul__(self, n):
        return self.__class__(self.data * n)

    def __mod__(self, args):
        return self.__class__(self.data % args)

    def __rmod__(self, template):
        return self.__class__(str(template) % self)

    def capitalize(self):
        return self.__class__(self.data.capitalize())

    def casefold(self):
        return self.__class__(self.data.casefold())

    def center(self, width, *args):
        return self.__class__(self.data.center(width, *args))

    def count(self, sub, start=0, end=None):
        if isinstance(sub, UserString):
            sub = sub.data
        if end is None:
            end = len(self.data)
        return self.data.count(sub, start, end)

    def removeprefix(self, prefix, /):
        if isinstance(prefix, UserString):
            prefix = prefix.data
        return self.__class__(self.data.removeprefix(prefix))

    def removesuffix(self, suffix, /):
        if isinstance(suffix, UserString):
            suffix = suffix.data
        return self.__class__(self.data.removesuffix(suffix))

    def encode(self, encoding="utf-8", errors="strict"):
        encoding = "utf-8" if encoding is None else encoding
        errors = "strict" if errors is None else errors
        return self.data.encode(encoding, errors)

    def endswith(self, suffix, start=0, end=None):
        if end is None:
            end = len(self.data)
        return self.data.endswith(suffix, start, end)

    def expandtabs(self, tabsize=8):
        return self.__class__(self.data.expandtabs(tabsize))

    def find(self, sub, start=0, end=None):
        if isinstance(sub, UserString):
            sub = sub.data
        if end is None:
            end = len(self.data)
        return self.data.find(sub, start, end)

    def format(self, /, *args, **kwds):
        return self.data.format(*args, **kwds)

    def format_map(self, mapping):
        return self.data.format_map(mapping)

    def index(self, sub, start=0, end=None):
        if end is None:
            end = len(self.data)
        return self.data.index(sub, start, end)

    def isalpha(self):
        return self.data.isalpha()

    def isalnum(self):
        return self.data.isalnum()

    def isascii(self):
        return self.data.isascii()

    def isdecimal(self):
        return self.data.isdecimal()

    def isdigit(self):
        return self.data.isdigit()

    def isidentifier(self):
        return self.data.isidentifier()

    def islower(self):
        return self.data.islower()

    def isnumeric(self):
        return self.data.isnumeric()

    def isprintable(self):
        return self.data.isprintable()

    def isspace(self):
        return self.data.isspace()

    def istitle(self):
        return self.data.istitle()

    def isupper(self):
        return self.data.isupper()

    def join(self, seq):
        return self.data.join(seq)

    def ljust(self, width, *args):
        return self.__class__(self.data.ljust(width, *args))

    def lower(self):
        return self.__class__(self.data.lower())

    def lstrip(self, chars=None):
        return self.__class__(self.data.lstrip(chars))

    def partition(self, sep):
        return self.data.partition(sep)

    def replace(self, old, new, maxsplit=-1):
        if isinstance(old, UserString):
            old = old.data
        if isinstance(new, UserString):
            new = new.data
        return self.__class__(self.data.replace(old, new, maxsplit))

    def rfind(self, sub, start=0, end=None):
        if isinstance(sub, UserString):
            sub = sub.data
        if end is None:
            end = len(self.data)
        return self.data.rfind(sub, start, end)

    def rindex(self, sub, start=0, end=None):
        if end is None:
            end = len(self.data)
        return self.data.rindex(sub, start, end)

    def rjust(self, width, *args):
        return self.__class__(self.data.rjust(width, *args))

    def rpartition(self, sep):
        return self.data.rpartition(sep)

    def rstrip(self, chars=None):
        return self.__class__(self.data.rstrip(chars))

    def split(self, sep=None, maxsplit=-1):
        return self.data.split(sep, maxsplit)

    def rsplit(self, sep=None, maxsplit=-1):
        return self.data.rsplit(sep, maxsplit)

    def splitlines(self, keepends=False):
        return self.data.splitlines(keepends)

    def startswith(self, prefix, start=0, end=None):
        if end is None:
            end = len(self.data)
        return self.data.startswith(prefix, start, end)

    def strip(self, chars=None):
        return self.__class__(self.data.strip(chars))

    def swapcase(self):
        return self.__class__(self.data.swapcase())

    def title(self):
        return self.__class__(self.data.title())

    def translate(self, *args):
        return self.__class__(self.data.translate(*args))

    def upper(self):
        return self.__class__(self.data.upper())

    def zfill(self, width):
        return self.__class__(self.data.zfill(width))
