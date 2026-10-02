# `hashlib` — the digests the VM models (md5, sha1, the SHA-2 family, the
# SHA-3 family and the unkeyed BLAKE2 pair), computed natively by
# `_hash_digest`; sizes by `_hash_sizes`; `pbkdf2_hmac` by `_hash_pbkdf2`.
# Hash objects buffer their input and digest on demand.

_ALGORITHMS = {"md5", "sha1", "sha224", "sha256", "sha384", "sha512",
               "sha3_224", "sha3_256", "sha3_384", "sha3_512",
               "blake2b", "blake2s"}
algorithms_guaranteed = _ALGORITHMS
algorithms_available = _ALGORITHMS


class _Hash:
    def __init__(self, name, data=b""):
        self._name = name
        self._data = b""
        self.update(data)

    @property
    def name(self):
        return self._name

    @property
    def digest_size(self):
        return _hash_sizes(self._name)[0]

    @property
    def block_size(self):
        return _hash_sizes(self._name)[1]

    def update(self, data):
        if isinstance(data, str):
            raise TypeError("Strings must be encoded before hashing")
        # `bytearray` supports the buffer protocol, so CPython hashes it
        # exactly as it hashes `bytes`.
        if isinstance(data, bytearray):
            data = bytes(data)
        if not isinstance(data, bytes):
            raise TypeError("object supporting the buffer API required")
        self._data = self._data + data

    def digest(self):
        return _hash_digest(self._name, self._data)

    def hexdigest(self):
        return self.digest().hex()

    def copy(self):
        h = _Hash(self._name)
        h._data = self._data
        return h

    def __repr__(self):
        return "<%s _hashlib.HASH object @ %s>" % (self._name, hex(id(self)))


def new(name, data=b"", *, usedforsecurity=True):
    if name not in algorithms_available:
        raise ValueError("unsupported hash type " + name)
    return _Hash(name, data)


def md5(data=b"", *, usedforsecurity=True):
    return _Hash("md5", data)


def sha1(data=b"", *, usedforsecurity=True):
    return _Hash("sha1", data)


def sha256(data=b"", *, usedforsecurity=True):
    return _Hash("sha256", data)


def sha512(data=b"", *, usedforsecurity=True):
    return _Hash("sha512", data)


def sha224(data=b"", *, usedforsecurity=True):
    return _Hash("sha224", data)


def sha384(data=b"", *, usedforsecurity=True):
    return _Hash("sha384", data)


def sha3_224(data=b"", *, usedforsecurity=True):
    return _Hash("sha3_224", data)


def sha3_256(data=b"", *, usedforsecurity=True):
    return _Hash("sha3_256", data)


def sha3_384(data=b"", *, usedforsecurity=True):
    return _Hash("sha3_384", data)


def sha3_512(data=b"", *, usedforsecurity=True):
    return _Hash("sha3_512", data)


def _blake2(name, data, digest_size, kwargs):
    # The keyed and personalised forms need the parameter block the VM's
    # compression does not take; say so rather than return a wrong digest.
    for unsupported in ["key", "salt", "person", "fanout", "depth", "leaf_size",
                        "node_offset", "node_depth", "inner_size", "last_node"]:
        if kwargs.get(unsupported):
            raise ValueError("%s is not supported by the Typhon VM's %s — use `tyc run --compile`"
                             % (unsupported, name))
    if digest_size is not None:
        raise ValueError("digest_size is not supported by the Typhon VM's %s — use `tyc run --compile`" % name)
    return _Hash(name, data)


def blake2b(data=b"", *, digest_size=None, **kwargs):
    return _blake2("blake2b", data, digest_size, kwargs)


def blake2s(data=b"", *, digest_size=None, **kwargs):
    return _blake2("blake2s", data, digest_size, kwargs)


def _bytes_arg(value):
    if isinstance(value, bytearray):
        return bytes(value)
    if not isinstance(value, bytes):
        raise TypeError("a bytes-like object is required, not '%s'" % type(value).__name__)
    return value


def pbkdf2_hmac(hash_name, password, salt, iterations, dklen=None):
    """PBKDF2 keyed on HMAC-`hash_name`; `dklen` defaults to the digest size."""
    if not isinstance(hash_name, str):
        raise TypeError("pbkdf2_hmac() argument 'hash_name' must be str, not %s"
                        % type(hash_name).__name__)
    if hash_name not in algorithms_available:
        raise ValueError("unsupported hash type " + hash_name)
    password = _bytes_arg(password)
    salt = _bytes_arg(salt)
    if isinstance(iterations, bool) or not isinstance(iterations, int):
        raise TypeError("'%s' object cannot be interpreted as an integer" % type(iterations).__name__)
    if iterations < 1:
        raise ValueError("iteration value must be greater than 0.")
    if dklen is not None:
        if isinstance(dklen, bool) or not isinstance(dklen, int):
            raise TypeError("'%s' object cannot be interpreted as an integer" % type(dklen).__name__)
        if dklen < 1:
            raise ValueError("key length must be greater than 0.")
    return _hash_pbkdf2(hash_name, password, salt, iterations, dklen)


def file_digest(fileobj, digest, /, *, _bufsize=2**18):
    """Hash a file opened in binary mode; `digest` is a name or a constructor."""
    if isinstance(digest, str):
        digestobj = new(digest)
    else:
        digestobj = digest()
    getbuffer = getattr(fileobj, "getbuffer", None)
    if getbuffer is not None:
        digestobj.update(bytes(getbuffer()))
        return digestobj
    read = getattr(fileobj, "read", None)
    readable = getattr(fileobj, "readable", None)
    if read is None or readable is None or not readable():
        raise ValueError("'%r' is not a file-like object in binary reading mode." % (fileobj,))
    while True:
        chunk = read(_bufsize)
        if isinstance(chunk, str):
            raise ValueError("'%r' is not a file-like object in binary reading mode." % (fileobj,))
        if not chunk:
            break
        digestobj.update(chunk)
    return digestobj
