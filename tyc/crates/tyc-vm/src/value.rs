//! Value representation for the Typhon VM.
//!
//! Values are reference-counted (single-threaded `Rc`) so cheap clones don't
//! deep-copy containers — matching Python semantics where `a = b` aliases
//! mutable containers.
//!
//! Numeric ints use `num_bigint::BigInt` to match Python's arbitrary-precision
//! semantics (FINDINGS #19). Before this, the VM stored ints as `i64` and
//! tripped `OverflowError` on programs like `2 ** 100` that worked fine
//! under CPython — making `tyc run` diverge from `tyc build && python`
//! for any program that does big-number arithmetic.

use std::borrow::Cow;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use indexmap::IndexMap;
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{FromPrimitive, Signed, ToPrimitive};

use crate::error::{type_error, value_error, Unwind};

/// Maximum structural-recursion depth for the value comparison/ordering
/// routines (`py_eq` / `py_cmp`). Cyclic containers would otherwise recurse
/// without bound and overflow the native stack, aborting the process. The
/// bound is far deeper than any realistic data structure but shallow enough to
/// stay well within the VM's worker stack.
/// It is CPython 3.13's: comparing two lists nested 10,000 deep raises
/// `RecursionError` there (see `Interpreter::eq_values`).
const MAX_STRUCTURAL_DEPTH: usize = 9_999;

thread_local! {
    static STRUCTURAL_DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Set when a structural walk hit the depth bound, so a caller that can
    /// raise (an ordering comparison) reports CPython's `RecursionError`
    /// instead of the "not supported" a bare `None` would suggest.
    static STRUCTURAL_OVERFLOW: Cell<bool> = const { Cell::new(false) };
}

/// Whether a structural walk hit the depth bound since the last call
/// (clearing the flag).
pub(crate) fn take_structural_overflow() -> bool {
    STRUCTURAL_OVERFLOW.with(|f| f.replace(false))
}

/// RAII guard that decrements the structural-recursion depth on drop. Using a
/// guard (rather than a manual paired call) means a panic partway through a
/// comparison can't leave the thread-local counter stuck incremented — which
/// would otherwise make every later comparison on that thread spuriously bail
/// once the (now-unreachable) limit is hit. Thread-locals persist across tasks
/// on a reused thread, so this matters in the LSP / test harness where a
/// panic may be caught.
pub(crate) struct StructuralDepthGuard;

impl Drop for StructuralDepthGuard {
    fn drop(&mut self) {
        STRUCTURAL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Enter one level of structural recursion, returning a guard that restores the
/// depth on drop. Returns `None` when the depth bound has been reached (the
/// caller should bail without recursing).
pub(crate) fn structural_depth_enter() -> Option<StructuralDepthGuard> {
    STRUCTURAL_DEPTH.with(|d| {
        let cur = d.get();
        if cur >= MAX_STRUCTURAL_DEPTH {
            STRUCTURAL_OVERFLOW.with(|f| f.set(true));
            None
        } else {
            d.set(cur + 1);
            Some(StructuralDepthGuard)
        }
    })
}
use ruff_python_ast::{Parameters, Stmt};

/// Reference-counted, interior-mutable list. Cloning a `Value::List` aliases
/// the same storage.
pub type RcList = Rc<RefCell<Vec<Value>>>;
/// Dicts preserve insertion order on iteration — matching CPython 3.7+
/// semantics (FINDINGS #18) — and delete in O(1); see `pydict`.
pub type DictMap = crate::pydict::PyDict;
/// Container storage with metadata outside the user-visible contents.
/// Clones of a Value share this flag along with the underlying storage.
#[derive(Debug)]
pub struct FrozenCell<T> {
    values: RefCell<T>,
    pub frozen: std::cell::Cell<bool>,
}
impl<T> FrozenCell<T> {
    pub fn new(values: T) -> Self {
        Self {
            values: RefCell::new(values),
            frozen: std::cell::Cell::new(false),
        }
    }
    pub fn frozen(values: T) -> Self {
        Self {
            values: RefCell::new(values),
            frozen: std::cell::Cell::new(true),
        }
    }
}
impl<T> std::ops::Deref for FrozenCell<T> {
    type Target = RefCell<T>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
pub type RcDict = Rc<crate::value::FrozenCell<DictMap>>;
pub type RcSet = Rc<crate::value::FrozenCell<crate::pyset::PySet>>;
#[derive(Clone, Copy)]
pub struct VmFloat {
    pub value: f64,
    pub identity: u64,
}
impl VmFloat {
    fn new(value: f64) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            value,
            identity: if value.is_nan() {
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            } else {
                0
            },
        }
    }
}
impl std::ops::Deref for VmFloat {
    type Target = f64;
    fn deref(&self) -> &f64 {
        &self.value
    }
}
impl std::fmt::Debug for VmFloat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}
impl PartialEq for VmFloat {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl PartialOrd for VmFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.value.partial_cmp(&other.value)
    }
}
impl PartialEq<f64> for VmFloat {
    fn eq(&self, other: &f64) -> bool {
        self.value == *other
    }
}
impl PartialOrd<f64> for VmFloat {
    fn partial_cmp(&self, other: &f64) -> Option<std::cmp::Ordering> {
        self.value.partial_cmp(other)
    }
}
pub type RcStr = Rc<String>;

/// The VM's arbitrary-precision integer, with a small-value fast path.
///
/// `Value::Int` used to be a bare `num_bigint::BigInt`, so every integer op —
/// even `i + 1` in a tight loop — allocated a fresh heap `BigInt`. `VmInt`
/// keeps CPython's exact arbitrary-precision semantics while representing any
/// value that fits in `i64` inline, so the common case never touches the heap.
///
/// # Invariant
///
/// `Small(n)` is used for **every** value in `i64` range; `Big` only ever holds
/// a value strictly outside `[i64::MIN, i64::MAX]`. Every constructor and every
/// arithmetic result funnels through [`VmInt::from_bigint`], which demotes an
/// in-range `BigInt` back to `Small`. This canonicalisation is what makes
/// `Eq` / `Ord` / hashing trivial: a `Small` and a `Big` can never be
/// numerically equal, so cross-representation comparison is always `false` /
/// decided-by-sign, with no value inspection.
#[derive(Clone)]
pub enum VmInt {
    Small(i64),
    /// Held behind an `Rc` so cloning a huge integer is a refcount bump, not a
    /// limb-vector copy. Always outside `i64` range (see the type invariant).
    Big(Rc<BigInt>),
}

impl VmInt {
    /// Normalising constructor — the single choke point that upholds the type
    /// invariant. An in-range `BigInt` becomes `Small`; anything else `Big`.
    #[inline]
    pub fn from_bigint(b: BigInt) -> VmInt {
        match b.to_i64() {
            Some(n) => VmInt::Small(n),
            None => VmInt::Big(Rc::new(b)),
        }
    }

    /// Borrow as a `BigInt` without allocating when already `Big`. Used by the
    /// cold arithmetic / formatting paths that want the full `BigInt` API.
    #[inline]
    pub fn as_bigint(&self) -> Cow<'_, BigInt> {
        match self {
            VmInt::Small(n) => Cow::Owned(BigInt::from(*n)),
            VmInt::Big(b) => Cow::Borrowed(b),
        }
    }

    /// Owned `BigInt` copy (allocates for `Small`; clones the limbs for `Big`).
    #[inline]
    pub fn to_bigint(&self) -> BigInt {
        match self {
            VmInt::Small(n) => BigInt::from(*n),
            VmInt::Big(b) => (**b).clone(),
        }
    }

    /// `Some(i64)` iff the value fits `i64` — always `Some` for `Small`, always
    /// `None` for `Big` (the invariant guarantees `Big` is out of range).
    #[inline]
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            VmInt::Small(n) => Some(*n),
            VmInt::Big(_) => None,
        }
    }

    #[inline]
    pub fn to_usize(&self) -> Option<usize> {
        match self {
            VmInt::Small(n) => usize::try_from(*n).ok(),
            // A positive `Big` may still fit `usize` (e.g. 2^63 on a 64-bit
            // target), so defer to the `BigInt` conversion here.
            VmInt::Big(b) => b.to_usize(),
        }
    }

    #[inline]
    pub fn to_u32(&self) -> Option<u32> {
        match self {
            VmInt::Small(n) => u32::try_from(*n).ok(),
            VmInt::Big(b) => b.to_u32(),
        }
    }

    /// Lossy `f64` conversion matching CPython's quiet `int → float` down-cast.
    #[inline]
    pub fn to_f64(&self) -> f64 {
        match self {
            VmInt::Small(n) => *n as f64,
            VmInt::Big(b) => bigint_to_f64(b),
        }
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        // `Big` is never in `i64` range, so never zero.
        matches!(self, VmInt::Small(0))
    }

    #[inline]
    pub fn is_negative(&self) -> bool {
        match self {
            VmInt::Small(n) => *n < 0,
            VmInt::Big(b) => b.is_negative(),
        }
    }

    #[inline]
    pub fn is_positive(&self) -> bool {
        match self {
            VmInt::Small(n) => *n > 0,
            VmInt::Big(b) => b.is_positive(),
        }
    }

    /// Number of bits in the minimal representation of the absolute value —
    /// matches `BigInt::bits` (and backs `int.bit_length()`).
    #[inline]
    pub fn bits(&self) -> u64 {
        match self {
            VmInt::Small(0) => 0,
            VmInt::Small(n) => 64 - n.unsigned_abs().leading_zeros() as u64,
            VmInt::Big(b) => b.bits(),
        }
    }

    /// Decimal (or arbitrary-radix) string, matching `BigInt::to_str_radix`.
    /// The radix-10 `Small` case — by far the hottest, used by every `print` /
    /// `str()` / f-string — formats the `i64` directly without a `BigInt`.
    pub fn to_str_radix(&self, radix: u32) -> String {
        match self {
            VmInt::Small(n) if radix == 10 => n.to_string(),
            VmInt::Small(n) => BigInt::from(*n).to_str_radix(radix),
            VmInt::Big(b) => b.to_str_radix(radix),
        }
    }

    pub fn abs(&self) -> VmInt {
        match self {
            // `i64::MIN.checked_abs()` is `None` — that magnitude is `Big`.
            VmInt::Small(n) => match n.checked_abs() {
                Some(v) => VmInt::Small(v),
                None => VmInt::from_bigint(BigInt::from(*n).abs()),
            },
            VmInt::Big(b) => VmInt::from_bigint(b.abs()),
        }
    }

    #[inline]
    pub fn add(&self, other: &VmInt) -> VmInt {
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            if let Some(v) = a.checked_add(*b) {
                return VmInt::Small(v);
            }
        }
        VmInt::from_bigint(&*self.as_bigint() + &*other.as_bigint())
    }

    #[inline]
    pub fn sub(&self, other: &VmInt) -> VmInt {
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            if let Some(v) = a.checked_sub(*b) {
                return VmInt::Small(v);
            }
        }
        VmInt::from_bigint(&*self.as_bigint() - &*other.as_bigint())
    }

    #[inline]
    pub fn mul(&self, other: &VmInt) -> VmInt {
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            if let Some(v) = a.checked_mul(*b) {
                return VmInt::Small(v);
            }
        }
        VmInt::from_bigint(&*self.as_bigint() * &*other.as_bigint())
    }

    /// Floor division (rounds toward negative infinity, like Python `//` and
    /// `BigInt::div_floor`). The caller guarantees `other` is non-zero.
    pub fn div_floor(&self, other: &VmInt) -> VmInt {
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            // `checked_*` guards the single overflow case, `i64::MIN / -1`.
            if let (Some(q), Some(r)) = (a.checked_div(*b), a.checked_rem(*b)) {
                // Adjust the truncating quotient toward -inf when the remainder
                // is non-zero and its sign differs from the divisor's. When the
                // adjustment fires, `|q| < 2^63`, so `q - 1` cannot overflow.
                let q = if r != 0 && ((r < 0) != (*b < 0)) {
                    q - 1
                } else {
                    q
                };
                return VmInt::Small(q);
            }
        }
        VmInt::from_bigint(self.as_bigint().div_floor(&other.as_bigint()))
    }

    /// Python `%` — result takes the sign of the divisor (like
    /// `BigInt::mod_floor`). The caller guarantees `other` is non-zero.
    pub fn mod_floor(&self, other: &VmInt) -> VmInt {
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            if let Some(r) = a.checked_rem(*b) {
                // `r` has the dividend's sign; nudge toward the divisor's sign.
                // `r + b` cannot overflow: `|r| < |b|`, both `i64`.
                let m = if r != 0 && ((r < 0) != (*b < 0)) {
                    r + *b
                } else {
                    r
                };
                return VmInt::Small(m);
            }
            // `i64::MIN % -1` overflows `checked_rem`; the true result is 0,
            // which the `BigInt` path below computes correctly.
        }
        VmInt::from_bigint(self.as_bigint().mod_floor(&other.as_bigint()))
    }

    /// `self ** exp` for a non-negative exponent.
    pub fn pow(&self, exp: u32) -> VmInt {
        if let VmInt::Small(a) = self {
            if let Some(v) = a.checked_pow(exp) {
                return VmInt::Small(v);
            }
        }
        VmInt::from_bigint(self.as_bigint().pow(exp))
    }

    /// Three-argument modular exponentiation (`pow(base, exp, modulus)`).
    pub fn modpow(&self, exp: &VmInt, modulus: &VmInt) -> VmInt {
        VmInt::from_bigint(
            self.as_bigint()
                .modpow(&exp.as_bigint(), &modulus.as_bigint()),
        )
    }

    #[inline]
    pub fn bitand(&self, other: &VmInt) -> VmInt {
        // Bitwise ops on two `i64`s stay within `i64` (the two's-complement bit
        // pattern is unchanged), and match Python's infinite-precision result
        // for in-range operands, so `Small & Small` needs no overflow check.
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            return VmInt::Small(a & b);
        }
        VmInt::from_bigint(&*self.as_bigint() & &*other.as_bigint())
    }

    #[inline]
    pub fn bitor(&self, other: &VmInt) -> VmInt {
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            return VmInt::Small(a | b);
        }
        VmInt::from_bigint(&*self.as_bigint() | &*other.as_bigint())
    }

    #[inline]
    pub fn bitxor(&self, other: &VmInt) -> VmInt {
        if let (VmInt::Small(a), VmInt::Small(b)) = (self, other) {
            return VmInt::Small(a ^ b);
        }
        VmInt::from_bigint(&*self.as_bigint() ^ &*other.as_bigint())
    }

    /// Left shift by `shift` bits. Always routed through `BigInt` (a left shift
    /// grows without bound and isn't on any hot path), then normalised.
    pub fn shl(&self, shift: usize) -> VmInt {
        VmInt::from_bigint(&*self.as_bigint() << shift)
    }

    /// Arithmetic right shift by `shift` bits (floor-toward-negative for
    /// negatives, matching Python and `BigInt`).
    pub fn shr(&self, shift: usize) -> VmInt {
        if let VmInt::Small(a) = self {
            if shift >= 64 {
                return VmInt::Small(if *a < 0 { -1 } else { 0 });
            }
            return VmInt::Small(a >> shift);
        }
        VmInt::from_bigint(&*self.as_bigint() >> shift)
    }
}

impl From<i64> for VmInt {
    #[inline]
    fn from(n: i64) -> Self {
        VmInt::Small(n)
    }
}
impl From<i32> for VmInt {
    #[inline]
    fn from(n: i32) -> Self {
        VmInt::Small(n as i64)
    }
}
impl From<u8> for VmInt {
    #[inline]
    fn from(n: u8) -> Self {
        VmInt::Small(n as i64)
    }
}
impl From<u32> for VmInt {
    #[inline]
    fn from(n: u32) -> Self {
        VmInt::Small(n as i64)
    }
}
impl From<usize> for VmInt {
    #[inline]
    fn from(n: usize) -> Self {
        match i64::try_from(n) {
            Ok(v) => VmInt::Small(v),
            Err(_) => VmInt::Big(Rc::new(BigInt::from(n))),
        }
    }
}
impl From<u64> for VmInt {
    #[inline]
    fn from(n: u64) -> Self {
        match i64::try_from(n) {
            Ok(v) => VmInt::Small(v),
            Err(_) => VmInt::Big(Rc::new(BigInt::from(n))),
        }
    }
}
impl From<BigInt> for VmInt {
    #[inline]
    fn from(b: BigInt) -> Self {
        VmInt::from_bigint(b)
    }
}
impl From<&BigInt> for VmInt {
    #[inline]
    fn from(b: &BigInt) -> Self {
        match b.to_i64() {
            Some(n) => VmInt::Small(n),
            None => VmInt::Big(Rc::new(b.clone())),
        }
    }
}

impl std::ops::Neg for &VmInt {
    type Output = VmInt;
    fn neg(self) -> VmInt {
        match self {
            // `-i64::MIN` overflows into `Big`; every `-Big` re-normalises
            // because e.g. `-(2^63)` lands back in `i64` range.
            VmInt::Small(n) => match n.checked_neg() {
                Some(v) => VmInt::Small(v),
                None => VmInt::from_bigint(-BigInt::from(*n)),
            },
            VmInt::Big(b) => VmInt::from_bigint(-&**b),
        }
    }
}

impl std::ops::Not for &VmInt {
    type Output = VmInt;
    fn not(self) -> VmInt {
        match self {
            // `!n` == `-n - 1` for `i64`, always in range.
            VmInt::Small(n) => VmInt::Small(!n),
            VmInt::Big(b) => VmInt::from_bigint(!&**b),
        }
    }
}

impl PartialEq for VmInt {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (VmInt::Small(a), VmInt::Small(b)) => a == b,
            (VmInt::Big(a), VmInt::Big(b)) => a == b,
            // A `Small` and a `Big` can never be numerically equal (invariant).
            _ => false,
        }
    }
}
impl Eq for VmInt {}

impl Ord for VmInt {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (VmInt::Small(a), VmInt::Small(b)) => a.cmp(b),
            (VmInt::Big(a), VmInt::Big(b)) => a.cmp(b),
            // A `Big` is out of `i64` range, so its sign alone orders it
            // against any `Small` (`Big` is never zero).
            (VmInt::Small(_), VmInt::Big(b)) => {
                if b.is_positive() {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (VmInt::Big(a), VmInt::Small(_)) => {
                if a.is_positive() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
        }
    }
}
impl PartialOrd for VmInt {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for VmInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmInt::Small(n) => write!(f, "{n}"),
            VmInt::Big(b) => write!(f, "{b}"),
        }
    }
}

impl fmt::Debug for VmInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Print the numeric value (like the old bare-`BigInt` payload) so a
        // derived `Debug` on `HashKey` / `Value` reads `Int(5)`, not
        // `Int(Small(5))`.
        write!(f, "{self}")
    }
}

