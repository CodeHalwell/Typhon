# `os` — the process / filesystem surface over the `_fs_*` natives, with
# `path` (posixpath) and `environ` seeded in by the VM.

name = "posix"
sep = "/"
altsep = None
extsep = "."
pathsep = ":"
curdir = "."
pardir = ".."
linesep = "\n"
devnull = "/dev/null"
defpath = "/bin:/usr/bin"
SEEK_SET = 0
SEEK_CUR = 1
SEEK_END = 2
F_OK = 0
X_OK = 1
W_OK = 2
R_OK = 4
O_RDONLY = 0
O_WRONLY = 1
O_RDWR = 2
O_CREAT = 64
O_EXCL = 128
O_TRUNC = 512
O_APPEND = 1024
error = OSError


class PathLike:
    def __fspath__(self):
        raise NotImplementedError


def fspath(path):
    return _fspath(path)


def fsencode(filename):
    filename = _fspath(filename)
    if isinstance(filename, str):
        return filename.encode("utf-8")
    return filename


def fsdecode(filename):
    filename = _fspath(filename)
    if isinstance(filename, bytes):
        return filename.decode("utf-8")
    return filename


class stat_result:
    __typhon_builtin_bases__ = ("tuple",)
    n_fields = 19
    n_sequence_fields = 10
    n_unnamed_fields = 3

    def __init__(self, raw):
        self._raw = raw

    @property
    def st_mode(self):
        return self._raw[0]

    @property
    def st_ino(self):
        return self._raw[1]

    @property
    def st_dev(self):
        return self._raw[2]

    @property
    def st_nlink(self):
        return self._raw[3]

    @property
    def st_uid(self):
        return self._raw[4]

    @property
    def st_gid(self):
        return self._raw[5]

    @property
    def st_size(self):
        return self._raw[6]

    @property
    def st_atime(self):
        return self._raw[10]

    @property
    def st_mtime(self):
        return self._raw[11]

    @property
    def st_ctime(self):
        return self._raw[12]

    @property
    def st_atime_ns(self):
        return self._raw[13]

    @property
    def st_mtime_ns(self):
        return self._raw[14]

    @property
    def st_ctime_ns(self):
        return self._raw[15]

    @property
    def st_blksize(self):
        return self._raw[16]

    @property
    def st_blocks(self):
        return self._raw[17]

    def __getitem__(self, i):
        return self._raw[:10][i]

    def __len__(self):
        return 10

    def __iter__(self):
        return iter(self._raw[:10])

    def __eq__(self, other):
        return isinstance(other, stat_result) and self._raw[:10] == other._raw[:10]

    def __hash__(self):
        return hash(self._raw[:10])

    def __repr__(self):
        r = self._raw
        return ("os.stat_result(st_mode=%d, st_ino=%d, st_dev=%d, st_nlink=%d, st_uid=%d, st_gid=%d, "
                "st_size=%d, st_atime=%d, st_mtime=%d, st_ctime=%d)") % (
            r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8], r[9])


def stat(path, *, dir_fd=None, follow_symlinks=True):
    return stat_result(_fs_stat(path, follow_symlinks))


def lstat(path, *, dir_fd=None):
    return stat_result(_fs_stat(path, False))


def getcwd():
    return _fs_getcwd()


def getcwdb():
    return _fs_getcwd().encode("utf-8")


def chdir(path):
    _fs_chdir(path)


def listdir(path="."):
    return _fs_listdir(path)


class DirEntry:
    def __init__(self, dirpath, name):
        self.name = name
        self.path = name if dirpath == "." else path.join(dirpath, name)

    def __fspath__(self):
        return self.path

    def __repr__(self):
        return "<DirEntry %r>" % (self.name,)

    def stat(self, *, follow_symlinks=True):
        return stat(self.path, follow_symlinks=follow_symlinks)

    def inode(self):
        return _fs_stat(self.path, False)[1]

    def is_dir(self, *, follow_symlinks=True):
        try:
            st = _fs_stat(self.path, follow_symlinks)
        except OSError:
            return False
        return (st[0] & 0o170000) == 0o040000

    def is_file(self, *, follow_symlinks=True):
        try:
            st = _fs_stat(self.path, follow_symlinks)
        except OSError:
            return False
        return (st[0] & 0o170000) == 0o100000

    def is_symlink(self):
        try:
            st = _fs_stat(self.path, False)
        except OSError:
            return False
        return (st[0] & 0o170000) == 0o120000

    def is_junction(self):
        return False


class _ScandirIterator:
    def __init__(self, entries):
        self._entries = entries
        self._i = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self._i >= len(self._entries):
            raise StopIteration
        e = self._entries[self._i]
        self._i += 1
        return e

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def close(self):
        pass


def scandir(path="."):
    p = _fspath(path)
    return _ScandirIterator([DirEntry(p, name) for name in _fs_listdir(p)])


def mkdir(path, mode=0o777, *, dir_fd=None):
    _fs_mkdir(path)


def makedirs(name, mode=0o777, exist_ok=False):
    name = _fspath(name)
    head, tail = path.split(name)
    if not tail:
        head, tail = path.split(head)
    if head and tail and not path.exists(head):
        try:
            makedirs(head, exist_ok=exist_ok)
        except FileExistsError:
            pass
        if tail == curdir:
            return
    try:
        _fs_mkdir(name)
    except OSError:
        if not exist_ok or not path.isdir(name):
            raise


def rmdir(path, *, dir_fd=None):
    _fs_rmdir(path)


def remove(path, *, dir_fd=None):
    _fs_unlink(path)


def unlink(path, *, dir_fd=None):
    _fs_unlink(path)


