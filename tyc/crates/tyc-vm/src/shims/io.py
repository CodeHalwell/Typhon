# `io` — text and binary streams. `StringIO` / `BytesIO` are in-memory;
# `open()` builds a `TextIOWrapper` or a `Buffered*` over the whole file held
# in memory (read at open, written back on flush / close / interpreter
# exit). Natives: `_fs_read(path)`, `_fs_write(path, data, mode)`,
# `_fs_exists(path)`, `_fspath(obj)`.

SEEK_SET = 0
SEEK_CUR = 1
SEEK_END = 2
DEFAULT_BUFFER_SIZE = 8192


class UnsupportedOperation(OSError, ValueError):
    pass


_open_files = []


def _flush_all():
    # Called by the VM at interpreter exit: CPython flushes every
    # still-open file object when it is finalised.
    for f in list(_open_files):
        try:
            if not f.closed:
                f.flush()
        except Exception:
            pass


class IOBase:
    _closed_message = "I/O operation on closed file."

    def __init__(self):
        self._closed = False

    @property
    def closed(self):
        return self._closed

    def _check_closed(self):
        if self._closed:
            raise ValueError(self._closed_message)

    def close(self):
        if not self._closed:
            try:
                self.flush()
            finally:
                self._closed = True

    def flush(self):
        self._check_closed()

    def readable(self):
        return False

    def writable(self):
        return False

    def seekable(self):
        return False

    def isatty(self):
        self._check_closed()
        return False

    def fileno(self):
        raise UnsupportedOperation("fileno")

    def __enter__(self):
        self._check_closed()
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()
        return False

    def __iter__(self):
        self._check_closed()
        return self

    def __next__(self):
        line = self.readline()
        if not line:
            raise StopIteration
        return line

    def readlines(self, hint=-1):
        lines = []
        total = 0
        while True:
            line = self.readline()
            if not line:
                break
            lines.append(line)
            total += len(line)
            if hint is not None and hint > 0 and total >= hint:
                break
        return lines

    def writelines(self, lines):
        self._check_closed()
        for line in lines:
            self.write(line)


class RawIOBase(IOBase):
    pass


class BufferedIOBase(IOBase):
    pass


class TextIOBase(IOBase):
    pass


def _split_newline(buf, pos, newlines):
    # Index just past the first line terminator at or after `pos`, or -1.
    # `\r\n` counts as one terminator when it is recognised.
    tail = buf[pos:]
    best_start = -1
    best_len = 0
    for nl in newlines:
        i = tail.find(nl)
        if i >= 0 and (best_start < 0 or i < best_start or (i == best_start and len(nl) > best_len)):
            best_start = i
            best_len = len(nl)
    if best_start < 0:
        return -1
    return pos + best_start + best_len