/// Compare a [`VmInt`] to an `f64` with the exact semantics CPython uses for
/// `int == float` — delegating to the proven `BigInt` routine so large-value
/// precision handling is identical. Not on any hot path (numeric literals in
/// loops are int-vs-int), so the `Small → BigInt` lift is acceptable.
pub fn vmint_eq_f64(a: &VmInt, b: f64) -> bool {
    bigint_eq_f64(&a.as_bigint(), b)
}

/// Ordering counterpart to [`vmint_eq_f64`].
pub fn vmint_cmp_f64(a: &VmInt, b: f64) -> Option<std::cmp::Ordering> {
    bigint_cmp_f64(&a.as_bigint(), b)
}

/// Hashable wrapper around a subset of `Value`s. Used as dict keys and set
/// elements. Floats are stored bitwise so `NaN != NaN` (matching Python).
#[derive(Debug, Clone)]
pub enum HashKey {
    None,
    Bool(bool),
    Int(VmInt),
    Float(u64),
    NaN(u64),
    /// A complex number stored as the bit patterns of its real and
    /// imaginary `f64` parts (bitwise, like `Float`). Python's `complex`
    /// is hashable, so `{1j: ...}` / `set([1j])` work.
    Complex(u64, u64),
    Str(RcStr),
    Tuple(Rc<Vec<HashKey>>),
    /// A `frozenset` used as a dict key. The elements are stored sorted by
    /// their hashed representation so two frozensets with the same members
    /// in different insertion order hash equal.
    FrozenSet(Rc<Vec<HashKey>>),
    /// A (frozen) dataclass instance used as a dict/set key. CPython makes
    /// `@dataclass(frozen=True)` instances hashable via the hash of the
    /// field tuple. The original instance is retained so iterating the
    /// keys back out (`list(d)`, dict repr) round-trips to the same
    /// value; equality and hashing derive from `key` — the class name
    /// plus the fields sorted by name (the VM stores fields unordered) —
    /// so two instances of the same class with equal fields hash and
    /// compare equal regardless of field insertion order.
    Instance {
        instance: Rc<Instance>,
        key: Rc<InstanceKey>,
    },
    /// An instance whose class inherits `object.__hash__` (a plain class, an
    /// enum member, an exception): CPython keys it by identity.
    Identity(Rc<Instance>),
    /// A *class object* used as a key (`registry[SomeClass]`). CPython hashes
    /// a type by identity, so two distinct classes with the same name are
    /// different keys. Builtin types reach the VM as native constructors
    /// rather than `Class` values, so they key on their name instead.
    Class(Rc<Class>),
    BuiltinType(&'static str),
    /// An instance whose class defines `__hash__`. `hash` is what the user
    /// method returned; `__eq__` is consulted by `Interpreter::settle_key`
    /// *before* the key reaches a container, so inside the container two
    /// keys are equal only when they wrap the very same instance.
    UserHashed {
        hash: i64,
        instance: Rc<Instance>,
    },
    /// A value-mixin enum member (`IntEnum`, `StrEnum`, `class Mode(str,
    /// Enum)`) used as a key. In CPython the member *is* an `int` / `str`
    /// subclass instance, so it hashes and compares as its value
    /// (`{"fast": 1}[Mode.FAST]` works) — but the key is still the member,
    /// so iterating the keys back gives `Lvl.LOW`, not `1`.
    Mixin {
        value: Box<HashKey>,
        member: Rc<Instance>,
    },
}

/// How instances of a class hash and compare as dict/set keys — CPython's
/// `__hash__` slot resolved along the MRO the way attribute lookup would:
/// a class body's own `__hash__` wins; `@dataclass` applies its hash-action
/// table (`eq=True` without `frozen=True` sets `__hash__ = None`, while
/// `frozen=True` / `unsafe_hash=True` synthesise a field-tuple hash); a body
/// defining `__eq__` without `__hash__` (or assigning `__hash__ = None`) is
/// unhashable; a pydantic `model` (field-based `BaseModel.__eq__`) is
/// unhashable; anything that inherits nothing better keys by identity like
/// `object`.
pub enum HashMode {
    Unhashable,
    Identity,
    Fields,
    User(Rc<Function>),
}

/// A boolean class-kind marker (`__typhon_*` class attribute) with a default.
pub fn class_flag(class: &Class, name: &str, default: bool) -> bool {
    match class.class_attrs.borrow().get(name) {
        Some(Value::Bool(b)) => *b,
        _ => default,
    }
}

pub fn instance_hash_mode(class: &Rc<Class>) -> HashMode {
    fn own_hash(c: &Rc<Class>) -> Option<Rc<Function>> {
        if class_flag(c, "__typhon_own_hash__", false) {
            c.methods.borrow().get("__hash__").cloned()
        } else {
            None
        }
    }
    fn walk(c: &Rc<Class>) -> Option<HashMode> {
        let own = own_hash(c);
        let hash_none = class_flag(c, "__typhon_hash_none__", false);
        let own_eq = class_flag(c, "__typhon_own_eq__", false);
        let decided = if class_is_dataclass(c) {
            let eq = class_flag(c, "__typhon_dc_eq__", true);
            let frozen = class_flag(c, "__typhon_dc_frozen__", false);
            let unsafe_hash = class_flag(c, "__typhon_dc_unsafe_hash__", false);
            match own {
                Some(f) => Some(HashMode::User(f)),
                None if unsafe_hash || (eq && frozen) => Some(HashMode::Fields),
                None if eq || hash_none => Some(HashMode::Unhashable),
                None => None,
            }
        } else if class_is_pydantic_model(c) {
            Some(match own {
                Some(f) => HashMode::User(f),
                None => HashMode::Unhashable,
            })
        } else {
            match own {
                Some(f) => Some(HashMode::User(f)),
                None if hash_none || own_eq => Some(HashMode::Unhashable),
                None => None,
            }
        };
        if decided.is_some() {
            return decided;
        }
        c.bases.iter().find_map(walk)
    }
    if class_is_enum(class) {
        return HashMode::Identity;
    }
    walk(class).unwrap_or(HashMode::Identity)
}

/// Whether `a == b` on two instances of `class` compares fields (a dataclass
/// with `eq=True`, a pydantic model, or — as the best interpreter-free
/// approximation — a class with its own `__eq__`, which `Interpreter::cmp_op`
/// dispatches for real) rather than identity (`object.__eq__`).
pub fn class_eq_by_fields(class: &Rc<Class>) -> bool {
    fn walk(c: &Rc<Class>) -> Option<bool> {
        if class_flag(c, "__typhon_own_eq__", false) {
            return Some(true);
        }
        if class_is_dataclass(c) {
            if class_flag(c, "__typhon_dc_eq__", true) {
                return Some(true);
            }
        } else if class_is_pydantic_model(c) {
            return Some(true);
        }
        c.bases.iter().find_map(walk)
    }
    if class_is_enum(class) {
        return false;
    }
    walk(class).unwrap_or(false)
}

/// Whether any class along the MRO is a `@dataclass(frozen=True)` — its
/// `__setattr__` / `__delattr__` raise `FrozenInstanceError`, and subclasses
/// inherit them.
pub fn class_is_frozen_dataclass(class: &Rc<Class>) -> bool {
    (class_is_dataclass(class) && class_flag(class, "__typhon_dc_frozen__", false))
        || class.bases.iter().any(class_is_frozen_dataclass)
}

/// `repr()` of a native: the builtin *types* the VM models as constructor
/// natives print as classes (`<class 'int'>`, `<class 'ValueError'>`),
/// everything else as `<built-in function name>`.
pub fn native_repr(name: &str) -> String {
    if name == "NotImplemented" {
        return name.to_owned();
    }
    let is_type = matches!(
        name,
        "int"
            | "float"
            | "str"
            | "bool"
            | "list"
            | "dict"
            | "set"
            | "frozenset"
            | "tuple"
            | "bytes"
            | "bytearray"
            | "object"
            | "type"
            | "range"
            | "complex"
            | "slice"
            | "memoryview"
            | "enumerate"
            | "zip"
            | "map"
            | "filter"
            | "reversed"
            | "property"
            | "staticmethod"
            | "classmethod"
            | "super"
            | "BaseException"
            | "KeyboardInterrupt"
            | "SystemExit"
            | "GeneratorExit"
            | "StopIteration"
            | "StopAsyncIteration"
            | "ExceptionGroup"
            | "BaseExceptionGroup"
    ) || name.ends_with("Error")
        || name.ends_with("Exception")
        || name.ends_with("Warning");
    if is_type {
        return format!("<class '{name}'>");
    }
    // Two prelude names the VM models as natives are *classes* in CPython,
    // and print with the module that defines them.
    match name {
        "enum.auto" => "<class 'enum.auto'>".to_owned(),
        "NewType" => "<class 'typing.NewType'>".to_owned(),
        _ => format!("<built-in function {name}>"),
    }
}

/// The VM models a `slice` as the tuple `("__slice__", start, stop, step)`
/// (what `eval_subscript_index` builds for `a[i:j:k]`).
/// The `Ellipsis` singleton. Like the slice marker it rides inside a tagged
/// tuple rather than its own `Value` variant, and it is one shared `Rc` per
/// thread so `x is Ellipsis` holds.
pub fn ellipsis_value() -> Value {
    thread_local! {
        static ELLIPSIS: Rc<Vec<Value>> =
            Rc::new(vec![Value::Str(Rc::new("__ellipsis__".to_owned()))]);
    }
    Value::Tuple(ELLIPSIS.with(|e| e.clone()))
}

/// Whether a tuple is the [`ellipsis_value`] marker.
pub fn is_ellipsis_marker(items: &[Value]) -> bool {
    items.len() == 1 && matches!(&items[0], Value::Str(tag) if tag.as_str() == "__ellipsis__")
}

pub fn is_slice_marker(items: &[Value]) -> bool {
    items.len() == 4 && matches!(&items[0], Value::Str(tag) if tag.as_str() == "__slice__")
}

/// `repr(slice(1, 5, None))`.
pub fn slice_repr(items: &[Value]) -> String {
    format!(
        "slice({}, {}, {})",
        items[1].py_repr(),
        items[2].py_repr(),
        items[3].py_repr()
    )
}

/// When `v` is a member of an enum class that mixes in a value type
/// (`StrEnum`, `IntEnum`, `IntFlag` — detected by walking the base chain
/// for the VM's `__typhon_enum_base__`-tagged marker classes of those
/// names), return the member's underlying `value`. CPython makes such
/// members genuine `str` / `int` subclasses, so equality, ordering,
/// hashing, and `str()` all flow through the value; plain `Enum` members
/// intentionally return `None` here (`Color.RED == 1` is False).
/// Whether `class` is a plain `enum.Flag` subclass — one whose members
/// combine into composite pseudo-members under `|` / `&` / `^` / `~` but,
/// unlike `IntFlag`, are *not* ints. `IntFlag` deliberately answers `false`
/// here: its members already flow through their int mixin.
pub fn is_plain_flag_class(class: &Rc<Class>) -> bool {
    fn marker(class: &Rc<Class>) -> Option<&'static str> {
        if class
            .class_attrs
            .borrow()
            .contains_key("__typhon_enum_base__")
        {
            return match class.name.as_str() {
                "Flag" => Some("Flag"),
                "IntFlag" => Some("IntFlag"),
                _ => None,
            };
        }
        class.bases.iter().find_map(marker)
    }
    marker(class) == Some("Flag")
}

/// The integer a `Flag` member carries, if it is one.
pub fn flag_member_bits(v: &Value) -> Option<i64> {
    let Value::Instance(inst) = v else {
        return None;
    };
    if !is_plain_flag_class(&inst.class) {
        return None;
    }
    match inst.fields.borrow().get("_value_") {
        Some(Value::Int(i)) => i.to_i64(),
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        _ => None,
    }
}

/// Whether `str()` / `format()` of an enum member shows its *value*: true
/// for `StrEnum` / `IntEnum` / `IntFlag` (whose `__str__` is the mixin
/// type's), false for a plain data-type mixin such as `class Mode(str,
/// Enum)`, whose members still print `Mode.FAST` (CPython 3.12+).
pub fn enum_str_is_value(v: &Value) -> bool {
    fn marker(class: &Rc<Class>) -> bool {
        (class
            .class_attrs
            .borrow()
            .contains_key("__typhon_enum_base__")
            && matches!(class.name.as_str(), "StrEnum" | "IntEnum" | "IntFlag"))
            || class.bases.iter().any(marker)
    }
    matches!(v, Value::Instance(inst) if marker(&inst.class))
}

pub fn enum_mixin_value(v: &Value) -> Option<Value> {
    fn mixin_base(class: &Rc<Class>) -> bool {
        let is_marker = class
            .class_attrs
            .borrow()
            .contains_key("__typhon_enum_base__")
            && matches!(class.name.as_str(), "StrEnum" | "IntEnum" | "IntFlag");
        if is_marker {
            return true;
        }
        class.bases.iter().any(mixin_base)
    }
    // `class Mode(str, Enum)` / `class L(int, Enum)`: a data-type mixin makes
    // every member an instance of that type, exactly as `StrEnum` /
    // `IntEnum` do — equal to and hashing like its value.
    fn data_type_mixin(class: &Rc<Class>) -> bool {
        matches!(
            class.class_attrs.borrow().get("__typhon_builtin_bases__"),
            Some(Value::Tuple(names)) if names.iter().any(|n| matches!(
                n,
                Value::Str(s) if matches!(s.as_str(), "str" | "int" | "float" | "bytes" | "complex")
            ))
        )
    }
    if let Value::Instance(inst) = v {
        if mixin_base(&inst.class) || (class_is_enum(&inst.class) && data_type_mixin(&inst.class)) {
            return inst.fields.borrow().get("value").cloned();
        }
    }
    None
}

/// Canonical, hashable projection of a dataclass instance: the class
/// identity, name, and the fields sorted by name with each value lowered
/// to a `HashKey`. Drives `Eq` / `Hash` / ordering for `HashKey::Instance`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceKey {
    /// Address of the class's single cached `Rc<Class>`, used as the
    /// class *identity* for `Eq`/`Hash`. Two distinct classes that happen
    /// to share a name have different addresses, so their instances never
    /// collide as dict/set keys (CPython only treats same-class instances
    /// as equal keys). Stable for the lifetime of the program — classes
    /// are never dropped. NOT used by `canonical_sort_key`, which keys on
    /// `class_name` to keep set/frozenset ordering deterministic.
    pub class_id: usize,
    pub class_name: String,
    pub fields: Vec<(String, HashKey)>,
}

/// CPython treats numerically-equal `bool` / `int` / `float` as the same
/// mapping/set key: `hash(1) == hash(1.0) == hash(True)` and `1 == 1.0 == True`,
/// so `{1: a, 1.0: b, True: c}` collapses to a single entry. `Bool ↔ Int`
/// already shared a slot here; this returns the integer an *integral* float
/// represents (`1.0 → 1`) so a `Float` key joins the same slot. A non-integral
/// or non-finite float returns `None` and keeps its own bit-pattern identity.
fn integral_float_to_bigint(bits: u64) -> Option<BigInt> {
    let f = f64::from_bits(bits);
    if f.is_finite() && f.fract() == 0.0 {
        BigInt::from_f64(f)
    } else {
        None
    }
}

/// Append the canonical byte encoding of an integer value to `out`, shared by
/// every numeric `HashKey` variant (`Bool`, `Int`, integral `Float`) so they
/// sort/canonicalise identically — required for `frozenset` element ordering to
/// stay consistent across numeric types (e.g. `frozenset({1, 2.0})` must equal
/// `frozenset({1.0, 2})`).
/// Feed an integer key into a hasher with a representation-independent
/// encoding: values in `i64` range hash through the `i64` (they can only be
/// `Small`, bool, or an in-range integral float), larger values through their
/// `BigInt` limbs. The two partitions can never contain numerically-equal
/// values (an `i64`-representable integer can't equal one that isn't), so they
/// need no cross-consistency — while every member of one equivalence class
/// (`1`, `1.0`, `True`) reaches the same branch and hashes identically.
fn hash_int_key<H: std::hash::Hasher>(state: &mut H, v: &VmInt) {
    use std::hash::Hash;
    match v {
        VmInt::Small(n) => n.hash(state),
        VmInt::Big(b) => b.hash(state),
    }
}

fn push_int_canonical(out: &mut Vec<u8>, i: &BigInt) {
    out.push(2);
    let (sign, digits) = i.to_bytes_le();
    out.push(match sign {
        num_bigint::Sign::Minus => 0,
        num_bigint::Sign::NoSign => 1,
        num_bigint::Sign::Plus => 2,
    });
    out.extend_from_slice(&(digits.len() as u32).to_be_bytes());
    out.extend_from_slice(&digits);
}

/// The sorted canonical encodings of a frozenset's members — the
/// order-independent identity `Eq`, `Hash` and the sort key use.
fn frozenset_canonical(items: &[HashKey]) -> Vec<Vec<u8>> {
    let mut keys: Vec<Vec<u8>> = items.iter().map(HashKey::canonical_sort_key).collect();
    keys.sort();
    keys
}