def rename(src, dst, *, src_dir_fd=None, dst_dir_fd=None):
    _fs_rename(src, dst)


def replace(src, dst, *, src_dir_fd=None, dst_dir_fd=None):
    _fs_rename(src, dst)


def renames(old, new):
    head, tail = path.split(new)
    if head and tail and not path.exists(head):
        makedirs(head)
    rename(old, new)
    head, tail = path.split(old)
    if head and tail:
        try:
            removedirs(head)
        except OSError:
            pass


def removedirs(name):
    rmdir(name)
    head, tail = path.split(name)
    if not tail:
        head, tail = path.split(head)
    while head and tail:
        try:
            rmdir(head)
        except OSError:
            break
        head, tail = path.split(head)


def walk(top, topdown=True, onerror=None, followlinks=False):
    top = _fspath(top)
    stack = [top]
    while stack:
        top = stack.pop()
        if isinstance(top, tuple):
            yield top
            continue
        dirs = []
        nondirs = []
        walk_dirs = []
        try:
            entries = _fs_listdir(top)
        except OSError as error:
            if onerror is not None:
                onerror(error)
            continue
        for entry_name in entries:
            full = path.join(top, entry_name)
            is_dir = path.isdir(full)
            if is_dir:
                dirs.append(entry_name)
            else:
                nondirs.append(entry_name)
            if not topdown and is_dir:
                if followlinks or not path.islink(full):
                    walk_dirs.append(full)
        if topdown:
            yield top, dirs, nondirs
            for dirname in reversed(dirs):
                new_path = path.join(top, dirname)
                if followlinks or not path.islink(new_path):
                    stack.append(new_path)
        else:
            stack.append((top, dirs, nondirs))
            for new_path in reversed(walk_dirs):
                stack.append(new_path)


def getenv(key, default=None):
    return environ.get(key, default)


def putenv(key, value):
    environ[key] = value


def unsetenv(key):
    if key in environ:
        del environ[key]


def getpid():
    return _fs_getpid()


def getppid():
    return _fs_getppid()


def cpu_count():
    return _fs_cpu_count()


def process_cpu_count():
    return _fs_cpu_count()


def system(command):
    return _fs_system(command)


def urandom(n):
    return _fs_urandom(n)


def access(path, mode, *, dir_fd=None, effective_ids=False, follow_symlinks=True):
    # `access(2)`: existence alone answers `F_OK`; the rest is the
    # owner/group/other triad of the mode bits against the calling user.
    # Answering `True` for any existing path made
    # `os.access(f, os.X_OK)` pick a non-executable file.
    try:
        st = _fs_stat(path, follow_symlinks)
    except OSError:
        return False
    if mode == F_OK:
        return True
    st_mode, st_uid, st_gid = st[0], st[4], st[5]
    if _fs_getuid() == 0:
        # root bypasses read and write checks; execute still needs a bit set
        # somewhere, as the kernel requires.
        return mode & X_OK == 0 or st_mode & 0o111 != 0
    if _fs_getuid() == st_uid:
        bits = (st_mode >> 6) & 7
    elif _fs_getgid() == st_gid:
        bits = (st_mode >> 3) & 7
    else:
        bits = st_mode & 7
    want = 0
    if mode & R_OK:
        want = want | 4
    if mode & W_OK:
        want = want | 2
    if mode & X_OK:
        want = want | 1
    return bits & want == want


def chmod(path, mode, *, dir_fd=None, follow_symlinks=True):
    _fs_chmod(path, mode)


def utime(path, times=None, *, ns=None, dir_fd=None, follow_symlinks=True):
    _fs_touch(path)


def symlink(src, dst, target_is_directory=False, *, dir_fd=None):
    _fs_symlink(src, dst)


def readlink(path, *, dir_fd=None):
    return _fs_readlink(path)


def truncate(path, length):
    # Growing a file pads it with NULs; slicing alone left it unchanged.
    data = _fs_read(path)
    if length > len(data):
        data = data + b"\x00" * (length - len(data))
    else:
        data = data[:length]
    _fs_write(path, data, "w")


def strerror(code):
    return _fs_strerror(code)


_fd_paths = {}
_next_fd = [3]


def _register_fd(p):
    fd = _next_fd[0]
    _next_fd[0] += 1
    _fd_paths[fd] = p
    return fd


def close(fd):
    _fd_paths.pop(fd, None)


def fdopen(fd, mode="r", buffering=-1, encoding=None, errors=None, newline=None):
    p = _fd_paths.get(fd)
    if p is None:
        raise OSError(9, "Bad file descriptor")
    return open(p, mode, buffering, encoding, errors, newline)


def get_terminal_size(fd=1):
    return terminal_size((80, 24))


class terminal_size:
    __typhon_builtin_bases__ = ("tuple",)

    def __init__(self, pair):
        self.columns = pair[0]
        self.lines = pair[1]

    def __getitem__(self, i):
        return (self.columns, self.lines)[i]

    def __len__(self):
        return 2

    def __iter__(self):
        return iter((self.columns, self.lines))

    def __repr__(self):
        return "os.terminal_size(columns=%d, lines=%d)" % (self.columns, self.lines)


def getuid():
    return _fs_getuid()


def getgid():
    return _fs_getgid()


def getlogin():
    return environ.get("USER", environ.get("LOGNAME", "root"))


def umask(mask):
    return 0o022


def isatty(fd):
    return False


def kill(pid, sig):
    raise PermissionError(1, "Operation not permitted")


def _exit(code=0):
    raise SystemExit(code)


def abort():
    raise SystemExit(134)
