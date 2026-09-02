# `os.path` (posixpath) — CPython's pure-Python path functions, plus the
# filesystem predicates over the `_fs_*` natives.

curdir = "."
pardir = ".."
extsep = "."
sep = "/"
pathsep = ":"
defpath = "/bin:/usr/bin"
altsep = None
devnull = "/dev/null"


def _check(s):
    if isinstance(s, str):
        return s
    return _fspath(s)


def normcase(s):
    return _check(s)


def isabs(s):
    return _check(s).startswith("/")


def join(a, *p):
    a = _check(a)
    path = a
    for b in p:
        b = _check(b)
        if b.startswith("/"):
            path = b
        elif not path or path.endswith("/"):
            path += b
        else:
            path += "/" + b
    return path


def split(p):
    p = _check(p)
    i = p.rfind("/") + 1
    head, tail = p[:i], p[i:]
    if head and head != "/" * len(head):
        head = head.rstrip("/")
    return head, tail


def splitext(p):
    p = _check(p)
    sep_index = p.rfind("/")
    dot_index = p.rfind(".")
    if dot_index > sep_index:
        filename_index = sep_index + 1
        while filename_index < dot_index:
            if p[filename_index:filename_index + 1] != ".":
                return p[:dot_index], p[dot_index:]
            filename_index += 1
    return p, p[:0]


def splitdrive(p):
    p = _check(p)
    return p[:0], p


def splitroot(p):
    p = _check(p)
    if p[:1] != "/":
        return "", "", p
    elif p[1:2] != "/" or p[2:3] == "/":
        return "", "/", p[1:]
    else:
        return "", "//", p[2:]


def basename(p):
    p = _check(p)
    i = p.rfind("/") + 1
    return p[i:]


def dirname(p):
    p = _check(p)
    i = p.rfind("/") + 1
    head = p[:i]
    if head and head != "/" * len(head):
        head = head.rstrip("/")
    return head


def normpath(path):
    path = _check(path)
    if path == "":
        return "."
    initial_slashes = 1 if path.startswith("/") else 0
    if initial_slashes and path.startswith("//") and not path.startswith("///"):
        initial_slashes = 2
    comps = path.split("/")
    new_comps = []
    for comp in comps:
        if comp in ("", "."):
            continue
        if comp != ".." or (not initial_slashes and not new_comps) or (new_comps and new_comps[-1] == ".."):
            new_comps.append(comp)
        elif new_comps:
            new_comps.pop()
    path = "/".join(new_comps)
    if initial_slashes:
        path = "/" * initial_slashes + path
    return path or "."


def abspath(path):
    path = _check(path)
    if not path.startswith("/"):
        path = join(_fs_getcwd(), path)
    return normpath(path)


def realpath(filename, *, strict=False):
    filename = _check(filename)
    if strict and not lexists(filename):
        raise FileNotFoundError(2, "No such file or directory", filename)
    return _fs_realpath(filename)


def commonprefix(m):
    if not m:
        return ""
    if not isinstance(m[0], (list, tuple)):
        m = tuple(_check(x) for x in m)
    s1 = min(m)
    s2 = max(m)
    for i, c in enumerate(s1):
        if c != s2[i]:
            return s1[:i]
    return s1


def relpath(path, start=None):
    path = _check(path)
    if not path:
        raise ValueError("no path specified")
    if start is None:
        start = "."
    else:
        start = _check(start)
    start_list = [x for x in abspath(start).split("/") if x]
    path_list = [x for x in abspath(path).split("/") if x]
    i = len(commonprefix([start_list, path_list]))
    rel_list = [".."] * (len(start_list) - i) + path_list[i:]
    if not rel_list:
        return "."
    return join(*rel_list)


def commonpath(paths):
    paths = tuple(_check(p) for p in paths)
    if not paths:
        raise ValueError("commonpath() arg is an empty sequence")
    split_paths = [path.split("/") for path in paths]
    kinds = set(p[:1] == "/" for p in paths)
    if len(kinds) != 1:
        raise ValueError("Can't mix absolute and relative paths")
    is_abs = paths[0][:1] == "/"
    split_paths = [[c for c in s if c and c != "."] for s in split_paths]
    s1 = min(split_paths)
    s2 = max(split_paths)
    common = s1
    for i, c in enumerate(s1):
        if c != s2[i]:
            common = s1[:i]
            break
    prefix = "/" if is_abs else ""
    return prefix + "/".join(common)


def expanduser(path):
    path = _check(path)
    if not path.startswith("~"):
        return path
    i = path.find("/", 1)
    if i < 0:
        i = len(path)
    if i == 1:
        userhome = _fs_home()
    else:
        # `~user` needs the passwd database, which the VM does not model.
        return path
    userhome = userhome.rstrip("/")
    return (userhome + path[i:]) or "/"


def expandvars(path):
    path = _check(path)
    if "$" not in path:
        return path
    out = []
    i = 0
    n = len(path)
    while i < n:
        c = path[i]
        if c != "$":
            out.append(c)
            i += 1
            continue
        if i + 1 < n and path[i + 1] == "{":
            j = path.find("}", i + 2)
            if j < 0:
                out.append(path[i:])
                break
            name = path[i + 2:j]
            value = _environ.get(name)
            out.append(path[i:j + 1] if value is None else value)
            i = j + 1
            continue
        j = i + 1
        while j < n and (path[j].isalnum() or path[j] == "_"):
            j += 1
        name = path[i + 1:j]
        if not name:
            out.append("$")
            i += 1
            continue
        value = _environ.get(name)
        out.append(path[i:j] if value is None else value)
        i = j
    return "".join(out)


def exists(path):
    try:
        _fs_stat(_check(path), True)
    except (OSError, ValueError):
        return False
    return True


def lexists(path):
    try:
        _fs_stat(_check(path), False)
    except (OSError, ValueError):
        return False
    return True


def isfile(path):
    try:
        st = _fs_stat(_check(path), True)
    except (OSError, ValueError):
        return False
    return (st[0] & 0o170000) == 0o100000


def isdir(path):
    try:
        st = _fs_stat(_check(path), True)
    except (OSError, ValueError):
        return False
    return (st[0] & 0o170000) == 0o040000


def islink(path):
    try:
        st = _fs_stat(_check(path), False)
    except (OSError, ValueError):
        return False
    return (st[0] & 0o170000) == 0o120000


def ismount(path):
    return _check(path) == "/"


def isjunction(path):
    return False


def isdevdrive(path):
    return False


def getsize(filename):
    return _fs_stat(_check(filename), True)[6]


def getmtime(filename):
    return _fs_stat(_check(filename), True)[11]


def getatime(filename):
    return _fs_stat(_check(filename), True)[10]


def getctime(filename):
    return _fs_stat(_check(filename), True)[12]


def samefile(f1, f2):
    return _fs_samefile(_check(f1), _check(f2))


def samestat(s1, s2):
    return s1[1] == s2[1] and s1[2] == s2[2]