class _TextStream(TextIOBase):
    """Shared text buffer: `StringIO` and the text file wrapper."""

    def __init__(self, newline):
        IOBase.__init__(self)
        self._buf = ""
        self._pos = 0
        self._newline = newline
        # Line terminators `readline` recognises.
        if newline is None or newline == "":
            self._line_ends = ("\r\n", "\r", "\n")
        else:
            self._line_ends = (newline,)

    def readable(self):
        return True

    def writable(self):
        return True

    def seekable(self):
        return True

    def _load(self):
        pass

    def read(self, size=-1):
        self._check_closed()
        self._load()
        if size is None or size < 0:
            out = self._buf[self._pos:]
            self._pos = len(self._buf)
            return out
        out = self._buf[self._pos:self._pos + size]
        self._pos += len(out)
        return out

    def readline(self, size=-1):
        self._check_closed()
        self._load()
        if self._pos >= len(self._buf):
            return ""
        end = _split_newline(self._buf, self._pos, self._line_ends)
        if end < 0:
            end = len(self._buf)
        if size is not None and size >= 0 and end - self._pos > size:
            end = self._pos + size
        out = self._buf[self._pos:end]
        self._pos = end
        return out

    def write(self, s):
        self._check_closed()
        if not isinstance(s, str):
            raise TypeError(self._write_type_message % type(s).__name__)
        self._load()
        text = self._translate_out(s)
        if self._pos == len(self._buf):
            self._buf = self._buf + text
        else:
            self._buf = self._buf[:self._pos] + text + self._buf[self._pos + len(text):]
        self._pos += len(text)
        self._dirty = True
        return len(s)

    def _translate_out(self, s):
        return s

    def seek(self, offset, whence=0):
        self._check_closed()
        self._load()
        if whence == 0:
            if offset < 0:
                raise ValueError("negative seek position %r" % (offset,))
            self._pos = offset
        elif whence == 1:
            if offset != 0:
                raise UnsupportedOperation("can't do nonzero cur-relative seeks")
        elif whence == 2:
            if offset != 0:
                raise UnsupportedOperation("can't do nonzero end-relative seeks")
            self._pos = len(self._buf)
        else:
            raise ValueError("invalid whence (%r, should be 0, 1 or 2)" % (whence,))
        return self._pos

    def tell(self):
        self._check_closed()
        return self._pos

    def truncate(self, pos=None):
        self._check_closed()
        self._load()
        if pos is None:
            pos = self._pos
        self._buf = self._buf[:pos]
        self._dirty = True
        return pos


class StringIO(_TextStream):
    _closed_message = "I/O operation on closed file"
    _write_type_message = "string argument expected, got '%s'"

    def __init__(self, initial_value="", newline="\n"):
        if initial_value is None:
            initial_value = ""
        if not isinstance(initial_value, str):
            raise TypeError("initial_value must be str or None, not %s" % type(initial_value).__name__)
        _TextStream.__init__(self, newline)
        self._buf = initial_value
        self._dirty = False

    def getvalue(self):
        self._check_closed()
        return self._buf

    @property
    def encoding(self):
        return None

    @property
    def errors(self):
        return None

    @property
    def line_buffering(self):
        return False

    def __repr__(self):
        return "<_io.StringIO object at %s>" % hex(id(self))


class TextIOWrapper(_TextStream):
    _write_type_message = "write() argument must be str, not %s"

    def __init__(self, name, mode, encoding, errors, newline, readable, writable, raw_bytes, dirty):
        _TextStream.__init__(self, newline)
        self.name = name
        self.mode = mode
        self.encoding = encoding
        self.errors = errors
        self.newlines = None
        self.line_buffering = False
        self._readable = readable
        self._writable = writable
        self._raw = raw_bytes
        self._loaded = False
        self._dirty = dirty

    def readable(self):
        return self._readable

    def writable(self):
        return self._writable

    def _load(self):
        if self._loaded:
            return
        self._loaded = True
        raw = self._raw
        self._raw = None
        text = raw.decode(self.encoding, self.errors)
        if self._newline is None:
            text = text.replace("\r\n", "\n").replace("\r", "\n")
        self._buf = text

    def _translate_out(self, s):
        if self._newline in ("\r", "\r\n"):
            return s.replace("\n", self._newline)
        return s

    def read(self, size=-1):
        if not self._readable:
            self._check_closed()
            raise UnsupportedOperation("not readable")
        return _TextStream.read(self, size)

    def readline(self, size=-1):
        if not self._readable:
            self._check_closed()
            raise UnsupportedOperation("not readable")
        return _TextStream.readline(self, size)

    def write(self, s):
        if not self._writable:
            self._check_closed()
            raise UnsupportedOperation("not writable")
        # CPython encodes as it buffers, so an unencodable character fails
        # here rather than at flush time.
        if isinstance(s, str):
            s.encode(self.encoding, self.errors)
        return _TextStream.write(self, s)

    def flush(self):
        self._check_closed()
        if self._dirty and self._writable:
            self._load()
            _fs_write(self.name, self._buf.encode(self.encoding, self.errors), "w")
            self._dirty = False

    # CPython's text-file positions are byte offsets into the encoded file.
    def tell(self):
        self._check_closed()
        self._load()
        return len(self._buf[:self._pos].encode(self.encoding, self.errors))

    def seek(self, offset, whence=0):
        self._check_closed()
        self._load()
        if whence == 0:
            if offset < 0:
                raise ValueError("negative seek position %r" % (offset,))
            encoded = self._buf.encode(self.encoding, self.errors)
            self._pos = len(encoded[:offset].decode(self.encoding, "ignore"))
        elif whence == 1:
            if offset != 0:
                raise UnsupportedOperation("can't do nonzero cur-relative seeks")
        elif whence == 2:
            if offset != 0:
                raise UnsupportedOperation("can't do nonzero end-relative seeks")
            self._pos = len(self._buf)
        else:
            raise ValueError("invalid whence (%r, should be 0, 1 or 2)" % (whence,))
        return self.tell()

    def __repr__(self):
        return "<_io.TextIOWrapper name=%r mode=%r encoding=%r>" % (self.name, self.mode, self.encoding)


