# VM `time` module extras: struct_time and the calendar conversions, in the
# dialect the tree-walking VM interprets. Validated against CPython's own
# `time` module (see validate_dt.py). Local time is UTC in the VM.

_DAY_NAMES = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]
_MONTH_NAMES = ["", "January", "February", "March", "April", "May", "June", "July",
                "August", "September", "October", "November", "December"]
_DAYS_IN_MONTH = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]

timezone = 0
altzone = 0
daylight = 0
tzname = ("UTC", "UTC")


def _is_leap(year):
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


def _days_before_year(year):
    y = year - 1
    return y * 365 + y // 4 - y // 100 + y // 400


def _days_in_month(year, month):
    if month == 2 and _is_leap(year):
        return 29
    return _DAYS_IN_MONTH[month]


def _days_before_month(year, month):
    total = 0
    m = 1
    while m < month:
        total += _days_in_month(year, m)
        m += 1
    return total


def _ymd2ord(year, month, day):
    return _days_before_year(year) + _days_before_month(year, month) + day


_DI400Y = _days_before_year(401)
_DI100Y = _days_before_year(101)
_DI4Y = _days_before_year(5)


def _ord2ymd(n):
    n -= 1
    n400, n = divmod(n, _DI400Y)
    year = n400 * 400 + 1
    n100, n = divmod(n, _DI100Y)
    n4, n = divmod(n, _DI4Y)
    n1, n = divmod(n, 365)
    year += n100 * 100 + n4 * 4 + n1
    if n1 == 4 or n100 == 4:
        return (year - 1, 12, 31)
    # `_days_before_month` / `_days_in_month` are already leap-aware for the
    # computed year, which is exactly CPython's `leapyear` here.
    month = (n + 50) >> 5
    preceding = _days_before_month(year, month)
    if preceding > n:
        month -= 1
        preceding -= _days_in_month(year, month)
    n -= preceding
    return (year, month, n + 1)


def _weekday(year, month, day):
    # Monday == 0
    return (_ymd2ord(year, month, day) + 6) % 7


def _isoweek1monday(year):
    firstday = _ymd2ord(year, 1, 1)
    firstweekday = (firstday + 6) % 7
    week1monday = firstday - firstweekday
    if firstweekday > 3:
        week1monday += 7
    return week1monday


def _isocalendar(year, month, day):
    week1monday = _isoweek1monday(year)
    today = _ymd2ord(year, month, day)
    week, day = divmod(today - week1monday, 7)
    if week < 0:
        year -= 1
        week1monday = _isoweek1monday(year)
        week, day = divmod(today - week1monday, 7)
    elif week >= 52:
        if today >= _isoweek1monday(year + 1):
            year += 1
            week = 0
    return (year, week + 1, day + 1)


class struct_time:
    __typhon_builtin_bases__ = ("tuple",)
    def __init__(self, seq):
        items = list(seq)
        if len(items) < 9:
            raise TypeError("time.struct_time() takes an at least 9-sequence (%d-sequence given)" % len(items))
        self._t = tuple(items[:9])
        self.tm_year = items[0]
        self.tm_mon = items[1]
        self.tm_mday = items[2]
        self.tm_hour = items[3]
        self.tm_min = items[4]
        self.tm_sec = items[5]
        self.tm_wday = items[6]
        self.tm_yday = items[7]
        self.tm_isdst = items[8]
        self.tm_zone = "UTC"
        self.tm_gmtoff = 0

    def __getitem__(self, i):
        return self._t[i]

    def __len__(self):
        return 9

    def __iter__(self):
        return iter(self._t)

    def __eq__(self, other):
        if isinstance(other, struct_time):
            return self._t == other._t
        return self._t == other

    def __hash__(self):
        return hash(self._t)

    def __repr__(self):
        return ("time.struct_time(tm_year=%d, tm_mon=%d, tm_mday=%d, tm_hour=%d, tm_min=%d, "
                "tm_sec=%d, tm_wday=%d, tm_yday=%d, tm_isdst=%d)") % self._t


