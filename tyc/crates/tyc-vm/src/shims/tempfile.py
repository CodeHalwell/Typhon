# `tempfile` — temporary files and directories over `os`; names come from
# the module-level random generator like CPython's `_RandomNameSequence`.

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
    return "".join([random.choice(_characters) for _ in range(8)])


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


def mkstemp(suffix=None, prefix=None, dir=None, text=False):
    prefix, suffix, dir = _sanitize(prefix, suffix, dir)
    for _ in range(TMP_MAX):
        name = _random_name()
        file = os.path.join(dir, prefix + name + suffix)
        try:
            # 0600 at creation, as CPython's `mkstemp` guarantees — a
            # create-then-chmod would leave the file world-readable for a
            # moment, and under a 022 umask it stayed 0644 outright.
            _fs_write(file, b"", "x", 0o600)
        except FileExistsError:
            continue
        return os._register_fd(file), os.path.abspath(file)
    raise FileExistsError(17, "No usable temporary file name found")


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
    if "b" in mode:
        create_mode = "wb"
    else:
        create_mode = "w"
    for _ in range(TMP_MAX):
        name = _random_name()
        file = os.path.join(dir, prefix + name + suffix)
        if os.path.exists(file):
            continue
        break
    file = os.path.abspath(file)
    # Owner-only, like `mkstemp` above and like CPython.
    _fs_write(file, b"", "x", 0o600)
    f = open(file, mode, buffering, encoding, errors, newline)
    return _TemporaryFileWrapper(f, file, delete, delete_on_close)


def TemporaryFile(mode="w+b", buffering=-1, encoding=None, newline=None, suffix=None, prefix=None, dir=None, *, errors=None):
    return NamedTemporaryFile(mode, buffering, encoding, newline, suffix, prefix, dir, True, errors=errors)


SpooledTemporaryFile = NamedTemporaryFile
