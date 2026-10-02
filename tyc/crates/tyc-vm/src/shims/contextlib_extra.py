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
        # Every callback runs, even when an earlier one raises: a failing
        # inner `__exit__` must not leave an outer file or lock open. The new
        # exception becomes the one in flight and propagates at the end.
        suppressed = False
        pending_type = exc_type
        pending = exc
        pending_tb = tb
        while self._callbacks:
            cb = self._callbacks.pop()
            try:
                if cb(pending_type, pending, pending_tb):
                    suppressed = True
                    pending_type = None
                    pending = None
                    pending_tb = None
            except BaseException as raised:
                pending_type = type(raised)
                pending = raised
                pending_tb = None
                suppressed = False
        if pending is not None and pending is not exc:
            raise pending
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


# ── the abstract bases, the decorator mixins, `chdir`, `aclosing`,
#    `AsyncExitStack` ─────────────────────────────────────────────────────────
# All plain Python in CPython too; `wraps` and `os` come from the assembled
# modules (this shim's own namespace holds only what is defined here).


class AbstractContextManager:
    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        return None


class AbstractAsyncContextManager:
    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc_value, traceback):
        return None


class ContextDecorator:
    """A context manager that can also decorate a function: the call runs
    inside a fresh `with self._recreate_cm():`."""

    def _recreate_cm(self):
        return self

    def __call__(self, func):
        from functools import wraps

        @wraps(func)
        def inner(*args, **kwds):
            with self._recreate_cm():
                return func(*args, **kwds)

        return inner


class AsyncContextDecorator:
    def _recreate_cm(self):
        return self

    def __call__(self, func):
        from functools import wraps

        @wraps(func)
        async def inner(*args, **kwds):
            async with self._recreate_cm():
                return await func(*args, **kwds)

        return inner


class chdir(AbstractContextManager):
    """Non-reentrant working-directory change, restored on exit."""

    def __init__(self, path):
        self.path = path
        self._old_cwd = []

    def __enter__(self):
        import os
        self._old_cwd.append(os.getcwd())
        os.chdir(self.path)

    def __exit__(self, *excinfo):
        import os
        os.chdir(self._old_cwd.pop())


class aclosing(AbstractAsyncContextManager):
    def __init__(self, thing):
        self.thing = thing

    async def __aenter__(self):
        return self.thing

    async def __aexit__(self, *exc_info):
        await self.thing.aclose()


class AsyncExitStack:
    """`ExitStack` for `async with`: sync and async exits and callbacks are
    unwound together, in reverse order, each seeing the exception the
    previous one left in flight."""

    def __init__(self):
        # (is_async, callback) pairs; a callback takes (exc_type, exc, tb).
        self._exit_callbacks = []

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb):
        suppressed = False
        pending_type = exc_type
        pending = exc
        pending_tb = tb
        while self._exit_callbacks:
            is_async, cb = self._exit_callbacks.pop()
            try:
                if is_async:
                    result = await cb(pending_type, pending, pending_tb)
                else:
                    result = cb(pending_type, pending, pending_tb)
                if result:
                    suppressed = True
                    pending_type = None
                    pending = None
                    pending_tb = None
            except BaseException as raised:
                pending_type = type(raised)
                pending = raised
                pending_tb = None
                suppressed = False
        if pending is not None and pending is not exc:
            raise pending
        return suppressed

    def enter_context(self, cm):
        result = cm.__enter__()
        self._exit_callbacks.append((False, cm.__exit__))
        return result

    async def enter_async_context(self, cm):
        result = await cm.__aenter__()
        self._exit_callbacks.append((True, cm.__aexit__))
        return result

    def push(self, exit):
        # A context manager's own `__exit__` is registered bound, as
        # CPython does; anything else is taken to be an exit callback.
        if hasattr(type(exit), "__exit__"):
            self._exit_callbacks.append((False, exit.__exit__))
        else:
            self._exit_callbacks.append((False, exit))
        return exit

    def push_async_exit(self, exit):
        if hasattr(type(exit), "__aexit__"):
            self._exit_callbacks.append((True, exit.__aexit__))
        else:
            self._exit_callbacks.append((True, exit))
        return exit

    def callback(self, callback, /, *args, **kwds):
        def _exit_wrapper(exc_type, exc, tb):
            callback(*args, **kwds)
            return False
        self._exit_callbacks.append((False, _exit_wrapper))
        return callback

    def push_async_callback(self, callback, /, *args, **kwds):
        async def _exit_wrapper(exc_type, exc, tb):
            await callback(*args, **kwds)
            return False
        self._exit_callbacks.append((True, _exit_wrapper))
        return callback

    def pop_all(self):
        other = AsyncExitStack()
        other._exit_callbacks = self._exit_callbacks
        self._exit_callbacks = []
        return other

    async def aclose(self):
        await self.__aexit__(None, None, None)
