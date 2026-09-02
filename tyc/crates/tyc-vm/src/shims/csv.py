# `csv` — the RFC 4180 reader/writer pair plus the dict wrappers.
#
# Dialect support is the part programs actually reach for: the delimiter,
# quote character, doubled-quote escaping, `QUOTE_*` policies and the line
# terminator. Anything beyond that (`Sniffer`, `escapechar`-based escaping,
# `strict` error positions) is deliberately out of scope.

QUOTE_MINIMAL = 0
QUOTE_ALL = 1
QUOTE_NONNUMERIC = 2
QUOTE_NONE = 3


class Error(Exception):
    pass


class Dialect:
    delimiter = ","
    quotechar = '"'
    doublequote = True
    skipinitialspace = False
    lineterminator = "\r\n"
    quoting = QUOTE_MINIMAL
    escapechar = None


class excel(Dialect):
    pass


class excel_tab(excel):
    delimiter = "\t"


class unix_dialect(Dialect):
    lineterminator = "\n"
    quoting = QUOTE_ALL


_DIALECTS = {
    "excel": excel,
    "excel-tab": excel_tab,
    "unix": unix_dialect,
}


def register_dialect(name, dialect):
    _DIALECTS[name] = dialect


def unregister_dialect(name):
    del _DIALECTS[name]


def get_dialect(name):
    return _DIALECTS[name]


def list_dialects():
    return list(_DIALECTS.keys())


def _resolve(dialect, kwargs):
    base = dialect
    if isinstance(base, str):
        if base not in _DIALECTS:
            raise Error("unknown dialect")
        base = _DIALECTS[base]
    resolved = Dialect()
    for name in ["delimiter", "quotechar", "doublequote", "skipinitialspace",
                 "lineterminator", "quoting", "escapechar"]:
        setattr(resolved, name, getattr(base, name))
    for key in kwargs:
        if kwargs[key] is not None:
            setattr(resolved, key, kwargs[key])
    return resolved


class _Reader:
    def __init__(self, source, dialect):
        self._source = source
        self.dialect = dialect
        self.line_num = 0

    def __iter__(self):
        return self

    def _split(self, line):
        """Parse one logical record. Returns (row, unterminated).

        `unterminated` is True when the record ended inside a quoted field,
        which is how an embedded newline is carried to the next raw line.
        """
        d = self.dialect
        row = []
        field = []
        quoted = False
        in_quotes = False
        i = 0
        n = len(line)
        while i < n:
            ch = line[i]
            # `escapechar` takes the next character literally, inside a
            # quoted field or out of one.
            if d.escapechar is not None and ch == d.escapechar and i + 1 < n:
                field.append(line[i + 1])
                i += 2
                continue
            if in_quotes:
                if ch == d.quotechar:
                    if d.doublequote and i + 1 < n and line[i + 1] == d.quotechar:
                        field.append(d.quotechar)
                        i += 2
                        continue
                    in_quotes = False
                    i += 1
                    continue
                field.append(ch)
                i += 1
                continue
            # A quote opens a quoted field only at the *start* of one:
            # `ab"c,d` is two fields, the first holding a literal quote.
            if ch == d.quotechar and d.quoting != QUOTE_NONE and not field:
                in_quotes = True
                quoted = True
                i += 1
                continue
            if ch == d.delimiter:
                row.append(self._convert(field, quoted))
                field = []
                quoted = False
                i += 1
                if d.skipinitialspace:
                    while i < n and line[i] == " ":
                        i += 1
                continue
            field.append(ch)
            i += 1
        row.append(self._convert(field, quoted))
        return (row, in_quotes)

    def _convert(self, field, quoted):
        text = "".join(field)
        if self.dialect.quoting == QUOTE_NONNUMERIC and not quoted:
            if text == "":
                return text
            return float(text)
        return text

    def _strip_newline(self, raw):
        if raw.endswith("\r\n"):
            return raw[:-2]
        if raw.endswith("\n") or raw.endswith("\r"):
            return raw[:-1]
        return raw

    def __next__(self):
        # A quoted field may contain the line terminator, so a *record* can
        # span several raw lines: keep pulling until the quotes balance.
        pending = None
        for raw in self._source:
            self.line_num += 1
            line = self._strip_newline(raw)
            if pending is None:
                # CPython yields an empty row for a blank line rather than
                # skipping it.
                if line == "":
                    return []
                pending = line
            else:
                pending = pending + "\n" + line
            row, unterminated = self._split(pending)
            if not unterminated:
                return row
        if pending is not None:
            row, _ = self._split(pending)
            return row
        raise StopIteration


