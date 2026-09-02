# `shutil` — file operations over `os` / `os.path` and the `_fs_copyfile`
# native.


class Error(OSError):
    pass


class SameFileError(Error):
    pass


class SpecialFileError(OSError):
    pass


class ExecError(OSError):
    pass


def _samefile(src, dst):
    try:
        return os.path.samefile(src, dst)
    except OSError:
        return False


def copyfileobj(fsrc, fdst, length=0):
    while True:
        buf = fsrc.read(length if length > 0 else 65536)
        if not buf:
            break
        fdst.write(buf)


def copyfile(src, dst, *, follow_symlinks=True):
    if _samefile(src, dst):
        raise SameFileError("%r and %r are the same file" % (str(src), str(dst)))
    _fs_copyfile(src, dst)
    return dst


def copymode(src, dst, *, follow_symlinks=True):
    st = os.stat(src)
    os.chmod(dst, st.st_mode & 0o7777)


def copystat(src, dst, *, follow_symlinks=True):
    copymode(src, dst)


def copy(src, dst, *, follow_symlinks=True):
    if os.path.isdir(dst):
        dst = os.path.join(dst, os.path.basename(src))
    copyfile(src, dst, follow_symlinks=follow_symlinks)
    copymode(src, dst, follow_symlinks=follow_symlinks)
    return dst


def copy2(src, dst, *, follow_symlinks=True):
    if os.path.isdir(dst):
        dst = os.path.join(dst, os.path.basename(src))
    copyfile(src, dst, follow_symlinks=follow_symlinks)
    copystat(src, dst, follow_symlinks=follow_symlinks)
    return dst


def ignore_patterns(*patterns):
    def _ignore_patterns(path, names):
        ignored = []
        for pattern in patterns:
            for name in names:
                if _fnmatch(name, pattern):
                    ignored.append(name)
        return set(ignored)
    return _ignore_patterns


def _fnmatch(name, pat):
    return _pathlib_fnmatch(name, pat)


def copytree(src, dst, symlinks=False, ignore=None, copy_function=copy2, ignore_dangling_symlinks=False, dirs_exist_ok=False):
    src = _fspath(src)
    dst = _fspath(dst)
    names = os.listdir(src)
    if ignore is not None:
        ignored_names = ignore(src, names)
    else:
        ignored_names = set()
    os.makedirs(dst, exist_ok=dirs_exist_ok)
    errors = []
    for name in names:
        if name in ignored_names:
            continue
        srcname = os.path.join(src, name)
        dstname = os.path.join(dst, name)
        try:
            if os.path.isdir(srcname):
                copytree(srcname, dstname, symlinks, ignore, copy_function, ignore_dangling_symlinks, dirs_exist_ok)
            else:
                copy_function(srcname, dstname)
        except Error as err:
            errors.extend(err.args[0])
        except OSError as why:
            errors.append((srcname, dstname, str(why)))
    if errors:
        raise Error(errors)
    return dst


def rmtree(path, ignore_errors=False, onerror=None, *, onexc=None, dir_fd=None):
    def handle(func, p, exc):
        if ignore_errors:
            return
        if onexc is not None:
            onexc(func, p, exc)
            return
        if onerror is not None:
            onerror(func, p, (type(exc), exc, None))
            return
        raise exc
    try:
        st = os.lstat(path)
    except OSError as e:
        # CPython's `rmtree` re-points the error at the argument it was
        # given, so a `Path` reports as `PosixPath('x')` where the `os`
        # call underneath reports the plain `'x'`.
        handle(os.lstat, path, OSError(e.errno, e.strerror, path))
        return
    if (st.st_mode & 0o170000) != 0o040000:
        try:
            raise NotADirectoryError(20, "Not a directory", _fspath(path))
        except OSError as e:
            handle(os.rmdir, path, e)
        return
    _rmtree_inner(_fspath(path), handle)


def _rmtree_inner(path, handle):
    try:
        names = os.listdir(path)
    except OSError as e:
        handle(os.listdir, path, e)
        return
    for name in names:
        full = os.path.join(path, name)
        try:
            st = os.lstat(full)
        except OSError as e:
            handle(os.lstat, full, e)
            continue
        if (st.st_mode & 0o170000) == 0o040000:
            _rmtree_inner(full, handle)
        else:
            try:
                os.unlink(full)
            except OSError as e:
                handle(os.unlink, full, e)
    try:
        os.rmdir(path)
    except OSError as e:
        handle(os.rmdir, path, e)


def _basename(path):
    path = _fspath(path)
    return os.path.basename(path.rstrip("/"))


def move(src, dst, copy_function=copy2):
    real_dst = dst
    if os.path.isdir(dst):
        if _samefile(src, dst):
            os.rename(src, dst)
            return dst
        real_dst = os.path.join(dst, _basename(src))
        if os.path.exists(real_dst):
            raise Error("Destination path '%s' already exists" % real_dst)
    try:
        os.rename(src, real_dst)
    except OSError:
        if os.path.isdir(src):
            copytree(src, real_dst, symlinks=True)
            rmtree(src)
        else:
            copy_function(src, real_dst)
            os.unlink(src)
    return real_dst


def which(cmd, mode=os.X_OK, path=None):
    cmd = _fspath(cmd)
    if "/" in cmd:
        return cmd if os.path.isfile(cmd) else None
    if path is None:
        path = os.environ.get("PATH", os.defpath)
    for d in path.split(os.pathsep):
        if not d:
            d = "."
        candidate = os.path.join(d, cmd)
        if os.path.isfile(candidate):
            st = os.stat(candidate)
            if st.st_mode & 0o111:
                return candidate
    return None


class _ntuple_diskusage:
    __typhon_builtin_bases__ = ("tuple",)

    def __init__(self, total, used, free):
        self.total = total
        self.used = used
        self.free = free

    def __getitem__(self, i):
        return (self.total, self.used, self.free)[i]

    def __len__(self):
        return 3

    def __iter__(self):
        return iter((self.total, self.used, self.free))

    def __repr__(self):
        return "usage(total=%d, used=%d, free=%d)" % (self.total, self.used, self.free)


def disk_usage(path):
    total, free = _fs_disk_usage(path)
    return _ntuple_diskusage(total, total - free, free)


def get_terminal_size(fallback=(80, 24)):
    return os.terminal_size(fallback)


def chown(path, user=None, group=None):
    pass
