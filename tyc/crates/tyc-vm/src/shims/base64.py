# `base64` — the RFC 4648 encodings over `bytes`.

_B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
_B64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
_B32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
_B16 = "0123456789ABCDEF"


def _as_bytes(s):
    if isinstance(s, str):
        return s.encode("ascii")
    return bytes(s)


def _encode(data, alphabet, pad):
    data = _as_bytes(data)
    out = []
    i = 0
    n = len(data)
    while i + 2 < n:
        block = (data[i] << 16) | (data[i + 1] << 8) | data[i + 2]
        out.append(alphabet[(block >> 18) & 63])
        out.append(alphabet[(block >> 12) & 63])
        out.append(alphabet[(block >> 6) & 63])
        out.append(alphabet[block & 63])
        i += 3
    rest = n - i
    if rest == 1:
        block = data[i] << 16
        out.append(alphabet[(block >> 18) & 63])
        out.append(alphabet[(block >> 12) & 63])
        if pad:
            out.append("=")
            out.append("=")
    elif rest == 2:
        block = (data[i] << 16) | (data[i + 1] << 8)
        out.append(alphabet[(block >> 18) & 63])
        out.append(alphabet[(block >> 12) & 63])
        out.append(alphabet[(block >> 6) & 63])
        if pad:
            out.append("=")
    return "".join(out).encode("ascii")


def _decode(data, alphabet):
    if isinstance(data, bytes):
        data = data.decode("ascii")
    data = data.replace("\n", "").replace("\r", "")
    stripped = data.rstrip("=")
    if (len(data) % 4) != 0:
        raise ValueError("Invalid base64-encoded string")
    index = {}
    i = 0
    for ch in alphabet:
        index[ch] = i
        i += 1
    bits = 0
    nbits = 0
    out = []
    for ch in stripped:
        if ch not in index:
            raise ValueError("Invalid base64-encoded string")
        bits = (bits << 6) | index[ch]
        nbits += 6
        if nbits >= 8:
            nbits -= 8
            out.append((bits >> nbits) & 255)
    return bytes(out)


def b64encode(s, altchars=None):
    encoded = _encode(s, _B64, True)
    if altchars is not None:
        altchars = _as_bytes(altchars)
        encoded = encoded.replace(b"+", altchars[0:1]).replace(b"/", altchars[1:2])
    return encoded


def b64decode(s, altchars=None, validate=False):
    if altchars is not None:
        altchars = _as_bytes(altchars)
        if isinstance(s, bytes):
            s = s.replace(altchars[0:1], b"+").replace(altchars[1:2], b"/")
        else:
            s = s.replace(chr(altchars[0]), "+").replace(chr(altchars[1]), "/")
    return _decode(s, _B64)


def urlsafe_b64encode(s):
    return _encode(s, _B64URL, True)


def urlsafe_b64decode(s):
    return _decode(s, _B64URL)


def standard_b64encode(s):
    return b64encode(s)


def standard_b64decode(s):
    return b64decode(s)


def b32encode(s):
    data = _as_bytes(s)
    out = []
    i = 0
    n = len(data)
    while i < n:
        chunk = data[i:i + 5]
        i += 5
        bits = 0
        for byte in chunk:
            bits = (bits << 8) | byte
        pad_bytes = 5 - len(chunk)
        bits = bits << (8 * pad_bytes)
        chars = []
        j = 0
        while j < 8:
            chars.append(_B32[(bits >> (35 - 5 * j)) & 31])
            j += 1
        used = [2, 4, 5, 7, 8][len(chunk) - 1]
        out.append("".join(chars[:used]))
        out.append("=" * (8 - used))
    return "".join(out).encode("ascii")


def b32decode(s, casefold=False):
    if isinstance(s, bytes):
        s = s.decode("ascii")
    if casefold:
        s = s.upper()
    stripped = s.rstrip("=")
    index = {}
    i = 0
    for ch in _B32:
        index[ch] = i
        i += 1
    bits = 0
    nbits = 0
    out = []
    for ch in stripped:
        if ch not in index:
            raise ValueError("Non-base32 digit found")
        bits = (bits << 5) | index[ch]
        nbits += 5
        if nbits >= 8:
            nbits -= 8
            out.append((bits >> nbits) & 255)
    return bytes(out)


def b16encode(s):
    data = _as_bytes(s)
    out = []
    for byte in data:
        out.append(_B16[(byte >> 4) & 15])
        out.append(_B16[byte & 15])
    return "".join(out).encode("ascii")


def b16decode(s, casefold=False):
    if isinstance(s, bytes):
        s = s.decode("ascii")
    if casefold:
        s = s.upper()
    if (len(s) % 2) != 0:
        raise ValueError("Odd-length string")
    out = []
    i = 0
    while i < len(s):
        hi = _B16.find(s[i])
        lo = _B16.find(s[i + 1])
        if hi < 0 or lo < 0:
            raise ValueError("Non-base16 digit found")
        out.append((hi << 4) | lo)
        i += 2
    return bytes(out)


def encodebytes(s):
    data = _as_bytes(s)
    out = []
    i = 0
    while i < len(data):
        out.append(b64encode(data[i:i + 57]))
        out.append(b"\n")
        i += 57
    return b"".join(out)


def decodebytes(s):
    return b64decode(s)
