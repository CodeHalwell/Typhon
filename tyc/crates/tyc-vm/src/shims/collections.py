# VM `collections`: Counter / deque / OrderedDict / ChainMap / namedtuple in
# the dialect the tree-walking VM interprets. Each mapping type keeps its
# entries in a plain dict (`_data`) and speaks the mapping protocol through
# dunders, so `c[k]`, `k in c`, `for k in c`, `len(c)`, `dict(c)` and the
# `most_common` / `elements` / `rotate` / `move_to_end` APIs all behave as in
# CPython. Validated against the real module (see validate_collections.py).


class _MappingBase:
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

    def move_to_end(self, key, last=True):
        if key not in self._data:
            raise KeyError(key)
        v = self._data[key]
        del self._data[key]
        if last:
            self._data[key] = v
        else:
            items = list(self._data.items())
            self._data.clear()
            self._data[key] = v
            for k, val in items:
                self._data[k] = val

    def popitem(self, last=True):
        if not self._data:
            raise KeyError("dictionary is empty")
        if last:
            k, v = self._data.popitem()
            return (k, v)
        k = list(self._data.keys())[0]
        v = self._data[k]
        del self._data[k]
        return (k, v)

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
    def __init__(self, iterable=None, maxlen=None):
        if maxlen is not None:
            if not isinstance(maxlen, int):
                raise TypeError("an integer is required")
            if maxlen < 0:
                raise ValueError("maxlen must be non-negative")
        self.maxlen = maxlen
        self._data = []
        if iterable is not None:
            for x in iterable:
                self.append(x)

    def append(self, x):
        self._data.append(x)
        if self.maxlen is not None and len(self._data) > self.maxlen:
            del self._data[0]

    def appendleft(self, x):
        self._data.insert(0, x)
        if self.maxlen is not None and len(self._data) > self.maxlen:
            self._data.pop()

    def pop(self):
        if not self._data:
            raise IndexError("pop from an empty deque")
        return self._data.pop()

    def popleft(self):
        if not self._data:
            raise IndexError("pop from an empty deque")
        v = self._data[0]
        del self._data[0]
        return v

    def extend(self, iterable):
        for x in list(iterable):
            self.append(x)

    def extendleft(self, iterable):
        for x in list(iterable):
            self.appendleft(x)

    def clear(self):
        self._data = []

    def copy(self):
        return deque(self._data, self.maxlen)

    def count(self, x):
        return self._data.count(x)

    def index(self, x, *args):
        return self._data.index(x, *args)

    def insert(self, i, x):
        if self.maxlen is not None and len(self._data) >= self.maxlen:
            raise IndexError("deque already at its maximum size")
        self._data.insert(i, x)

    def remove(self, x):
        if x not in self._data:
            raise ValueError("%r is not in deque" % (x,))
        self._data.remove(x)

    def reverse(self):
        self._data.reverse()

    def rotate(self, n=1):
        length = len(self._data)
        if length == 0:
            return
        n = n % length
        if n:
            self._data = self._data[-n:] + self._data[:-n]

    def __len__(self):
        return len(self._data)

    def __bool__(self):
        return len(self._data) > 0

    def __iter__(self):
        return iter(list(self._data))

    def __reversed__(self):
        return iter(self._data[::-1])

    def __contains__(self, x):
        return x in self._data

    def __getitem__(self, i):
        if not isinstance(i, int):
            raise TypeError("sequence index must be integer, not '%s'" % type(i).__name__)
        if i >= len(self._data) or i < -len(self._data):
            raise IndexError("deque index out of range")
        return self._data[i]

    def __setitem__(self, i, v):
        if i >= len(self._data) or i < -len(self._data):
            raise IndexError("deque index out of range")
        self._data[i] = v

    def __delitem__(self, i):
        if i >= len(self._data) or i < -len(self._data):
            raise IndexError("deque index out of range")
        del self._data[i]

    def __eq__(self, other):
        if isinstance(other, deque):
            return self._data == other._data
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def __lt__(self, other):
        return self._data < other._data

    def __le__(self, other):
        return self._data <= other._data

    def __gt__(self, other):
        return self._data > other._data

    def __ge__(self, other):
        return self._data >= other._data

    def __add__(self, other):
        if not isinstance(other, deque):
            raise TypeError("can only concatenate deque (not \"%s\") to deque" % type(other).__name__)
        return deque(self._data + other._data, self.maxlen)

    def __iadd__(self, other):
        self.extend(other)
        return self

    def __mul__(self, n):
        return deque(self._data * n, self.maxlen)

    def __rmul__(self, n):
        return deque(self._data * n, self.maxlen)

    def __hash__(self):
        raise TypeError("unhashable type: 'collections.deque'")

    def __repr__(self):
        if self.maxlen is None:
            return "deque(%r)" % self._data
        return "deque(%r, maxlen=%d)" % (self._data, self.maxlen)


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