class _BytesStream(BufferedIOBase):
    def __init__(self):
        IOBase.__init__(self)
        self._buf = b""
        self._pos = 0
        self._dirty = False

    def readable(self):
        return True

    def writable(self):
        return True

    def seekable(self):
        return True

    def _load(self):
        pass

    def read(self, size=-1):
        self._check_closed()
        self._load()
        if size is None or size < 0:
            out = self._buf[self._pos:]
            self._pos = len(self._buf)
            return out
        out = self._buf[self._pos:self._pos + size]
        self._pos += len(out)
        return out

    def read1(self, size=-1):
        return self.read(size)

    def readline(self, size=-1):
        self._check_closed()
        self._load()
        if self._pos >= len(self._buf):
            return b""
        i = self._buf[self._pos:].find(b"\n")
        end = len(self._buf) if i < 0 else self._pos + i + 1
        if size is not None and size >= 0 and end - self._pos > size:
            end = self._pos + size
        out = self._buf[self._pos:end]
        self._pos = end
        return out

    def write(self, data):
        self._check_closed()
        if not isinstance(data, bytes):
            raise TypeError("a bytes-like object is required, not '%s'" % type(data).__name__)
        self._load()
        if self._pos == len(self._buf):
            self._buf = self._buf + data
        else:
            self._buf = self._buf[:self._pos] + data + self._buf[self._pos + len(data):]
        self._pos += len(data)
        self._dirty = True
        return len(data)

    def seek(self, offset, whence=0):
        self._check_closed()
        self._load()
        if whence == 0:
            if offset < 0:
                raise ValueError("negative seek value %d" % offset)
            self._pos = offset
        elif whence == 1:
            self._pos = max(0, self._pos + offset)
        elif whence == 2:
            self._pos = max(0, len(self._buf) + offset)
        else:
            raise ValueError("invalid whence (%r, should be 0, 1 or 2)" % (whence,))
        return self._pos

    def tell(self):
        self._check_closed()
        return self._pos

    def truncate(self, pos=None):
        self._check_closed()
        self._load()
        if pos is None:
            pos = self._pos
        self._buf = self._buf[:pos]
        self._dirty = True
        return pos


class BytesIO(_BytesStream):
    _closed_message = "I/O operation on closed file."

    def __init__(self, initial_bytes=b""):
        if initial_bytes is None:
            initial_bytes = b""
        if not isinstance(initial_bytes, bytes):
            raise TypeError("a bytes-like object is required, not '%s'" % type(initial_bytes).__name__)
        _BytesStream.__init__(self)
        self._buf = initial_bytes

    def getvalue(self):
        self._check_closed()
        return self._buf

    def getbuffer(self):
        return self._buf

    def __repr__(self):
        return "<_io.BytesIO object at %s>" % hex(id(self))