impl HashKey {
    /// Stable, collision-safe sort key. Two distinct `HashKey` values
    /// have distinct sort keys (the discriminant byte differs across
    /// variants and the payload is encoded deterministically). Used
    /// by the `FrozenSet` canonicalisation path so two frozensets
    /// with the same members hash equal regardless of insertion
    /// order — review thread copilot on PR #147.
    pub fn canonical_sort_key(&self) -> Vec<u8> {
        if let HashKey::Mixin { value, .. } = self {
            return value.canonical_sort_key();
        }
        let mut out = Vec::with_capacity(16);
        match self {
            HashKey::Mixin { .. } => unreachable!("handled above"),
            HashKey::NaN(id) => {
                out.push(9);
                out.extend_from_slice(&id.to_be_bytes());
            }
            HashKey::None => out.push(0),
            // Numeric keys share one canonical encoding so equal values across
            // bool/int/float sort identically (see `push_int_canonical`).
            HashKey::Bool(b) => push_int_canonical(&mut out, &BigInt::from(*b as i64)),
            HashKey::Int(i) => push_int_canonical(&mut out, &i.to_bigint()),
            HashKey::Float(bits) => match integral_float_to_bigint(*bits) {
                Some(bi) => push_int_canonical(&mut out, &bi),
                None => {
                    out.push(3);
                    out.extend_from_slice(&bits.to_be_bytes());
                }
            },
            HashKey::Complex(re, im) => {
                out.push(8);
                out.extend_from_slice(&re.to_be_bytes());
                out.extend_from_slice(&im.to_be_bytes());
            }
            HashKey::Str(s) => {
                out.push(4);
                out.extend_from_slice(&(s.len() as u32).to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            HashKey::Tuple(items) => {
                out.push(5);
                out.extend_from_slice(&(items.len() as u32).to_be_bytes());
                for item in items.iter() {
                    let inner = item.canonical_sort_key();
                    out.extend_from_slice(&(inner.len() as u32).to_be_bytes());
                    out.extend_from_slice(&inner);
                }
            }
            HashKey::FrozenSet(items) => {
                out.push(6);
                out.extend_from_slice(&(items.len() as u32).to_be_bytes());
                // Members are kept in the frozenset's iteration order;
                // sort their encodings so equal frozensets encode equally.
                for inner in frozenset_canonical(items) {
                    out.extend_from_slice(&(inner.len() as u32).to_be_bytes());
                    out.extend_from_slice(&inner);
                }
            }
            HashKey::Instance { key, .. } => {
                out.push(7);
                out.extend_from_slice(&(key.class_name.len() as u32).to_be_bytes());
                out.extend_from_slice(key.class_name.as_bytes());
                out.extend_from_slice(&(key.fields.len() as u32).to_be_bytes());
                // Fields are stored pre-sorted by name at construction.
                for (name, val) in key.fields.iter() {
                    out.extend_from_slice(&(name.len() as u32).to_be_bytes());
                    out.extend_from_slice(name.as_bytes());
                    let inner = val.canonical_sort_key();
                    out.extend_from_slice(&(inner.len() as u32).to_be_bytes());
                    out.extend_from_slice(&inner);
                }
            }
            // Identity-keyed instances order by address (stable for the
            // life of the instance, which the key retains).
            HashKey::Identity(inst) => {
                out.push(9);
                out.extend_from_slice(&(Rc::as_ptr(inst) as usize as u64).to_be_bytes());
            }
            HashKey::UserHashed { hash, instance } => {
                out.push(10);
                out.extend_from_slice(&hash.to_be_bytes());
                out.extend_from_slice(&(Rc::as_ptr(instance) as usize as u64).to_be_bytes());
            }
            HashKey::Class(c) => {
                out.push(9);
                out.extend_from_slice(&(Rc::as_ptr(c) as usize as u64).to_be_bytes());
            }
            HashKey::BuiltinType(name) => {
                out.push(10);
                out.extend_from_slice(&(name.len() as u32).to_be_bytes());
                out.extend_from_slice(name.as_bytes());
            }
        }
        out
    }

    pub fn into_value(self) -> Value {
        match self {
            HashKey::Mixin { member, .. } => Value::Instance(member),
            HashKey::NaN(identity) => Value::FloatData(VmFloat {
                value: f64::NAN,
                identity,
            }),
            HashKey::None => Value::None,
            HashKey::Bool(b) => Value::Bool(b),
            HashKey::Int(i) => Value::Int(i),
            HashKey::Float(bits) => Value::Float(f64::from_bits(bits)),
            HashKey::Complex(re, im) => Value::Complex(f64::from_bits(re), f64::from_bits(im)),
            HashKey::Str(s) => Value::Str(s),
            HashKey::Tuple(items) => Value::Tuple(Rc::new(
                items.iter().cloned().map(HashKey::into_value).collect(),
            )),
            HashKey::FrozenSet(items) => {
                let set: crate::pyset::PySet = items.iter().cloned().collect();
                // Surface back as a frozenset.
                Value::Set(Rc::new(crate::value::FrozenCell::frozen(set)))
            }
            HashKey::Instance { instance, .. } => Value::Instance(instance),
            HashKey::Identity(instance) => Value::Instance(instance),
            HashKey::Class(c) => Value::Class(c),
            HashKey::BuiltinType(name) => Value::Str(Rc::new(name.to_owned())),
            HashKey::UserHashed { instance, .. } => Value::Instance(instance),
        }
    }
}

impl PartialEq for HashKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (HashKey::Mixin { value: a, .. }, b) => **a == *b,
            (a, HashKey::Mixin { value: b, .. }) => *a == **b,
            (HashKey::NaN(a), HashKey::NaN(b)) => a == b,
            (HashKey::None, HashKey::None) => true,
            (HashKey::Bool(a), HashKey::Bool(b)) => a == b,
            // Python: bool ↔ int comparison shares hash slot. A bool is
            // 0 or 1, so any BigInt that doesn't fit in i64 can't equal
            // it — fold through `to_i64()` to avoid allocating a BigInt
            // on every lookup.
            (HashKey::Bool(a), HashKey::Int(b)) | (HashKey::Int(b), HashKey::Bool(a)) => {
                b.to_i64().is_some_and(|v| v == *a as i64)
            }
            (HashKey::Int(a), HashKey::Int(b)) => a == b,
            // Compare floats by numeric value, not bit pattern: CPython
            // collapses `0.0` and `-0.0` into one dict/set slot (they are
            // `==` and hash-equal). Bit equality alone also broke `Eq`
            // transitivity — `Float(-0.0) == Int(0) == Float(0.0)` held via
            // the integral arms below while `Float(-0.0) != Float(0.0)`.
            // The bit-pattern fallback keeps NaN keys reflexively equal to
            // themselves (`f64::NAN == f64::NAN` is false), which `Eq`
            // requires; hashing stays consistent because both zeros hash
            // through `integral_float_to_bigint` to the integer 0 and a
            // NaN hashes by its own bits.
            (HashKey::Float(a), HashKey::Float(b)) => {
                a == b || f64::from_bits(*a) == f64::from_bits(*b)
            }
            // Python: an integral float shares a slot with the equal int /
            // bool (`1 == 1.0 == True`, all hash-equal). A non-integral float
            // never equals an int/bool.
            (HashKey::Float(f), HashKey::Int(i)) | (HashKey::Int(i), HashKey::Float(f)) => {
                integral_float_to_bigint(*f).map(VmInt::from).as_ref() == Some(i)
            }
            (HashKey::Float(f), HashKey::Bool(b)) | (HashKey::Bool(b), HashKey::Float(f)) => {
                f64::from_bits(*f) == (*b as i64) as f64
            }
            (HashKey::Complex(ar, ai), HashKey::Complex(br, bi)) => ar == br && ai == bi,
            (HashKey::Str(a), HashKey::Str(b)) => a == b,
            (HashKey::Tuple(a), HashKey::Tuple(b)) => a == b,
            (HashKey::FrozenSet(a), HashKey::FrozenSet(b)) => {
                // Order-independent: the members are kept in iteration
                // order, so compare their sorted canonical encodings.
                a.len() == b.len() && frozenset_canonical(a) == frozenset_canonical(b)
            }
            // Instance keys compare on their canonical projection: same
            // class name and equal field set. The original `instance`
            // Rc is ignored so two distinct-but-equal instances match.
            (HashKey::Instance { key: a, .. }, HashKey::Instance { key: b, .. }) => a == b,
            (HashKey::Identity(a), HashKey::Identity(b)) => Rc::ptr_eq(a, b),
            (HashKey::Class(a), HashKey::Class(b)) => Rc::ptr_eq(a, b),
            (HashKey::BuiltinType(a), HashKey::BuiltinType(b)) => a == b,
            // Equal-by-`__eq__` probes are settled onto the stored key's
            // instance before they get here (`Interpreter::settle_key`).
            (
                HashKey::UserHashed {
                    hash: ha,
                    instance: a,
                },
                HashKey::UserHashed {
                    hash: hb,
                    instance: b,
                },
            ) => ha == hb && Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}
impl Eq for HashKey {}

impl std::hash::Hash for HashKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            HashKey::Mixin { value, .. } => value.hash(state),
            HashKey::NaN(id) => {
                9u8.hash(state);
                id.hash(state);
            }
            HashKey::None => 0u8.hash(state),
            // bool/int collide intentionally — Python's `hash(True) == hash(1)`.
            // A bool widens to a BigInt before hashing so it produces the
            // same hash as the equivalent `Int(BigInt::from(b))`. Large
            // ints just hash through their full BigInt representation.
            HashKey::Bool(b) => hash_int_key(state, &VmInt::from(*b as i64)),
            HashKey::Int(i) => hash_int_key(state, i),
            // An integral float hashes like the equal int (`hash(1.0) ==
            // hash(1)`); a non-integral float hashes by its bit pattern. Keeps
            // `Hash` consistent with the `Eq` cases above.
            HashKey::Float(bits) => match integral_float_to_bigint(*bits) {
                Some(bi) => hash_int_key(state, &VmInt::from(bi)),
                None => bits.hash(state),
            },
            HashKey::Complex(re, im) => {
                re.hash(state);
                im.hash(state);
            }
            HashKey::Str(s) => s.hash(state),
            HashKey::Tuple(items) => items.hash(state),
            HashKey::FrozenSet(items) => frozenset_canonical(items).hash(state),
            // Hash only the canonical projection so it stays consistent
            // with `Eq` (which ignores the retained `instance` Rc).
            HashKey::Instance { key, .. } => key.hash(state),
            HashKey::Identity(inst) => (Rc::as_ptr(inst) as usize).hash(state),
            HashKey::Class(c) => (Rc::as_ptr(c) as usize).hash(state),
            HashKey::BuiltinType(name) => name.hash(state),
            // Only the user hash feeds the hasher, so every probe with the
            // same `__hash__` lands in the same bucket chain and
            // `Interpreter::settle_key` can find its `__eq__` candidates.
            HashKey::UserHashed { hash, .. } => hash.hash(state),
        }
    }
}

// ── ExceptionGroup ────────────────────────────────────────────────────────
//
// PEP 654 exception groups are modelled with the *existing*
// `Value::Exception` shape rather than a new `Value` variant, because CPython
// itself stores a group's payload in its `args`: `ExceptionGroup("g", [e])`
// has `args == ("g", [e])` and exposes `.exceptions` as `tuple(args[1])`.
// Reusing `Value::Exception` therefore gives `e.args`, `repr(e)`, class-name
// lookup (`type(e).__name__`) and dict/set hashing for free, and keeps the
// blast radius of the feature to the handful of helpers below.
//
// What is modelled: construction (`ExceptionGroup` / `BaseExceptionGroup`,
// including CPython's auto-downcast of a `BaseExceptionGroup` whose members
// are all ordinary `Exception`s), `.exceptions` / `.message`, `str()` /
// `repr()`, `except*` splitting/binding/re-raise, and the `asyncio.TaskGroup`
// failure path. What is NOT modelled: `.split()` / `.subgroup()` / `.derive()`
// as user-callable methods, `__notes__`, and CPython's nested
// "Exception Group Traceback" rendering for an uncaught group (the VM prints
// the single summary line).

/// Whether `kind` names one of the two builtin exception-group types.
pub fn is_exception_group_kind(kind: &str) -> bool {
    matches!(kind, "ExceptionGroup" | "BaseExceptionGroup")
}

/// The sub-exceptions of an exception-group value, or `None` when `v` is not
/// a group. Reads `args[1]`, which is a tuple for the implicit wrapper
/// `except*` builds around a non-group exception and a list for an explicitly
/// constructed or derived group — matching CPython's own `args` in both cases.
pub fn exception_group_subs(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Exception { kind, args, .. } if is_exception_group_kind(kind.as_str()) => {
            match args.get(1) {
                Some(Value::List(l)) => Some(l.borrow().clone()),
                Some(Value::Tuple(t)) => Some((**t).clone()),
                _ => Some(Vec::new()),
            }
        }
        _ => None,
    }
}

/// Build an exception-group value. `as_tuple` selects the `args[1]` shape:
/// `true` for the implicit wrapper `except*` puts around a bare exception
/// (CPython: `ExceptionGroup('', (ValueError('v'),))`), `false` for an
/// explicitly constructed or split-derived group (`ExceptionGroup('g', [...])`).
pub fn make_exception_group(kind: &str, message: &str, subs: Vec<Value>, as_tuple: bool) -> Value {
    let subs_value = if as_tuple {
        Value::Tuple(Rc::new(subs))
    } else {
        Value::List(Rc::new(RefCell::new(subs)))
    };
    Value::Exception {
        kind: Rc::new(kind.to_owned()),
        message: Rc::new(message.to_owned()),
        args: Rc::new(vec![Value::Str(Rc::new(message.to_owned())), subs_value]),
        chain: None,
    }
}

/// Whether `v` is an exception that derives from `BaseException` but *not*
/// `Exception` — the set `ExceptionGroup` refuses to hold, and which forces
/// `BaseExceptionGroup` to stay a `BaseExceptionGroup`.
pub fn is_base_only_exception(v: &Value) -> bool {
    match v {
        Value::Exception { kind, .. } => matches!(
            kind.as_str(),
            "BaseException" | "KeyboardInterrupt" | "SystemExit" | "GeneratorExit"
        ),
        _ => false,
    }
}

/// Whether a prospective exception-group member derives from `Exception`
/// (as opposed to being `BaseException`-only). A nested `BaseExceptionGroup`
/// is *not* an `Exception` — only its downcast `ExceptionGroup` sibling is —
/// which is what keeps a mixed group's base-only side base-only through
/// splits and re-wraps.
pub fn is_ordinary_exception_member(v: &Value) -> bool {
    match v {
        Value::Exception { kind, .. } => {
            kind.as_str() != "BaseExceptionGroup" && !is_base_only_exception(v)
        }
        _ => true,
    }
}

/// The exception-group class CPython's `BaseExceptionGroup.__new__` would
/// choose for `members`: `ExceptionGroup` when every member is an ordinary
/// `Exception`, `BaseExceptionGroup` otherwise. Splits, handler-failure
/// re-wraps, and the `TaskGroup` failure path all funnel through `__new__`
/// in CPython, so every VM site that builds a group derives its kind here
/// (verified against 3.13: `BaseExceptionGroup("g", [ValueError("v"),
/// KeyboardInterrupt()])` split by `except* ValueError` binds an
/// `ExceptionGroup('g', [ValueError('v')])` on the matched side).
pub fn exception_group_kind_for(members: &[Value]) -> &'static str {
    if members.iter().all(is_ordinary_exception_member) {
        "ExceptionGroup"
    } else {
        "BaseExceptionGroup"
    }
}

/// The PEP 3134 chaining state attached to an exception value, if any.
/// Handles both exception shapes: the built-in `Value::Exception` and a
/// user-defined exception `Instance`.
pub fn exception_chain(v: &Value) -> Option<Rc<ExcChain>> {
    match v {
        Value::Exception { chain, .. } => chain.clone(),
        Value::Instance(i) => i.chain.borrow().clone(),
        _ => None,
    }
}

/// Whether `v` is an exception value at all — the two shapes a raise can
/// produce, a built-in `Value::Exception` and an instance of a user class
/// deriving from `Exception`.
pub fn is_exception_value(v: &Value) -> bool {
    match v {
        Value::Exception { .. } => true,
        Value::Instance(i) => i.class.is_exception,
        _ => false,
    }
}

/// Read one of `__cause__` / `__context__` / `__suppress_context__` off an
/// exception value. An unchained exception answers `None` / `False`, which is
/// what CPython gives a freshly constructed one.
pub fn exception_chain_attr(v: &Value, attr: &str) -> Option<Value> {
    if !is_exception_value(v) {
        return None;
    }
    let chain = exception_chain(v);
    match attr {
        "__cause__" => Some(chain.and_then(|c| c.cause.clone()).unwrap_or(Value::None)),
        "__context__" => Some(chain.and_then(|c| c.context.clone()).unwrap_or(Value::None)),
        "__suppress_context__" => Some(Value::Bool(chain.is_some_and(|c| c.suppress_context))),
        _ => None,
    }
}

fn update_exception_chain(v: Value, edit: impl FnOnce(&mut ExcChain)) -> Value {
    match v {
        Value::Exception {
            kind,
            message,
            args,
            chain,
        } => {
            let mut next = chain.map(|c| (*c).clone()).unwrap_or_default();
            edit(&mut next);
            Value::Exception {
                kind,
                message,
                args,
                chain: Some(Rc::new(next)),
            }
        }
        Value::Instance(i) => {
            let mut next = i
                .chain
                .borrow()
                .as_ref()
                .map(|c| (**c).clone())
                .unwrap_or_default();
            edit(&mut next);
            *i.chain.borrow_mut() = Some(Rc::new(next));
            Value::Instance(i)
        }
        other => other,
    }
}

/// Record `raise X from cause`: sets `__cause__`, and with it the
/// `__suppress_context__` flag CPython sets on the same statement.
/// `raise X from None` passes `Value::None` and just suppresses the context.
pub fn with_exception_cause(v: Value, cause: Value) -> Value {
    update_exception_chain(v, |c| {
        c.cause = Some(cause);
        c.suppress_context = true;
    })
}

/// Attach the traceback recorded when the exception was caught.
pub fn with_exception_traceback(v: Value, frames: Rc<Vec<crate::error::Frame>>) -> Value {
    update_exception_chain(v, |c| c.traceback = Some(frames))
}

/// Whether `v` already carries a recorded traceback.
pub fn has_exception_traceback(v: Option<&Value>) -> bool {
    v.and_then(exception_chain)
        .is_some_and(|c| c.traceback.is_some())
}

/// Record the exception that was being handled when this one was raised
/// (`__context__`). Never overwrites a context already set, and never chains
/// an exception to itself — both would build a cycle CPython does not.
pub fn with_exception_context(v: Value, context: Value) -> Value {
    if exception_values_identical(&v, &context) {
        return v;
    }
    if let Some(chain) = exception_chain(&v) {
        if chain.context.is_some() {
            return v;
        }
    }
    update_exception_chain(v, |c| c.context = Some(context))
}

