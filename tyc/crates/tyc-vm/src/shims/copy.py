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


def _function():
    pass


class _Holder:
    def method(self):
        pass


# Functions, builtins and methods copy as themselves, as in CPython.
_FUNCTION_TYPES = (type(_function), type(len), type(_Holder().method))

# The builtin iterator types: each answers CPython's `__reduce__`, which is
# how they are copied.
_ITER_TYPES = (
    type(iter([])),
    type(iter(())),
    type(iter("")),
    type(iter(range(0))),
    type(iter({})),
    type(iter({}.values())),
    type(iter({}.items())),
    type(iter(set())),
    type(reversed([])),
    type(reversed({})),
    type(enumerate([])),
    type(zip()),
    type(map(len, [])),
    type(filter(None, [])),
    type(_function() for _ in ()),
)

# The class of a real bytearray. Under the VM `bytearray` names the builtin
# constructor, so `type(x) is bytearray` alone does not recognise one.
_BYTEARRAY = type(bytearray())


def _atomic(x):
    return (
        x is None
        or x is Ellipsis
        or x is NotImplemented
        # The exact types, as CPython's dispatch table keys on `type(x)`: an
        # instance of `class C(int)` with state of its own is not atomic.
        or type(x) in (int, float, bool, complex, str, bytes, range)
        or isinstance(x, type)
        or type(x) in _FUNCTION_TYPES
        or type(x) is slice
        or type(x) is property
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


def _combined(state, slots):
    # The single state value CPython's reduce protocol hands over: the dict
    # state, paired with the slot state when there is any. `None` means
    # there is nothing to restore, and `__setstate__` is not called.
    return (state, slots) if slots else state


def _apply_state(y, cls, state):
    if _defines(cls, "__setstate__"):
        y.__setstate__(state)
        return
    slotstate = None
    if isinstance(state, tuple) and len(state) == 2:
        state, slotstate = state
    if state:
        for k, v in state.items():
            object.__setattr__(y, k, v)
    if slotstate:
        for k, v in slotstate.items():
            setattr(y, k, v)


def _rebuild(x, state, slots):
    cls = type(x)
    y = object.__new__(cls)
    state = _combined(state, slots)
    if state is not None:
        _apply_state(y, cls, state)
    return y


def _reduction(x):
    # The reduce value CPython's `copy` falls back on, when there is one to
    # follow: a builtin iterator's, or a class's own `__reduce_ex__` /
    # `__reduce__`. Anything else is rebuilt from its state.
    cls = type(x)
    if cls in _ITER_TYPES:
        return x.__reduce__()
    if _defines(cls, "__reduce_ex__"):
        return x.__reduce_ex__(4)
    if _defines(cls, "__reduce__"):
        return x.__reduce__()
    return None


def _reconstruct(x, memo, rv, deep):
    # `copy._reconstruct`: call the reduce value's callable on its
    # arguments, then restore the state and any list or dict items.
    func = rv[0]
    args = rv[1]
    state = rv[2] if len(rv) > 2 else None
    listiter = rv[3] if len(rv) > 3 else None
    dictiter = rv[4] if len(rv) > 4 else None
    if deep and args:
        args = deepcopy(args, memo)
    y = func(*args)
    if deep:
        memo[id(x)] = y
    if state is not None:
        if deep:
            state = deepcopy(state, memo)
        if type(y) in _ITER_TYPES or hasattr(y, "__setstate__"):
            y.__setstate__(state)
        else:
            _apply_state(y, type(y), state)
    if listiter is not None:
        for item in listiter:
            y.append(deepcopy(item, memo) if deep else item)
    if dictiter is not None:
        for key, value in dictiter:
            if deep:
                key = deepcopy(key, memo)
                value = deepcopy(value, memo)
            y[key] = value
    return y


def _defines(cls, name):
    for c in cls.__mro__:
        if c is not object and name in getattr(c, "__dict__", ()):
            return True
    return False


def _exception_state(x):
    # A builtin exception has no instance `__dict__` under the VM.
    try:
        d = vars(x)
    except TypeError:
        return None
    return dict(d) if d else None


def _get_state(x):
    if _defines(type(x), "__getstate__"):
        return x.__getstate__(), {}
    return _state(x)


def copy(x):
    cls = type(x)
    if isinstance(x, enum.Enum):
        return x
    if cls in (list, dict, set) or cls is bytearray or cls is _BYTEARRAY:
        return bytearray(x) if cls is _BYTEARRAY else x.copy()
    if cls in (tuple, frozenset):
        return x
    # Atomic objects first, as CPython's dispatch table does: a type or a
    # function is never asked for its `__copy__`.
    if _atomic(x):
        return x
    copier = getattr(cls, "__copy__", None)
    if copier is not None:
        return copier(x)
    # A VM shim (`Counter`, `OrderedDict`) whose storage must not be shared.
    copier = getattr(cls, "_typhon_copy", None)
    if copier is not None:
        return copier(x)
    rv = _reduction(x)
    if isinstance(rv, str):
        return x
    if rv is not None:
        return _reconstruct(x, None, rv, False)
    if isinstance(x, BaseException):
        # `BaseException.__reduce__`: the class called on the same args.
        y = cls(*x.args)
        state = _exception_state(x)
        if state is not None:
            _apply_state(y, cls, state)
        return y
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
    elif cls is bytearray or cls is _BYTEARRAY:
        y = bytearray(x)
    elif cls in _FUNCTION_TYPES and hasattr(x, "__func__") and hasattr(x, "__self__"):
        # A bound user method: CPython's `_deepcopy_method` rebinds the
        # function to a deep copy of the receiver.
        y = x.__func__.__get__(deepcopy(x.__self__, memo))
    elif cls is slice:
        # Not atomic for a deep copy: CPython rebuilds it from its parts.
        y = slice(deepcopy(x.start, memo), deepcopy(x.stop, memo), deepcopy(x.step, memo))
    else:
        copier = None if _atomic(x) else getattr(x, "__deepcopy__", None)
        if copier is not None:
            y = copier(memo)
        elif _atomic(x):
            y = x
        elif (rv := _reduction(x)) is not None:
            y = x if isinstance(rv, str) else _reconstruct(x, memo, rv, True)
        elif isinstance(x, BaseException):
            args = deepcopy(x.args, memo)
            y = cls(*args)
            memo[d] = y
            state = _exception_state(x)
            if state is not None:
                _apply_state(y, cls, deepcopy(state, memo))
        else:
            state = _combined(*_get_state(x))
            y = object.__new__(cls)
            memo[d] = y
            if state is not None:
                _apply_state(y, cls, deepcopy(state, memo))
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
