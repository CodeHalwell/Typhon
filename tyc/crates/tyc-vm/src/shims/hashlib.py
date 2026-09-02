# `hashlib` — the digests the VM models (md5, sha1, the SHA-2 family and the
# unkeyed BLAKE2 pair), computed natively by `_hash_digest`; sizes by
# `_hash_sizes`. Hash objects buffer their input and digest on demand.

_ALGORITHMS = {"md5", "sha1", "sha224", "sha256", "sha384", "sha512",
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