/// Object identity (`is`) for exception values. Clones of one raised
/// exception share their `Rc`s, so pointer equality on the payload is the
/// VM's notion of "the same exception object" — the test PEP 654 reraise
/// merging and `TaskGroup.__aexit__` deduplication rely on.
pub fn exception_values_identical(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Exception { args: a, .. }, Value::Exception { args: b, .. }) => Rc::ptr_eq(a, b),
        (Value::Instance(a), Value::Instance(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

/// Append every leaf (non-group) exception reachable through `v` to `out`,
/// descending nested groups — the flattening CPython's
/// `BaseExceptionGroup` machinery calls the group's "leaf exceptions".
pub fn collect_exception_leaves(v: &Value, out: &mut Vec<Value>) {
    match exception_group_subs(v) {
        Some(subs) => {
            for sub in subs {
                collect_exception_leaves(&sub, out);
            }
        }
        None => out.push(v.clone()),
    }
}

/// Rebuild the subgroup of `orig` containing exactly the leaves that are
/// identity-members of `keep`, preserving nesting and each level's message
/// and recomputing each level's kind through the constructor's downcast
/// rule — CPython's `exception_group_projection`, which
/// `_PyExc_PrepReraiseStar` uses to reconstitute naked-`raise`d subgroups
/// and the unhandled remainder into the original group's shape. Returns
/// `None` when nothing is kept at this level.
pub fn project_exception_group(orig: &Value, keep: &[Value]) -> Option<Value> {
    match exception_group_subs(orig) {
        Some(subs) => {
            let members: Vec<Value> = subs
                .iter()
                .filter_map(|sub| project_exception_group(sub, keep))
                .collect();
            if members.is_empty() {
                return None;
            }
            let message = match orig {
                Value::Exception { message, .. } => (**message).clone(),
                _ => String::new(),
            };
            let kind = exception_group_kind_for(&members);
            Some(make_exception_group(kind, &message, members, false))
        }
        None => keep
            .iter()
            .any(|k| exception_values_identical(k, orig))
            .then(|| orig.clone()),
    }
}

/// PEP 3134 exception-chaining state carried by an exception *value*.
///
/// `cause` is what `raise X from Y` records (`__cause__`, which also implies
/// `__suppress_context__`); `context` is the exception that was being handled
/// when this one was raised (`__context__`). Both hold the chained exception
/// *value*, so `e.__cause__.args` works.
#[derive(Clone, Default)]
pub struct ExcChain {
    pub cause: Option<Value>,
    pub context: Option<Value>,
    pub suppress_context: bool,
    /// The frames the exception unwound through before it was caught,
    /// innermost first, ending in the catching frame — what an uncaught
    /// exception chained to this one prints for it.
    pub traceback: Option<Rc<Vec<crate::error::Frame>>>,
}

#[derive(Clone)]
pub enum Value {
    None,
    Bool(bool),
    Int(VmInt),
    FloatData(VmFloat),
    /// A complex number `(real, imag)`. Constructed from imaginary literals
    /// (`2j` → `Complex(0.0, 2.0)`) and the builtins agent's `complex(re, im)`
    /// constructor.
    Complex(f64, f64),
    Str(RcStr),
    Bytes(Rc<Vec<u8>>),
    List(RcList),
    Tuple(Rc<Vec<Value>>),
    Dict(RcDict),
    Set(RcSet),
    Range {
        start: i64,
        stop: i64,
        step: i64,
    },
    /// Native (Rust) callable. Receives positional args; keyword args handled
    /// by the call-site builtin if applicable.
    Native(Rc<NativeFn>),
    Function(Rc<Function>),
    BoundMethod {
        receiver: Box<Value>,
        function: Rc<Function>,
    },
    Class(Rc<Class>),
    Instance(Rc<Instance>),
    /// `typhon_runtime.Ok(value)` — native ADT variant for the `?` operator.
    ResultOk(Box<Value>),
    /// `typhon_runtime.Err(error)`.
    ResultErr(Box<Value>),
    /// A module — a namespace dictionary.
    Module(Rc<Module>),
    /// A *deferred* call to an `async def` — created when the function is
    /// called, executed when awaited (matching CPython, where a coroutine's
    /// body doesn't run until it's driven). The VM executes coroutines
    /// sequentially at force points (`await`, `asyncio.run`, `gather`,
    /// `TaskGroup.create_task`, `spawn`).
    Coroutine(Rc<CoroutineThunk>),
    /// An exception instance — held when a Python-style `except X as e` binds it.
    /// `message` is the str-form of the first arg (kept for cheap display);
    /// `args` is the full constructor argument tuple so `e.args` and the
    /// multi-arg `str(e)` / `repr(e)` forms match CPython.
    Exception {
        kind: RcStr,
        message: RcStr,
        args: Rc<Vec<Value>>,
        /// PEP 3134 chaining state (`__cause__` / `__context__`), `None`
        /// until something chains onto this exception. Exception values are
        /// copied freely, so the chain rides along with every clone.
        chain: Option<Rc<ExcChain>>,
    },
    /// Iterator state — opaque to the AST walker; consumed by `next`.
    Iter(Rc<RefCell<IterState>>),
    /// A dict view produced by `dict.keys()` / `.values()` / `.items()`.
    /// The builtins agent materialises the `items` vector (already containing
    /// the keys, values, or `(k, v)` tuples respectively) and tags it with the
    /// matching `kind`. The VM provides repr, iteration, `len()`, and `in`.
    /// `dict.keys()` / `.values()` / `.items()`: a *live* view of `dict`
    /// (CPython's views reflect later changes to the dict).
    DictView {
        kind: DictViewKind,
        dict: RcDict,
    },
}

impl IterState {
    /// `reversed()` over `dict`'s keys, values or items.
    pub fn dict_rev(dict: &RcDict, kind: DictViewKind) -> IterState {
        let (used, end) = {
            let d = dict.borrow();
            (d.len(), d.slot_end())
        };
        IterState::DictRev {
            dict: dict.clone(),
            kind,
            index: end,
            used,
        }
    }

    /// A fresh iterator over `dict`'s keys, values or items.
    pub fn dict_iter(dict: &RcDict, kind: DictViewKind) -> IterState {
        let used = dict.borrow().len();
        IterState::Dict {
            dict: dict.clone(),
            kind,
            index: 0,
            used,
            remaining: used,
        }
    }
}

/// `NotImplemented`: the singleton a binary or comparison dunder returns to
/// decline. The VM binds one native of that name (see `builtins`).
pub fn is_not_implemented(v: &Value) -> bool {
    matches!(v, Value::Native(n) if n.name == "NotImplemented")
}

/// The current contents of a dict view, in dict order.
pub fn view_items(kind: DictViewKind, dict: &RcDict) -> Vec<Value> {
    let d = dict.borrow();
    match kind {
        DictViewKind::Keys => d.keys().cloned().map(HashKey::into_value).collect(),
        DictViewKind::Values => d.values().cloned().collect(),
        DictViewKind::Items => d
            .iter()
            .map(|(k, v)| Value::Tuple(Rc::new(vec![k.clone().into_value(), v.clone()])))
            .collect(),
    }
}

/// Which flavour of dict view a `Value::DictView` represents. Controls the
/// repr prefix (`dict_keys` / `dict_values` / `dict_items`) and `type_name`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DictViewKind {
    Keys,
    Values,
    Items,
}

pub type NativeFnImpl =
    dyn Fn(&mut crate::interp::Interpreter, Vec<Value>) -> Result<Value, Unwind>;

pub struct NativeFn {
    pub name: &'static str,
    pub func: Box<NativeFnImpl>,
    /// The native stands in for a CPython *coroutine function*
    /// (`asyncio.sleep`, `Queue.get`, a `Lock.__aenter__`, …). The VM's
    /// scheduler is sequential, so the body runs at the call and the result
    /// is already known — but the caller still has to `await` it, so
    /// `call_value` hands the result back wrapped in a completed task rather
    /// than bare. That keeps `await` strict: a plain value in `await` is
    /// CPython's `TypeError`, and only a coroutine, a task, or a flagged
    /// native's result gets through.
    pub awaitable: bool,
    /// The native stands in for a Python *function* — the wrapper a
    /// decorator such as `@contextmanager` or `@functools.cache` returns.
    /// Stored on a class and read through an instance, it binds that
    /// instance as its first argument, as CPython's function descriptor
    /// does. Other natives model C builtins, which do not bind.
    pub method_like: bool,
}

impl NativeFn {
    pub fn new<F>(name: &'static str, f: F) -> Self
    where
        F: Fn(&mut crate::interp::Interpreter, Vec<Value>) -> Result<Value, Unwind> + 'static,
    {
        NativeFn {
            name,
            func: Box::new(f),
            awaitable: false,
            method_like: false,
        }
    }

    /// A native whose CPython counterpart is `async def` — see
    /// [`NativeFn::awaitable`].
    pub fn new_awaitable<F>(name: &'static str, f: F) -> Self
    where
        F: Fn(&mut crate::interp::Interpreter, Vec<Value>) -> Result<Value, Unwind> + 'static,
    {
        NativeFn {
            name,
            func: Box::new(f),
            awaitable: true,
            method_like: false,
        }
    }

    /// A native that binds like a Python function — see
    /// [`NativeFn::method_like`].
    pub fn new_method_like<F>(name: &'static str, f: F) -> Self
    where
        F: Fn(&mut crate::interp::Interpreter, Vec<Value>) -> Result<Value, Unwind> + 'static,
    {
        NativeFn {
            name,
            func: Box::new(f),
            awaitable: false,
            method_like: true,
        }
    }

    /// `native` with `receiver` bound as its first argument — the method
    /// object a read through an instance makes. With no receiver it is the
    /// same callable minus the binding behaviour, which is what
    /// `@staticmethod` makes of a method-like wrapper.
    pub fn rebind(native: &Rc<NativeFn>, receiver: Option<Value>) -> Value {
        let inner = native.clone();
        Value::Native(Rc::new(NativeFn {
            name: native.name,
            func: Box::new(move |interp, mut args| {
                if let Some(r) = &receiver {
                    args.insert(0, r.clone());
                }
                (inner.func)(interp, args)
            }),
            awaitable: native.awaitable,
            method_like: false,
        }))
    }
}

pub struct Function {
    pub name: String,
    pub params: Box<Parameters>,
    pub body: Rc<Vec<Stmt>>,
    /// Default values for non-variadic params, evaluated at def-time and
    /// stored in source order matching `iter_non_variadic_params`.
    pub defaults: Vec<Option<Value>>,
    /// Closure scope captured at def-time.
    pub closure: crate::env::EnvRef,
    pub is_async: bool,
    /// `@staticmethod` — the function takes no implicit receiver, so
    /// reading it through an instance must not bind `self`.
    pub is_static: bool,
    /// `@classmethod` — reading it through an instance binds the class
    /// object as the first argument, not the instance.
    pub is_classmethod: bool,
    /// The source the function was defined in, captured at def time so
    /// traceback frames (and the statement offsets recorded while the body
    /// runs) attribute to the right file when a function defined in one
    /// module is called from another.
    pub source: Option<std::rc::Rc<crate::interp::SourceInfo>>,
    /// Slot-resolved-locals layout + eligibility (VM performance Tier 1b),
    /// computed once from `params` + `body` when the function value is built.
    /// Ineligible functions use the classic per-call `Env` HashMap path.
    pub slot_info: std::rc::Rc<crate::slots::SlotInfo>,
    /// Whether the body contains a `yield`, and if so which execution
    /// strategy the VM uses for it — computed once at def time.
    pub generator: GeneratorKind,
    /// A function's own `__dict__`. CPython lets any attribute be attached
    /// to a function object, which is how decorators publish extra API on
    /// the wrapper they return (`wrapper.register = …` in `singledispatch`,
    /// `fn.cache_clear`, a test framework's markers).
    pub attrs: RefCell<HashMap<String, Value>>,
}

/// How a `yield`-bearing function body is executed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeneratorKind {
    /// No `yield` in the body — an ordinary function.
    NotGenerator,
    /// Every `yield` sits in a resumable statement position (`yield v` as a
    /// statement, `x = yield v`, `yield from it`), so the body runs lazily:
    /// calling the function returns a suspended generator object and each
    /// `next()` re-enters the tree-walk at the recorded resume path
    /// (`Interpreter::generator_resume`).
    Lazy,
    /// A `yield` sits inside a larger expression (`f((yield x))`,
    /// `return (yield)`, a `yield` in a `while` test, …) or the function is
    /// an `async` generator. The tree-walker cannot resume mid-expression, so
    /// the body runs eagerly to completion and the call returns an iterator
    /// over the collected values (bounded by `GENERATOR_CAP`).
    Eager,
}

/// One level of the path at which a lazy generator body is suspended.
///
/// A `yield` unwinds out of the tree-walk as `Unwind::Yield`; every enclosing
/// construct it passes through records the frame describing how to re-enter
/// it (innermost first). On the next `next()` the frames are consumed
/// outermost-first: each construct pops the frame that describes itself and
/// jumps straight back into the recorded branch / iteration / handler instead
/// of re-evaluating its header.
#[derive(Clone, Debug)]
pub enum ResumeFrame {
    /// Inside a statement list, at statement `index`.
    Block { index: usize },
    /// The suspended `yield` statement itself.
    YieldPoint,
    /// The suspended `yield from` statement; `iter` is the delegated-to
    /// sub-iterator.
    YieldFrom { iter: Value },
    /// Inside an `if` body (`None`) or the `elif` / `else` clause at `clause`.
    IfBranch { clause: Option<usize> },
    /// Inside a `while` body.
    WhileBody,
    /// Inside a loop's `else` clause.
    LoopElse,
    /// Inside a `for` body; `iter` is the live loop iterator.
    ForBody { iter: Value },
    /// Inside a `try` body.
    TryBody,
    /// Inside the `except` handler at `index`, handling `exc` (boxed to keep
    /// the frame small — it travels through every statement executor).
    TryHandler {
        index: usize,
        exc: Box<crate::error::VmException>,
    },
    /// Inside a `try` statement's `else` block.
    TryElse,
    /// Inside a `finally` block; `pending` is the outcome the block will
    /// re-deliver when it completes (`None` = the guarded code finished
    /// normally).
    TryFinally {
        pending: Option<Box<crate::error::Unwind>>,
    },
    /// Inside a `with` body; `entered` are the context managers whose
    /// `__exit__` still has to run.
    WithBody { entered: Vec<Value> },
    /// Inside the `case` body at `index` of a `match`.
    MatchCase { index: usize },
}

/// A lazy generator object: the function's own call frame kept alive between
/// `next()` calls, plus the resume path recorded at the last `yield`.
pub struct GeneratorState {
    pub function: Rc<Function>,
    /// The generator's local scope — persists across suspensions.
    pub env: crate::env::EnvRef,
    /// The suspension path (innermost frame first, consumed from the back).
    /// Empty before the first `next()` and after exhaustion.
    pub resume: Vec<ResumeFrame>,
    pub started: bool,
    pub finished: bool,
    /// Set while the body is executing — a re-entrant `next()` raises
    /// `ValueError: generator already executing` as CPython does.
    pub running: bool,
    /// The value carried by the terminating `return` (`StopIteration.value`).
    pub return_value: Option<Value>,
}

/// A lazy generator expression `(elt for … in … if …)`: the comprehension AST
/// plus one live iterator per `for` clause.
pub struct GenExprState {
    pub node: Rc<ruff_python_ast::ExprGenerator>,
    /// The comprehension's private scope (targets bind here).
    pub env: crate::env::EnvRef,
    /// `iters[i]` is the live iterator of the i-th `for` clause; `None` past
    /// the innermost active clause. `iters[0]` is created eagerly when the
    /// expression is evaluated (CPython evaluates the outermost iterable at
    /// once), everything else on demand.
    pub iters: Vec<Option<Value>>,
    pub finished: bool,
    pub running: bool,
}

pub struct Class {
    pub name: String,
    /// Method table — looked up on instance attribute access.
    pub methods: RefCell<HashMap<String, Rc<Function>>>,
    /// Annotated field names, in source order. Used to synthesise `__init__`
    /// when none was defined.
    pub fields: Vec<ClassField>,
    /// Class-level attributes (constants, defaults pulled out of class body).
    pub class_attrs: RefCell<HashMap<String, Value>>,
    /// The direct bases, in declaration order (`__bases__`).
    pub bases: Vec<Rc<Class>>,
    /// The C3 linearisation of the ancestors (`__mro__` without the class
    /// itself and without the implicit `object`), computed once when the
    /// class is created. Every attribute lookup, `super()` and `__mro__`
    /// walk this order, as CPython does.
    pub mro: Vec<Rc<Class>>,
    /// Method names decorated with `@property` — accessed without `()` and
    /// invoked lazily on attribute read.
    pub properties: RefCell<std::collections::HashSet<String>>,
    /// Method names decorated with `@classmethod` — the receiver is bound to
    /// the class object (`cls`) rather than the instance.
    pub classmethods: RefCell<std::collections::HashSet<String>>,
    /// `true` when the class subclasses a builtin or user exception. Such a
    /// class must behave like `BaseException` on construction — accepting
    /// positional `args` and rendering them through `str()` — rather than
    /// getting the dataclass "takes N arguments" treatment, so that the
    /// ubiquitous `raise FooError("message")` idiom works under the VM.
    pub is_exception: bool,
    /// `true` when the class was declared with a `Protocol` base — i.e. it is
    /// the lowering of a Typhon `interface`.
    ///
    /// Recorded from the base *expression* rather than the resolved base,
    /// because `typing.Protocol` resolves to an identity native in the VM, not
    /// a class, so it never appears in `bases`. Without this the `as!` cast
    /// could not tell an interface target from an ordinary class and fell back
    /// to a nominal `isinstance`, which can never succeed against a protocol —
    /// making `EXPR as! SomeInterface` impossible under `tyc run`.
    pub is_protocol: bool,
}

thread_local! {
    /// The VM's placeholder for the builtin `object` class, bound as
    /// `object` in every interpreter's root scope. It is one object per
    /// thread so it can be told apart from a user class that is also named
    /// `object` (`plain class object:`) by identity rather than by name.
    static BUILTIN_OBJECT: Rc<Class> = Rc::new(Class {
        name: "object".to_owned(),
        methods: RefCell::new(HashMap::new()),
        fields: vec![],
        class_attrs: RefCell::new(HashMap::new()),
        bases: vec![],
        mro: vec![],
        properties: RefCell::new(std::collections::HashSet::new()),
        classmethods: RefCell::new(std::collections::HashSet::new()),
        is_exception: false,
        is_protocol: false,
    });
}

/// The VM's builtin `object` class (see `BUILTIN_OBJECT`).
pub fn builtin_object_class() -> Rc<Class> {
    BUILTIN_OBJECT.with(Rc::clone)
}

/// Whether `c` is the builtin `object` class, not merely a class named so.
pub fn is_builtin_object(c: &Rc<Class>) -> bool {
    BUILTIN_OBJECT.with(|o| Rc::ptr_eq(o, c))
}

/// C3 linearisation of a class with direct `bases`: the ancestors in method
/// resolution order, without the class itself. `Err` carries the base names
/// for CPython's "Cannot create a consistent method resolution order (MRO)"
/// `TypeError`. The VM's placeholder `object` class sorts last, as the
/// implicit root does in CPython.
pub fn linearize(bases: &[Rc<Class>]) -> Result<Vec<Rc<Class>>, String> {
    let is_object = is_builtin_object;
    let mut seqs: Vec<Vec<Rc<Class>>> = bases
        .iter()
        .map(|b| {
            let mut seq = vec![b.clone()];
            seq.extend(b.mro.iter().cloned());
            seq
        })
        .collect();
    seqs.push(bases.to_vec());
    let mut out: Vec<Rc<Class>> = Vec::new();
    loop {
        seqs.retain(|s| !s.is_empty());
        if seqs.is_empty() {
            break;
        }
        // The first head that appears in no other sequence's tail.
        let candidate = seqs.iter().map(|s| s[0].clone()).find(|head| {
            !seqs
                .iter()
                .any(|s| s[1..].iter().any(|c| Rc::ptr_eq(c, head)))
        });
        let Some(next) = candidate else {
            let names: Vec<String> = bases.iter().map(|b| b.name.clone()).collect();
            return Err(names.join(", "));
        };
        for s in seqs.iter_mut() {
            if Rc::ptr_eq(&s[0], &next) {
                s.remove(0);
            }
        }
        out.push(next);
    }
    // A placeholder `object` reached through some base belongs at the end.
    if let Some(pos) = out.iter().position(is_object) {
        let obj = out.remove(pos);
        out.push(obj);
    }
    Ok(out)
}

#[derive(Clone)]
pub struct ClassField {
    pub name: String,
    pub default: Option<Value>,
    /// The field's annotation as source text (`int`, `list[str]`,
    /// `Address | None`), when the class body declared one. Surfaced by
    /// `dataclasses.fields(...)` as `Field.type` — the emitted Python runs
    /// under `from __future__ import annotations`, so CPython's `Field.type`
    /// is that same string.
    pub annotation: Option<String>,
}

/// Instance attributes, in assignment order — `vars(obj)`, `__dict__` and
/// every dict built from them iterate in that order, as in CPython.
pub type FieldMap = IndexMap<String, Value>;

pub struct Instance {
    pub class: Rc<Class>,
    pub fields: RefCell<FieldMap>,
    /// PEP 3134 chaining state for a user-defined exception instance. Kept
    /// out of `fields` because CPython holds `__cause__` / `__context__` in
    /// slots, not in `__dict__` — `vars(e)` must not show them.
    pub chain: RefCell<Option<Rc<ExcChain>>>,
}

// Hand-written so `HashKey::Class` can `#[derive(Debug)]` without pulling
// the whole class graph into a `Debug` bound: the name is what a key dump
// needs to be readable.
impl fmt::Debug for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<class '{}'>", self.name)
    }
}

// Hand-written so `HashKey::Instance` (which retains an `Rc<Instance>`)
// can `#[derive(Debug)]`. Neither `Class` nor `Value` implement `Debug`
// for the whole graph, so we print the dataclass-style repr instead.
impl fmt::Debug for Instance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", instance_repr(self))
    }
}

