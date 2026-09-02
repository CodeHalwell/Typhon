# VM `itertools`: the module's iterator recipes as lazy generator functions /
# iterator classes in the dialect the tree-walking VM interprets. Validated
# against the real module (see validate_itertools.py).


def count(start=0, step=1):
    n = start
    while True:
        yield n
        n = n + step


def cycle(iterable):
    saved = []
    for element in iterable:
        yield element
        saved.append(element)
    while saved:
        for element in saved:
            yield element


def repeat(obj, times=None):
    if times is None:
        while True:
            yield obj
    else:
        i = 0
        while i < times:
            yield obj
            i += 1


def accumulate(iterable, func=None, *, initial=None):
    it = iter(iterable)
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


def compress(data, selectors):
    for d, s in zip(data, selectors):
        if s:
            yield d


def dropwhile(predicate, iterable):
    it = iter(iterable)
    for x in it:
        if not predicate(x):
            yield x
            break
    for x in it:
        yield x


def filterfalse(predicate, iterable):
    if predicate is None:
        predicate = bool
    for x in iterable:
        if not predicate(x):
            yield x


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
        while self._currkey is self._tgtkey:
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


def islice(iterable, *args):
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
    it = iter(iterable)
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


def pairwise(iterable):
    it = iter(iterable)
    try:
        a = next(it)
    except StopIteration:
        return
    for b in it:
        yield (a, b)
        a = b


def starmap(function, iterable):
    for args in iterable:
        yield function(*args)


def takewhile(predicate, iterable):
    for x in iterable:
        if predicate(x):
            yield x
        else:
            break


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


def zip_longest(*iterables, fillvalue=None):
    iterators = [iter(it) for it in iterables]
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


def product(*iterables, repeat=1):
    pools = [tuple(pool) for pool in iterables] * repeat
    result = [[]]
    for pool in pools:
        result = [x + [y] for x in result for y in pool]
    for prod in result:
        yield tuple(prod)


def permutations(iterable, r=None):
    pool = tuple(iterable)
    n = len(pool)
    r = n if r is None else r
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


def combinations(iterable, r):
    pool = tuple(iterable)
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


def combinations_with_replacement(iterable, r):
    pool = tuple(iterable)
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


def batched(iterable, n, *, strict=False):
    if n < 1:
        raise ValueError("n must be at least one")
    it = iter(iterable)
    while True:
        batch = tuple(islice(it, n))
        if not batch:
            return
        if strict and len(batch) != n:
            raise ValueError("batched(): incomplete batch")
        yield batch
