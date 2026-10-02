# `heapq.merge` — the one `heapq` function CPython writes in Python. The
# heap primitives stay native; this is a plain k-way merge driven by the
# iterator protocol, lazy like CPython's (a generator), with the same tie
# rule: equal keys come from the earlier iterable first, in both directions.


def merge(*iterables, key=None, reverse=False):
    entries = []
    for order, iterable in enumerate(iterables):
        it = iter(iterable)
        try:
            value = next(it)
        except StopIteration:
            continue
        entries.append([value if key is None else key(value), order, value, it])
    while entries:
        best = 0
        for i in range(1, len(entries)):
            if reverse:
                if entries[i][0] > entries[best][0]:
                    best = i
            elif entries[i][0] < entries[best][0]:
                best = i
        entry = entries[best]
        yield entry[2]
        try:
            value = next(entry[3])
        except StopIteration:
            del entries[best]
            continue
        entry[0] = value if key is None else key(value)
        entry[2] = value
