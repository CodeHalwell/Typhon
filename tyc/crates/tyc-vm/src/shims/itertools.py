# VM `itertools`: the module's iterators in the dialect the tree-walking VM
# interprets. Validated against the real module (see validate_itertools.py).
#
# Every public name is a class, as in CPython (`type(itertools.count)` is
# `type`, `type(itertools.count())` is `itertools.count`), so `isinstance`
# against them, subclassing them and their reprs all behave as there. Each
# class validates its arguments and takes its snapshot of the input (`tuple`
# for the combinatorics) at construction, as the C implementations do, and
# then delegates `__next__` to a private generator holding the recipe. The
# private names are not exported (see `make_itertools_module`).


class count:
    def __init__(self, start=0, step=1):
        self._n = start
        self._step = step

    def __iter__(self):
        return self

    def __next__(self):
        n = self._n
        self._n = n + self._step
        return n

    def __repr__(self):
        if type(self._step) is int and self._step == 1:
            return "count(%r)" % (self._n,)
        return "count(%r, %r)" % (self._n, self._step)


def _cycle(iterable):
    saved = []
    for element in iterable:
        yield element
        saved.append(element)
    while saved:
        for element in saved:
            yield element


class cycle:
    def __init__(self, iterable):
        self._g = _cycle(iter(iterable))

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


class repeat:
    def __init__(self, object, times=None):
        self._obj = object
        if times is not None and times < 0:
            times = 0
        self._times = times

    def __iter__(self):
        return self

    def __next__(self):
        if self._times is None:
            return self._obj
        if self._times <= 0:
            raise StopIteration
        self._times -= 1
        return self._obj

    def __length_hint__(self):
        if self._times is None:
            raise TypeError("len() of unsized object")
        return self._times

    def __repr__(self):
        if self._times is None:
            return "repeat(%r)" % (self._obj,)
        return "repeat(%r, %r)" % (self._obj, self._times)


def _accumulate(it, func, initial):
    total = initial
    if initial is None:
        try:
            total = next(it)
        except StopIteration:
            return
    yield total
    for element in it:
        if func is None:
            total = total + element
        else:
            total = func(total, element)
        yield total


class accumulate:
    def __init__(self, iterable, func=None, *, initial=None):
        self._g = _accumulate(iter(iterable), func, initial)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


class chain:
    def __init__(self, *iterables):
        self._iterables = iter(iterables)
        self._current = None

    @classmethod
    def from_iterable(cls, iterables):
        c = cls()
        c._iterables = iter(iterables)
        return c

    def __iter__(self):
        return self

    def __next__(self):
        while True:
            if self._current is None:
                try:
                    self._current = iter(next(self._iterables))
                except StopIteration:
                    raise StopIteration
            try:
                return next(self._current)
            except StopIteration:
                self._current = None


def _compress(data, selectors):
    for d, s in zip(data, selectors):
        if s:
            yield d


class compress:
    def __init__(self, data, selectors):
        self._g = _compress(iter(data), iter(selectors))

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _dropwhile(predicate, it):
    for x in it:
        if not predicate(x):
            yield x
            break
    for x in it:
        yield x


class dropwhile:
    def __init__(self, predicate, iterable):
        self._g = _dropwhile(predicate, iter(iterable))

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _filterfalse(predicate, it):
    if predicate is None:
        predicate = bool
    for x in it:
        if not predicate(x):
            yield x


class filterfalse:
    def __init__(self, function, iterable):
        self._g = _filterfalse(function, iter(iterable))

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


class groupby:
    def __init__(self, iterable, key=None):
        if key is None:
            key = lambda x: x
        self._keyfunc = key
        self._it = iter(iterable)
        self._tgtkey = self._currkey = self._currvalue = object()
        self._exhausted = False
        self._id = 0

    def __iter__(self):
        return self

    def __next__(self):
        self._id += 1
        # Keys are compared by *equality*, as CPython does: advancing the
        # outer iterator without draining a subgroup must still skip the rest
        # of it when the key is a fresh object each time (`key=lambda x: [x]`).
        while self._currkey == self._tgtkey:
            try:
                self._currvalue = next(self._it)
            except StopIteration:
                self._exhausted = True
                raise StopIteration
            self._currkey = self._keyfunc(self._currvalue)
        self._tgtkey = self._currkey
        return (self._currkey, self._grouper(self._tgtkey, self._id))

    def _grouper(self, tgtkey, gid):
        while self._id == gid and self._currkey == tgtkey:
            yield self._currvalue
            try:
                self._currvalue = next(self._it)
            except StopIteration:
                self._exhausted = True
                return
            self._currkey = self._keyfunc(self._currvalue)


def _islice(it, start, stop, step):
    i = 0
    nexti = start
    while stop is None or i < stop:
        try:
            element = next(it)
        except StopIteration:
            return
        if i == nexti:
            yield element
            nexti += step
        i += 1


