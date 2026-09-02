# `random` — CPython 3.13's random.py transcribed for the VM. The Mersenne
# Twister core (`_random.Random`) is native: `_rng_new`, `_rng_seed`,
# `_rng_random`, `_rng_getrandbits`, `_rng_getstate`, `_rng_setstate` act on
# a state slot by id; slot 0 is the module-level generator. Everything built
# on top of `random()` / `getrandbits()` is the stdlib algorithm to the
# letter, so a seeded program draws the same sequence under `tyc run` and
# `tyc build && python`.

from math import log as _log, exp as _exp, pi as _pi, e as _e, ceil as _ceil
from math import sqrt as _sqrt, acos as _acos, cos as _cos, sin as _sin
from math import tau as TWOPI, floor as _floor, isfinite as _isfinite

NV_MAGICCONST = 4 * _exp(-0.5) / _sqrt(2.0)
LOG4 = _log(4.0)
SG_MAGICCONST = 1.0 + _log(4.5)
BPF = 53
RECIP_BPF = 2 ** -BPF


def _index(x):
    if isinstance(x, bool):
        return int(x)
    if isinstance(x, int):
        return x
    raise TypeError("'%s' object cannot be interpreted as an integer" % type(x).__name__)


def _accumulate(xs):
    out = []
    total = None
    for x in xs:
        total = x if total is None else total + x
        out.append(total)
    return out


def _bisect(a, x, lo=0, hi=None):
    if hi is None:
        hi = len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        if x < a[mid]:
            hi = mid
        else:
            lo = mid + 1
    return lo


