# `pathlib` — CPython 3.13's PurePosixPath / PosixPath semantics. Pure-path
# parsing is self-contained; the concrete methods go through the `os` module
# seeded in by the VM and the `open` builtin.


def _fnmatch(name, pat, case_sensitive=None):
    # fnmatch on one path component: `*`, `?`, `[seq]`, `[!seq]`. Matching is
    # case-sensitive unless the caller asked otherwise.
    if case_sensitive is False:
        name = name.lower()
        pat = pat.lower()
    return _fnmatch_at(name, 0, pat, 0)


def _fnmatch_at(name, i, pat, j):
    while j < len(pat):
        c = pat[j]
        if c == "*":
            j += 1
            while j < len(pat) and pat[j] == "*":
                j += 1
            if j == len(pat):
                return True
            k = i
            while k <= len(name):
                if _fnmatch_at(name, k, pat, j):
                    return True
                k += 1
            return False
        if i >= len(name):
            return False
        if c == "?":
            i += 1
            j += 1
            continue
        if c == "[":
            k = j + 1
            negate = False
            if k < len(pat) and pat[k] == "!":
                negate = True
                k += 1
            if k < len(pat) and pat[k] == "]":
                k += 1
            while k < len(pat) and pat[k] != "]":
                k += 1
            if k >= len(pat):
                # No closing bracket: a literal '['.
                if name[i] != "[":
                    return False
                i += 1
                j += 1
                continue
            body = pat[j + 1:k]
            if negate:
                body = body[1:]
            matched = _class_match(name[i], body)
            if matched == negate:
                return False
            i += 1
            j = k + 1
            continue
        if name[i] != c:
            return False
        i += 1
        j += 1
    return i == len(name)


def _class_match(ch, body):
    k = 0
    while k < len(body):
        if k + 2 < len(body) and body[k + 1] == "-":
            if body[k] <= ch <= body[k + 2]:
                return True
            k += 3
        else:
            if body[k] == ch:
                return True
            k += 1
    return False


def _is_pathlike(key):
    if isinstance(key, (str, PurePosixPath)):
        return True
    if isinstance(key, (int, float, bool, bytes, list, tuple, dict, set)) or key is None:
        return False
    try:
        p = key.__fspath__()
    except AttributeError:
        return False
    return isinstance(p, str)


def _has_magic(s):
    return "*" in s or "?" in s or "[" in s


class _Parents:
    def __init__(self, path):
        self._path = path
        self._tail = path._tail
        n = len(self._tail)
        self._len = n if path._root else max(n - 1, 0) if n else 0
        if not path._root and n:
            self._len = n
        # A relative path's parents end at '.', an absolute one at its root:
        # both are one more than the number of components dropped.
        self._len = n

    def __len__(self):
        return self._len

    def __getitem__(self, idx):
        if isinstance(idx, slice):
            return tuple(self[i] for i in range(*idx.indices(len(self))))
        if idx < 0:
            idx += len(self)
        if idx < 0 or idx >= len(self):
            raise IndexError(idx)
        return self._path._from_parts(self._path._root, self._tail[:-idx - 1])

    def __iter__(self):
        for i in range(len(self)):
            yield self[i]

    def __contains__(self, item):
        for p in self:
            if p == item:
                return True
        return False

    def __repr__(self):
        return "<%s.parents>" % type(self._path).__name__


