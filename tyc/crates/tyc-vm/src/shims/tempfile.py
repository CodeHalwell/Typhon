# `tempfile` — temporary files and directories over `os`.
#
# Natives: `_fs_create_private(path, mode)` creates a file with
# `O_CREAT | O_EXCL` at `mode` and binds the *name* to the file created, so
# every later write the VM makes to that name (its file objects write back
# by name) goes to that file or nowhere; `_fs_tempdir()`; `_fspath(obj)`.

tempdir = None
template = "tmp"
_characters = "abcdefghijklmnopqrstuvwxyz0123456789_"
TMP_MAX = 10000


def gettempdir():
    global tempdir
    if tempdir is None:
        tempdir = _fs_tempdir()
    return tempdir


def gettempdirb():
    return gettempdir().encode("utf-8")


def gettempprefix():
    return template


def _random_name():
    # CPython draws names from a private `random.Random()` seeded from the
    # OS (`_RandomNameSequence`), so the program's own `random.seed(n)` has
    # no say in them. Here each character comes straight from `os.urandom`;
    # bytes of 222 and above are skipped so that `b % 37` is unbiased.
    out = []
    while len(out) < 8:
        for b in os.urandom(16):
            if b < 222 and len(out) < 8:
                out.append(_characters[b % 37])
    return "".join(out)


def _sanitize(prefix, suffix, dir):
    if suffix is None:
        suffix = ""
    if prefix is None:
        prefix = template
    if dir is None:
        dir = gettempdir()
    else:
        dir = _fspath(dir)
    return prefix, suffix, dir


def mkdtemp(suffix=None, prefix=None, dir=None):
    prefix, suffix, dir = _sanitize(prefix, suffix, dir)
    for _ in range(TMP_MAX):
        name = _random_name()
        file = os.path.join(dir, prefix + name + suffix)
        try:
            os.mkdir(file, 0o700)
        except FileExistsError:
            continue
        return os.path.abspath(file)
    raise FileExistsError(17, "No usable temporary directory name found")


def _create_private(prefix, suffix, dir):
    # CPython's `_mkstemp_inner`: `os.open(file, O_RDWR | O_CREAT | O_EXCL,
    # 0o600)`, trying a new name (up to `TMP_MAX` of them) when one is
    # taken. The mode is part of the create, so the file is never visible
    # with wider permissions.
    dir = os.path.abspath(dir)
    for _ in range(TMP_MAX):
        file = os.path.join(dir, prefix + _random_name() + suffix)
        try:
            _fs_create_private(file, 0o600)
        except FileExistsError:
            continue
        return file
    raise FileExistsError(17, "No usable temporary file name found")


def mkstemp(suffix=None, prefix=None, dir=None, text=False):
    prefix, suffix, dir = _sanitize(prefix, suffix, dir)
    file = _create_private(prefix, suffix, dir)
    return os._register_fd(file), file


def mktemp(suffix="", prefix=template, dir=None):
    if dir is None:
        dir = gettempdir()
    for _ in range(TMP_MAX):
        name = _random_name()
        file = os.path.join(dir, prefix + name + suffix)
        if not os.path.exists(file):
            return file
    raise FileExistsError(17, "No usable temporary filename found")


class TemporaryDirectory:
    def __init__(self, suffix=None, prefix=None, dir=None, ignore_cleanup_errors=False, *, delete=True):
        self.name = mkdtemp(suffix, prefix, dir)
        self._ignore_cleanup_errors = ignore_cleanup_errors
        self._delete = delete

    def __repr__(self):
        return "<%s %r>" % (type(self).__name__, self.name)

    def __enter__(self):
        return self.name

    def __exit__(self, exc, value, tb):
        if self._delete:
            self.cleanup()

    def cleanup(self):
        if os.path.isdir(self.name):
            shutil.rmtree(self.name, ignore_errors=self._ignore_cleanup_errors)


class _TemporaryFileWrapper:
    def __init__(self, file, name, delete=True, delete_on_close=True):
        self.file = file
        self.name = name
        self.delete = delete
        self.delete_on_close = delete_on_close
        self._closed = False

    def __getattr__(self, name):
        return getattr(self.file, name)

    def __enter__(self):
        self.file.__enter__()
        return self

    def __exit__(self, exc, value, tb):
        self.close()
        # CPython: `delete=True, delete_on_close=False` removes the file
        # when the `with` block ends rather than at `close()`.
        if self.delete and not self.delete_on_close:
            try:
                os.unlink(self.name)
            except FileNotFoundError:
                pass
        return False

    def __iter__(self):
        return iter(self.file)

    def close(self):
        if self._closed:
            return
        self._closed = True
        try:
            self.file.close()
        finally:
            if self.delete and self.delete_on_close:
                try:
                    os.unlink(self.name)
                except FileNotFoundError:
                    pass

    def __repr__(self):
        return "<%s %r>" % (type(self).__name__, self.name)


