# `glob` — CPython's glob over `os.listdir`; wildcard matching shares the
# pathlib fnmatch.


def has_magic(s):
    return "*" in s or "?" in s or "[" in s


def escape(pathname):
    out = []
    for c in pathname:
        if c in "*?[":
            out.append("[" + c + "]")
        else:
            out.append(c)
    return "".join(out)


def _ishidden(name):
    return name.startswith(".")


def _listdir(dirname, root_dir):
    if not dirname:
        dirname = root_dir if root_dir is not None else "."
    elif root_dir is not None and not dirname.startswith("/"):
        dirname = os.path.join(root_dir, dirname)
    try:
        return os.listdir(dirname)
    except OSError:
        return []


def _lexists(pathname, root_dir):
    if root_dir is not None and not pathname.startswith("/"):
        pathname = os.path.join(root_dir, pathname)
    return os.path.lexists(pathname)


def _isdir(pathname, root_dir):
    if not pathname:
        pathname = root_dir if root_dir is not None else "."
    elif root_dir is not None and not pathname.startswith("/"):
        pathname = os.path.join(root_dir, pathname)
    return os.path.isdir(pathname)


def _glob1(dirname, pattern, root_dir, include_hidden):
    names = _listdir(dirname, root_dir)
    if not (include_hidden or _ishidden(pattern)):
        names = [x for x in names if not _ishidden(x)]
    return [x for x in names if _pathlib_fnmatch(x, pattern)]


def _glob0(dirname, basename, root_dir):
    if not basename:
        if _isdir(dirname, root_dir):
            return [basename]
    elif _lexists(os.path.join(dirname, basename), root_dir):
        return [basename]
    return []


def _rlistdir(dirname, root_dir, include_hidden):
    names = _listdir(dirname, root_dir)
    for x in names:
        if include_hidden or not _ishidden(x):
            yield x
            path = os.path.join(dirname, x) if dirname else x
            if _isdir(path, root_dir) and not os.path.islink(path if root_dir is None else os.path.join(root_dir, path)):
                for y in _rlistdir(path, root_dir, include_hidden):
                    yield os.path.join(x, y)


def _glob2(dirname, pattern, root_dir, include_hidden):
    if _isdir(dirname, root_dir):
        yield pattern[:0]
    for x in _rlistdir(dirname, root_dir, include_hidden):
        yield x


def _iglob(pathname, root_dir, recursive, include_hidden):
    dirname, basename = os.path.split(pathname)
    if not has_magic(pathname):
        if basename:
            if _lexists(pathname, root_dir):
                yield pathname
        else:
            if _isdir(dirname, root_dir):
                yield pathname
        return
    if not dirname:
        if recursive and basename == "**":
            for x in _glob2(dirname, basename, root_dir, include_hidden):
                yield x
        else:
            for x in _glob1(dirname, basename, root_dir, include_hidden):
                yield x
        return
    if dirname != pathname and has_magic(dirname):
        dirs = _iglob(dirname, root_dir, recursive, include_hidden)
    else:
        dirs = [dirname]
    if has_magic(basename):
        if recursive and basename == "**":
            glob_in_dir = _glob2
        else:
            glob_in_dir = _glob1
    else:
        glob_in_dir = None
    for dirname in dirs:
        if glob_in_dir is None:
            names = _glob0(dirname, basename, root_dir)
        else:
            names = glob_in_dir(dirname, basename, root_dir, include_hidden)
        for name in names:
            yield os.path.join(dirname, name)


def iglob(pathname, *, root_dir=None, dir_fd=None, recursive=False, include_hidden=False):
    pathname = _fspath(pathname)
    if root_dir is not None:
        root_dir = _fspath(root_dir)
    return iter(list(_iglob(pathname, root_dir, recursive, include_hidden)))


def glob(pathname, *, root_dir=None, dir_fd=None, recursive=False, include_hidden=False):
    return list(iglob(pathname, root_dir=root_dir, dir_fd=dir_fd, recursive=recursive, include_hidden=include_hidden))