class PurePosixPath:
    def __init__(self, *args):
        raw = []
        for a in args:
            if isinstance(a, PurePosixPath):
                raw.append(a._str)
            elif isinstance(a, str):
                raw.append(a)
            else:
                try:
                    p = a.__fspath__()
                except AttributeError:
                    p = None
                if not isinstance(p, str):
                    raise TypeError("argument should be a str or an os.PathLike object "
                                    "where __fspath__ returns a str, not %r" % type(a).__name__)
                raw.append(p)
        path = ""
        for b in raw:
            if b.startswith("/"):
                path = b
            elif not path or path.endswith("/"):
                path += b
            else:
                path += "/" + b
        if path.startswith("//") and not path.startswith("///"):
            root = "//"
        elif path.startswith("/"):
            root = "/"
        else:
            root = ""
        rest = path.lstrip("/") if root else path
        self._root = root
        self._tail = [p for p in rest.split("/") if p and p != "."]
        self._str = self._root + "/".join(self._tail) or "."

    def _from_parts(self, root, tail):
        p = type(self)()
        p._root = root
        p._tail = list(tail)
        p._str = root + "/".join(tail) or "."
        return p

    def with_segments(self, *args):
        return type(self)(*args)

    def __str__(self):
        return self._str

    def __fspath__(self):
        return self._str

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, self._str)

    def __bytes__(self):
        return self._str.encode("utf-8")

    def as_posix(self):
        return self._str

    def as_uri(self):
        if not self.is_absolute():
            raise ValueError("relative path can't be expressed as a file URI")
        out = []
        for ch in self._str.encode("utf-8"):
            c = chr(ch)
            if c.isalnum() and ch < 128 or c in "/-._~":
                out.append(c)
            else:
                out.append("%%%02X" % ch)
        return "file://" + "".join(out)

    def __eq__(self, other):
        if not isinstance(other, PurePosixPath):
            return False
        return self._str == other._str

    def __ne__(self, other):
        if not isinstance(other, PurePosixPath):
            return True
        return self._str != other._str

    def __hash__(self):
        return hash(self._str)

    def _parts_normcase(self):
        return self._str.split("/")

    def _cmp_check(self, other, op):
        if not isinstance(other, PurePosixPath):
            raise TypeError("'%s' not supported between instances of '%s' and '%s'" % (
                op, type(self).__name__, type(other).__name__))

    def __lt__(self, other):
        self._cmp_check(other, "<")
        return self._parts_normcase() < other._parts_normcase()

    def __le__(self, other):
        self._cmp_check(other, "<=")
        return self._parts_normcase() <= other._parts_normcase()

    def __gt__(self, other):
        self._cmp_check(other, ">")
        return self._parts_normcase() > other._parts_normcase()

    def __ge__(self, other):
        self._cmp_check(other, ">=")
        return self._parts_normcase() >= other._parts_normcase()

    @property
    def drive(self):
        return ""

    @property
    def root(self):
        return self._root

    @property
    def anchor(self):
        return self._root

    @property
    def parts(self):
        if self._root:
            return (self._root,) + tuple(self._tail)
        return tuple(self._tail)

    @property
    def name(self):
        return self._tail[-1] if self._tail else ""

    @property
    def suffix(self):
        name = self.name
        i = name.rfind(".")
        if 0 < i < len(name) - 1:
            return name[i:]
        return ""

    @property
    def suffixes(self):
        name = self.name
        if name.endswith("."):
            return []
        name = name.lstrip(".")
        return ["." + s for s in name.split(".")[1:]]

    @property
    def stem(self):
        name = self.name
        i = name.rfind(".")
        if 0 < i < len(name) - 1:
            return name[:i]
        return name

    @property
    def parent(self):
        if not self._tail:
            return self
        return self._from_parts(self._root, self._tail[:-1])

    @property
    def parents(self):
        return _Parents(self)

    def with_name(self, name):
        if not self.name:
            raise ValueError("%r has an empty name" % (self,))
        if not name or "/" in name or name == ".":
            raise ValueError("Invalid name %r" % (name,))
        return self._from_parts(self._root, self._tail[:-1] + [name])

    def with_stem(self, stem):
        suffix = self.suffix
        if not suffix:
            return self.with_name(stem)
        elif not stem:
            raise ValueError("%r has an empty name" % (self,))
        return self.with_name(stem + suffix)

    def with_suffix(self, suffix):
        if suffix and not suffix.startswith(".") or suffix == ".":
            raise ValueError("Invalid suffix %r" % (suffix,))
        name = self.name
        if not name:
            raise ValueError("%r has an empty name" % (self,))
        old = self.suffix
        if old:
            name = name[:-len(old)]
        return self.with_name(name + suffix)

    def joinpath(self, *pathsegments):
        return type(self)(self, *pathsegments)

    def __truediv__(self, key):
        if not _is_pathlike(key):
            raise TypeError("unsupported operand type(s) for /: '%s' and '%s'" % (
                type(self).__name__, type(key).__name__))
        return type(self)(self, key)

    def __rtruediv__(self, key):
        if not _is_pathlike(key):
            raise TypeError("unsupported operand type(s) for /: '%s' and '%s'" % (
                type(key).__name__, type(self).__name__))
        return type(self)(key, self)

    def is_absolute(self):
        return bool(self._root)

    def is_reserved(self):
        return False

    def is_relative_to(self, other, *_deprecated):
        other = type(self)(other)
        return other == self or other in self.parents

    def relative_to(self, other, *_deprecated, walk_up=False):
        other = type(self)(other)
        candidates = [other] + list(other.parents)
        step = 0
        found = None
        for path in candidates:
            if path == self or path in self.parents:
                found = path
                break
            elif not walk_up:
                raise ValueError("%r is not in the subpath of %r" % (str(self), str(other)))
            elif path.name == "..":
                raise ValueError("'..' segment in %r cannot be walked" % (str(other),))
            step += 1
        if found is None:
            raise ValueError("%r and %r have different anchors" % (str(self), str(other)))
        parts = [".."] * step + self._tail[len(found._tail):]
        return self._from_parts("", parts)

    def match(self, path_pattern, *, case_sensitive=None):
        pattern = type(self)(path_pattern)
        pat_parts = list(pattern.parts)
        if not pat_parts:
            raise ValueError("empty pattern")
        parts = list(self.parts)
        if pattern._root:
            if len(pat_parts) != len(parts):
                return False
        elif len(pat_parts) > len(parts):
            return False
        for part, pat in zip(reversed(parts), reversed(pat_parts)):
            if pat == "**":
                pat = "*"
            if pat == part and pat in ("/", "//"):
                continue
            if not _fnmatch(part, pat):
                return False
        return True

    def full_match(self, pattern, *, case_sensitive=None):
        pattern = type(self)(pattern)
        pat_parts = list(pattern.parts)
        if not pat_parts:
            raise ValueError("empty pattern")
        parts = list(self.parts)
        if bool(pattern._root) != bool(self._root):
            return False
        if pattern._root:
            pat_parts = pat_parts[1:]
            parts = parts[1:]
        return _match_parts(parts, 0, pat_parts, 0)