def NamedTemporaryFile(mode="w+b", buffering=-1, encoding=None, newline=None, suffix=None, prefix=None, dir=None, delete=True, *, errors=None, delete_on_close=True):
    prefix, suffix, dir = _sanitize(prefix, suffix, dir)
    file = _create_private(prefix, suffix, dir)
    # The file exists, empty, and is ours. A mode that would create it
    # (`x`) or read it by name first (`r+`) opens it as the writable,
    # empty file it is; the file object reports the mode asked for.
    open_mode = mode
    if "x" in mode:
        open_mode = mode.replace("x", "w")
    elif "r" in mode and "+" in mode:
        open_mode = mode.replace("r", "w")
    try:
        f = open(file, open_mode, buffering, encoding, errors, newline)
    except BaseException:
        os.unlink(file)
        raise
    if open_mode != mode and "b" not in mode:
        f.mode = mode
    return _TemporaryFileWrapper(f, file, delete, delete_on_close)


def TemporaryFile(mode="w+b", buffering=-1, encoding=None, newline=None, suffix=None, prefix=None, dir=None, *, errors=None):
    # CPython unlinks the file at once (or never links it: `O_TMPFILE`) and
    # keeps the descriptor. The VM's files are written back by name, so the
    # name stays until `close()`; it is owner-only and bound to the file
    # created, like `NamedTemporaryFile`'s.
    return NamedTemporaryFile(mode, buffering, encoding, newline, suffix, prefix, dir, True, errors=errors)


class SpooledTemporaryFile:
    """In memory until `max_size` is exceeded (or `fileno()` / `rollover()`
    is called), then a `TemporaryFile`."""

    def __init__(self, max_size=0, mode="w+b", buffering=-1, encoding=None, newline=None, suffix=None, prefix=None, dir=None, *, errors=None):
        if not isinstance(max_size, int):
            raise TypeError("'%s' object cannot be interpreted as an integer" % type(max_size).__name__)
        if "b" in mode:
            self._file = io.BytesIO()
        else:
            self._file = io.StringIO(newline=newline)
        self._max_size = max_size
        self._rolled = False
        self._mode = mode
        self._encoding = encoding
        self._errors = errors
        self._newline = newline
        self._args = (buffering, suffix, prefix, dir)

    def _check(self):
        if self._rolled:
            return
        if self._max_size and self._file.tell() > self._max_size:
            self.rollover()

    def rollover(self):
        if self._rolled:
            return
        old = self._file
        buffering, suffix, prefix, dir = self._args
        new = TemporaryFile(self._mode, buffering, self._encoding, self._newline, suffix, prefix, dir, errors=self._errors)
        pos = old.tell()
        new.write(old.getvalue())
        new.seek(pos, 0)
        self._file = new
        self._rolled = True

    def __enter__(self):
        if self._file.closed:
            raise ValueError("Cannot enter context with closed file")
        return self

    def __exit__(self, exc, value, tb):
        self._file.close()

    def __iter__(self):
        return iter(self._file)

    def close(self):
        self._file.close()

    @property
    def closed(self):
        return self._file.closed

    @property
    def encoding(self):
        if "b" in self._mode:
            raise AttributeError("'_io.BytesIO' object has no attribute 'encoding'")
        if self._rolled:
            return self._file.encoding
        return self._encoding or "utf-8"

    @property
    def errors(self):
        if "b" in self._mode:
            raise AttributeError("'_io.BytesIO' object has no attribute 'errors'")
        if self._rolled:
            return self._file.errors
        return self._errors or "strict"

    @property
    def mode(self):
        if self._rolled:
            return self._file.mode
        return self._mode

    @property
    def name(self):
        if self._rolled:
            return self._file.name
        return None

    @property
    def newlines(self):
        if "b" in self._mode:
            raise AttributeError("'_io.BytesIO' object has no attribute 'newlines'")
        return self._file.newlines

    def fileno(self):
        self.rollover()
        return self._file.fileno()

    def flush(self):
        self._file.flush()

    def isatty(self):
        return self._file.isatty()

    def read(self, *args):
        return self._file.read(*args)

    def readline(self, *args):
        return self._file.readline(*args)

    def readlines(self, *args):
        return self._file.readlines(*args)

    def readable(self):
        return self._file.readable()

    def writable(self):
        return self._file.writable()

    def seekable(self):
        return self._file.seekable()

    def seek(self, *args):
        return self._file.seek(*args)

    def tell(self):
        return self._file.tell()

    def truncate(self, size=None):
        if size is None:
            return self._file.truncate()
        if size > self._max_size:
            self.rollover()
        return self._file.truncate(size)

    def write(self, s):
        rv = self._file.write(s)
        self._check()
        return rv

    def writelines(self, iterable):
        rv = self._file.writelines(iterable)
        self._check()
        return rv

    def __repr__(self):
        return "<%s object at %s>" % (type(self).__name__, hex(id(self)))