pub struct CoroutineThunk {
    pub function: Rc<Function>,
    pub args: std::cell::RefCell<Vec<Value>>,
    pub kwargs: Vec<(String, Value)>,
    pub receiver: Option<Value>,
    /// A coroutine runs at most once (CPython raises on re-await).
    pub forced: std::cell::Cell<bool>,
}

pub struct Module {
    pub name: String,
    pub members: RefCell<HashMap<String, Value>>,
    /// The namespace the module body ran in, for modules the VM executed
    /// (loaded siblings, stdlib shims): attribute reads consult it first so a
    /// global a module function rebinds later (`global counter`) is seen
    /// through `module.counter`, as in CPython. `None` for native modules.
    pub env: Option<crate::env::EnvRef>,
}

pub enum IterState {
    Range {
        current: i64,
        stop: i64,
        step: i64,
    },
    List {
        items: RcList,
        index: usize,
    },
    Tuple {
        items: Rc<Vec<Value>>,
        index: usize,
    },
    Str {
        chars: Vec<char>,
        index: usize,
    },
    /// A dict (or dict view) iterator over the live dict: `used` is the
    /// size it started with (a change raises CPython's `RuntimeError`) and
    /// `remaining` how many entries it may still yield.
    Dict {
        dict: RcDict,
        kind: DictViewKind,
        index: usize,
        used: usize,
        remaining: usize,
    },
    /// `reversed(list)`: yields `list[index - 1]` while that index is still
    /// inside the (live) list.
    ListRev {
        list: RcList,
        index: usize,
    },
    /// `reversed(dict)` / `reversed(view)`: `index` counts down the live
    /// dict's entries; a size change raises.
    DictRev {
        dict: RcDict,
        kind: DictViewKind,
        index: usize,
        used: usize,
    },
    /// A set iterator over the live table (`pos` is a slot index).
    Set {
        set: RcSet,
        pos: usize,
        used: usize,
        remaining: usize,
    },
    Enumerate {
        inner: Rc<RefCell<IterState>>,
        index: i64,
    },
    Zip {
        inners: Vec<Rc<RefCell<IterState>>>,
    },
    Map {
        func: Value,
        inner: Rc<RefCell<IterState>>,
        /// `map(f, xs, ys, …)`: `inner` zips the iterables and each item is
        /// a tuple spread over `f`'s parameters (stopping at the shortest
        /// iterable, as CPython does).
        star: bool,
    },
    Filter {
        func: Value,
        inner: Rc<RefCell<IterState>>,
    },
    /// `reversed(seq)` — `items` already reversed. Kept distinct from `List`
    /// only so `type(reversed(xs)).__name__` reports `list_reverseiterator`.
    Reversed {
        items: Rc<Vec<Value>>,
        index: usize,
    },
    /// A lazily-driven generator function body.
    Generator(Rc<RefCell<GeneratorState>>),
    /// A lazily-driven generator expression.
    GenExpr(Rc<RefCell<GenExprState>>),
    /// A user iterator object (an instance whose class defines `__next__`),
    /// stepped through `__next__` on demand.
    UserIter(Value),
    /// The legacy sequence protocol: `obj[0]`, `obj[1]`, … until `IndexError`.
    SeqIter {
        obj: Value,
        index: usize,
    },
}

/// The `type(it).__name__` CPython reports for an iterator of this shape.
pub fn iter_type_name(state: &IterState) -> &'static str {
    match state {
        IterState::Range { .. } => "range_iterator",
        IterState::List { .. } => "list_iterator",
        IterState::Tuple { .. } => "tuple_iterator",
        IterState::Str { .. } => "str_ascii_iterator",
        IterState::Dict {
            kind: DictViewKind::Keys,
            ..
        } => "dict_keyiterator",
        IterState::Dict {
            kind: DictViewKind::Values,
            ..
        } => "dict_valueiterator",
        IterState::Dict {
            kind: DictViewKind::Items,
            ..
        } => "dict_itemiterator",
        IterState::Set { .. } => "set_iterator",
        IterState::Enumerate { .. } => "enumerate",
        IterState::Zip { .. } => "zip",
        IterState::Map { .. } => "map",
        IterState::Filter { .. } => "filter",
        IterState::Reversed { .. } => "reversed",
        IterState::ListRev { .. } => "list_reverseiterator",
        IterState::DictRev {
            kind: DictViewKind::Keys,
            ..
        } => "dict_reversekeyiterator",
        IterState::DictRev {
            kind: DictViewKind::Values,
            ..
        } => "dict_reversevalueiterator",
        IterState::DictRev {
            kind: DictViewKind::Items,
            ..
        } => "dict_reverseitemiterator",
        IterState::Generator(_) | IterState::GenExpr(_) => "generator",
        IterState::UserIter(_) => "iterator",
        IterState::SeqIter { .. } => "iterator",
    }
}

// ── Debug / display ────────────────────────────────────────────────────────

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::None => write!(f, "None"),
            Value::Bool(b) => write!(f, "{b:?}"),
            Value::Int(i) => write!(f, "{}", i.to_str_radix(10)),
            Value::FloatData(crate::value::VmFloat { value: x, .. }) => write!(f, "{x:?}"),
            Value::Complex(re, im) => write!(f, "{}", format_complex(*re, *im)),
            Value::Str(s) => write!(f, "{:?}", s.as_str()),
            Value::Bytes(b) => write!(f, "{}", python_repr_bytes(b)),
            Value::List(l) => write!(f, "{:?}", l.borrow()),
            Value::Tuple(t) => write!(f, "{:?}", &t[..]),
            Value::Dict(d) => {
                let is_frozen = d.frozen.get();
                if is_frozen {
                    write!(f, "mappingproxy({{")?;
                } else {
                    write!(f, "{{")?;
                }
                let d = d.borrow();
                for (emitted, (k, v)) in d.iter().enumerate() {
                    // Hide the internal freeze sentinel from user output.
                    if emitted > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:?}: {:?}", k.clone().into_value(), v)?;
                }
                if is_frozen {
                    write!(f, "}})")
                } else {
                    write!(f, "}}")
                }
            }
            Value::Set(_) => write!(f, "<set>"),
            Value::Range { start, stop, step } => {
                write!(f, "range({start}, {stop}, {step})")
            }
            Value::Native(n) => write!(f, "{}", native_repr(n.name)),
            Value::Function(func) => write!(f, "<function {}>", func.name),
            Value::BoundMethod { function, .. } => {
                write!(f, "<bound method {}>", function.name)
            }
            Value::Class(c) => write!(f, "<class {}>", c.name),
            Value::Instance(i) => write!(f, "{}", instance_repr(i)),
            // Match the dataclass-default `repr` shape that
            // `typhon_runtime`'s `Ok` / `Err` produce under CPython
            // (`@dataclass(frozen=True)` generates `Foo(value=42)` and
            // `Foo(error=...)` reprs). Without this the VM prints
            // `Ok(42)` and the CPython exec prints `Ok(value=42)`,
            // which makes `tyc run` vs `tyc run --compile` stdout
            // diverge for screenshot-driven docs and test fixtures
            // (FINDINGS O24).
            Value::ResultOk(v) => write!(f, "Ok(value={:?})", v),
            Value::ResultErr(v) => write!(f, "Err(error={:?})", v),
            Value::Module(m) => write!(f, "<module {}>", m.name),
            Value::Coroutine(c) => write!(f, "<coroutine {}>", c.function.name),
            Value::Exception {
                kind,
                message,
                args,
                ..
            } => {
                if args.is_empty() {
                    if message.is_empty() {
                        write!(f, "{kind}()")
                    } else {
                        write!(f, "{kind}({:?})", message.as_str())
                    }
                } else {
                    let parts: Vec<String> = args.iter().map(|a| a.py_repr()).collect();
                    write!(f, "{kind}({})", parts.join(", "))
                }
            }
            Value::Iter(_) => write!(f, "<iterator>"),
            Value::DictView { .. } => write!(f, "{}", self.py_str()),
        }
    }
}

// ── Conversion / introspection helpers ────────────────────────────────────

/// Exact `float` → `BigInt` conversion with CPython's error behaviour.
///
/// Rust's `as` cast saturates (`1e30 as i64` is `i64::MAX`) and maps NaN to
/// zero, so every out-of-range float silently became a wrong integer — in an
/// interpreter whose defining property is that its integers are arbitrary
/// precision. `BigInt::from_f64` truncates toward zero exactly like
/// `int(x)` and returns `None` only for the values CPython refuses.
fn float_to_bigint(x: f64) -> Result<BigInt, Unwind> {
    if x.is_nan() {
        return Err(Unwind::Exception(crate::error::VmException::new(
            "ValueError",
            "cannot convert float NaN to integer",
        )));
    }
    if x.is_infinite() {
        return Err(Unwind::Exception(crate::error::VmException::new(
            "OverflowError",
            "cannot convert float infinity to integer",
        )));
    }
    BigInt::from_f64(x.trunc()).ok_or_else(|| {
        Unwind::Exception(crate::error::VmException::new(
            "ValueError",
            format!("cannot convert float {x} to integer"),
        ))
    })
}

/// A dict's `{k: v, …}` rendering. `wrap_frozen` adds the `mappingproxy(…)`
/// wrapper a `freeze`-marked dict shows under `repr` — CPython's proxy
/// delegates `__str__` to the mapping it wraps, so only `repr` names it.
fn dict_render(v: &Value, wrap_frozen: bool) -> String {
    let Value::Dict(d) = v else {
        return String::new();
    };
    let wrap = wrap_frozen && d.frozen.get();
    let d = d.borrow();
    let mut s = String::new();
    s.push_str(if wrap { "mappingproxy({" } else { "{" });
    for (emitted, (k, v)) in d.iter().enumerate() {
        if emitted > 0 {
            s.push_str(", ");
        }
        s.push_str(&k.clone().into_value().py_repr());
        s.push_str(": ");
        s.push_str(&v.py_repr());
    }
    s.push_str(if wrap { "})" } else { "}" });
    s
}

