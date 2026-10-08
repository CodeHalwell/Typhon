# `copy` — shallow and deep copies, after CPython's `Lib/copy.py`.
#
# CPython copies an arbitrary object through `__reduce_ex__`; the VM has no
# pickle protocol, so an instance is rebuilt directly: a bare object of the
# same class (`object.__new__`, no `__init__`), then its `__dict__` and
# `__slots__` state, or `__setstate__` when the class defines one.


import dataclasses
import enum


class Error(Exception):
    pass


error = Error


def _atomic(x):
    return (
        x is None
        or x is Ellipsis
        or x is NotImplemented
        or isinstance(x, (int, float, bool, complex, str, bytes, range, type))
        or callable(x) and not hasattr(x, "__dict__") and not hasattr(type(x), "__copy__")
    )


def _slot_names(cls):
    names = []
    for c in cls.__mro__:
        slots = c.__dict__.get("__slots__", ())
        if isinstance(slots, str):
            slots = (slots,)
        for n in slots:
            if n not in ("__dict__", "__weakref__") and n not in names:
                names.append(n)
    return names


def _state(x):
    try:
        d = vars(x)
    except TypeError:
        d = None
    slots = {}
    for n in _slot_names(type(x)):
        if hasattr(x, n):
            slots[n] = getattr(x, n)
    return (dict(d) if d else None), slots


def _rebuild(x, state, slots):
    cls = type(x)
    y = object.__new__(cls)
    if _defines(cls, "__setstate__"):
        y.__setstate__(state if not slots else (state, slots))
        return y
    if state:
        for k, v in state.items():
            object.__setattr__(y, k, v)
    for k, v in slots.items():
        object.__setattr__(y, k, v)
    return y


def _defines(cls, name):
    for c in cls.__mro__:
        if c is not object and name in c.__dict__:
            return True
    return False


def _get_state(x):
    if _defines(type(x), "__getstate__"):
        return x.__getstate__(), {}
    return _state(x)


def copy(x):
    cls = type(x)
    if isinstance(x, enum.Enum):
        return x
    if cls in (list, dict, set, bytearray):
        return x.copy()
    if cls in (tuple, frozenset):
        return x
    copier = getattr(cls, "__copy__", None)
    if copier is not None:
        return copier(x)
    if _atomic(x):
        return x
    state, slots = _get_state(x)
    return _rebuild(x, state, slots)


def deepcopy(x, memo=None, _nil=[]):
    if memo is None:
        memo = {}
    d = id(x)
    y = memo.get(d, _nil)
    if y is not _nil:
        return y
    cls = type(x)
    if isinstance(x, enum.Enum):
        return x
    if cls is list:
        y = []
        memo[d] = y
        for a in x:
            y.append(deepcopy(a, memo))
    elif cls is dict:
        y = {}
        memo[d] = y
        for k, v in x.items():
            y[deepcopy(k, memo)] = deepcopy(v, memo)
    elif cls is tuple:
        items = [deepcopy(a, memo) for a in x]
        # A tuple that contains itself was already copied by the recursion.
        if d in memo:
            return memo[d]
        same = True
        for a, b in zip(x, items):
            if a is not b:
                same = False
                break
        y = x if same else tuple(items)
    elif cls is set or cls is frozenset:
        y = cls(deepcopy(a, memo) for a in x)
    elif cls is bytearray:
        y = bytearray(x)
    else:
        copier = getattr(x, "__deepcopy__", None)
        if copier is not None:
            y = copier(memo)
        elif _atomic(x):
            y = x
        else:
            state, slots = _get_state(x)
            y = object.__new__(cls)
            memo[d] = y
            if state is not None and not isinstance(state, dict):
                y.__setstate__(deepcopy(state, memo))
            else:
                if state:
                    state = deepcopy(state, memo)
                    if _defines(cls, "__setstate__"):
                        y.__setstate__(state)
                    else:
                        for k, v in state.items():
                            object.__setattr__(y, k, v)
                for k, v in slots.items():
                    object.__setattr__(y, k, deepcopy(v, memo))
    # Keep `x` alive alongside its copy, as CPython's memo does, so its id
    # cannot be reused by a later object while the memo is live.
    if y is not x:
        memo[d] = y
        memo.setdefault(id(memo), []).append(x)
    return y


def replace(obj, /, **changes):
    func = getattr(type(obj), "__replace__", None)
    if func is None and dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        return dataclasses.replace(obj, **changes)
    if func is None and hasattr(obj, "_replace") and isinstance(obj, tuple):
        return obj._replace(**changes)
    if func is None:
        raise TypeError(f"replace() does not support {type(obj).__name__} objects")
    return func(obj, **changes)