class islice:
    def __init__(self, iterable, *args):
        if not args:
            raise TypeError("islice expected at least 2 arguments, got 1")
        if len(args) > 3:
            raise TypeError("islice expected at most 4 arguments, got %d" % (len(args) + 1))
        if len(args) == 1:
            start, stop, step = 0, args[0], 1
        else:
            start = args[0] if args[0] is not None else 0
            stop = args[1]
            step = args[2] if len(args) == 3 and args[2] is not None else 1
        if not isinstance(start, int) or start < 0:
            raise ValueError("Indices for islice() must be None or an integer: 0 <= x <= sys.maxsize.")
        if stop is not None and (not isinstance(stop, int) or stop < 0):
            raise ValueError("Stop argument for islice() must be None or an integer: 0 <= x <= sys.maxsize.")
        if not isinstance(step, int) or step < 1:
            raise ValueError("Step for islice() must be a positive integer or None.")
        self._g = _islice(iter(iterable), start, stop, step)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _pairwise(it):
    try:
        a = next(it)
    except StopIteration:
        return
    for b in it:
        yield (a, b)
        a = b


class pairwise:
    def __init__(self, iterable):
        self._g = _pairwise(iter(iterable))

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _starmap(function, it):
    for args in it:
        yield function(*args)


class starmap:
    def __init__(self, function, iterable):
        self._g = _starmap(function, iter(iterable))

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _takewhile(predicate, it):
    for x in it:
        if predicate(x):
            yield x
        else:
            break


class takewhile:
    def __init__(self, predicate, iterable):
        self._g = _takewhile(predicate, iter(iterable))

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def tee(iterable, n=2):
    if n < 0:
        raise ValueError("n must be >= 0")
    it = iter(iterable)
    buffers = [[] for _ in range(n)]

    def gen(mybuf):
        while True:
            if not mybuf:
                try:
                    newval = next(it)
                except StopIteration:
                    return
                for b in buffers:
                    b.append(newval)
            yield mybuf.pop(0)
    return tuple(gen(b) for b in buffers)


def _zip_longest(iterators, fillvalue):
    num_active = len(iterators)
    if not num_active:
        return
    while True:
        values = []
        for i, it in enumerate(iterators):
            try:
                value = next(it)
            except StopIteration:
                num_active -= 1
                if not num_active:
                    return
                iterators[i] = repeat(fillvalue)
                value = fillvalue
            values.append(value)
        yield tuple(values)


class zip_longest:
    def __init__(self, *iterables, fillvalue=None):
        self._g = _zip_longest([iter(it) for it in iterables], fillvalue)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _product(pools):
    result = [[]]
    for pool in pools:
        result = [x + [y] for x in result for y in pool]
    for prod in result:
        yield tuple(prod)


class product:
    def __init__(self, *iterables, repeat=1):
        if repeat < 0:
            raise ValueError("repeat argument cannot be negative")
        self._g = _product([tuple(pool) for pool in iterables] * repeat)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _permutations(pool, r):
    n = len(pool)
    if r > n:
        return
    indices = list(range(n))
    cycles = list(range(n, n - r, -1))
    yield tuple(pool[i] for i in indices[:r])
    while n:
        found = False
        for i in reversed(range(r)):
            cycles[i] -= 1
            if cycles[i] == 0:
                indices[i:] = indices[i + 1:] + indices[i:i + 1]
                cycles[i] = n - i
            else:
                j = cycles[i]
                indices[i], indices[-j] = indices[-j], indices[i]
                yield tuple(pool[i] for i in indices[:r])
                found = True
                break
        if not found:
            return


class permutations:
    def __init__(self, iterable, r=None):
        pool = tuple(iterable)
        r = len(pool) if r is None else r
        if r < 0:
            raise ValueError("r must be non-negative")
        self._g = _permutations(pool, r)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _combinations(pool, r):
    n = len(pool)
    if r > n:
        return
    indices = list(range(r))
    yield tuple(pool[i] for i in indices)
    while True:
        found = False
        for i in reversed(range(r)):
            if indices[i] != i + n - r:
                found = True
                break
        if not found:
            return
        indices[i] += 1
        for j in range(i + 1, r):
            indices[j] = indices[j - 1] + 1
        yield tuple(pool[i] for i in indices)


class combinations:
    def __init__(self, iterable, r):
        pool = tuple(iterable)
        if r < 0:
            raise ValueError("r must be non-negative")
        self._g = _combinations(pool, r)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _combinations_with_replacement(pool, r):
    n = len(pool)
    if not n and r:
        return
    indices = [0] * r
    yield tuple(pool[i] for i in indices)
    while True:
        found = False
        for i in reversed(range(r)):
            if indices[i] != n - 1:
                found = True
                break
        if not found:
            return
        indices[i:] = [indices[i] + 1] * (r - i)
        yield tuple(pool[i] for i in indices)


class combinations_with_replacement:
    def __init__(self, iterable, r):
        pool = tuple(iterable)
        if r < 0:
            raise ValueError("r must be non-negative")
        self._g = _combinations_with_replacement(pool, r)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)


def _batched(it, n, strict):
    while True:
        batch = tuple(islice(it, n))
        if not batch:
            return
        if strict and len(batch) != n:
            raise ValueError("batched(): incomplete batch")
        yield batch


class batched:
    def __init__(self, iterable, n, *, strict=False):
        if n < 1:
            raise ValueError("n must be at least one")
        self._g = _batched(iter(iterable), n, strict)

    def __iter__(self):
        return self

    def __next__(self):
        return next(self._g)