impl Value {
    #[allow(non_snake_case)]
    pub fn Float(value: f64) -> Self {
        Self::FloatData(VmFloat::new(value))
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::FloatData(a), Self::FloatData(b)) => a.identity != 0 && a.identity == b.identity,
            (Self::Instance(a), Self::Instance(b)) => Rc::ptr_eq(a, b),
            (Self::List(a), Self::List(b)) => Rc::ptr_eq(a, b),
            (Self::Tuple(a), Self::Tuple(b)) => Rc::ptr_eq(a, b),
            (Self::Dict(a), Self::Dict(b)) => Rc::ptr_eq(a, b),
            (Self::Set(a), Self::Set(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
    pub fn identical_or_equal(&self, other: &Self) -> bool {
        self.same_identity(other) || self.py_eq(other)
    }

    /// The type name CPython puts in an error message: a user instance
    /// reports its *class*, everything else its structural `type_name`.
    pub fn type_display_name(&self) -> String {
        match self {
            Value::Instance(i) => i.class.name.clone(),
            Value::Class(c) => c.name.clone(),
            other => other.type_name().to_owned(),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::None => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::FloatData(_) => "float",
            Value::Complex(..) => "complex",
            Value::Str(_) => "str",
            Value::Bytes(_) => "bytes",
            Value::List(_) => "list",
            Value::Tuple(t) if is_slice_marker(t) => "slice",
            Value::Tuple(t) if is_ellipsis_marker(t) => "ellipsis",
            Value::Tuple(_) => "tuple",
            // A `freeze let` dict is CPython's read-only `mappingproxy`.
            Value::Dict(d) if d.frozen.get() => "mappingproxy",
            Value::Dict(_) => "dict",
            Value::Set(s) if s.frozen.get() => "frozenset",
            Value::Set(_) => "set",
            Value::Range { .. } => "range",
            Value::Native(n) if n.name == "NotImplemented" => "NotImplementedType",
            Value::Native(_) | Value::Function(_) | Value::BoundMethod { .. } => "function",
            Value::Class(_) => "type",
            // Don't leak the class name into a `'static str`. Callers that
            // need the specific class name read `instance.class.name`
            // directly; everywhere else `"instance"` is descriptive enough
            // for an error message.
            // A class name is not `'static`, so callers that want it in a
            // message reach for `type_display_name`. This stays the coarse
            // structural label.
            Value::Instance(_) => "instance",
            Value::ResultOk(_) => "Ok",
            Value::ResultErr(_) => "Err",
            Value::Module(_) => "module",
            Value::Coroutine(_) => "coroutine",
            Value::Exception { .. } => "Exception",
            Value::Iter(it) => iter_type_name(&it.borrow()),
            Value::DictView { kind, .. } => match kind {
                DictViewKind::Keys => "dict_keys",
                DictViewKind::Values => "dict_values",
                DictViewKind::Items => "dict_items",
            },
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Int(i) => !i.is_zero(),
            Value::FloatData(crate::value::VmFloat { value: x, .. }) => *x != 0.0,
            Value::Complex(re, im) => *re != 0.0 || *im != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::List(l) => !l.borrow().is_empty(),
            Value::Tuple(t) => !t.is_empty(),
            // Frozen metadata lives outside user-visible container contents.
            Value::Dict(d) => !d.borrow().is_empty(),
            Value::Set(s) => !s.borrow().is_empty(),
            Value::Range { start, stop, step } => {
                if *step > 0 {
                    stop > start
                } else if *step < 0 {
                    stop < start
                } else {
                    false
                }
            }
            Value::DictView { dict, .. } => !dict.borrow().is_empty(),
            _ => true,
        }
    }

    /// Convert to a `HashKey`, failing for unhashable values (lists, dicts,
    /// sets, instances without `__hash__`).
    pub fn to_hash_key(&self) -> Result<HashKey, Unwind> {
        match self {
            Value::None => Ok(HashKey::None),
            Value::Bool(b) => Ok(HashKey::Bool(*b)),
            Value::Int(i) => Ok(HashKey::Int(i.clone())),
            Value::FloatData(x) => Ok(if x.value.is_nan() {
                HashKey::NaN(x.identity)
            } else {
                HashKey::Float(x.value.to_bits())
            }),
            Value::Complex(re, im) => Ok(HashKey::Complex(re.to_bits(), im.to_bits())),
            Value::Str(s) => Ok(HashKey::Str(s.clone())),
            Value::Tuple(items) => {
                let mut keys = Vec::with_capacity(items.len());
                for v in items.iter() {
                    keys.push(v.to_hash_key()?);
                }
                Ok(HashKey::Tuple(Rc::new(keys)))
            }
            // The VM doesn't track set-vs-frozenset distinctly today —
            // hashing through here means a regular `set` literal can also
            // appear as a dict key, which is more permissive than CPython.
            // The (much more common) flow we care about is `frozenset(...)`
            // as a dict key, which now works.
            Value::Set(s) => {
                // Members in the set's iteration order, so a frozenset read
                // back out of a dict or set iterates as it did going in;
                // `Eq` / `Hash` compare the sorted canonical encodings.
                let keys: Vec<HashKey> = s.borrow().iter().cloned().collect();
                Ok(HashKey::FrozenSet(Rc::new(keys)))
            }
            // Dataclass instances are hashable: CPython makes
            // `@dataclass(frozen=True)` instances hashable via the hash
            // of the field tuple. The VM can't observe the `frozen` flag
            // here (the `@dataclass` decorator is a no-op that drops its
            // kwargs before reaching value.rs — see the report note), so
            // we hash every instance, which is more permissive than
            // CPython but makes frozen-instance dict/set keys work. Equal
            // fields ⇒ equal key ⇒ `p2 in seen` is True.
            Value::Instance(inst) => {
                // Value-mixin enum members (`StrEnum` / `IntEnum`) hash as
                // their underlying value so `{"active": 1}[Status.ACTIVE]`
                // works exactly as it does under CPython (where the member
                // IS a str / int subclass).
                if let Some(v) = enum_mixin_value(self) {
                    return Ok(HashKey::Mixin {
                        value: Box::new(v.to_hash_key()?),
                        member: inst.clone(),
                    });
                }
                match instance_hash_mode(&inst.class) {
                    HashMode::Unhashable => {
                        return Err(type_error(format!(
                            "unhashable type: '{}'",
                            inst.class.name
                        )))
                    }
                    HashMode::Identity => return Ok(HashKey::Identity(inst.clone())),
                    // A user `__hash__` needs the interpreter
                    // (`Interpreter::hash_key`). This interpreter-free
                    // fallback keys on the field structure, which agrees
                    // with the usual "hash the fields, compare the fields"
                    // implementations.
                    HashMode::User(_) | HashMode::Fields => {}
                }
                let mut fields: Vec<(String, HashKey)> = Vec::new();
                for (name, v) in inst.fields.borrow().iter() {
                    fields.push((name.clone(), v.to_hash_key()?));
                }
                // Sort by field name so two instances with the same
                // fields in different insertion order produce identical
                // keys (the VM stores fields in an unordered HashMap).
                fields.sort_by(|a, b| a.0.cmp(&b.0));
                let key = InstanceKey {
                    class_id: Rc::as_ptr(&inst.class) as usize,
                    class_name: inst.class.name.clone(),
                    fields,
                };
                Ok(HashKey::Instance {
                    instance: inst.clone(),
                    key: Rc::new(key),
                })
            }
            // A class object is hashable by identity in CPython — this is
            // what makes a type-keyed registry (`functools.singledispatch`,
            // a visitor table) work.
            Value::Class(c) => {
                // `type(5)` hands back the VM's cached stand-in class for
                // `int` while the global `int` is the constructor native —
                // a type-keyed registry (`functools.singledispatch`) has to
                // see a single key for the two.
                if crate::builtins::is_builtin_type_class(c) {
                    return Ok(HashKey::BuiltinType(crate::interp::intern_type_name(
                        &c.name,
                    )));
                }
                Ok(HashKey::Class(c.clone()))
            }
            Value::Native(n) => Ok(HashKey::BuiltinType(n.name)),
            other => Err(type_error(format!(
                "unhashable type: '{}'",
                other.type_name()
            ))),
        }
    }

    /// Python-style equality. Unlike `PartialEq` we cross between `int` and
    /// `float`, and between `bool` and the numeric types.
    ///
    /// Guards against cyclic / pathologically deep structures: without a bound,
    /// `a = []; a.append(a); b = []; b.append(b); a == b` recurses forever and
    /// overflows the native stack, aborting the whole process. CPython raises
    /// `RecursionError` there; we can't from a `bool` fn, so we treat
    /// beyond-bound comparisons as not-provably-equal (`false`). The bound is
    /// far deeper than any real data.
    pub fn py_eq(&self, other: &Value) -> bool {
        let Some(_guard) = structural_depth_enter() else {
            return false;
        };
        self.py_eq_inner(other)
    }

    fn py_eq_inner(&self, other: &Value) -> bool {
        use Value::*;
        // A value-mixin enum member equals its value, inside containers too.
        if matches!(self, Instance(_)) != matches!(other, Instance(_)) {
            let (l, r) = (enum_mixin_value(self), enum_mixin_value(other));
            if l.is_some() || r.is_some() {
                let l = l.unwrap_or_else(|| self.clone());
                let r = r.unwrap_or_else(|| other.clone());
                return l.py_eq_inner(&r);
            }
        }
        match (self, other) {
            (None, None) => true,
            (Bool(a), Bool(b)) => a == b,
            (Bool(a), Int(b)) | (Int(b), Bool(a)) => &VmInt::from(*a as i64) == b,
            (Bool(a), FloatData(crate::value::VmFloat { value: b, .. }))
            | (FloatData(crate::value::VmFloat { value: b, .. }), Bool(a)) => {
                (*a as i64 as f64) == *b
            }
            (Int(a), Int(b)) => a == b,
            (
                FloatData(crate::value::VmFloat { value: a, .. }),
                FloatData(crate::value::VmFloat { value: b, .. }),
            ) => a == b,
            (Int(a), FloatData(crate::value::VmFloat { value: b, .. }))
            | (FloatData(crate::value::VmFloat { value: b, .. }), Int(a)) => vmint_eq_f64(a, *b),
            (Complex(ar, ai), Complex(br, bi)) => ar == br && ai == bi,
            // `complex == float` / `complex == int` only when the imaginary
            // part is zero (matching CPython).
            (Complex(re, im), FloatData(crate::value::VmFloat { value: f, .. }))
            | (FloatData(crate::value::VmFloat { value: f, .. }), Complex(re, im)) => {
                *im == 0.0 && re == f
            }
            (Complex(re, im), Int(i)) | (Int(i), Complex(re, im)) => {
                *im == 0.0 && vmint_eq_f64(i, *re)
            }
            (Complex(re, im), Bool(b)) | (Bool(b), Complex(re, im)) => {
                *im == 0.0 && *re == (*b as i64 as f64)
            }
            (Str(a), Str(b)) => a == b,
            (Bytes(a), Bytes(b)) => a == b,
            (List(a), List(b)) => {
                // Identity short-circuit: the same list object is equal to
                // itself without recursing into its (possibly self-cyclic)
                // elements.
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                let a = a.borrow();
                let b = b.borrow();
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.identical_or_equal(y))
            }
            (Tuple(a), Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.identical_or_equal(y))
            }
            (Dict(a), Dict(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                let a = a.borrow();
                let b = b.borrow();
                if a.len() != b.len() {
                    return false;
                }
                a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|w| v.identical_or_equal(w)))
            }
            (ResultOk(a), ResultOk(b)) => a.py_eq(b),
            (ResultErr(a), ResultErr(b)) => a.py_eq(b),
            // Type objects (from `type(x)`) compare by identity. Builtin type
            // objects are cached singletons and user classes are single `Rc`s,
            // so `type(a) == type(b)` and `type(inst) == SomeClass` both hold
            // without name matching (which would wrongly equate same-named
            // classes from different modules).
            (Class(a), Class(b)) => Rc::ptr_eq(a, b),
            // `type(5) == int`: the RHS `int` is the builtin constructor
            // (a native named "int"); match it against the type object's name.
            (Class(c), Native(n)) | (Native(n), Class(c)) => c.name == n.name,
            (Native(a), Native(b)) => Rc::ptr_eq(a, b),
            // A function object is equal only to itself.
            (Function(a), Function(b)) => Rc::ptr_eq(a, b),
            // Dataclass instances compare by value: same class and all
            // fields equal (recursively). CPython's generated `__eq__`
            // compares the field tuple only when the two operands are of
            // the same class; otherwise it returns `NotImplemented` →
            // `False`. Same-class is object identity (the single cached
            // `Rc<Class>` per definition), NOT name equality — two distinct
            // classes that share a name must not compare equal.
            (Instance(a), Instance(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                if !Rc::ptr_eq(&a.class, &b.class) {
                    return false;
                }
                // `object.__eq__` is identity: a plain class (no dataclass
                // `__eq__`, no user `__eq__`) never equals another instance.
                if !class_eq_by_fields(&a.class) {
                    return false;
                }
                let fa = a.fields.borrow();
                let fb = b.fields.borrow();
                if fa.len() != fb.len() {
                    return false;
                }
                // Compare in declared field order so the recursion is
                // deterministic; fall back to whatever keys exist for
                // dynamically-added attributes.
                fa.iter()
                    .all(|(k, v)| fb.get(k).is_some_and(|w| v.identical_or_equal(w)))
            }
            // Frozen metadata lives outside user-visible container contents.
            (Set(a), Set(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                a.len() == b.len() && a.iter().all(|k| b.contains(k))
            }
            _ => false,
        }
    }

    /// Python-style ordering. Returns None for incomparable types.
    ///
    /// Depth-guarded like [`py_eq`]: a cyclic list/tuple would otherwise
    /// recurse without bound. Beyond the bound we return `None` (treat as
    /// incomparable) rather than overflowing the stack.
    pub fn py_cmp(&self, other: &Value) -> Option<std::cmp::Ordering> {
        let _guard = structural_depth_enter()?;
        self.py_cmp_inner(other)
    }

    fn py_cmp_inner(&self, other: &Value) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering::*;
        use Value::*;
        // A value-mixin enum member orders as its value (it *is* an `int` /
        // `str` subclass instance in CPython).
        if matches!(self, Instance(_)) || matches!(other, Instance(_)) {
            let (l, r) = (enum_mixin_value(self), enum_mixin_value(other));
            if l.is_some() || r.is_some() {
                let l = l.unwrap_or_else(|| self.clone());
                let r = r.unwrap_or_else(|| other.clone());
                if !matches!(l, Instance(_)) && !matches!(r, Instance(_)) {
                    return l.py_cmp_inner(&r);
                }
            }
        }
        match (self, other) {
            (Int(a), Int(b)) => a.partial_cmp(b),
            (
                FloatData(crate::value::VmFloat { value: a, .. }),
                FloatData(crate::value::VmFloat { value: b, .. }),
            ) => a.partial_cmp(b),
            (Int(a), FloatData(crate::value::VmFloat { value: b, .. })) => vmint_cmp_f64(a, *b),
            (FloatData(crate::value::VmFloat { value: a, .. }), Int(b)) => {
                vmint_cmp_f64(b, *a).map(|o| o.reverse())
            }
            (Bool(a), Bool(b)) => a.partial_cmp(b),
            (Bool(a), Int(b)) => VmInt::from(*a as i64).partial_cmp(b),
            (Int(a), Bool(b)) => a.partial_cmp(&VmInt::from(*b as i64)),
            // `bool` is an `int` in CPython, so it orders against floats too
            // — `sorted([True, False, 0.5])` needs this.
            (Bool(a), FloatData(crate::value::VmFloat { value: b, .. })) => {
                f64::from(*a).partial_cmp(b)
            }
            (FloatData(crate::value::VmFloat { value: a, .. }), Bool(b)) => {
                a.partial_cmp(&f64::from(*b))
            }
            (Str(a), Str(b)) => a.partial_cmp(b),
            // `bytes` is ordered in CPython (lexicographic over the byte
            // values). Without this arm every bytes pair was "incomparable",
            // which the `Equal` fallback in `value_cmp` then turned into
            // "equal" — so `sorted([b"b", b"a"])` returned the list untouched.
            (Bytes(a), Bytes(b)) => a.partial_cmp(b),
            (Tuple(a), Tuple(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    match x.py_cmp(y)? {
                        Equal => continue,
                        ord => return Some(ord),
                    }
                }
                a.len().partial_cmp(&b.len())
            }
            (List(a), List(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                for (x, y) in a.iter().zip(b.iter()) {
                    match x.py_cmp(y)? {
                        Equal => continue,
                        ord => return Some(ord),
                    }
                }
                a.len().partial_cmp(&b.len())
            }
            _ => Option::None,
        }
    }

    /// Convert to a signed 64-bit integer. Used as the bridge between
    /// Python-style arbitrary-precision ints and Rust APIs that need a
    /// machine-sized integer (slicing, indexing, FFI). Values that
    /// don't fit in `i64` produce an `OverflowError` rather than
    /// silently truncating.
    pub fn to_int(&self) -> Result<i64, Unwind> {
        match self {
            Value::Int(i) => i.to_i64().ok_or_else(|| {
                Unwind::Exception(crate::error::VmException::new(
                    "OverflowError",
                    "Python int too large to convert to C int",
                ))
            }),
            Value::Bool(b) => Ok(*b as i64),
            // Same saturation hazard as `to_bigint`: reject rather than
            // silently clamp to `i64::MAX`.
            Value::FloatData(crate::value::VmFloat { value: x, .. }) => float_to_bigint(*x)?.to_i64().ok_or_else(|| {
                Unwind::Exception(crate::error::VmException::new(
                    "OverflowError",
                    "Python int too large to convert to C int",
                ))
            }),
            // `int(b"12")` / `float(b"1.5")` — CPython parses a
            // bytes-like the same way it parses a string.
            Value::Bytes(b) => parse_decimal_str::<i64>(&String::from_utf8_lossy(b)).map_err(|_| {
                value_error(format!(
                    "invalid literal for int() with base 10: {}",
                    Value::Bytes(b.clone()).py_repr()
                ))
            }),
            Value::Str(s) => parse_decimal_str::<i64>(s).map_err(|_| {
                value_error(format!(
                    "invalid literal for int() with base 10: {}",
                    Value::Str(s.clone()).py_repr()
                ))
            }),
            Value::Instance(_) => match enum_mixin_value(self) {
                Some(inner) => inner.to_int(),
                None => Err(type_error(format!(
                    "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                    self.type_display_name()
                ))),
            },
            _ => Err(type_error(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                self.type_display_name()
            ))),
        }
    }

    /// Convert to a `BigInt`. Use this when arithmetic should preserve
    /// arbitrary precision (FINDINGS #19).
    pub fn to_bigint(&self) -> Result<BigInt, Unwind> {
        match self {
            Value::Int(i) => Ok(i.to_bigint()),
            Value::Bool(b) => Ok(BigInt::from(*b as i64)),
            // `f64 as i64` in Rust *saturates* at `i64::MIN`/`i64::MAX` and
            // maps NaN to 0, so `int(1e30)` silently produced
            // 9223372036854775807 and `int(float("inf"))` produced the same
            // instead of raising. The VM's whole point is arbitrary precision,
            // and CPython raises `OverflowError` / `ValueError` here — so
            // convert exactly and reject what has no integer value.
            Value::FloatData(crate::value::VmFloat { value: x, .. }) => float_to_bigint(*x),
            Value::Bytes(b) => {
                parse_decimal_str::<BigInt>(&String::from_utf8_lossy(b)).map_err(|_| {
                    value_error(format!(
                        "invalid literal for int() with base 10: {}",
                        Value::Bytes(b.clone()).py_repr()
                    ))
                })
            }
            Value::Str(s) => parse_decimal_str::<BigInt>(s).map_err(|_| {
                value_error(format!(
                    "invalid literal for int() with base 10: {}",
                    Value::Str(s.clone()).py_repr()
                ))
            }),
            // An `IntEnum` / `IntFlag` / `StrEnum` member *is* its value in
            // CPython, so every numeric conversion sees through the member —
            // `"%d" % IntE.ONE` and `math.sqrt(Size.BIG)` included.
            Value::Instance(_) => match enum_mixin_value(self) {
                Some(inner) => inner.to_bigint(),
                None => Err(type_error(format!(
                    "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                    self.type_display_name()
                ))),
            },
            _ => Err(type_error(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                self.type_display_name()
            ))),
        }
    }

    pub fn to_float(&self) -> Result<f64, Unwind> {
        match self {
            Value::FloatData(crate::value::VmFloat { value: x, .. }) => Ok(*x),
            Value::Int(i) => {
                let x = i.to_f64();
                if x.is_infinite() {
                    return Err(Unwind::Exception(crate::error::VmException::new(
                        "OverflowError",
                        "int too large to convert to float",
                    )));
                }
                Ok(x)
            }
            Value::Bool(b) => Ok(*b as i64 as f64),
            Value::Bytes(b) => {
                parse_decimal_str::<f64>(&String::from_utf8_lossy(b)).map_err(|_| {
                    value_error(format!(
                        "could not convert string to float: {}",
                        Value::Bytes(b.clone()).py_repr()
                    ))
                })
            }
            Value::Str(s) => parse_decimal_str::<f64>(s).map_err(|_| {
                value_error(format!(
                    "could not convert string to float: {}",
                    Value::Str(s.clone()).py_repr()
                ))
            }),
            Value::Instance(_) => match enum_mixin_value(self) {
                Some(inner) => inner.to_float(),
                None => Err(type_error(format!(
                    "float() argument must be a string or a number, not '{}'",
                    self.type_display_name()
                ))),
            },
            _ => Err(type_error(format!(
                "float() argument must be a string or a number, not '{}'",
                self.type_display_name()
            ))),
        }
    }

    /// Python-style `str(x)` — readable representation.
    pub fn py_str(&self) -> String {
        match self {
            Value::None => "None".into(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            Value::Int(i) => i.to_str_radix(10),
            Value::FloatData(crate::value::VmFloat { value: x, .. }) => format_float(*x),
            Value::Complex(re, im) => format_complex(*re, *im),
            Value::Str(s) => (**s).clone(),
            Value::Bytes(b) => python_repr_bytes(b),
            Value::List(l) => {
                let l = l.borrow();
                let mut s = String::from("[");
                for (i, v) in l.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&v.py_repr());
                }
                s.push(']');
                s
            }
            Value::Tuple(t) => {
                if is_slice_marker(t) {
                    return slice_repr(t);
                }
                if is_ellipsis_marker(t) {
                    return "Ellipsis".to_owned();
                }
                let mut s = String::from("(");
                for (i, v) in t.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&v.py_repr());
                }
                if t.len() == 1 {
                    s.push(',');
                }
                s.push(')');
                s
            }
            Value::Dict(_) => dict_render(self, false),
            Value::Set(set) => {
                let s = set.borrow();
                let is_frozen = set.frozen.get();
                // Frozen metadata lives outside user-visible container
                // contents. CPython's iteration order (see `pyset`).
                let keys: Vec<&HashKey> = s.iter().collect();
                let items: Vec<String> = keys
                    .into_iter()
                    .map(|k| k.clone().into_value().py_repr())
                    .collect();
                if items.is_empty() {
                    return if is_frozen {
                        "frozenset()".into()
                    } else {
                        "set()".into()
                    };
                }
                let body = items.join(", ");
                if is_frozen {
                    format!("frozenset({{{body}}})")
                } else {
                    format!("{{{body}}}")
                }
            }
            Value::Range { start, stop, step } => {
                if *step == 1 {
                    format!("range({}, {})", start, stop)
                } else {
                    format!("range({}, {}, {})", start, stop, step)
                }
            }
            Value::Native(n) => native_repr(n.name),
            Value::Function(func) => format!("<function {}>", func.name),
            // CPython names the class the method was found on and reprs
            // the receiver: `<bound method Path.iterdir of PosixPath('/t')>`.
            Value::BoundMethod { function, receiver } => {
                let owner = match receiver.as_ref() {
                    Value::Instance(i) => format!("{}.", i.class.name),
                    Value::Class(c) => format!("{}.", c.name),
                    _ => String::new(),
                };
                format!(
                    "<bound method {}{} of {}>",
                    owner,
                    function.name,
                    receiver.py_repr()
                )
            }
            Value::Class(c) => class_repr(c),
            // `str(exc)` differs from `repr(exc)` for a field-less exception
            // instance: the message/args, not `ClassName('msg')`.
            Value::Instance(i) => match exception_instance_args(i) {
                Some(args) => exception_instance_str(i, &args),
                None if class_is_pydantic_model(&i.class) => model_instance_str(i),
                None => instance_repr(i),
            },
            // Match the dataclass-default `repr` shape that
            // `typhon_runtime`'s `Ok` / `Err` produce under CPython
            // (`Foo(value=42)` / `Foo(error=...)`). The Debug-impl
            // arm above already carries the same comment — keeping
            // these in sync prevents `tyc run` vs `tyc run --compile`
            // stdout from diverging on Result printing (FINDINGS O24).
            Value::ResultOk(v) => format!("Ok(value={})", v.py_repr()),
            Value::ResultErr(v) => format!("Err(error={})", v.py_repr()),
            Value::Module(m) => {
                if m.members
                    .borrow()
                    .contains_key(crate::interp::LAZY_MODULE_TARGET)
                {
                    format!("<lazy module '{}': unloaded>", m.name)
                } else {
                    format!("<module '{}'>", m.name)
                }
            }
            Value::Coroutine(c) => format!("<coroutine object {}>", c.function.name),
            Value::Exception {
                kind,
                message,
                args,
                ..
            } => match args.len() {
                // `str(ExceptionGroup("g", [e]))` is `"g (1 sub-exception)"`,
                // not the generic multi-arg tuple form below.
                _ if is_exception_group_kind(kind.as_str()) => {
                    let n = exception_group_subs(self).map(|s| s.len()).unwrap_or(0);
                    let plural = if n == 1 { "" } else { "s" };
                    format!("{message} ({n} sub-exception{plural})")
                }
                // `OSError(errno, strerror[, filename])` renders the
                // `[Errno N] strerror: 'filename'` form kept in `message`.
                n if n >= 2 && crate::builtins::is_os_error_kind(kind.as_str()) => {
                    (**message).clone()
                }
                // `str(ValueError("a", "b"))` is the tuple `('a', 'b')`.
                n if n >= 2 => {
                    let parts: Vec<String> = args.iter().map(|a| a.py_repr()).collect();
                    format!("({})", parts.join(", "))
                }
                // `KeyError` is the one builtin whose `str()` shows the
                // *repr* of its single argument: `str(KeyError("k"))` is
                // `"'k'"`, not `"k"` (so a missing key is unambiguous).
                1 if kind.as_str() == "KeyError" => args[0].py_repr(),
                1 => args[0].py_str(),
                // No arguments: `str(ValueError())` is the empty string. A
                // VM-raised exception that carries only a message (no
                // `args`) still renders that message.
                _ => (**message).clone(),
            },
            Value::Iter(it) => {
                let state = it.borrow();
                match &*state {
                    IterState::Generator(g) => format!(
                        "<generator object {} at {:#x}>",
                        g.borrow().function.name,
                        Rc::as_ptr(it) as usize
                    ),
                    IterState::GenExpr(_) => {
                        format!(
                            "<generator object <genexpr> at {:#x}>",
                            Rc::as_ptr(it) as usize
                        )
                    }
                    other => format!(
                        "<{} object at {:#x}>",
                        iter_type_name(other),
                        Rc::as_ptr(it) as usize
                    ),
                }
            }
            Value::DictView { kind, dict } => {
                let items = view_items(*kind, dict);
                let prefix = match kind {
                    DictViewKind::Keys => "dict_keys",
                    DictViewKind::Values => "dict_values",
                    DictViewKind::Items => "dict_items",
                };
                let body: Vec<String> = items.iter().map(|v| v.py_repr()).collect();
                format!("{prefix}([{}])", body.join(", "))
            }
        }
    }

    /// Python-style `repr(x)`. Differs from `py_str` for strings — adds quotes.
    /// CPython's repr prefers single quotes (`'hello'`) and falls back to
    /// double quotes only when the string itself contains a single quote
    /// but no double quote; matching that shape keeps `tyc run` and
    /// `tyc run --compile` byte-equal for collections of strings, which
    /// the docs and test fixtures rely on (FINDINGS O24, companion to
    /// the `Ok(value=...)` / `Err(error=...)` rename).
    pub fn py_repr(&self) -> String {
        match self {
            Value::Str(s) => python_repr_str(s.as_str()),
            Value::Dict(_) => dict_render(self, true),
            // `repr(exc)` keeps the `ClassName('msg')` form even though
            // `str(exc)` (py_str) renders just the message.
            Value::Instance(i) => instance_repr(i),
            // `repr(ValueError("boom"))` is `ValueError('boom')` (and
            // `KeyError('k')`, `ValueError()` for no args) — the constructor
            // form, not the `str()` message. A VM-raised exception carrying
            // only a message renders it as the single argument.
            Value::Exception {
                kind,
                args,
                message,
                ..
            } => {
                if args.is_empty() && !message.is_empty() {
                    return format!("{}({})", kind, python_repr_str(message));
                }
                let parts: Vec<String> = args.iter().map(|a| a.py_repr()).collect();
                format!("{}({})", kind, parts.join(", "))
            }
            other => other.py_str(),
        }
    }
}

