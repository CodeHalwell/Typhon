# `hashlib` — the digests the VM models (md5, sha1, sha256, sha512), computed
# natively by `_hash_digest`; sizes by `_hash_sizes`. Hash objects buffer
# their input and digest on demand.

algorithms_guaranteed = {"md5", "sha1", "sha256", "sha512"}
algorithms_available = {"md5", "sha1", "sha256", "sha512"}


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