class _BinaryFile(_BytesStream):
    def __init__(self, name, mode, readable, writable, raw_bytes, dirty, kind):
        _BytesStream.__init__(self)
        self.name = name
        self.mode = mode
        self._readable = readable
        self._writable = writable
        self._buf = raw_bytes
        self._dirty = dirty
        self._kind = kind

    def readable(self):
        return self._readable

    def writable(self):
        return self._writable

    def read(self, size=-1):
        if not self._readable:
            self._check_closed()
            raise UnsupportedOperation("read")
        return _BytesStream.read(self, size)

    def readline(self, size=-1):
        if not self._readable:
            self._check_closed()
            raise UnsupportedOperation("read")
        return _BytesStream.readline(self, size)

    def write(self, data):
        if not self._writable:
            self._check_closed()
            raise UnsupportedOperation("write")
        return _BytesStream.write(self, data)

    def flush(self):
        self._check_closed()
        if self._dirty and self._writable:
            _fs_write(self.name, self._buf, "w")
            self._dirty = False

    def __repr__(self):
        return "<_io.%s name=%r>" % (self._kind, self.name)


def _parse_mode(mode):
    if not isinstance(mode, str):
        raise TypeError("open() argument 'mode' must be str, not %s" % type(mode).__name__)
    seen = set()
    for c in mode:
        if c not in "rwxabt+U" or c in seen:
            raise ValueError("invalid mode: %r" % (mode,))
        seen.add(c)
    creating = "x" in seen
    reading = "r" in seen or "U" in seen
    writing = "w" in seen
    appending = "a" in seen
    updating = "+" in seen
    text = "t" in seen
    binary = "b" in seen
    if text and binary:
        raise ValueError("can't have text and binary mode at once")
    if creating + reading + writing + appending > 1:
        raise ValueError("must have exactly one of create/read/write/append mode")
    if not (creating or reading or writing or appending):
        raise ValueError("Must have exactly one of create/read/write/append mode and at most one plus")
    return creating, reading, writing, appending, updating, binary


def open(file, mode="r", buffering=-1, encoding=None, errors=None, newline=None, closefd=True, opener=None):
    path = _fspath(file)
    creating, reading, writing, appending, updating, binary = _parse_mode(mode)
    if binary and encoding is not None:
        raise ValueError("binary mode doesn't take an encoding argument")
    if binary and errors is not None:
        raise ValueError("binary mode doesn't take an errors argument")
    if binary and newline is not None:
        raise ValueError("binary mode doesn't take a newline argument")
    if newline not in (None, "", "\n", "\r", "\r\n"):
        raise ValueError("illegal newline value: %r" % (newline,))
    raw = b""
    dirty = False
    if reading:
        raw = _fs_read(path)
    elif creating:
        _fs_write(path, b"", "x")
    elif writing:
        _fs_write(path, b"", "w")
    elif appending:
        if _fs_exists(path):
            raw = _fs_read(path)
        else:
            _fs_write(path, b"", "a")
    readable = reading or updating
    writable = creating or writing or appending or updating
    if binary:
        if readable and writable:
            kind = "BufferedRandom"
        elif writable:
            kind = "BufferedWriter"
        else:
            kind = "BufferedReader"
        # `FileIO` normalises the mode it reports: `w+b` / `r+b` are `rb+`.
        if updating and (reading or writing):
            file_mode = "rb+"
        elif updating and appending:
            file_mode = "ab+"
        elif updating:
            file_mode = "xb+"
        elif creating:
            file_mode = "xb"
        elif writing:
            file_mode = "wb"
        elif appending:
            file_mode = "ab"
        else:
            file_mode = "rb"
        f = _BinaryFile(path, file_mode, readable, writable, raw, dirty, kind)
        if appending:
            f._pos = len(raw)
    else:
        if encoding is None:
            encoding = "utf-8"
        if errors is None:
            errors = "strict"
        f = TextIOWrapper(path, mode, encoding, errors, newline, readable, writable, raw, dirty)
        if appending:
            f._load()
            f._pos = len(f._buf)
    _open_files.append(f)
    return f