/// CPython-style `repr` for `bytes`: `b'...'`, falling back to `b"..."` if
/// the value contains a `'` but no `"`. Non-printable bytes use `\xNN`,
/// `\n` / `\r` / `\t` retain their named escapes, and `\\` is escaped.
fn python_repr_bytes(b: &[u8]) -> String {
    let has_single = b.contains(&b'\'');
    let has_double = b.contains(&b'"');
    let quote = if has_single && !has_double {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::with_capacity(b.len() + 3);
    out.push('b');
    out.push(quote as char);
    for &byte in b {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c as char);
            }
            c if (0x20..0x7f).contains(&c) => out.push(c as char),
            c => {
                out.push_str("\\x");
                let hex = b"0123456789abcdef";
                out.push(hex[(c as usize >> 4) & 0xf] as char);
                out.push(hex[c as usize & 0xf] as char);
            }
        }
    }
    out.push(quote as char);
    out
}

/// CPython-style `repr` for a string: prefer single quotes, escape the
/// active quote and backslashes, and fall back to double quotes when
/// the string itself contains a `'` but no `"`. The escape set matches
/// CPython's reprlib: `\\`, `\n`, `\r`, `\t`, and `\x..` for other
/// ASCII control characters.
pub(crate) fn python_repr_str(s: &str) -> String {
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // CPython escapes exactly what `str.isprintable()` rejects — the
            // controls, but also the format, separator, private-use and
            // unassigned characters, which pass through invisibly otherwise.
            c if !crate::builtins::unicode_is_printable(c) => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Convert a `BigInt` to `f64`, losing precision for very large values
/// the same way CPython does (`int → float` is a quiet "down-cast").
pub fn bigint_to_f64(i: &BigInt) -> f64 {
    if let Some(v) = i.to_f64() {
        return v;
    }
    // Fallback for values too large for `to_f64` (shouldn't happen with
    // num-bigint's impl, but stay defensive).
    if i.is_negative() {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    }
}

/// Compare a `BigInt` to an `f64` without precision loss for large
/// integers. Converting `a` through `f64` would round any value outside
/// the 53-bit float mantissa and cause `a == b` (or wrong-direction
/// ordering) for very large operands. Instead, handle infinities
/// directly, lift `b` to a `BigInt` via its floor/ceil, and compare
/// against those exact integers.
pub fn bigint_cmp_f64(a: &BigInt, b: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if b.is_nan() {
        return None;
    }
    if b == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if b == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    // When `b` is itself an exact integer in f64, lift it directly.
    if b.trunc() == b {
        if let Some(bi) = BigInt::from_f64(b) {
            return Some(a.cmp(&bi));
        }
    }
    // Otherwise compare against the floor: a ≤ ⌊b⌋ → Less, else Greater.
    let bi_floor = BigInt::from_f64(b.floor())?;
    if *a <= bi_floor {
        Some(Ordering::Less)
    } else {
        Some(Ordering::Greater)
    }
}

/// `int == float` modelled after CPython: equal only when the float
/// represents the same integer value exactly.
pub fn bigint_eq_f64(a: &BigInt, b: f64) -> bool {
    if !b.is_finite() {
        return false;
    }
    if b.trunc() != b {
        return false;
    }
    // Convert b to BigInt via the integral part.
    let bi = match BigInt::from_f64(b) {
        Some(v) => v,
        None => return false,
    };
    a == &bi
}

/// CPython-style `repr` of a dataclass instance: `ClassName(field=value,
/// ...)` with fields in declaration order. CPython's synthesised
/// `__repr__` walks `__dataclass_fields__` (declaration order) and uses
/// `repr()` of each value. The VM stores fields in an unordered
/// `HashMap`, so we iterate `class.fields` (which preserves source
/// order) to recover the order; any field not declared on the class
/// (dynamically assigned) is appended afterwards, sorted by name for
/// determinism.
/// Whether `class` is an enum class — its own `class_attrs` carry the
/// `__typhon_enum_base__` sentinel, or one of its bases does (the user's
/// `Color(Enum)` inherits the flag from the synthetic `Enum` base).
fn class_is_enum(class: &Class) -> bool {
    class
        .class_attrs
        .borrow()
        .contains_key("__typhon_enum_base__")
        || class.bases.iter().any(|b| class_is_enum(b))
}

/// `args` tuple of a field-less exception instance built by the VM's
/// `BaseException`-style construction path (see `Interpreter::instantiate`).
/// `Some` only when the instance carries the stashed `args` tuple, so
/// field-carrying exceptions (which keep dataclass-style rendering) and
/// ordinary instances are unaffected.
fn exception_instance_args(inst: &Instance) -> Option<Rc<Vec<Value>>> {
    if !inst.class.is_exception {
        return None;
    }
    match inst.fields.borrow().get("args") {
        Some(Value::Tuple(t)) => Some(t.clone()),
        _ => None,
    }
}

/// Whether an exception class derives (directly or through its user base
/// chain) from the builtin `KeyError`. Reads the `__typhon_exc_bases__`
/// record stamped on each class by the interpreter's `build_class`.
fn class_derives_from_keyerror(class: &Class) -> bool {
    if class.name == "KeyError" {
        return true;
    }
    if let Some(Value::Tuple(names)) = class.class_attrs.borrow().get("__typhon_exc_bases__") {
        if names
            .iter()
            .any(|nm| matches!(nm, Value::Str(s) if s.as_str() == "KeyError"))
        {
            return true;
        }
    }
    class.bases.iter().any(|b| class_derives_from_keyerror(b))
}

/// CPython `str(exc)` for a field-less exception instance: `""` for no args,
/// the single arg for one, the args tuple otherwise. `KeyError` is the one
/// builtin whose single-arg `str()` shows the *repr* of the key
/// (`str(KeyError("k")) == "'k'"`), so its subclasses inherit that.
fn exception_instance_str(inst: &Instance, args: &[Value]) -> String {
    match args.len() {
        0 => String::new(),
        1 if class_derives_from_keyerror(&inst.class) => args[0].py_repr(),
        1 => args[0].py_str(),
        _ => {
            let parts: Vec<String> = args.iter().map(|a| a.py_repr()).collect();
            format!("({})", parts.join(", "))
        }
    }
}

fn instance_repr(inst: &Instance) -> String {
    // Field-less exception instances repr as `ClassName(arg_reprs)` —
    // CPython's `repr(FooError("x"))` shape — not as dataclass fields.
    if let Some(args) = exception_instance_args(inst) {
        let parts: Vec<String> = args.iter().map(|a| a.py_repr()).collect();
        return format!("{}({})", inst.class.name, parts.join(", "));
    }
    instance_repr_inner(inst)
}

fn instance_repr_inner(inst: &Instance) -> String {
    let fields = inst.fields.borrow();
    // A `__slots__` member descriptor (`Cls.field` on a slots dataclass)
    // reprs as CPython's `<member 'name' of 'Cls' objects>`.
    if inst.class.name == "member_descriptor" {
        if let (Some(Value::Str(name)), Some(Value::Str(owner))) =
            (fields.get("__name__"), fields.get("__objclass__"))
        {
            return format!("<member '{name}' of '{owner}' objects>");
        }
    }
    // Enum members repr as `<Class.NAME: value>` (CPython default), not as
    // their backing dataclass fields.
    if class_is_enum(&inst.class) {
        if let (Some(Value::Str(name)), Some(val)) = (fields.get("_name_"), fields.get("_value_")) {
            return format!("<{}.{}: {}>", inst.class.name, name, val.py_repr());
        }
    }
    // `@dataclass(repr=False)` generates no `__repr__`: the nearest
    // ancestor's applies — another dataclass's (its own fields), else
    // `object.__repr__`.
    let mut repr_fields = &inst.class.fields;
    if class_is_dataclass(&inst.class) && !class_flag(&inst.class, "__typhon_dc_repr__", true) {
        match inst
            .class
            .mro
            .iter()
            .find(|c| class_is_dataclass(c) && class_flag(c, "__typhon_dc_repr__", true))
        {
            Some(provider) => repr_fields = &provider.fields,
            None => return object_default_repr(inst),
        }
    }
    let mut parts: Vec<String> = Vec::with_capacity(fields.len());
    for cf in repr_fields {
        if let Some(v) = fields.get(&cf.name) {
            parts.push(format!("{}={}", cf.name, v.py_repr()));
        }
    }
    // A dataclass (or pydantic model) repr shows its declared fields and
    // nothing else — not class attributes copied onto the instance (a
    // `model`'s `model_config` leaked into every repr before this) and not
    // attributes assigned later. A class with no declared fields that is
    // neither (a `plain class` / `class!` without a `__repr__`) gets
    // `object.__repr__`, exactly like CPython.
    if inst.class.fields.is_empty()
        && !class_is_dataclass(&inst.class)
        && !class_is_pydantic_model(&inst.class)
    {
        return object_default_repr(inst);
    }
    format!("{}({})", inst.class.name, parts.join(", "))
}

/// `repr()` of a class object. CPython qualifies it with the module the class
/// body ran in — `<class '__main__.User'>`, not `<class 'User'>` — which the
/// VM records as `__typhon_module__`. A stdlib shim's classes carry no module
/// (CPython's name for them is not the shim's), so those stay bare.
pub fn class_repr(class: &Class) -> String {
    match class.class_attrs.borrow().get("__typhon_module__") {
        Some(Value::Str(m)) if !m.is_empty() => format!("<class '{m}.{}'>", class.name),
        _ => format!("<class '{}'>", class.name),
    }
}

/// `object.__repr__`: `<module.Class object at 0x…>`. The module is the one
/// the class body ran in (`__typhon_module__`, recorded by `build_class`);
/// the address is the instance's allocation, stable for its lifetime.
pub fn object_default_repr(inst: &Instance) -> String {
    let module = match inst.class.class_attrs.borrow().get("__typhon_module__") {
        Some(Value::Str(s)) => (**s).clone(),
        _ => "__main__".to_owned(),
    };
    format!(
        "<{}.{} object at {:#x}>",
        module, inst.class.name, inst as *const Instance as usize
    )
}

/// `str()` of a pydantic `model` instance: `BaseModel.__str__` renders the
/// fields space-separated with no class name — `id=2 name='Grace' age=None`.
fn model_instance_str(inst: &Instance) -> String {
    let fields = inst.fields.borrow();
    let parts: Vec<String> = inst
        .class
        .fields
        .iter()
        .filter_map(|cf| {
            fields
                .get(&cf.name)
                .map(|v| format!("{}={}", cf.name, v.py_repr()))
        })
        .collect();
    parts.join(" ")
}

/// Whether a class was declared with Typhon's `model` keyword (a pydantic
/// `BaseModel` subclass). The interpreter stamps the marker at class-definition
/// time; it inherits along with the other class attributes.
pub fn class_is_pydantic_model(class: &Class) -> bool {
    class
        .class_attrs
        .borrow()
        .contains_key("__typhon_pydantic_model__")
}

/// Whether a class is a dataclass — Typhon's default `class` (and `class …
/// frozen`) — as `dataclasses.is_dataclass` reports it.
pub fn class_is_dataclass(class: &Class) -> bool {
    class
        .class_attrs
        .borrow()
        .contains_key("__typhon_dataclass__")
}

/// CPython-compatible `repr(float)`. Produces the shortest string that
/// round-trips to the same `f64`, switching to scientific notation with
/// the same thresholds CPython uses: a decimal exponent `< -4` or
/// `>= 16` uses `e+NN` / `e-NN` with at least two exponent digits;
/// everything else uses fixed notation. Whole-valued floats keep a
/// trailing `.0`.
/// Format a single component of a complex number the way CPython does inside
/// `repr(complex)`: like `repr(float)` but trailing `.0` is dropped (so `3.0`
/// → `3`) and signed zero shows as `0` / `-0`.
fn format_complex_part(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = format_float(x);
    // Drop a trailing `.0` (CPython prints `3+4j`, not `3.0+4.0j`).
    if let Some(stripped) = s.strip_suffix(".0") {
        stripped.to_owned()
    } else {
        s
    }
}

/// CPython-exact `repr(complex)`. Bare `4j` when the real part is `+0.0`;
/// otherwise parenthesised `(a+bj)` / `(a-bj)`.
fn format_complex(re: f64, im: f64) -> String {
    // Bare imaginary form only when the real part is positive zero.
    if re == 0.0 && re.is_sign_positive() {
        return format!("{}j", format_complex_part(im));
    }
    let real = format_complex_part(re);
    let imag = format_complex_part(im);
    // The imaginary part always carries an explicit sign separator in the
    // parenthesised form. `format_complex_part` already emits a leading `-`
    // for negatives (and for `-0`), so prepend `+` only for the rest.
    if imag.starts_with('-') {
        format!("({real}{imag}j)")
    } else {
        format!("({real}+{imag}j)")
    }
}

/// The code point of `0` in each Unicode decimal-digit (`Nd`) block, from
/// the Unicode data CPython itself uses (generated with `unicodedata`).
/// Every block is exactly ten consecutive code points, so a character's
/// digit value is its offset from the block's zero.
const DECIMAL_DIGIT_BLOCKS: &[u32] = &[
    0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
    0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80,
    0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0,
    0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60,
    0x16AC0, 0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0,
    0x1E950, 0x1FBF0,
];

/// The decimal value of a Unicode digit character, as CPython's `int()` /
/// `float()` accept: ASCII plus every `Nd` block, so `int("１２３")` is 123
/// and `int("٣")` is 3.
pub fn unicode_decimal_digit(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c as u32 - u32::from(b'0'));
    }
    let cp = c as u32;
    DECIMAL_DIGIT_BLOCKS
        .iter()
        .find(|&&zero| cp >= zero && cp < zero + 10)
        .map(|&zero| cp - zero)
}

/// `s` with every Unicode decimal digit folded to its ASCII counterpart,
/// or `None` when it holds no non-ASCII digit (the overwhelmingly common
/// case, which then needs no allocation).
pub fn fold_unicode_digits(s: &str) -> Option<String> {
    if s.is_ascii() {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut folded = false;
    for c in s.chars() {
        match unicode_decimal_digit(c) {
            Some(d) if !c.is_ascii() => {
                out.push(char::from(b'0' + d as u8));
                folded = true;
            }
            _ => out.push(c),
        }
    }
    if folded {
        Some(out)
    } else {
        None
    }
}

/// `str::parse` over a Python numeric literal: leading / trailing
/// whitespace is stripped, and Unicode decimal digits are folded to ASCII
/// first, so `int("１２３")` and `float("１.５")` work as they do in CPython.
fn parse_decimal_str<T: std::str::FromStr>(s: &str) -> Result<T, T::Err> {
    let trimmed = s.trim();
    let folded = fold_unicode_digits(trimmed);
    let text = folded.as_deref().unwrap_or(trimmed);
    match strip_python_underscores(text) {
        Some(clean) => clean.parse::<T>(),
        // Python allows a single `_` only *between* digits (`1_000.5` yes,
        // `1__0` / `_1` / `1_` no). `num-bigint` accepts the invalid forms
        // and `f64` rejects the valid one, so both are settled here; a
        // string no parser accepts produces the right error type.
        None => "\u{0}".parse::<T>(),
    }
}

/// `s` without its Python digit-group underscores, or `None` when one of
/// them is not between two digits.
fn strip_python_underscores(s: &str) -> Option<std::borrow::Cow<'_, str>> {
    if !s.contains('_') {
        return Some(std::borrow::Cow::Borrowed(s));
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'_' {
            continue;
        }
        let prev = i.checked_sub(1).map(|p| bytes[p]);
        let next = bytes.get(i + 1).copied();
        if !prev.is_some_and(|c| c.is_ascii_digit()) || !next.is_some_and(|c| c.is_ascii_digit()) {
            return None;
        }
    }
    Some(std::borrow::Cow::Owned(s.replace('_', "")))
}

/// `str(x)` for a float, for callers outside this module.
pub fn float_str(x: f64) -> String {
    format_float(x)
}