def reader(csvfile, dialect="excel", **kwargs):
    return _Reader(iter(csvfile), _resolve(dialect, kwargs))


class _Writer:
    def __init__(self, target, dialect):
        self._target = target
        self.dialect = dialect

    def _render(self, value):
        d = self.dialect
        if value is None:
            text = ""
        else:
            text = str(value)
        special = [d.delimiter, d.quotechar, "\r", "\n"]
        if d.escapechar is not None:
            special.append(d.escapechar)
        if d.quoting == QUOTE_NONE:
            # No quoting is available, so a special character has to be
            # escaped — and without an `escapechar` there is no way to write
            # the row at all. CPython raises rather than corrupt the shape.
            out = []
            for ch in text:
                if ch in special:
                    if d.escapechar is None:
                        raise Error("need to escape, but no escapechar set")
                    out.append(d.escapechar)
                out.append(ch)
            return "".join(out)
        needs = False
        if d.quoting == QUOTE_ALL:
            needs = True
        elif d.quoting == QUOTE_NONNUMERIC:
            needs = not isinstance(value, (int, float)) or isinstance(value, bool)
        else:
            for ch in [d.delimiter, d.quotechar, "\r", "\n"]:
                if ch in text:
                    needs = True
        if not needs:
            return text
        if d.doublequote:
            text = text.replace(d.quotechar, d.quotechar + d.quotechar)
        elif d.escapechar is not None:
            text = text.replace(d.escapechar, d.escapechar + d.escapechar)
            text = text.replace(d.quotechar, d.escapechar + d.quotechar)
        return d.quotechar + text + d.quotechar

    def writerow(self, row):
        line = self.dialect.delimiter.join([self._render(v) for v in row])
        self._target.write(line + self.dialect.lineterminator)
        return len(line) + len(self.dialect.lineterminator)

    def writerows(self, rows):
        for row in rows:
            self.writerow(row)


def writer(csvfile, dialect="excel", **kwargs):
    return _Writer(csvfile, _resolve(dialect, kwargs))


class DictReader:
    def __init__(self, f, fieldnames=None, restkey=None, restval=None,
                 dialect="excel", **kwargs):
        self._reader = reader(f, dialect, **kwargs)
        self._fieldnames = fieldnames
        self.restkey = restkey
        self.restval = restval
        self.line_num = 0

    def __iter__(self):
        return self

    @property
    def fieldnames(self):
        if self._fieldnames is None:
            try:
                self._fieldnames = next(self._reader)
            except StopIteration:
                pass
            self.line_num = self._reader.line_num
        return self._fieldnames

    def __next__(self):
        names = self.fieldnames
        row = next(self._reader)
        self.line_num = self._reader.line_num
        out = {}
        i = 0
        while i < len(names):
            out[names[i]] = row[i] if i < len(row) else self.restval
            i += 1
        if len(row) > len(names):
            out[self.restkey] = row[len(names):]
        return out


class DictWriter:
    def __init__(self, f, fieldnames, restval="", extrasaction="raise",
                 dialect="excel", **kwargs):
        self.fieldnames = fieldnames
        self.restval = restval
        self.extrasaction = extrasaction
        self.writer = writer(f, dialect, **kwargs)

    def writeheader(self):
        return self.writer.writerow(self.fieldnames)

    def _row(self, rowdict):
        if self.extrasaction == "raise":
            extras = [k for k in rowdict if k not in self.fieldnames]
            if extras:
                raise ValueError("dict contains fields not in fieldnames: "
                                 + ", ".join([repr(k) for k in sorted(extras)]))
        return [rowdict.get(k, self.restval) for k in self.fieldnames]

    def writerow(self, rowdict):
        return self.writer.writerow(self._row(rowdict))

    def writerows(self, rowdicts):
        for rowdict in rowdicts:
            self.writerow(rowdict)