def _fields_from_seconds(secs):
    # secs: int or float seconds since the epoch → (Y, m, d, H, M, S, wday, yday)
    s = int(secs // 1) if isinstance(secs, float) else secs
    days, rem = divmod(s, 86400)
    hour, rem = divmod(rem, 3600)
    minute, second = divmod(rem, 60)
    year, month, day = _ord2ymd(_EPOCH_ORD + days)
    wday = _weekday(year, month, day)
    yday = _days_before_month(year, month) + day
    return (year, month, day, hour, minute, second, wday, yday)


_EPOCH_ORD = _ymd2ord(1970, 1, 1)


def gmtime(secs=None):
    if secs is None:
        secs = time()
    f = _fields_from_seconds(secs)
    return struct_time((f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], 0))


def localtime(secs=None):
    return gmtime(secs)


def mktime(t):
    days = _ymd2ord(t[0], t[1], t[2]) - _EPOCH_ORD
    return float(days * 86400 + t[3] * 3600 + t[4] * 60 + t[5])


def _asctime_fields(y, mo, d, h, mi, s, wday):
    return "%s %s %2d %02d:%02d:%02d %d" % (
        _DAY_NAMES[wday][:3], _MONTH_NAMES[mo][:3], d, h, mi, s, y)


def asctime(t=None):
    if t is None:
        t = localtime()
    return _asctime_fields(t[0], t[1], t[2], t[3], t[4], t[5], t[6])


def ctime(secs=None):
    return asctime(localtime(secs))