fn format_float(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        // Preserve the sign of zero (`-0.0`), matching CPython.
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }

    // Rust's `{}` formatter already yields the shortest round-tripping
    // decimal (Ryū), but never uses scientific notation and never appends
    // a trailing `.0`. CPython's `repr` switches to scientific notation
    // based on the decimal exponent of the most-significant digit, so we
    // derive that exponent and reformat to match.
    // The exponent comes from the shortest round-tripping form itself, not
    // from `log10`: `9999999999999998.0_f64.log10()` rounds up to exactly
    // 16.0, which pushed a value CPython prints in full into scientific
    // notation.
    let sci = format!("{:e}", x);
    let exp10: i32 = sci
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);

    if !(-4..16).contains(&exp10) {
        return format_float_scientific(x);
    }

    let s = tie_break_even(&format!("{}", x), x);
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{}.0", s)
    }
}

/// CPython's `repr` is the shortest digit string that round-trips and, when
/// several strings of that length do, the one *closest* to the double's
/// exact value (ties to even) — David Gay's `dtoa` mode 0. Rust's shortest
/// formatter finds the same length but not always the same digits:
/// `(-4819706.21)**2` is exactly `23229567950712.5625`, and Rust prints
/// `…712.563` where CPython prints `…712.562`; `1e15 + 0.3` (exactly
/// `…0.25`) likewise.
///
/// The closest string of that length is the correctly rounded one, which
/// Rust's fixed-precision formatting produces (it rounds ties to even, as
/// C does); use it whenever it also round-trips. Below 16 significant
/// digits a double's rounding interval holds only one candidate, so only
/// the long forms need the check.
fn tie_break_even(s: &str, x: f64) -> String {
    let mantissa_end = s.find(['e', 'E']).unwrap_or(s.len());
    let digits: Vec<u8> = s[..mantissa_end]
        .bytes()
        .filter(u8::is_ascii_digit)
        .collect();
    let lead = digits.iter().take_while(|d| **d == b'0').count();
    let k = digits.len() - lead;
    if k < 16 {
        return s.to_owned();
    }
    let rounded = format!("{:.*e}", k - 1, x.abs());
    let best: Vec<u8> = rounded[..rounded.find('e').unwrap_or(rounded.len())]
        .bytes()
        .filter(u8::is_ascii_digit)
        .collect();
    if best.len() != k || best[..] == digits[lead..] {
        return s.to_owned();
    }
    // Put the correctly rounded digits in place of the shortest ones.
    let mut seen = 0usize;
    let candidate: String = s
        .char_indices()
        .map(|(i, c)| {
            if i < mantissa_end && c.is_ascii_digit() {
                seen += 1;
                if seen > lead {
                    return best[seen - lead - 1] as char;
                }
            }
            c
        })
        .collect();
    if candidate
        .parse::<f64>()
        .is_ok_and(|v| v.to_bits() == x.to_bits())
    {
        candidate
    } else {
        s.to_owned()
    }
}

/// Format a float in CPython's scientific-notation style: shortest
/// round-tripping mantissa, `e+NN` / `e-NN` exponent with at least two
/// digits.
fn format_float_scientific(x: f64) -> String {
    // Rust's `{:e}` gives a shortest mantissa with a base-10 exponent but
    // formats the exponent without a sign or zero-padding (`1e20`,
    // `1.5e-5`). Reformat the exponent to CPython's `e+NN` / `e-NN`.
    let raw = tie_break_even(&format!("{:e}", x), x);
    let (mantissa, exp_str) = match raw.split_once('e') {
        Some((m, e)) => (m, e),
        None => return raw,
    };
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{}e{}{:02}", mantissa, sign, exp.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_small(v: &VmInt) -> bool {
        matches!(v, VmInt::Small(_))
    }
    fn is_big(v: &VmInt) -> bool {
        matches!(v, VmInt::Big(_))
    }

    #[test]
    fn vmint_i64_boundary_promotion() {
        // i64::MAX + 1 overflows i64 → must promote to Big and hold the exact
        // arbitrary-precision value.
        let max = VmInt::from(i64::MAX);
        let one = VmInt::from(1i64);
        let over = max.add(&one);
        assert!(is_big(&over), "i64::MAX + 1 must be Big");
        assert_eq!(
            over.to_string(),
            (BigInt::from(i64::MAX) + BigInt::from(1i64)).to_string()
        );

        // i64::MIN - 1 underflows → Big.
        let min = VmInt::from(i64::MIN);
        let under = min.sub(&one);
        assert!(is_big(&under), "i64::MIN - 1 must be Big");
        assert_eq!(
            under.to_string(),
            (BigInt::from(i64::MIN) - BigInt::from(1i64)).to_string()
        );

        // i64::MIN / -1 overflows the machine div → Big (= 2^63).
        let neg_one = VmInt::from(-1i64);
        let q = min.div_floor(&neg_one);
        assert!(is_big(&q), "i64::MIN / -1 must be Big");
        assert_eq!(q.to_string(), "9223372036854775808");

        // -(i64::MIN) overflows negation → Big.
        let negated = -&min;
        assert!(is_big(&negated), "-(i64::MIN) must be Big");
        assert_eq!(negated.to_string(), "9223372036854775808");

        // abs(i64::MIN) likewise.
        assert!(is_big(&min.abs()));
        assert_eq!(min.abs().to_string(), "9223372036854775808");

        // i64::MIN % -1 == 0 (the overflow-prone case), and normalises to Small.
        let m = min.mod_floor(&neg_one);
        assert!(is_small(&m));
        assert!(m.is_zero());
    }

    #[test]
    fn vmint_normalises_in_range_bigint_to_small() {
        // A `BigInt` that fits i64 must land back in `Small` — the whole
        // invariant that makes Eq/Ord/Hash trivial.
        assert!(is_small(&VmInt::from(BigInt::from(5i64))));
        assert!(is_small(&VmInt::from(BigInt::from(i64::MAX))));
        assert!(is_small(&VmInt::from(BigInt::from(i64::MIN))));
        let two_pow_63 = BigInt::from(i64::MAX) + BigInt::from(1i64); // 2^63
        assert!(is_big(&VmInt::from(two_pow_63.clone())));

        // Big + Big whose result fits i64 must demote back to Small
        // (2^63 + (-(2^63) - 1) = -1).
        let a = VmInt::from(two_pow_63.clone()); // 2^63, Big
        let b = VmInt::from(-two_pow_63 - BigInt::from(1i64)); // -(2^63)-1, Big
        assert!(is_big(&a) && is_big(&b));
        let sum = a.add(&b);
        assert!(is_small(&sum), "Big+Big landing in range must renormalise");
        assert_eq!(sum, VmInt::from(-1i64));
    }

    #[test]
    fn vmint_2_pow_100() {
        let two = VmInt::from(2i64);
        let r = two.pow(100);
        assert!(is_big(&r));
        assert_eq!(r.to_string(), "1267650600228229401496703205376");
    }

    #[test]
    fn vmint_floordiv_mod_sign_matrix_small_and_big() {
        // Exhaustive sign matrix, checked against the BigInt reference for both
        // in-range (`Small`) and out-of-range (`Big`) operands. Python `//`
        // floors toward -inf and `%` takes the divisor's sign.
        let base: [i64; 6] = [7, -7, 8, -8, 1, -1];
        let scales: [i64; 2] = [1, 5_000_000_000]; // 2nd scale forces Big
        for &sa in &base {
            for &sb in &base {
                for &scale in &scales {
                    let ba = BigInt::from(sa) * scale;
                    let bb = BigInt::from(sb) * scale;
                    if bb == BigInt::from(0) {
                        continue;
                    }
                    let va = VmInt::from(ba.clone());
                    let vb = VmInt::from(bb.clone());
                    assert_eq!(
                        va.div_floor(&vb).to_string(),
                        ba.div_floor(&bb).to_string(),
                        "div_floor {ba} // {bb}"
                    );
                    assert_eq!(
                        va.mod_floor(&vb).to_string(),
                        ba.mod_floor(&bb).to_string(),
                        "mod_floor {ba} % {bb}"
                    );
                }
            }
        }
    }

    #[test]
    fn vmint_eq_ord_across_representations() {
        let small = VmInt::from(5i64);
        let big = VmInt::from(BigInt::from(i64::MAX) + 10); // positive Big
        let neg_big = VmInt::from(BigInt::from(i64::MIN) - 10); // negative Big
                                                                // A Small and a Big are never equal.
        assert_ne!(small, big);
        assert_ne!(small, neg_big);
        // Sign of the Big alone orders it against any Small.
        assert!(small < big);
        assert!(small > neg_big);
        assert!(neg_big < big);
        // Same-representation ordering still holds.
        assert!(VmInt::from(3i64) < VmInt::from(4i64));
        assert_eq!(VmInt::from(42i64), VmInt::from(BigInt::from(42)));
    }

    fn hash_of(k: &HashKey) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        k.hash(&mut h);
        h.finish()
    }

    #[test]
    fn hashkey_numeric_collapse_holds_with_vmint() {
        // CPython: `1 == 1.0 == True` as dict keys, and their hashes agree.
        let ki = HashKey::Int(VmInt::from(1i64));
        let kf = HashKey::Float(1.0f64.to_bits());
        let kb = HashKey::Bool(true);
        assert_eq!(ki, kf);
        assert_eq!(ki, kb);
        assert_eq!(kf, kb);
        assert_eq!(hash_of(&ki), hash_of(&kf));
        assert_eq!(hash_of(&ki), hash_of(&kb));

        // A Small int and the same value arriving as a normalised BigInt are
        // the same key (invariant: both are `Small`).
        assert_eq!(
            HashKey::Int(VmInt::from(7i64)),
            HashKey::Int(VmInt::from(BigInt::from(7)))
        );
        assert_eq!(
            hash_of(&HashKey::Int(VmInt::from(7i64))),
            hash_of(&HashKey::Int(VmInt::from(BigInt::from(7))))
        );

        // A large integral float collapses with the equal Big int key.
        let big = BigInt::from(1u64 << 60) * BigInt::from(16i64); // 2^64, exactly representable as f64
        let kbig_int = HashKey::Int(VmInt::from(big.clone()));
        let kbig_float = HashKey::Float((2f64.powi(64)).to_bits());
        assert_eq!(kbig_int, kbig_float);
        assert_eq!(hash_of(&kbig_int), hash_of(&kbig_float));

        // A non-integral float is a distinct key from any int.
        assert_ne!(
            HashKey::Int(VmInt::from(1i64)),
            HashKey::Float(1.5f64.to_bits())
        );
    }

    #[test]
    fn hashkey_zero_floats_collapse_regardless_of_sign() {
        // CPython: `0.0 == -0.0` and `hash(0.0) == hash(-0.0)`, so
        // `{0.0: "a", -0.0: "b"}` is a single entry. The keys differ in
        // bit pattern (`-0.0` has the sign bit set), so a bit-pattern
        // `Eq` split them into two slots — and broke transitivity, since
        // both were already equal to `Int(0)` through the integral arms.
        let pz = HashKey::Float(0.0f64.to_bits());
        let nz = HashKey::Float((-0.0f64).to_bits());
        let zero = HashKey::Int(VmInt::from(0i64));
        assert_eq!(pz, nz);
        assert_eq!(pz, zero);
        assert_eq!(nz, zero);
        assert_eq!(hash_of(&pz), hash_of(&nz));
        assert_eq!(hash_of(&pz), hash_of(&zero));

        // NaN keys stay reflexively equal (same bits) — required for `Eq` —
        // while NaNs with different payloads remain distinct keys.
        let nan = HashKey::Float(f64::NAN.to_bits());
        assert_eq!(nan, nan.clone());
        let other_nan = HashKey::Float(f64::NAN.to_bits() ^ 1);
        assert_ne!(nan, other_nan);
    }
    use std::rc::Rc;

    #[test]
    fn py_repr_string_uses_single_quotes_by_default() {
        let v = Value::Str(Rc::new("hello".to_owned()));
        assert_eq!(v.py_repr(), "'hello'");
    }

    #[test]
    fn py_repr_string_falls_back_to_double_quotes_when_single_present() {
        let v = Value::Str(Rc::new("it's".to_owned()));
        assert_eq!(v.py_repr(), "\"it's\"");
    }

    #[test]
    fn py_repr_string_escapes_active_quote_when_both_present() {
        let v = Value::Str(Rc::new("it's \"hard\"".to_owned()));
        assert_eq!(v.py_repr(), "'it\\'s \"hard\"'");
    }

    #[test]
    fn py_repr_ok_uses_dataclass_shape() {
        // FINDINGS O24: VM repr of `Ok(20)` was diverging from the
        // CPython dataclass default `Ok(value=20)`. The two must match
        // so `tyc run` and `tyc run --compile` produce byte-identical
        // stdout for documented Result programs.
        let v = Value::ResultOk(Box::new(Value::Int(VmInt::from(20))));
        assert_eq!(v.py_repr(), "Ok(value=20)");
        let e = Value::ResultErr(Box::new(Value::Str(Rc::new("oops".to_owned()))));
        assert_eq!(e.py_repr(), "Err(error='oops')");
    }

    /// A `class X frozen:` shape — a frozen dataclass, whose instances
    /// compare and hash by their fields.
    fn mk_class(name: &str, field_names: &[&str]) -> Rc<Class> {
        let mut class_attrs = HashMap::new();
        class_attrs.insert("__typhon_dataclass__".to_owned(), Value::Bool(true));
        class_attrs.insert("__typhon_dc_frozen__".to_owned(), Value::Bool(true));
        Rc::new(Class {
            name: name.to_owned(),
            methods: RefCell::new(HashMap::new()),
            fields: field_names
                .iter()
                .map(|n| ClassField {
                    name: (*n).to_owned(),
                    default: Option::None,
                    annotation: Option::None,
                })
                .collect(),
            class_attrs: RefCell::new(class_attrs),
            bases: vec![],
            mro: vec![],
            properties: RefCell::new(std::collections::HashSet::new()),
            classmethods: RefCell::new(std::collections::HashSet::new()),
            is_exception: false,
            is_protocol: false,
        })
    }

    fn mk_instance(class: &Rc<Class>, fields: &[(&str, Value)]) -> Value {
        let mut map: FieldMap = FieldMap::new();
        for (k, v) in fields {
            map.insert((*k).to_owned(), v.clone());
        }
        Value::Instance(Rc::new(Instance {
            class: class.clone(),
            fields: RefCell::new(map),
            chain: RefCell::new(None),
        }))
    }

    #[test]
    fn instance_value_equality_and_repr() {
        let p = mk_class("P", &["x", "y"]);
        let a = mk_instance(
            &p,
            &[("x", Value::Int(1.into())), ("y", Value::Int(2.into()))],
        );
        let b = mk_instance(
            &p,
            &[("y", Value::Int(2.into())), ("x", Value::Int(1.into()))],
        );
        let c = mk_instance(
            &p,
            &[("x", Value::Int(3.into())), ("y", Value::Int(4.into()))],
        );
        assert!(a.py_eq(&b)); // same class, equal fields (order-independent)
        assert!(!a.py_eq(&c));
        // repr is in declared field order regardless of insertion order.
        assert_eq!(a.py_repr(), "P(x=1, y=2)");
        assert_eq!(b.py_repr(), "P(x=1, y=2)");
    }

    #[test]
    fn instance_hash_key_equal_for_equal_fields() {
        let p = mk_class("P", &["x", "y"]);
        let a = mk_instance(
            &p,
            &[("x", Value::Int(1.into())), ("y", Value::Int(2.into()))],
        );
        let b = mk_instance(
            &p,
            &[("y", Value::Int(2.into())), ("x", Value::Int(1.into()))],
        );
        let ka = a.to_hash_key().unwrap();
        let kb = b.to_hash_key().unwrap();
        assert_eq!(ka, kb);
        assert!(matches!(ka.into_value(), Value::Instance(_)));
    }

    #[test]
    fn distinct_same_named_classes_do_not_collide() {
        // Two separate `class P` definitions (distinct `Rc<Class>`) with
        // identical fields must NOT compare equal nor share a dict/set key —
        // class identity is the cached `Rc`, not the name.
        let p1 = mk_class("P", &["x"]);
        let p2 = mk_class("P", &["x"]);
        let a = mk_instance(&p1, &[("x", Value::Int(1.into()))]);
        let b = mk_instance(&p2, &[("x", Value::Int(1.into()))]);
        // Value equality: different classes ⇒ not equal.
        assert!(!a.py_eq(&b));
        // Hash-key identity: different classes ⇒ distinct keys.
        assert_ne!(a.to_hash_key().unwrap(), b.to_hash_key().unwrap());
        // Same class, equal fields ⇒ still equal / same key (regression).
        let a2 = mk_instance(&p1, &[("x", Value::Int(1.into()))]);
        assert!(a.py_eq(&a2));
        assert_eq!(a.to_hash_key().unwrap(), a2.to_hash_key().unwrap());
    }

    #[test]
    fn set_equality_is_order_independent() {
        use crate::pyset::PySet as HashSet;
        let mut s1 = HashSet::new();
        s1.insert(HashKey::Int(1.into()));
        s1.insert(HashKey::Int(2.into()));
        s1.insert(HashKey::Int(3.into()));
        let mut s2 = HashSet::new();
        s2.insert(HashKey::Int(3.into()));
        s2.insert(HashKey::Int(2.into()));
        s2.insert(HashKey::Int(1.into()));
        let a = Value::Set(Rc::new(crate::value::FrozenCell::new(s1)));
        let b = Value::Set(Rc::new(crate::value::FrozenCell::new(s2)));
        assert!(a.py_eq(&b));
    }

    #[test]
    fn set_repr_follows_cpython_iteration_order() {
        let s: crate::pyset::PySet = [5, 3, 1, 4, 2, 0, 7, 6]
            .into_iter()
            .map(|n| HashKey::Int(n.into()))
            .collect();
        let v = Value::Set(Rc::new(crate::value::FrozenCell::new(s)));
        assert_eq!(v.py_str(), "{0, 1, 2, 3, 4, 5, 6, 7}");
        let s: crate::pyset::PySet = [5, 3, 1, 100, 33, 2]
            .into_iter()
            .map(|n| HashKey::Int(n.into()))
            .collect();
        let v = Value::Set(Rc::new(crate::value::FrozenCell::new(s)));
        assert_eq!(v.py_str(), "{1, 33, 3, 100, 5, 2}");
    }

    #[test]
    fn float_repr_matches_cpython() {
        assert_eq!(format_float(1e20), "1e+20");
        assert_eq!(format_float(1e16), "1e+16");
        assert_eq!(format_float(0.0001), "0.0001");
        assert_eq!(format_float(0.00001), "1e-05");
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(3.25), "3.25");
        assert_eq!(format_float(1.0 / 3.0), "0.3333333333333333");
        assert_eq!(format_float(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(format_float(f64::INFINITY), "inf");
        assert_eq!(format_float(-0.0), "-0.0");
        assert_eq!(format_float(0.0), "0.0");
    }
}