class Random:
    VERSION = 3

    def __init__(self, x=None, _state=None):
        if _state is None:
            self._id = _rng_new()
            self.seed(x)
        else:
            self._id = _state
            self.gauss_next = None

    def seed(self, a=None, version=2):
        if version == 1 and isinstance(a, (str, bytes)):
            a = a.decode("latin-1") if isinstance(a, bytes) else a
            x = ord(a[0]) << 7 if a else 0
            for c in map(ord, a):
                x = ((1000003 * x) ^ c) & 0xFFFFFFFFFFFFFFFF
            x ^= len(a)
            a = -2 if x == -1 else x
        elif not (a is None or isinstance(a, (int, float, str, bytes))):
            raise TypeError("The only supported seed types are:\n"
                            "None, int, float, str, bytes, and bytearray.")
        _rng_seed(self._id, a)
        self.gauss_next = None

    def getstate(self):
        return self.VERSION, _rng_getstate(self._id), self.gauss_next

    def setstate(self, state):
        version = state[0]
        if version == 3:
            version, internalstate, self.gauss_next = state
            _rng_setstate(self._id, internalstate)
        else:
            raise ValueError("state with version %s passed to "
                             "Random.setstate() of version %s" %
                             (version, self.VERSION))

    def random(self):
        return _rng_random(self._id)

    def getrandbits(self, k):
        return _rng_getrandbits(self._id, k)

    def _randbelow(self, n):
        getrandbits = self.getrandbits
        k = n.bit_length()
        r = getrandbits(k)
        while r >= n:
            r = getrandbits(k)
        return r

    def randbytes(self, n):
        return self.getrandbits(n * 8).to_bytes(n, "little")

    def randrange(self, start, stop=None, step=1):
        istart = _index(start)
        if stop is None:
            if step != 1:
                raise TypeError("Missing a non-None stop argument")
            if istart > 0:
                return self._randbelow(istart)
            raise ValueError("empty range for randrange()")
        istop = _index(stop)
        width = istop - istart
        istep = _index(step)
        if istep == 1:
            if width > 0:
                return istart + self._randbelow(width)
            raise ValueError("empty range in randrange(%s, %s)" % (start, stop))
        if istep > 0:
            n = (width + istep - 1) // istep
        elif istep < 0:
            n = (width + istep + 1) // istep
        else:
            raise ValueError("zero step for randrange()")
        if n <= 0:
            raise ValueError("empty range in randrange(%s, %s, %s)" % (start, stop, step))
        return istart + istep * self._randbelow(n)

    def randint(self, a, b):
        return self.randrange(a, b + 1)

    def choice(self, seq):
        if not len(seq):
            raise IndexError("Cannot choose from an empty sequence")
        return seq[self._randbelow(len(seq))]

    def shuffle(self, x):
        randbelow = self._randbelow
        for i in reversed(range(1, len(x))):
            j = randbelow(i + 1)
            x[i], x[j] = x[j], x[i]

    def sample(self, population, k, *, counts=None):
        if not isinstance(population, (list, tuple, str, range, bytes)):
            raise TypeError("Population must be a sequence.  "
                            "For dicts or sets, use sorted(d).")
        n = len(population)
        if counts is not None:
            cum_counts = _accumulate(counts)
            if len(cum_counts) != n:
                raise ValueError("The number of counts does not match the population")
            total = cum_counts.pop()
            if not isinstance(total, int):
                raise TypeError("Counts must be integers")
            if total <= 0:
                raise ValueError("Total of counts must be greater than zero")
            selections = self.sample(range(total), k=k)
            return [population[_bisect(cum_counts, s)] for s in selections]
        randbelow = self._randbelow
        if not 0 <= k <= n:
            raise ValueError("Sample larger than population or is negative")
        result = [None] * k
        setsize = 21
        if k > 5:
            setsize += 4 ** _ceil(_log(k * 3, 4))
        if n <= setsize:
            pool = list(population)
            for i in range(k):
                j = randbelow(n - i)
                result[i] = pool[j]
                pool[j] = pool[n - i - 1]
        else:
            selected = set()
            selected_add = selected.add
            for i in range(k):
                j = randbelow(n)
                while j in selected:
                    j = randbelow(n)
                selected_add(j)
                result[i] = population[j]
        return result

    def choices(self, population, weights=None, *, cum_weights=None, k=1):
        random = self.random
        n = len(population)
        if cum_weights is None:
            if weights is None:
                n += 0.0
                return [population[_floor(random() * n)] for i in range(k)]
            if isinstance(weights, int):
                raise TypeError("The number of choices must be a keyword argument: k=%s" % weights)
            cum_weights = _accumulate(weights)
        elif weights is not None:
            raise TypeError("Cannot specify both weights and cumulative weights")
        if len(cum_weights) != n:
            raise ValueError("The number of weights does not match the population")
        total = cum_weights[-1] + 0.0
        if total <= 0.0:
            raise ValueError("Total of weights must be greater than zero")
        if not _isfinite(total):
            raise ValueError("Total of weights must be finite")
        hi = n - 1
        return [population[_bisect(cum_weights, random() * total, 0, hi)] for i in range(k)]

    def uniform(self, a, b):
        return a + (b - a) * self.random()

    def triangular(self, low=0.0, high=1.0, mode=None):
        u = self.random()
        try:
            c = 0.5 if mode is None else (mode - low) / (high - low)
        except ZeroDivisionError:
            return low
        if u > c:
            u = 1.0 - u
            c = 1.0 - c
            low, high = high, low
        return low + (high - low) * _sqrt(u * c)

    def normalvariate(self, mu=0.0, sigma=1.0):
        random = self.random
        while True:
            u1 = random()
            u2 = 1.0 - random()
            z = NV_MAGICCONST * (u1 - 0.5) / u2
            zz = z * z / 4.0
            if zz <= -_log(u2):
                break
        return mu + z * sigma

    def gauss(self, mu=0.0, sigma=1.0):
        random = self.random
        z = self.gauss_next
        self.gauss_next = None
        if z is None:
            x2pi = random() * TWOPI
            g2rad = _sqrt(-2.0 * _log(1.0 - random()))
            z = _cos(x2pi) * g2rad
            self.gauss_next = _sin(x2pi) * g2rad
        return mu + z * sigma

    def lognormvariate(self, mu, sigma):
        return _exp(self.normalvariate(mu, sigma))

    def expovariate(self, lambd=1.0):
        return -_log(1.0 - self.random()) / lambd

    def vonmisesvariate(self, mu, kappa):
        random = self.random
        if kappa <= 1e-6:
            return TWOPI * random()
        s = 0.5 / kappa
        r = s + _sqrt(1.0 + s * s)
        while True:
            u1 = random()
            z = _cos(_pi * u1)
            d = z / (r + z)
            u2 = random()
            if u2 < 1.0 - d * d or u2 <= (1.0 - d) * _exp(d):
                break
        q = 1.0 / r
        f = (q + z) / (1.0 + q * z)
        u3 = random()
        if u3 > 0.5:
            theta = (mu + _acos(f)) % TWOPI
        else:
            theta = (mu - _acos(f)) % TWOPI
        return theta

    def gammavariate(self, alpha, beta):
        if alpha <= 0.0 or beta <= 0.0:
            raise ValueError("gammavariate: alpha and beta must be > 0.0")
        random = self.random
        if alpha > 1.0:
            ainv = _sqrt(2.0 * alpha - 1.0)
            bbb = alpha - LOG4
            ccc = alpha + ainv
            while True:
                u1 = random()
                if not 1e-7 < u1 < 0.9999999:
                    continue
                u2 = 1.0 - random()
                v = _log(u1 / (1.0 - u1)) / ainv
                x = alpha * _exp(v)
                z = u1 * u1 * u2
                r = bbb + ccc * v - x
                if r + SG_MAGICCONST - 4.5 * z >= 0.0 or r >= _log(z):
                    return x * beta
        elif alpha == 1.0:
            return -_log(1.0 - random()) * beta
        else:
            while True:
                u = random()
                b = (_e + alpha) / _e
                p = b * u
                if p <= 1.0:
                    x = p ** (1.0 / alpha)
                else:
                    x = -_log((b - p) / alpha)
                u1 = random()
                if p > 1.0:
                    if u1 <= x ** (alpha - 1.0):
                        break
                elif u1 <= _exp(-x):
                    break
            return x * beta

    def betavariate(self, alpha, beta):
        y = self.gammavariate(alpha, 1.0)
        if y:
            return y / (y + self.gammavariate(beta, 1.0))
        return 0.0

    def binomialvariate(self, n=1, p=0.5):
        if n < 0:
            raise ValueError("n must be non-negative")
        if p <= 0.0 or p >= 1.0:
            if p == 0.0:
                return 0
            if p == 1.0:
                return n
            raise ValueError("p must be in the range 0.0 <= p <= 1.0")
        random = self.random
        if n == 1:
            return _index(random() < p)
        if p > 0.5:
            return n - self.binomialvariate(n, 1.0 - p)
        if n * p < 10.0:
            x = y = 0
            c = _log(1.0 - p)
            if not c:
                return x
            while True:
                y += _floor(_log(random()) / c) + 1
                if y > n:
                    return x
                x += 1
        raise NotImplementedError("binomialvariate with n*p >= 10 needs math.lgamma, "
                                  "which tyc run does not model")

    def paretovariate(self, alpha):
        u = 1.0 - self.random()
        return u ** (-1.0 / alpha)

    def weibullvariate(self, alpha, beta):
        u = 1.0 - self.random()
        return alpha * (-_log(u)) ** (1.0 / beta)


_inst = Random(_state=0)
seed = _inst.seed
random = _inst.random
uniform = _inst.uniform
triangular = _inst.triangular
randint = _inst.randint
choice = _inst.choice
randrange = _inst.randrange
sample = _inst.sample
shuffle = _inst.shuffle
choices = _inst.choices
normalvariate = _inst.normalvariate
lognormvariate = _inst.lognormvariate
expovariate = _inst.expovariate
vonmisesvariate = _inst.vonmisesvariate
gammavariate = _inst.gammavariate
gauss = _inst.gauss
betavariate = _inst.betavariate
binomialvariate = _inst.binomialvariate
paretovariate = _inst.paretovariate
weibullvariate = _inst.weibullvariate
getstate = _inst.getstate
setstate = _inst.setstate
getrandbits = _inst.getrandbits
randbytes = _inst.randbytes