def _strftime_fields(fmt, year, month, day, hour, minute, second, microsecond, wday, yday, utcoff, tzname_):
    # utcoff: None (naive) or offset seconds; tzname_: None or str.
    out = []
    i = 0
    n = len(fmt)
    while i < n:
        c = fmt[i]
        if c != "%" or i + 1 >= n:
            out.append(c)
            i += 1
            continue
        d = fmt[i + 1]
        i += 2
        if d == "Y":
            out.append(str(year))
        elif d == "m":
            out.append("%02d" % month)
        elif d == "d":
            out.append("%02d" % day)
        elif d == "H":
            out.append("%02d" % hour)
        elif d == "M":
            out.append("%02d" % minute)
        elif d == "S":
            out.append("%02d" % second)
        elif d == "f":
            out.append("%06d" % microsecond)
        elif d == "y":
            out.append("%02d" % (year % 100))
        elif d == "C":
            out.append("%02d" % (year // 100))
        elif d == "I":
            h12 = hour % 12
            if h12 == 0:
                h12 = 12
            out.append("%02d" % h12)
        elif d == "p":
            out.append("AM" if hour < 12 else "PM")
        elif d == "a":
            out.append(_DAY_NAMES[wday][:3])
        elif d == "A":
            out.append(_DAY_NAMES[wday])
        elif d == "b" or d == "h":
            out.append(_MONTH_NAMES[month][:3])
        elif d == "B":
            out.append(_MONTH_NAMES[month])
        elif d == "j":
            out.append("%03d" % yday)
        elif d == "w":
            out.append(str((wday + 1) % 7))
        elif d == "u":
            out.append(str(wday + 1))
        elif d == "U":
            # Week of the year, Sunday first; days before the first Sunday are week 0.
            sunday_based_wday = (wday + 1) % 7
            out.append("%02d" % ((yday + 6 - sunday_based_wday) // 7))
        elif d == "W":
            out.append("%02d" % ((yday + 6 - wday) // 7))
        elif d == "G" or d == "V":
            iso = _isocalendar(year, month, day)
            if d == "G":
                out.append(str(iso[0]))
            else:
                out.append("%02d" % iso[1])
        elif d == "e":
            out.append("%2d" % day)
        elif d == "n":
            out.append("\n")
        elif d == "t":
            out.append("\t")
        elif d == "z":
            if utcoff is not None:
                sign = "-" if utcoff < 0 else "+"
                off = -utcoff if utcoff < 0 else utcoff
                hh, rem = divmod(off, 3600)
                mm, ss = divmod(rem, 60)
                out.append("%s%02d%02d" % (sign, hh, mm))
                if ss:
                    out.append("%02d" % ss)
        elif d == "Z":
            if tzname_ is not None:
                out.append(tzname_)
        elif d == "c":
            out.append(_asctime_fields(year, month, day, hour, minute, second, wday))
        elif d == "x":
            out.append("%02d/%02d/%02d" % (month, day, year % 100))
        elif d == "X":
            out.append("%02d:%02d:%02d" % (hour, minute, second))
        elif d == "D":
            out.append("%02d/%02d/%02d" % (month, day, year % 100))
        elif d == "F":
            out.append("%d-%02d-%02d" % (year, month, day))
        elif d == "T":
            out.append("%02d:%02d:%02d" % (hour, minute, second))
        elif d == "R":
            out.append("%02d:%02d" % (hour, minute))
        elif d == "r":
            h12 = hour % 12
            if h12 == 0:
                h12 = 12
            out.append("%02d:%02d:%02d %s" % (h12, minute, second, "AM" if hour < 12 else "PM"))
        elif d == "s":
            days = _ymd2ord(year, month, day) - _EPOCH_ORD
            out.append(str(days * 86400 + hour * 3600 + minute * 60 + second - (utcoff or 0)))
        elif d == "%":
            out.append("%")
        else:
            out.append("%" + d)
    return "".join(out)


def strftime(fmt, t=None):
    if t is None:
        t = localtime()
    if isinstance(t, struct_time):
        t = t._t
    year, month, day, hour, minute, second, wday, yday = t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7]
    if yday == 0 and month >= 1:
        yday = _days_before_month(year, month) + day
    return _strftime_fields(fmt, year, month, day, hour, minute, second, 0, wday, yday, 0, "UTC")


def _strptime_fields(s, fmt):
    # Returns (year, month, day, hour, minute, second, microsecond, utcoff or None, tzname or None)
    year, month, day, hour, minute, second, us = 1900, 1, 1, 0, 0, 0, 0
    utcoff = None
    tzname_ = None
    pm = None
    hour12 = None
    yday = None
    i = 0
    j = 0
    n = len(fmt)
    m = len(s)

    def fail():
        raise ValueError("time data %r does not match format %r" % (s, fmt))

    def read_int(maxdigits, minval, maxval):
        nonlocal j
        start = j
        while j < m and j - start < maxdigits and s[j].isdigit():
            j += 1
        if j == start:
            fail()
        v = int(s[start:j])
        if v < minval or v > maxval:
            fail()
        return v

    def read_name(names, allow_abbrev):
        nonlocal j
        low = s[j:].lower()
        best = -1
        best_len = 0
        for idx, name in enumerate(names):
            if not name:
                continue
            candidates = [name.lower()]
            if allow_abbrev:
                candidates.append(name[:3].lower())
            for cand in candidates:
                if low.startswith(cand) and len(cand) > best_len:
                    best = idx
                    best_len = len(cand)
        if best < 0:
            fail()
        j += best_len
        return best

    while i < n:
        c = fmt[i]
        if c == "%" and i + 1 < n:
            d = fmt[i + 1]
            i += 2
            if d == "Y":
                year = read_int(4, 1, 9999)
            elif d == "m":
                month = read_int(2, 1, 12)
            elif d == "d":
                day = read_int(2, 1, 31)
            elif d == "H":
                hour = read_int(2, 0, 23)
            elif d == "M":
                minute = read_int(2, 0, 59)
            elif d == "S":
                second = read_int(2, 0, 61)
            elif d == "f":
                start = j
                while j < m and j - start < 6 and s[j].isdigit():
                    j += 1
                if j == start:
                    fail()
                digits = s[start:j]
                us = int(digits) * (10 ** (6 - len(digits)))
            elif d == "y":
                yy = read_int(2, 0, 99)
                year = 2000 + yy if yy < 69 else 1900 + yy
            elif d == "I":
                hour12 = read_int(2, 1, 12)
            elif d == "p":
                low = s[j:j + 2].lower()
                if low == "am":
                    pm = False
                elif low == "pm":
                    pm = True
                else:
                    fail()
                j += 2
            elif d == "b" or d == "B" or d == "h":
                month = read_name(_MONTH_NAMES, True)
            elif d == "a" or d == "A":
                read_name(_DAY_NAMES, True)
            elif d == "j":
                yday = read_int(3, 1, 366)
            elif d == "z":
                if j < m and s[j] == "Z":
                    utcoff = 0
                    j += 1
                else:
                    if j >= m or s[j] not in "+-":
                        fail()
                    sign = -1 if s[j] == "-" else 1
                    j += 1
                    hh = read_int(2, 0, 23)
                    if j < m and s[j] == ":":
                        j += 1
                    mm = read_int(2, 0, 59)
                    ss = 0
                    if j < m and s[j] == ":":
                        j += 1
                        ss = read_int(2, 0, 59)
                    utcoff = sign * (hh * 3600 + mm * 60 + ss)
            elif d == "Z":
                start = j
                while j < m and s[j].isalpha():
                    j += 1
                tzname_ = s[start:j]
                if tzname_ == "UTC" or tzname_ == "GMT":
                    if utcoff is None:
                        utcoff = 0
            elif d == "%":
                if j < m and s[j] == "%":
                    j += 1
                else:
                    fail()
            else:
                fail()
        elif c.isspace():
            i += 1
            while j < m and s[j].isspace():
                j += 1
        else:
            if j < m and s[j] == c:
                i += 1
                j += 1
            else:
                fail()
    if j != m:
        raise ValueError("unconverted data remains: %s" % s[j:])
    if hour12 is not None:
        hour = hour12 % 12
        if pm:
            hour += 12
    if yday is not None:
        ymd = _ord2ymd(_ymd2ord(year, 1, 1) + yday - 1)
        month = ymd[1]
        day = ymd[2]
    if day > _days_in_month(year, month):
        fail()
    return (year, month, day, hour, minute, second, us, utcoff, tzname_)


def strptime(s, fmt):
    f = _strptime_fields(s, fmt)
    year, month, day = f[0], f[1], f[2]
    wday = _weekday(year, month, day)
    yday = _days_before_month(year, month) + day
    return struct_time((year, month, day, f[3], f[4], f[5], wday, yday, -1))


class _ClockInfo:
    def __init__(self, implementation, monotonic, adjustable, resolution):
        self.implementation = implementation
        self.monotonic = monotonic
        self.adjustable = adjustable
        self.resolution = resolution

    def __repr__(self):
        return "namespace(implementation=%r, monotonic=%r, adjustable=%r, resolution=%r)" % (
            self.implementation, self.monotonic, self.adjustable, self.resolution)


def get_clock_info(name):
    if name == "time":
        return _ClockInfo("clock_gettime(CLOCK_REALTIME)", False, True, 1e-09)
    if name == "monotonic" or name == "perf_counter":
        return _ClockInfo("clock_gettime(CLOCK_MONOTONIC)", True, False, 1e-09)
    if name == "process_time":
        return _ClockInfo("clock_gettime(CLOCK_PROCESS_CPUTIME_ID)", True, False, 1e-09)
    if name == "thread_time":
        return _ClockInfo("clock_gettime(CLOCK_THREAD_CPUTIME_ID)", True, False, 1e-09)
    raise ValueError("unknown clock")
