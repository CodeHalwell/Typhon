# `bisect` — binary search / insertion over a sorted sequence.


def bisect_left(a, x, lo=0, hi=None, key=None):
    if lo < 0:
        raise ValueError("lo must be non-negative")
    if hi is None:
        hi = len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        value = a[mid] if key is None else key(a[mid])
        if value < x:
            lo = mid + 1
        else:
            hi = mid
    return lo


def bisect_right(a, x, lo=0, hi=None, key=None):
    if lo < 0:
        raise ValueError("lo must be non-negative")
    if hi is None:
        hi = len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        value = a[mid] if key is None else key(a[mid])
        if x < value:
            hi = mid
        else:
            lo = mid + 1
    return lo


def insort_left(a, x, lo=0, hi=None, key=None):
    a.insert(bisect_left(a, x if key is None else key(x), lo, hi, key), x)


def insort_right(a, x, lo=0, hi=None, key=None):
    a.insert(bisect_right(a, x if key is None else key(x), lo, hi, key), x)


bisect = bisect_right
insort = insort_right
