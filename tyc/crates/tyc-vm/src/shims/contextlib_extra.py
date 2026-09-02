# `contextlib` helpers that are plain Python classes. `contextmanager` /
# `asynccontextmanager` stay native (they drive a generator through the VM's
# own coroutine machinery); everything here is just object protocol.


class suppress:
    def __init__(self, *exceptions):
        self._exceptions = exceptions

    def __enter__(self):
        return None

    def __exit__(self, exc_type, exc, tb):
        if exc_type is None:
            return False
        for kind in self._exceptions:
            if issubclass(exc_type, kind):
                return True
        return False


class nullcontext:
    def __init__(self, enter_result=None):
        self.enter_result = enter_result

    def __enter__(self):
        return self.enter_result

    def __exit__(self, exc_type, exc, tb):
        return False


class closing:
    def __init__(self, thing):
        self.thing = thing

    def __enter__(self):
        return self.thing

    def __exit__(self, exc_type, exc, tb):
        self.thing.close()
        return False


class _RedirectStream:
    _stream = ""

    def __init__(self, new_target):
        self._new_target = new_target
        self._old_targets = []

    def __enter__(self):
        import sys
        self._old_targets.append(getattr(sys, self._stream))
        setattr(sys, self._stream, self._new_target)
        return self._new_target

    def __exit__(self, exc_type, exc, tb):
        import sys
        setattr(sys, self._stream, self._old_targets.pop())
        return False


class redirect_stdout(_RedirectStream):
    _stream = "stdout"


class redirect_stderr(_RedirectStream):
    _stream = "stderr"


class ExitStack:
    def __init__(self):
        self._callbacks = []

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        suppressed = False
        while self._callbacks:
            cb = self._callbacks.pop()
            if cb(exc_type, exc, tb):
                suppressed = True
                exc_type = None
                exc = None
                tb = None
        return suppressed

    def enter_context(self, cm):
        result = cm.__enter__()
        self._callbacks.append(cm.__exit__)
        return result

    def callback(self, fn, *args, **kwargs):
        def _run(exc_type, exc, tb):
            fn(*args, **kwargs)
            return False
        self._callbacks.append(_run)
        return fn

    def push(self, cm):
        self._callbacks.append(cm.__exit__)
        return cm

    def pop_all(self):
        other = ExitStack()
        other._callbacks = self._callbacks
        self._callbacks = []
        return other

    def close(self):
        self.__exit__(None, None, None)
