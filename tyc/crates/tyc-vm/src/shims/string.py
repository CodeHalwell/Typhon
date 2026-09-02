# `string` — the constant tables plus `capwords` and `Template`.

whitespace = " \t\n\r\v\f"
ascii_lowercase = "abcdefghijklmnopqrstuvwxyz"
ascii_uppercase = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
ascii_letters = ascii_lowercase + ascii_uppercase
digits = "0123456789"
hexdigits = "0123456789abcdefABCDEF"
octdigits = "01234567"
punctuation = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"
printable = digits + ascii_letters + punctuation + whitespace


def capwords(s, sep=None):
    if sep is None:
        return " ".join([w.capitalize() for w in s.split()])
    return sep.join([w.capitalize() for w in s.split(sep)])


class Template:
    delimiter = "$"

    def __init__(self, template):
        self.template = template

    def _lookup(self, name, mapping, kws):
        if name in kws:
            return kws[name]
        return mapping[name]

    def _render(self, mapping, kws, safe):
        s = self.template
        out = []
        i = 0
        n = len(s)
        while i < n:
            ch = s[i]
            if ch != self.delimiter:
                out.append(ch)
                i += 1
                continue
            i += 1
            if i >= n:
                out.append(self.delimiter)
                break
            if s[i] == self.delimiter:
                out.append(self.delimiter)
                i += 1
                continue
            braced = s[i] == "{"
            if braced:
                i += 1
            start = i
            while i < n and (s[i].isalnum() or s[i] == "_"):
                i += 1
            name = s[start:i]
            if braced:
                if i >= n or s[i] != "}":
                    raise ValueError("Invalid placeholder in string: line 1, col %d" % start)
                i += 1
            if not name:
                raise ValueError("Invalid placeholder in string: line 1, col %d" % start)
            try:
                out.append(str(self._lookup(name, mapping, kws)))
            except KeyError:
                if not safe:
                    raise
                if braced:
                    out.append("%s{%s}" % (self.delimiter, name))
                else:
                    out.append("%s%s" % (self.delimiter, name))
        return "".join(out)

    def substitute(self, mapping=None, **kws):
        return self._render({} if mapping is None else mapping, kws, False)

    def safe_substitute(self, mapping=None, **kws):
        return self._render({} if mapping is None else mapping, kws, True)
