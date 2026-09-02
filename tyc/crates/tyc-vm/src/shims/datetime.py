# VM `datetime` module, in the dialect the tree-walking VM interprets.
# Ported from CPython's pure-Python `_pydatetime`; validated against the real
# module (see validate_dt.py). The VM's local time zone is UTC.
import time as _time

MINYEAR = 1
MAXYEAR = 9999

_DAY_NAMES = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]
_MONTH_NAMES = ["", "January", "February", "March", "April", "May", "June", "July",
                "August", "September", "October", "November", "December"]
_DAYS_IN_MONTH = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]


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


def _check_date_fields(year, month, day):
    if not isinstance(year, int) or isinstance(year, bool):
        raise TypeError("'%s' object cannot be interpreted as an integer" % type(year).__name__)
    if not isinstance(month, int) or not isinstance(day, int):
        raise TypeError("an integer is required")
    if not MINYEAR <= year <= MAXYEAR:
        raise ValueError("year %d is out of range" % year)
    if not 1 <= month <= 12:
        raise ValueError("month must be in 1..12")
    if not 1 <= day <= _days_in_month(year, month):
        raise ValueError("day is out of range for month")


def _check_time_fields(hour, minute, second, microsecond, fold):
    if not isinstance(hour, int) or not isinstance(minute, int) or not isinstance(second, int) or not isinstance(microsecond, int):
        raise TypeError("an integer is required")
    if not 0 <= hour <= 23:
        raise ValueError("hour must be in 0..23")
    if not 0 <= minute <= 59:
        raise ValueError("minute must be in 0..59")
    if not 0 <= second <= 59:
        raise ValueError("second must be in 0..59")
    if not 0 <= microsecond <= 999999:
        raise ValueError("microsecond must be in 0..999999")
    if fold not in (0, 1):
        raise ValueError("fold must be either 0 or 1")


def _check_tzinfo_arg(tz):
    if tz is not None and not isinstance(tz, tzinfo):
        raise TypeError("tzinfo argument must be None or of a tzinfo subclass")


def _weekday(year, month, day):
    return (_ymd2ord(year, month, day) + 6) % 7


def _isoweek1monday(year):
    firstday = _ymd2ord(year, 1, 1)
    firstweekday = (firstday + 6) % 7
    week1monday = firstday - firstweekday
    if firstweekday > 3:
        week1monday += 7
    return week1monday


def _isoweek_to_gregorian(year, week, day):
    if not MINYEAR <= year <= MAXYEAR:
        raise ValueError("Year is out of range: %d" % year)
    if not 0 < week < 53:
        out_of_range = True
        if week == 53:
            first_weekday = _weekday(year, 1, 1)
            if first_weekday == 3 or (first_weekday == 2 and _is_leap(year)):
                out_of_range = False
        if out_of_range:
            raise ValueError("Invalid week: %d" % week)
    if not 0 < day < 8:
        raise ValueError("Invalid weekday: %d (range is [1, 7])" % day)
    day_offset = (week - 1) * 7 + (day - 1)
    day_1 = _isoweek1monday(year)
    ord_day = day_1 + day_offset
    return _ord2ymd(ord_day)


def _tname(v):
    n = type(v).__name__
    if n in ("date", "datetime", "time", "timedelta", "timezone"):
        return "datetime." + n
    return n


def _modf(x):
    whole = float(int(x))
    return (x - whole, whole)


def _format_offset(off, sep=":"):
    s = ""
    if off is not None:
        if off.days < 0:
            sign = "-"
            off = -off
        else:
            sign = "+"
        hh, mm = divmod(off, timedelta(hours=1))
        mm, ss = divmod(mm, timedelta(minutes=1))
        s += "%s%02d%s%02d" % (sign, hh, sep, mm)
        if ss or ss.microseconds:
            s += "%s%02d" % (sep, ss.seconds)
            if ss.microseconds:
                s += ".%06d" % ss.microseconds
    return s