def _match_parts(parts, i, pats, j):
    while j < len(pats):
        pat = pats[j]
        if pat == "**":
            j += 1
            if j == len(pats):
                return True
            k = i
            while k <= len(parts):
                if _match_parts(parts, k, pats, j):
                    return True
                k += 1
            return False
        if i >= len(parts):
            return False
        if not _fnmatch(parts[i], pat):
            return False
        i += 1
        j += 1
    return i == len(parts)


PurePath = PurePosixPath


# CPython puts the filesystem methods on `Path` and makes `PosixPath` the
# concrete subclass, which is what a bound method's repr names
# (`<bound method Path.iterdir of PosixPath('/t')>`). The module-level
# `Path` is rebound to `PosixPath` at the bottom, so `Path(...)` still
# constructs the concrete class exactly as CPython's `Path.__new__` does.
class Path(PurePosixPath):
    @classmethod
    def cwd(cls):
        return cls(os.getcwd())

    @classmethod
    def home(cls):
        return cls(os.path.expanduser("~"))

    def _fs(self):
        return self._str

    def stat(self, *, follow_symlinks=True):
        return os.stat(self._str, follow_symlinks=follow_symlinks)

    def lstat(self):
        return os.lstat(self._str)

    def exists(self, *, follow_symlinks=True):
        try:
            self.stat(follow_symlinks=follow_symlinks)
        except OSError:
            return False
        except ValueError:
            return False
        return True

    def is_file(self, *, follow_symlinks=True):
        try:
            st = self.stat(follow_symlinks=follow_symlinks)
        except (OSError, ValueError):
            return False
        return (st.st_mode & 0o170000) == 0o100000

    def is_dir(self, *, follow_symlinks=True):
        try:
            st = self.stat(follow_symlinks=follow_symlinks)
        except (OSError, ValueError):
            return False
        return (st.st_mode & 0o170000) == 0o040000

    def is_symlink(self):
        try:
            st = self.lstat()
        except (OSError, ValueError):
            return False
        return (st.st_mode & 0o170000) == 0o120000

    def is_mount(self):
        return self._str == "/"

    def is_junction(self):
        return False

    def is_block_device(self):
        return False

    def is_char_device(self):
        return False

    def is_fifo(self):
        return False

    def is_socket(self):
        return False

    def samefile(self, other_path):
        other = other_path if isinstance(other_path, PurePosixPath) else type(self)(other_path)
        return os.path.samefile(self._str, other._str)

    def open(self, mode="r", buffering=-1, encoding=None, errors=None, newline=None):
        return open(self._str, mode, buffering, encoding, errors, newline)

    def read_bytes(self):
        with self.open(mode="rb") as f:
            return f.read()

    def read_text(self, encoding=None, errors=None, newline=None):
        with self.open(mode="r", encoding=encoding, errors=errors, newline=newline) as f:
            return f.read()

    def write_bytes(self, data):
        if not isinstance(data, bytes):
            raise TypeError("a bytes-like object is required, not '%s'" % type(data).__name__)
        with self.open(mode="wb") as f:
            return f.write(data)

    def write_text(self, data, encoding=None, errors=None, newline=None):
        if not isinstance(data, str):
            raise TypeError("data must be str, not %s" % type(data).__name__)
        with self.open(mode="w", encoding=encoding, errors=errors, newline=newline) as f:
            return f.write(data)

    def iterdir(self):
        for name in os.listdir(self._str):
            yield self._make_child(name)

    def _make_child(self, name):
        return self._from_parts(self._root, self._tail + [name])

    def _scandir_names(self):
        try:
            return os.listdir(self._str)
        except OSError:
            return []

    def glob(self, pattern, *, case_sensitive=None, recurse_symlinks=False):
        if not isinstance(pattern, str):
            pattern = _fspath(pattern)
        if not pattern:
            raise ValueError("Unacceptable pattern: %r" % (type(self)(pattern),))
        if pattern.startswith("/"):
            raise NotImplementedError("Non-relative patterns are unsupported")
        dirs_only = pattern.endswith("/")
        parts = [p for p in pattern.split("/") if p and p != "."]
        return iter(list(self._glob_parts(parts, dirs_only, case_sensitive, recurse_symlinks)))

    def rglob(self, pattern, *, case_sensitive=None, recurse_symlinks=False):
        if not isinstance(pattern, str):
            pattern = _fspath(pattern)
        if pattern.startswith("/"):
            raise NotImplementedError("Non-relative patterns are unsupported")
        dirs_only = pattern.endswith("/")
        parts = ["**"] + [p for p in pattern.split("/") if p and p != "."]
        return iter(list(self._glob_parts(parts, dirs_only, case_sensitive, recurse_symlinks)))

    def _glob_parts(self, parts, dirs_only, case_sensitive=None, recurse_symlinks=False):
        if not parts:
            if dirs_only and not self.is_dir():
                return
            yield self
            return
        part = parts[0]
        rest = parts[1:]
        if part == "**":
            for d in self._walk_all(rest, dirs_only, case_sensitive, recurse_symlinks):
                yield d
            return
        # A literal component also has to go through the matcher when the
        # comparison is case-insensitive — `glob("ITEM.TXT", case_sensitive=
        # False)` must find `item.txt`, which an exact child lookup cannot.
        if _has_magic(part) or case_sensitive is False:
            for name in self._scandir_names():
                if _fnmatch(name, part, case_sensitive):
                    child = self._make_child(name)
                    if rest and not child.is_dir():
                        continue
                    for r in child._glob_parts(rest, dirs_only, case_sensitive, recurse_symlinks):
                        yield r
        else:
            child = self._make_child(part)
            if rest:
                if child.is_dir():
                    for r in child._glob_parts(rest, dirs_only, case_sensitive, recurse_symlinks):
                        yield r
            elif os.path.lexists(child._str):
                if dirs_only and not child.is_dir():
                    return
                yield child

    def _walk_all(self, rest, dirs_only, case_sensitive=None, recurse_symlinks=False):
        # `**`: this directory and every directory below it, each continuing
        # with the rest of the pattern; a trailing `**` also yields files.
        # A directory symlink is only descended into when the caller asks.
        if not rest:
            if self.is_dir():
                yield self
            for name in self._scandir_names():
                child = self._make_child(name)
                if child.is_dir() and (recurse_symlinks or not child.is_symlink()):
                    for r in child._walk_all(rest, dirs_only, case_sensitive, recurse_symlinks):
                        yield r
                elif not dirs_only:
                    yield child
            return
        for r in self._glob_parts(rest, dirs_only, case_sensitive, recurse_symlinks):
            yield r
        for name in self._scandir_names():
            child = self._make_child(name)
            if child.is_dir() and (recurse_symlinks or not child.is_symlink()):
                for r in child._walk_all(rest, dirs_only, case_sensitive, recurse_symlinks):
                    yield r

    def walk(self, top_down=True, on_error=None, follow_symlinks=False):
        for dirpath, dirnames, filenames in os.walk(self._str, topdown=top_down, onerror=on_error, followlinks=follow_symlinks):
            yield type(self)(dirpath), dirnames, filenames

    def absolute(self):
        if self.is_absolute():
            return self
        return type(self)(os.getcwd(), self)

    def resolve(self, strict=False):
        if strict and not self.exists():
            raise FileNotFoundError(2, "No such file or directory", self._str)
        return type(self)(os.path.realpath(self._str))

    def expanduser(self):
        if self._tail and self._tail[0].startswith("~"):
            return type(self)(os.path.expanduser(self._str))
        return self

    def touch(self, mode=0o666, exist_ok=True):
        if not exist_ok and self.exists():
            raise FileExistsError(17, "File exists", self._str)
        _fs_touch(self._str)

    def mkdir(self, mode=0o777, parents=False, exist_ok=False):
        try:
            os.mkdir(self._str, mode)
        except FileNotFoundError:
            if not parents or self.parent == self:
                raise
            self.parent.mkdir(parents=True, exist_ok=True)
            self.mkdir(mode, parents=False, exist_ok=exist_ok)
        except OSError:
            if not exist_ok or not self.is_dir():
                raise

    def chmod(self, mode, *, follow_symlinks=True):
        os.chmod(self._str, mode)

    def lchmod(self, mode):
        os.chmod(self._str, mode)

    def unlink(self, missing_ok=False):
        try:
            os.unlink(self._str)
        except FileNotFoundError:
            if not missing_ok:
                raise

    def rmdir(self):
        os.rmdir(self._str)

    def rename(self, target):
        os.rename(self._str, target if isinstance(target, str) else _fspath(target))
        return type(self)(target)

    def replace(self, target):
        os.replace(self._str, target if isinstance(target, str) else _fspath(target))
        return type(self)(target)

    def symlink_to(self, target, target_is_directory=False):
        os.symlink(target if isinstance(target, str) else _fspath(target), self._str)

    def readlink(self):
        return type(self)(os.readlink(self._str))

    def owner(self, *, follow_symlinks=True):
        return os.getlogin()

    def group(self, *, follow_symlinks=True):
        return os.getlogin()


class PosixPath(Path):
    pass


Path = PosixPath