def _format_time(hh, mm, ss, us, timespec):
    if timespec == "auto":
        timespec = "microseconds" if us else "seconds"
    if timespec == "hours":
        return "%02d" % hh
    if timespec == "minutes":
        return "%02d:%02d" % (hh, mm)
    if timespec == "seconds":
        return "%02d:%02d:%02d" % (hh, mm, ss)
    if timespec == "milliseconds":
        return "%02d:%02d:%02d.%03d" % (hh, mm, ss, us // 1000)
    if timespec == "microseconds":
        return "%02d:%02d:%02d.%06d" % (hh, mm, ss, us)
    raise ValueError("Unknown timespec value")


def _wrap_strftime(fmt, year, month, day, hour, minute, second, microsecond, utcoff, tzname_):
    wday = _weekday(year, month, day)
    yday = _days_before_month(year, month) + day
    if utcoff is not None:
        utcoff = utcoff.days * 86400 + utcoff.seconds
    return _time._strftime_fields(fmt, year, month, day, hour, minute, second, microsecond, wday, yday, utcoff, tzname_)


def _parse_isoformat_date(dtstr):
    # YYYY-MM-DD, YYYYMMDD, YYYY-Www-D
    if len(dtstr) == 10 and dtstr[4] == "-" and dtstr[7] == "-":
        return (int(dtstr[0:4]), int(dtstr[5:7]), int(dtstr[8:10]))
    if len(dtstr) == 8 and dtstr.isdigit():
        return (int(dtstr[0:4]), int(dtstr[4:6]), int(dtstr[6:8]))
    if len(dtstr) >= 7 and dtstr[4] == "-" and dtstr[5] == "W":
        week = int(dtstr[6:8])
        if len(dtstr) == 8:
            return _isoweek_to_gregorian(int(dtstr[0:4]), week, 1)
        if len(dtstr) == 10 and dtstr[8] == "-":
            return _isoweek_to_gregorian(int(dtstr[0:4]), week, int(dtstr[9]))
    if len(dtstr) == 7 and dtstr[4] == "W":
        return _isoweek_to_gregorian(int(dtstr[0:4]), int(dtstr[5:7]), 1)
    if len(dtstr) == 8 and dtstr[4] == "W":
        return _isoweek_to_gregorian(int(dtstr[0:4]), int(dtstr[5:7]), int(dtstr[7]))
    raise ValueError("Invalid isoformat string: %r" % dtstr)


def _parse_hh_mm_ss_ff(tstr):
    # HH[:MM[:SS[.fff[fff]]]] or compact HHMMSS
    n = len(tstr)
    parts = [0, 0, 0, 0]
    pos = 0
    sep = ":" if ":" in tstr else None
    for comp in range(3):
        if pos + 2 > n:
            if comp == 0:
                raise ValueError("Incomplete time component")
            break
        seg = tstr[pos:pos + 2]
        if not seg.isdigit():
            raise ValueError("Invalid time component")
        parts[comp] = int(seg)
        pos += 2
        if pos >= n:
            break
        if tstr[pos] == ":":
            pos += 1
            if pos >= n:
                raise ValueError("Incomplete time component")
            continue
        if tstr[pos] == "." or tstr[pos] == ",":
            if comp != 2:
                raise ValueError("Invalid time separator: %c" % tstr[pos])
            break
        if sep is not None:
            raise ValueError("Invalid time separator: %c" % tstr[pos])
    if pos < n:
        if tstr[pos] != "." and tstr[pos] != ",":
            raise ValueError("Invalid microsecond component")
        pos += 1
        frac = tstr[pos:]
        if not frac or not frac.isdigit():
            raise ValueError("Invalid microsecond component")
        if len(frac) > 6:
            frac = frac[:6]
        parts[3] = int(frac) * (10 ** (6 - len(frac)))
    return parts


def _parse_isoformat_time(tstr):
    len_str = len(tstr)
    if len_str < 2:
        raise ValueError("Isoformat time too short")
    tz_pos = -1
    for k, ch in enumerate(tstr):
        if ch == "+" or ch == "-" or ch == "Z":
            tz_pos = k
            break
    timestr = tstr[:tz_pos] if tz_pos >= 0 else tstr
    time_comps = _parse_hh_mm_ss_ff(timestr)
    tzi = None
    if tz_pos >= 0:
        tzstr = tstr[tz_pos:]
        if tzstr == "Z":
            tzi = timezone.utc
        else:
            body = tzstr[1:]
            if len(body) not in (2, 5, 8, 15) and not (len(body) == 4 and body.isdigit()) and not (len(body) == 6 and body.isdigit()):
                raise ValueError("Malformed time zone string")
            tz_comps = _parse_hh_mm_ss_ff(body)
            if all(x == 0 for x in tz_comps):
                tzi = timezone.utc
            else:
                tzsign = -1 if tstr[tz_pos] == "-" else 1
                td = timedelta(hours=tz_comps[0], minutes=tz_comps[1], seconds=tz_comps[2], microseconds=tz_comps[3])
                tzi = timezone(tzsign * td)
    return (time_comps[0], time_comps[1], time_comps[2], time_comps[3], tzi)


class timedelta:
    def __init__(self, days=0, seconds=0, microseconds=0, milliseconds=0, minutes=0, hours=0, weeks=0):
        d = 0
        s = 0
        us = 0
        days += weeks * 7
        seconds += minutes * 60 + hours * 3600
        microseconds += milliseconds * 1000
        if isinstance(days, float):
            dayfrac, days = _modf(days)
            daysecondsfrac, daysecondswhole = _modf(dayfrac * (24.0 * 3600.0))
            s = int(daysecondswhole)
            d = int(days)
        else:
            daysecondsfrac = 0.0
            d = days
        if isinstance(seconds, float):
            secondsfrac, seconds = _modf(seconds)
            seconds = int(seconds)
            secondsfrac += daysecondsfrac
        else:
            secondsfrac = daysecondsfrac
        days, seconds = divmod(seconds, 24 * 3600)
        d += days
        s += int(seconds)
        usdouble = secondsfrac * 1e6
        if isinstance(microseconds, float):
            microseconds = round(microseconds + usdouble)
            seconds, microseconds = divmod(microseconds, 1000000)
            days, seconds = divmod(seconds, 24 * 3600)
            d += days
            s += seconds
        else:
            microseconds = int(microseconds)
            seconds, microseconds = divmod(microseconds, 1000000)
            days, seconds = divmod(seconds, 24 * 3600)
            d += days
            s += seconds
            microseconds = round(microseconds + usdouble)
        seconds, us = divmod(microseconds, 1000000)
        s += seconds
        days, s = divmod(s, 24 * 3600)
        d += days
        if abs(d) > 999999999:
            raise OverflowError("days=%d; must have magnitude <= 999999999" % d)
        self.days = d
        self.seconds = s
        self.microseconds = us

    def __repr__(self):
        args = []
        if self.days:
            args.append("days=%d" % self.days)
        if self.seconds:
            args.append("seconds=%d" % self.seconds)
        if self.microseconds:
            args.append("microseconds=%d" % self.microseconds)
        if not args:
            args.append("0")
        return "datetime.timedelta(%s)" % ", ".join(args)

    def __str__(self):
        mm, ss = divmod(self.seconds, 60)
        hh, mm = divmod(mm, 60)
        s = "%d:%02d:%02d" % (hh, mm, ss)
        if self.days:
            plural = "s" if abs(self.days) != 1 else ""
            s = ("%d day%s, " % (self.days, plural)) + s
        if self.microseconds:
            s = s + ".%06d" % self.microseconds
        return s

    def total_seconds(self):
        return ((self.days * 86400 + self.seconds) * 10 ** 6 + self.microseconds) / 10 ** 6

    def _to_us(self):
        return (self.days * 86400 + self.seconds) * 1000000 + self.microseconds

    def __add__(self, other):
        if isinstance(other, timedelta):
            return timedelta(self.days + other.days, self.seconds + other.seconds, self.microseconds + other.microseconds)
        if isinstance(other, datetime):
            return other + self
        if isinstance(other, date):
            return other + self
        raise TypeError("unsupported operand type(s) for +: 'datetime.timedelta' and '%s'" % _tname(other))

    def __radd__(self, other):
        return self.__add__(other)

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return timedelta(self.days - other.days, self.seconds - other.seconds, self.microseconds - other.microseconds)
        raise TypeError("unsupported operand type(s) for -: 'datetime.timedelta' and '%s'" % _tname(other))

    def __neg__(self):
        return timedelta(-self.days, -self.seconds, -self.microseconds)

    def __pos__(self):
        return self

    def __abs__(self):
        if self.days < 0:
            return -self
        return self

    def __mul__(self, other):
        if isinstance(other, int) and not isinstance(other, bool):
            return timedelta(self.days * other, self.seconds * other, self.microseconds * other)
        if isinstance(other, bool):
            return timedelta(self.days * int(other), self.seconds * int(other), self.microseconds * int(other))
        if isinstance(other, float):
            usec = self._to_us()
            return timedelta(0, 0, round(usec * other))
        raise TypeError("unsupported operand type(s) for *: 'datetime.timedelta' and '%s'" % _tname(other))

    def __rmul__(self, other):
        return self.__mul__(other)

    def __floordiv__(self, other):
        usec = self._to_us()
        if isinstance(other, timedelta):
            return usec // other._to_us()
        if isinstance(other, int):
            return timedelta(0, 0, usec // other)
        raise TypeError("unsupported operand type(s) for //: 'datetime.timedelta' and '%s'" % _tname(other))

    def __truediv__(self, other):
        usec = self._to_us()
        if isinstance(other, timedelta):
            return usec / other._to_us()
        if isinstance(other, int) and not isinstance(other, bool):
            return timedelta(0, 0, round(usec / other))
        if isinstance(other, float):
            return timedelta(0, 0, round(usec / other))
        raise TypeError("unsupported operand type(s) for /: 'datetime.timedelta' and '%s'" % _tname(other))

    def __mod__(self, other):
        if isinstance(other, timedelta):
            r = self._to_us() % other._to_us()
            return timedelta(0, 0, r)
        raise TypeError("unsupported operand type(s) for %%: 'datetime.timedelta' and '%s'" % _tname(other))

    def __divmod__(self, other):
        if isinstance(other, timedelta):
            q, r = divmod(self._to_us(), other._to_us())
            return (q, timedelta(0, 0, r))
        raise TypeError("unsupported operand type(s) for divmod(): 'datetime.timedelta' and '%s'" % _tname(other))

    def __eq__(self, other):
        if isinstance(other, timedelta):
            return self._to_us() == other._to_us()
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def _cmp_check(self, other, op):
        if not isinstance(other, timedelta):
            raise TypeError("'%s' not supported between instances of 'datetime.timedelta' and '%s'" % (op, _tname(other)))

    def __lt__(self, other):
        self._cmp_check(other, "<")
        return self._to_us() < other._to_us()

    def __le__(self, other):
        self._cmp_check(other, "<=")
        return self._to_us() <= other._to_us()

    def __gt__(self, other):
        self._cmp_check(other, ">")
        return self._to_us() > other._to_us()

    def __ge__(self, other):
        self._cmp_check(other, ">=")
        return self._to_us() >= other._to_us()

    def __hash__(self):
        return hash((self.days, self.seconds, self.microseconds))

    def __bool__(self):
        return self.days != 0 or self.seconds != 0 or self.microseconds != 0


timedelta.min = timedelta(-999999999)
timedelta.max = timedelta(days=999999999, hours=23, minutes=59, seconds=59, microseconds=999999)
timedelta.resolution = timedelta(microseconds=1)


class tzinfo:
    def tzname(self, dt):
        raise NotImplementedError("tzinfo subclass must override tzname()")

    def utcoffset(self, dt):
        raise NotImplementedError("tzinfo subclass must override utcoffset()")

    def dst(self, dt):
        raise NotImplementedError("tzinfo subclass must override dst()")

    def fromutc(self, dt):
        dtoff = dt.utcoffset()
        dtdst = dt.dst()
        if dtdst is None:
            dtdst = timedelta(0)
        delta = dtoff - dtdst
        if delta:
            dt = dt + delta
            dtdst = dt.dst()
            if dtdst is None:
                dtdst = timedelta(0)
        return dt + dtdst


class timezone(tzinfo):
    def __init__(self, offset, name=None):
        if not isinstance(offset, timedelta):
            raise TypeError("offset must be a timedelta")
        if name is not None and not isinstance(name, str):
            raise TypeError("name must be a string")
        if not (-timedelta(hours=24) < offset < timedelta(hours=24)):
            raise ValueError("offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24), not %r." % offset)
        self._offset = offset
        self._name = name

    def utcoffset(self, dt=None):
        return self._offset

    def tzname(self, dt=None):
        if self._name is None:
            return self._name_from_offset(self._offset)
        return self._name

    def dst(self, dt=None):
        return None

    def fromutc(self, dt):
        return dt + self._offset

    def _name_from_offset(self, delta):
        if not delta:
            return "UTC"
        if delta < timedelta(0):
            sign = "-"
            delta = -delta
        else:
            sign = "+"
        hours, rest = divmod(delta, timedelta(hours=1))
        minutes, rest = divmod(rest, timedelta(minutes=1))
        seconds = rest.seconds
        microseconds = rest.microseconds
        if microseconds:
            return "UTC%s%02d:%02d:%02d.%06d" % (sign, hours, minutes, seconds, microseconds)
        if seconds:
            return "UTC%s%02d:%02d:%02d" % (sign, hours, minutes, seconds)
        return "UTC%s%02d:%02d" % (sign, hours, minutes)

    def __repr__(self):
        if self._offset == timedelta(0) and self._name is None:
            return "datetime.timezone.utc"
        if self._name is None:
            return "datetime.timezone(%r)" % self._offset
        return "datetime.timezone(%r, %r)" % (self._offset, self._name)

    def __str__(self):
        return self.tzname(None)

    def __eq__(self, other):
        if isinstance(other, timezone):
            return self._offset == other._offset
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return hash(self._offset)


timezone.utc = timezone(timedelta(0))
timezone.min = timezone(-timedelta(hours=23, minutes=59))
timezone.max = timezone(timedelta(hours=23, minutes=59))
UTC = timezone.utc

_EPOCH_ORD = _ymd2ord(1970, 1, 1)


class date:
    def __init__(self, year, month, day):
        _check_date_fields(year, month, day)
        self.year = year
        self.month = month
        self.day = day

    @classmethod
    def fromtimestamp(cls, t):
        y, m, d, hh, mm, ss, us = _fields_from_timestamp(t)
        return cls(y, m, d)

    @classmethod
    def today(cls):
        return cls.fromtimestamp(_time.time())

    @classmethod
    def fromordinal(cls, n):
        y, m, d = _ord2ymd(n)
        return cls(y, m, d)

    @classmethod
    def fromisoformat(cls, date_string):
        if not isinstance(date_string, str):
            raise TypeError("fromisoformat: argument must be str")
        if len(date_string) not in (7, 8, 10):
            raise ValueError("Invalid isoformat string: %r" % date_string)
        try:
            y, m, d = _parse_isoformat_date(date_string)
        except Exception:
            raise ValueError("Invalid isoformat string: %r" % date_string)
        return cls(y, m, d)

    @classmethod
    def fromisocalendar(cls, year, week, day):
        y, m, d = _isoweek_to_gregorian(year, week, day)
        return cls(y, m, d)

    def __repr__(self):
        return "datetime.date(%d, %d, %d)" % (self.year, self.month, self.day)

    def ctime(self):
        weekday = self.toordinal() % 7 or 7
        return "%s %s %2d 00:00:00 %04d" % (_DAY_NAMES[weekday - 1][:3], _MONTH_NAMES[self.month][:3], self.day, self.year)

    def strftime(self, fmt):
        return _wrap_strftime(fmt, self.year, self.month, self.day, 0, 0, 0, 0, None, None)

    def __format__(self, fmt):
        if not isinstance(fmt, str):
            raise TypeError("must be str, not %s" % type(fmt).__name__)
        if len(fmt) != 0:
            return self.strftime(fmt)
        return str(self)

    def isoformat(self):
        return "%04d-%02d-%02d" % (self.year, self.month, self.day)

    def __str__(self):
        return self.isoformat()

    def timetuple(self):
        return _build_struct_time(self.year, self.month, self.day, 0, 0, 0, -1)

    def toordinal(self):
        return _ymd2ord(self.year, self.month, self.day)

    def replace(self, year=None, month=None, day=None):
        if year is None:
            year = self.year
        if month is None:
            month = self.month
        if day is None:
            day = self.day
        return type(self)(year, month, day)

    def _cmp_ok(self, other):
        return isinstance(other, date) and not isinstance(other, datetime)

    def __eq__(self, other):
        if self._cmp_ok(other):
            return (self.year, self.month, self.day) == (other.year, other.month, other.day)
        return False

    def __ne__(self, other):
        return not self.__eq__(other)

    def _cmp_check(self, other, op):
        if not self._cmp_ok(other):
            raise TypeError("'%s' not supported between instances of 'datetime.date' and '%s'" % (op, _tname(other)))

    def __lt__(self, other):
        self._cmp_check(other, "<")
        return (self.year, self.month, self.day) < (other.year, other.month, other.day)

    def __le__(self, other):
        self._cmp_check(other, "<=")
        return (self.year, self.month, self.day) <= (other.year, other.month, other.day)

    def __gt__(self, other):
        self._cmp_check(other, ">")
        return (self.year, self.month, self.day) > (other.year, other.month, other.day)

    def __ge__(self, other):
        self._cmp_check(other, ">=")
        return (self.year, self.month, self.day) >= (other.year, other.month, other.day)

    def __hash__(self):
        return hash(("date", self.year, self.month, self.day))

    def __add__(self, other):
        if isinstance(other, timedelta):
            o = self.toordinal() + other.days
            if 0 < o <= 3652059:
                return type(self).fromordinal(o)
            raise OverflowError("result out of range")
        raise TypeError("unsupported operand type(s) for +: 'datetime.date' and '%s'" % _tname(other))

    def __radd__(self, other):
        return self.__add__(other)

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return self + timedelta(-other.days)
        if isinstance(other, date) and not isinstance(other, datetime):
            days1 = self.toordinal()
            days2 = other.toordinal()
            return timedelta(days1 - days2)
        raise TypeError("unsupported operand type(s) for -: 'datetime.date' and '%s'" % _tname(other))

    def weekday(self):
        return (self.toordinal() + 6) % 7

    def isoweekday(self):
        return self.toordinal() % 7 or 7

    def isocalendar(self):
        year = self.year
        week1monday = _isoweek1monday(year)
        today = _ymd2ord(self.year, self.month, self.day)
        week, day = divmod(today - week1monday, 7)
        if week < 0:
            year -= 1
            week1monday = _isoweek1monday(year)
            week, day = divmod(today - week1monday, 7)
        elif week >= 52:
            if today >= _isoweek1monday(year + 1):
                year += 1
                week = 0
        return IsoCalendarDate(year, week + 1, day + 1)


date.min = date(1, 1, 1)
date.max = date(9999, 12, 31)
date.resolution = timedelta(days=1)


class IsoCalendarDate:
    def __init__(self, year, week, weekday):
        self.year = year
        self.week = week
        self.weekday = weekday
        self._t = (year, week, weekday)

    def __getitem__(self, i):
        return self._t[i]

    def __iter__(self):
        return iter(self._t)

    def __len__(self):
        return 3

    def __eq__(self, other):
        if isinstance(other, IsoCalendarDate):
            return self._t == other._t
        return self._t == other

    def __hash__(self):
        return hash(self._t)

    def __repr__(self):
        return "datetime.IsoCalendarDate(year=%d, week=%d, weekday=%d)" % self._t


def _build_struct_time(y, m, d, hh, mm, ss, dstflag):
    wday = (_ymd2ord(y, m, d) + 6) % 7
    dnum = _days_before_month(y, m) + d
    return _time.struct_time((y, m, d, hh, mm, ss, wday, dnum, dstflag))


def _fields_from_timestamp(t):
    frac, t = _modf(t)
    us = round(frac * 1e6)
    if us >= 1000000:
        t += 1
        us -= 1000000
    elif us < 0:
        t -= 1
        us += 1000000
    t = int(t)
    days, rem = divmod(t, 86400)
    hh, rem = divmod(rem, 3600)
    mm, ss = divmod(rem, 60)
    y, m, d = _ord2ymd(_EPOCH_ORD + days)
    return (y, m, d, hh, mm, ss, us)


class time:
    def __init__(self, hour=0, minute=0, second=0, microsecond=0, tzinfo=None, fold=0):
        _check_time_fields(hour, minute, second, microsecond, fold)
        _check_tzinfo_arg(tzinfo)
        self.hour = hour
        self.minute = minute
        self.second = second
        self.microsecond = microsecond
        self.tzinfo = tzinfo
        self.fold = fold

    def _tuple(self):
        return (self.hour, self.minute, self.second, self.microsecond)

    def _cmp_check(self, other, op):
        if not isinstance(other, time):
            raise TypeError("'%s' not supported between instances of 'datetime.time' and '%s'" % (op, _tname(other)))
        if (self.utcoffset() is None) != (other.utcoffset() is None):
            raise TypeError("can't compare offset-naive and offset-aware times")

    def _key(self):
        off = self.utcoffset()
        if off is None:
            return self._tuple()
        base = timedelta(hours=self.hour, minutes=self.minute, seconds=self.second, microseconds=self.microsecond) - off
        return (base.days, base.seconds, base.microseconds)

    def __eq__(self, other):
        if not isinstance(other, time):
            return False
        if (self.utcoffset() is None) != (other.utcoffset() is None):
            return False
        return self._key() == other._key()

    def __ne__(self, other):
        return not self.__eq__(other)

    def __lt__(self, other):
        self._cmp_check(other, "<")
        return self._key() < other._key()

    def __le__(self, other):
        self._cmp_check(other, "<=")
        return self._key() <= other._key()

    def __gt__(self, other):
        self._cmp_check(other, ">")
        return self._key() > other._key()

    def __ge__(self, other):
        self._cmp_check(other, ">=")
        return self._key() >= other._key()

    def __hash__(self):
        return hash(("time", self._key()))

    def utcoffset(self):
        if self.tzinfo is None:
            return None
        return self.tzinfo.utcoffset(None)

    def tzname(self):
        if self.tzinfo is None:
            return None
        return self.tzinfo.tzname(None)

    def dst(self):
        if self.tzinfo is None:
            return None
        return self.tzinfo.dst(None)

    def isoformat(self, timespec="auto"):
        s = _format_time(self.hour, self.minute, self.second, self.microsecond, timespec)
        tz = _format_offset(self.utcoffset())
        if tz:
            s += tz
        return s

    def __str__(self):
        return self.isoformat()

    def __repr__(self):
        if self.microsecond != 0:
            s = ", %d, %d" % (self.second, self.microsecond)
        elif self.second != 0:
            s = ", %d" % self.second
        else:
            s = ""
        s = "datetime.time(%d, %d%s)" % (self.hour, self.minute, s)
        if self.tzinfo is not None:
            s = s[:-1] + ", tzinfo=%r" % self.tzinfo + ")"
        if self.fold:
            s = s[:-1] + ", fold=1)"
        return s

    def strftime(self, fmt):
        return _wrap_strftime(fmt, 1900, 1, 1, self.hour, self.minute, self.second, self.microsecond, self.utcoffset(), self.tzname())

    def __format__(self, fmt):
        if len(fmt) != 0:
            return self.strftime(fmt)
        return str(self)

    def replace(self, hour=None, minute=None, second=None, microsecond=None, tzinfo=True, fold=None):
        if hour is None:
            hour = self.hour
        if minute is None:
            minute = self.minute
        if second is None:
            second = self.second
        if microsecond is None:
            microsecond = self.microsecond
        if tzinfo is True:
            tzinfo = self.tzinfo
        if fold is None:
            fold = self.fold
        return type(self)(hour, minute, second, microsecond, tzinfo, fold)

    @classmethod
    def fromisoformat(cls, time_string):
        if not isinstance(time_string, str):
            raise TypeError("fromisoformat: argument must be str")
        try:
            hh, mm, ss, us, tzi = _parse_isoformat_time(time_string)
        except Exception:
            raise ValueError("Invalid isoformat string: %r" % time_string)
        return cls(hh, mm, ss, us, tzi)

    def __bool__(self):
        return True


time.min = time(0, 0, 0)
time.max = time(23, 59, 59, 999999)
time.resolution = timedelta(microseconds=1)


class datetime(date):
    def __init__(self, year, month, day, hour=0, minute=0, second=0, microsecond=0, tzinfo=None, fold=0):
        _check_date_fields(year, month, day)
        _check_time_fields(hour, minute, second, microsecond, fold)
        _check_tzinfo_arg(tzinfo)
        self.year = year
        self.month = month
        self.day = day
        self.hour = hour
        self.minute = minute
        self.second = second
        self.microsecond = microsecond
        self.tzinfo = tzinfo
        self.fold = fold

    @classmethod
    def _fromtimestamp(cls, t, utc, tz):
        y, m, d, hh, mm, ss, us = _fields_from_timestamp(t)
        result = cls(y, m, d, hh, mm, ss, us, tz)
        if tz is not None:
            result = tz.fromutc(result)
        return result

    @classmethod
    def fromtimestamp(cls, t, tz=None):
        _check_tzinfo_arg(tz)
        return cls._fromtimestamp(t, tz is not None, tz)

    @classmethod
    def utcfromtimestamp(cls, t):
        return cls._fromtimestamp(t, True, None)

    @classmethod
    def now(cls, tz=None):
        return cls.fromtimestamp(_time.time(), tz)

    @classmethod
    def utcnow(cls):
        return cls.utcfromtimestamp(_time.time())

    @classmethod
    def today(cls):
        return cls.fromtimestamp(_time.time())

    @classmethod
    def combine(cls, date_, time_, tzinfo=True):
        if not isinstance(date_, date):
            raise TypeError("date argument must be a date instance")
        if not isinstance(time_, time):
            raise TypeError("time argument must be a time instance")
        if tzinfo is True:
            tzinfo = time_.tzinfo
        return cls(date_.year, date_.month, date_.day, time_.hour, time_.minute, time_.second, time_.microsecond, tzinfo, fold=time_.fold)

    @classmethod
    def fromisoformat(cls, date_string):
        if not isinstance(date_string, str):
            raise TypeError("fromisoformat: argument must be str")
        if len(date_string) < 7:
            raise ValueError("Invalid isoformat string: %r" % date_string)
        # Split date and time parts, if a time part is present.
        try:
            separator_location = 10 if len(date_string) > 10 and date_string[4] == "-" else (8 if len(date_string) > 8 and date_string[:8].isdigit() else -1)
            if separator_location == 10 and date_string[5] == "W":
                separator_location = 10 if len(date_string) > 10 else -1
            if separator_location > 0 and len(date_string) > separator_location:
                dstr = date_string[:separator_location]
                tstr = date_string[separator_location + 1:]
                if date_string[separator_location] not in "T t":
                    raise ValueError("bad separator")
            else:
                dstr = date_string
                tstr = None
            y, m, d = _parse_isoformat_date(dstr)
            if tstr:
                hh, mm, ss, us, tzi = _parse_isoformat_time(tstr)
            else:
                hh, mm, ss, us, tzi = 0, 0, 0, 0, None
        except Exception:
            raise ValueError("Invalid isoformat string: %r" % date_string)
        return cls(y, m, d, hh, mm, ss, us, tzi)

    @classmethod
    def strptime(cls, date_string, fmt):
        f = _time._strptime_fields(date_string, fmt)
        tzi = None
        if f[7] is not None:
            tzi = timezone(timedelta(seconds=f[7]), f[8]) if f[8] not in (None, "", "UTC", "GMT") else timezone(timedelta(seconds=f[7]))
        return cls(f[0], f[1], f[2], f[3], f[4], f[5], f[6], tzi)

    def timetuple(self):
        dst = self.dst()
        if dst is None:
            dst = -1
        elif dst:
            dst = 1
        else:
            dst = 0
        return _build_struct_time(self.year, self.month, self.day, self.hour, self.minute, self.second, dst)

    def _mktime(self):
        epoch = datetime(1970, 1, 1)
        return (self - epoch) // timedelta(0, 1)

    def timestamp(self):
        if self.tzinfo is None:
            s = self._mktime()
            return s + self.microsecond / 1e6
        return (self - _EPOCH).total_seconds()

    def utctimetuple(self):
        offset = self.utcoffset()
        self_ = self
        if offset:
            self_ = self - offset
        return _build_struct_time(self_.year, self_.month, self_.day, self_.hour, self_.minute, self_.second, 0)

    def date(self):
        return date(self.year, self.month, self.day)

    def time(self):
        return time(self.hour, self.minute, self.second, self.microsecond, fold=self.fold)

    def timetz(self):
        return time(self.hour, self.minute, self.second, self.microsecond, self.tzinfo, fold=self.fold)

    def replace(self, year=None, month=None, day=None, hour=None, minute=None, second=None, microsecond=None, tzinfo=True, fold=None):
        if year is None:
            year = self.year
        if month is None:
            month = self.month
        if day is None:
            day = self.day
        if hour is None:
            hour = self.hour
        if minute is None:
            minute = self.minute
        if second is None:
            second = self.second
        if microsecond is None:
            microsecond = self.microsecond
        if tzinfo is True:
            tzinfo = self.tzinfo
        if fold is None:
            fold = self.fold
        return type(self)(year, month, day, hour, minute, second, microsecond, tzinfo, fold)

    def astimezone(self, tz=None):
        if tz is None:
            tz = timezone.utc
        elif not isinstance(tz, tzinfo):
            raise TypeError("tz argument must be an instance of tzinfo")
        mytz = self.tzinfo
        if mytz is None:
            myoffset = timedelta(0)
        else:
            myoffset = mytz.utcoffset(self)
            if myoffset is None:
                myoffset = timedelta(0)
        if tz is mytz:
            return self
        utc = (self - myoffset).replace(tzinfo=tz)
        return tz.fromutc(utc)

    def ctime(self):
        weekday = self.toordinal() % 7 or 7
        return "%s %s %2d %02d:%02d:%02d %04d" % (
            _DAY_NAMES[weekday - 1][:3], _MONTH_NAMES[self.month][:3], self.day, self.hour, self.minute, self.second, self.year)

    def isoformat(self, sep="T", timespec="auto"):
        s = "%04d-%02d-%02d%s" % (self.year, self.month, self.day, sep) + _format_time(self.hour, self.minute, self.second, self.microsecond, timespec)
        off = self.utcoffset()
        tz = _format_offset(off)
        if tz:
            s += tz
        return s

    def __repr__(self):
        L = [self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond]
        if L[-1] == 0:
            del L[-1]
        if L[-1] == 0:
            del L[-1]
        s = "datetime.datetime(%s)" % ", ".join(map(str, L))
        if self.tzinfo is not None:
            s = s[:-1] + ", tzinfo=%r" % self.tzinfo + ")"
        if self.fold:
            s = s[:-1] + ", fold=1)"
        return s

    def __str__(self):
        return self.isoformat(sep=" ")

    def strftime(self, fmt):
        return _wrap_strftime(fmt, self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond, self.utcoffset(), self.tzname())

    def __format__(self, fmt):
        if len(fmt) != 0:
            return self.strftime(fmt)
        return str(self)

    def utcoffset(self):
        if self.tzinfo is None:
            return None
        return self.tzinfo.utcoffset(self)

    def tzname(self):
        if self.tzinfo is None:
            return None
        return self.tzinfo.tzname(self)

    def dst(self):
        if self.tzinfo is None:
            return None
        return self.tzinfo.dst(self)

    def _key(self):
        off = self.utcoffset()
        base = (self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond)
        if off is None:
            return base
        naive = datetime(self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond) - off
        return (naive.year, naive.month, naive.day, naive.hour, naive.minute, naive.second, naive.microsecond)

    def _cmp_ok(self, other):
        return isinstance(other, datetime)

    def __eq__(self, other):
        if not isinstance(other, datetime):
            return False
        if (self.utcoffset() is None) != (other.utcoffset() is None):
            return False
        return self._key() == other._key()

    def __ne__(self, other):
        return not self.__eq__(other)

    def _cmp_check(self, other, op):
        if not isinstance(other, datetime):
            raise TypeError("'%s' not supported between instances of 'datetime.datetime' and '%s'" % (op, _tname(other)))
        if (self.utcoffset() is None) != (other.utcoffset() is None):
            raise TypeError("can't compare offset-naive and offset-aware datetimes")

    def __lt__(self, other):
        self._cmp_check(other, "<")
        return self._key() < other._key()

    def __le__(self, other):
        self._cmp_check(other, "<=")
        return self._key() <= other._key()

    def __gt__(self, other):
        self._cmp_check(other, ">")
        return self._key() > other._key()

    def __ge__(self, other):
        self._cmp_check(other, ">=")
        return self._key() >= other._key()

    def __hash__(self):
        return hash(("datetime", self._key()))

    def __add__(self, other):
        if not isinstance(other, timedelta):
            raise TypeError("unsupported operand type(s) for +: 'datetime.datetime' and '%s'" % _tname(other))
        delta = timedelta(self.toordinal(), hours=self.hour, minutes=self.minute, seconds=self.second, microseconds=self.microsecond)
        delta += other
        hour, rem = divmod(delta.seconds, 3600)
        minute, second = divmod(rem, 60)
        if 0 < delta.days <= 3652059:
            return type(self).combine(date.fromordinal(delta.days), time(hour, minute, second, delta.microseconds, tzinfo=self.tzinfo))
        raise OverflowError("date value out of range")

    def __radd__(self, other):
        return self.__add__(other)

    def __sub__(self, other):
        if not isinstance(other, datetime):
            if isinstance(other, timedelta):
                return self + -other
            raise TypeError("unsupported operand type(s) for -: 'datetime.datetime' and '%s'" % _tname(other))
        days1 = self.toordinal()
        days2 = other.toordinal()
        secs1 = self.second + self.minute * 60 + self.hour * 3600
        secs2 = other.second + other.minute * 60 + other.hour * 3600
        base = timedelta(days1 - days2, secs1 - secs2, self.microsecond - other.microsecond)
        if self.tzinfo is other.tzinfo:
            return base
        myoff = self.utcoffset()
        otoff = other.utcoffset()
        if myoff == otoff:
            return base
        if myoff is None or otoff is None:
            raise TypeError(
                "can't subtract offset-naive and offset-aware datetimes"
            )
        return base + otoff - myoff


datetime.min = datetime(1, 1, 1)
datetime.max = datetime(9999, 12, 31, 23, 59, 59, 999999)
datetime.resolution = timedelta(microseconds=1)
_EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)
