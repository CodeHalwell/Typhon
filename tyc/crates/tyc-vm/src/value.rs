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
const MAX_STRUCTURAL_DEPTH: usize = 10_000;

thread_local! {
    static STRUCTURAL_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// RAII guard that decrements the structural-recursion depth on drop. Using a
/// guard (rather than a manual paired call) means a panic partway through a
/// comparison can't leave the thread-local counter stuck incremented — which
/// would otherwise make every later comparison on that thread spuriously bail
/// once the (now-unreachable) limit is hit. Thread-locals persist across tasks
/// on a reused thread, so this matters in the LSP / test harness where a
/// panic may be caught.
struct StructuralDepthGuard;

impl Drop for StructuralDepthGuard {
    fn drop(&mut self) {
        STRUCTURAL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Enter one level of structural recursion, returning a guard that restores the
/// depth on drop. Returns `None` when the depth bound has been reached (the
/// caller should bail without recursing).
fn structural_depth_enter() -> Option<StructuralDepthGuard> {
    STRUCTURAL_DEPTH.with(|d| {
        let cur = d.get();
        if cur >= MAX_STRUCTURAL_DEPTH {
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
/// Dicts use `IndexMap` so insertion order is preserved on iteration —
/// matching CPython 3.7+ semantics (FINDINGS #18). Previously a `HashMap`
/// gave non-deterministic iteration order, which made `tyc run` and
/// `tyc build && python build/main.py` produce different stdout for any
/// program that prints a dict literal.
pub type DictMap = IndexMap<HashKey, Value>;
pub type RcDict = Rc<RefCell<DictMap>>;
pub type RcSet = Rc<RefCell<std::collections::HashSet<HashKey>>>;
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
}

/// When `v` is a member of an enum class that mixes in a value type
/// (`StrEnum`, `IntEnum`, `IntFlag` — detected by walking the base chain
/// for the VM's `__typhon_enum_base__`-tagged marker classes of those
/// names), return the member's underlying `value`. CPython makes such
/// members genuine `str` / `int` subclasses, so equality, ordering,
/// hashing, and `str()` all flow through the value; plain `Enum` members
/// intentionally return `None` here (`Color.RED == 1` is False).
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
    if let Value::Instance(inst) = v {
        if mixin_base(&inst.class) {
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

impl HashKey {
    /// Stable, collision-safe sort key. Two distinct `HashKey` values
    /// have distinct sort keys (the discriminant byte differs across
    /// variants and the payload is encoded deterministically). Used
    /// by the `FrozenSet` canonicalisation path so two frozensets
    /// with the same members hash equal regardless of insertion
    /// order — review thread copilot on PR #147.
    pub fn canonical_sort_key(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        match self {
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
                // FrozenSet elements are already canonicalised at
                // construction so this is deterministic.
                for item in items.iter() {
                    let inner = item.canonical_sort_key();
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
        }
        out
    }

    pub fn into_value(self) -> Value {
        match self {
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
                use std::collections::HashSet;
                let mut set = HashSet::new();
                for k in items.iter() {
                    set.insert(k.clone());
                }
                // Surface back as a frozenset-tagged Value::Set.
                Value::Set(Rc::new(RefCell::new(set)))
            }
            HashKey::Instance { instance, .. } => Value::Instance(instance),
        }
    }
}

impl PartialEq for HashKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
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
            (HashKey::Float(a), HashKey::Float(b)) => a == b,
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
                // Frozenset equality is order-independent; the constructor
                // stores items pre-sorted by their hash representation so
                // this works as a vector compare.
                a == b
            }
            // Instance keys compare on their canonical projection: same
            // class name and equal field set. The original `instance`
            // Rc is ignored so two distinct-but-equal instances match.
            (HashKey::Instance { key: a, .. }, HashKey::Instance { key: b, .. }) => a == b,
            _ => false,
        }
    }
}
impl Eq for HashKey {}

impl std::hash::Hash for HashKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
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
            HashKey::FrozenSet(items) => items.hash(state),
            // Hash only the canonical projection so it stays consistent
            // with `Eq` (which ignores the retained `instance` Rc).
            HashKey::Instance { key, .. } => key.hash(state),
        }
    }
}

#[derive(Clone)]
pub enum Value {
    None,
    Bool(bool),
    Int(VmInt),
    Float(f64),
    /// A complex number `(real, imag)`. Constructed from imaginary literals
    /// (`2j` → `Complex(0.0, 2.0)`) and the builtins agent's `complex(re, im)`
    /// constructor.
    Complex(f64, f64),
    Str(RcStr),
    Bytes(Rc<Vec<u8>>),
    /// A `bytearray` — `bytes`'s mutable sibling. The payload sits behind a
    /// `RefCell` so an in-place mutation (`append` / `extend` / `ba[0] = n`)
    /// is visible through every alias of the same object, exactly as in
    /// CPython. `bytearray` is unhashable there, and `to_hash_key` keeps
    /// that here by falling through to the "unhashable type" arm.
    ByteArray(Rc<RefCell<Vec<u8>>>),
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
    },
    /// Iterator state — opaque to the AST walker; consumed by `next`.
    Iter(Rc<RefCell<IterState>>),
    /// A dict view produced by `dict.keys()` / `.values()` / `.items()`.
    /// The builtins agent materialises the `items` vector (already containing
    /// the keys, values, or `(k, v)` tuples respectively) and tags it with the
    /// matching `kind`. The VM provides repr, iteration, `len()`, and `in`.
    DictView {
        kind: DictViewKind,
        items: Vec<Value>,
    },
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
}

impl NativeFn {
    pub fn new<F>(name: &'static str, f: F) -> Self
    where
        F: Fn(&mut crate::interp::Interpreter, Vec<Value>) -> Result<Value, Unwind> + 'static,
    {
        NativeFn {
            name,
            func: Box::new(f),
        }
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
    /// Base classes in MRO order (after head). For v1 we only walk the chain
    /// for method lookup; we don't compute C3 linearisation.
    pub bases: Vec<Rc<Class>>,
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
}

#[derive(Clone)]
pub struct ClassField {
    pub name: String,
    pub default: Option<Value>,
}

pub struct Instance {
    pub class: Rc<Class>,
    pub fields: RefCell<HashMap<String, Value>>,
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
    Dict {
        keys: Vec<HashKey>,
        index: usize,
    },
    Set {
        keys: Vec<HashKey>,
        index: usize,
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
    },
    Filter {
        func: Value,
        inner: Rc<RefCell<IterState>>,
    },
}

// ── Debug / display ────────────────────────────────────────────────────────

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::None => write!(f, "None"),
            Value::Bool(b) => write!(f, "{b:?}"),
            Value::Int(i) => write!(f, "{}", i.to_str_radix(10)),
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Complex(re, im) => write!(f, "{}", format_complex(*re, *im)),
            Value::Str(s) => write!(f, "{:?}", s.as_str()),
            Value::Bytes(b) => write!(f, "{}", python_repr_bytes(b)),
            Value::ByteArray(b) => write!(f, "bytearray({})", python_repr_bytes(&b.borrow())),
            Value::List(l) => write!(f, "{:?}", l.borrow()),
            Value::Tuple(t) => write!(f, "{:?}", &t[..]),
            Value::Dict(d) => {
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                let is_frozen = matches!(d.borrow().get(&frozen_key), Some(Value::Bool(true)));
                if is_frozen {
                    write!(f, "mappingproxy({{")?;
                } else {
                    write!(f, "{{")?;
                }
                let d = d.borrow();
                let mut emitted = 0usize;
                for (k, v) in d.iter() {
                    // Hide the internal freeze sentinel from user output.
                    if matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__") {
                        continue;
                    }
                    if emitted > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:?}: {:?}", k.clone().into_value(), v)?;
                    emitted += 1;
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
            Value::Native(n) => write!(f, "<built-in function {}>", n.name),
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

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::None => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Complex(..) => "complex",
            Value::Str(_) => "str",
            Value::Bytes(_) => "bytes",
            Value::ByteArray(_) => "bytearray",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Dict(_) => "dict",
            Value::Set(_) => "set",
            Value::Range { .. } => "range",
            Value::Native(_) | Value::Function(_) | Value::BoundMethod { .. } => "function",
            Value::Class(_) => "type",
            // Don't leak the class name into a `'static str`. Callers that
            // need the specific class name read `instance.class.name`
            // directly; everywhere else `"instance"` is descriptive enough
            // for an error message.
            Value::Instance(_) => "instance",
            Value::ResultOk(_) => "Ok",
            Value::ResultErr(_) => "Err",
            Value::Module(_) => "module",
            Value::Coroutine(_) => "coroutine",
            Value::Exception { .. } => "Exception",
            Value::Iter(_) => "iterator",
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
            Value::Float(x) => *x != 0.0,
            Value::Complex(re, im) => *re != 0.0 || *im != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::ByteArray(b) => !b.borrow().is_empty(),
            Value::List(l) => !l.borrow().is_empty(),
            Value::Tuple(t) => !t.is_empty(),
            // Truthiness ignores the synthetic `__typhon_frozen__`
            // sentinel a `freeze let` may have inserted into a dict
            // or set (review thread copilot on PR #147 — otherwise
            // a frozen-and-otherwise-empty container would test as
            // truthy under `bool(d)` / `if d:`).
            Value::Dict(d) => {
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                d.borrow().keys().any(|k| *k != frozen_key)
            }
            Value::Set(s) => {
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                s.borrow().iter().any(|k| *k != frozen_key)
            }
            Value::Range { start, stop, step } => {
                if *step > 0 {
                    stop > start
                } else if *step < 0 {
                    stop < start
                } else {
                    false
                }
            }
            Value::DictView { items, .. } => !items.is_empty(),
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
            Value::Float(x) => Ok(HashKey::Float(x.to_bits())),
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
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                let mut keys: Vec<HashKey> = s
                    .borrow()
                    .iter()
                    .filter(|k| **k != frozen_key)
                    .cloned()
                    .collect();
                // Canonical ordering so two sets with the same members
                // produce identical `FrozenSet` payloads (and therefore
                // hash equal). We compare on a collision-safe sort key
                // — sorting by `DefaultHasher::finish()` alone allows
                // two distinct elements to share an ordering slot and
                // the resulting key ordering depends on insertion
                // history, breaking `Eq` / `Hash` consistency
                // (review thread copilot on PR #147).
                keys.sort_by_key(|a| a.canonical_sort_key());
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
                    return v.to_hash_key();
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
        match (self, other) {
            (None, None) => true,
            (Bool(a), Bool(b)) => a == b,
            (Bool(a), Int(b)) | (Int(b), Bool(a)) => &VmInt::from(*a as i64) == b,
            (Bool(a), Float(b)) | (Float(b), Bool(a)) => (*a as i64 as f64) == *b,
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Int(a), Float(b)) | (Float(b), Int(a)) => vmint_eq_f64(a, *b),
            (Complex(ar, ai), Complex(br, bi)) => ar == br && ai == bi,
            // `complex == float` / `complex == int` only when the imaginary
            // part is zero (matching CPython).
            (Complex(re, im), Float(f)) | (Float(f), Complex(re, im)) => *im == 0.0 && re == f,
            (Complex(re, im), Int(i)) | (Int(i), Complex(re, im)) => {
                *im == 0.0 && vmint_eq_f64(i, *re)
            }
            (Complex(re, im), Bool(b)) | (Bool(b), Complex(re, im)) => {
                *im == 0.0 && *re == (*b as i64 as f64)
            }
            (Str(a), Str(b)) => a == b,
            (Bytes(a), Bytes(b)) => a == b,
            // `bytes` and `bytearray` compare by content across the two
            // types (`b"ab" == bytearray(b"ab")` is `True` in CPython).
            (ByteArray(a), ByteArray(b)) => Rc::ptr_eq(a, b) || *a.borrow() == *b.borrow(),
            (Bytes(a), ByteArray(b)) | (ByteArray(b), Bytes(a)) => ***a == *b.borrow(),
            (List(a), List(b)) => {
                // Identity short-circuit: the same list object is equal to
                // itself without recursing into its (possibly self-cyclic)
                // elements.
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                let a = a.borrow();
                let b = b.borrow();
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.py_eq(y))
            }
            (Tuple(a), Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.py_eq(y))
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
                a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.py_eq(w)))
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
            // Dataclass instances compare by value: same class and all
            // fields equal (recursively). CPython's generated `__eq__`
            // compares the field tuple only when the two operands are of
            // the same class; otherwise it returns `NotImplemented` →
            // `False`. Same-class is object identity (the single cached
            // `Rc<Class>` per definition), NOT name equality — two distinct
            // classes that share a name must not compare equal.
            (Instance(a), Instance(b)) => {
                if !Rc::ptr_eq(&a.class, &b.class) {
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
                    .all(|(k, v)| fb.get(k).is_some_and(|w| v.py_eq(w)))
            }
            // Sets / frozensets compare with set semantics: equal iff
            // they contain the same elements, independent of iteration
            // order. The synthetic `__typhon_frozen__` sentinel is
            // excluded so a frozen and non-frozen copy of the same
            // members still compare equal.
            (Set(a), Set(b)) => {
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                let a = a.borrow();
                let b = b.borrow();
                let a_len = a.iter().filter(|k| **k != frozen_key).count();
                let b_len = b.iter().filter(|k| **k != frozen_key).count();
                if a_len != b_len {
                    return false;
                }
                a.iter()
                    .filter(|k| **k != frozen_key)
                    .all(|k| b.contains(k))
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
        match (self, other) {
            (Int(a), Int(b)) => a.partial_cmp(b),
            (Float(a), Float(b)) => a.partial_cmp(b),
            (Int(a), Float(b)) => vmint_cmp_f64(a, *b),
            (Float(a), Int(b)) => vmint_cmp_f64(b, *a).map(|o| o.reverse()),
            (Bool(a), Bool(b)) => a.partial_cmp(b),
            (Bool(a), Int(b)) => VmInt::from(*a as i64).partial_cmp(b),
            (Int(a), Bool(b)) => a.partial_cmp(&VmInt::from(*b as i64)),
            (Str(a), Str(b)) => a.partial_cmp(b),
            // `bytes` / `bytearray` order lexicographically by byte value,
            // and the two types are mutually comparable (CPython).
            (Bytes(a), Bytes(b)) => a.as_slice().partial_cmp(b.as_slice()),
            (ByteArray(a), ByteArray(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.as_slice().partial_cmp(b.as_slice())
            }
            (Bytes(a), ByteArray(b)) => a.as_slice().partial_cmp(b.borrow().as_slice()),
            (ByteArray(a), Bytes(b)) => a.borrow().as_slice().partial_cmp(b.as_slice()),
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
            Value::Float(x) => Ok(*x as i64),
            Value::Str(s) => s
                .trim()
                .parse::<i64>()
                .map_err(|_| value_error(format!("invalid literal for int(): {:?}", s.as_str()))),
            _ => Err(type_error(format!(
                "int() argument must be a string or a number, not '{}'",
                self.type_name()
            ))),
        }
    }

    /// Convert to a `BigInt`. Use this when arithmetic should preserve
    /// arbitrary precision (FINDINGS #19).
    pub fn to_bigint(&self) -> Result<BigInt, Unwind> {
        match self {
            Value::Int(i) => Ok(i.to_bigint()),
            Value::Bool(b) => Ok(BigInt::from(*b as i64)),
            Value::Float(x) => Ok(BigInt::from(*x as i64)),
            Value::Str(s) => s
                .trim()
                .parse::<BigInt>()
                .map_err(|_| value_error(format!("invalid literal for int(): {:?}", s.as_str()))),
            _ => Err(type_error(format!(
                "int() argument must be a string or a number, not '{}'",
                self.type_name()
            ))),
        }
    }

    pub fn to_float(&self) -> Result<f64, Unwind> {
        match self {
            Value::Float(x) => Ok(*x),
            Value::Int(i) => Ok(i.to_f64()),
            Value::Bool(b) => Ok(*b as i64 as f64),
            Value::Str(s) => s.trim().parse::<f64>().map_err(|_| {
                value_error(format!(
                    "could not convert string to float: {:?}",
                    s.as_str()
                ))
            }),
            _ => Err(type_error(format!(
                "float() argument must be a string or a number, not '{}'",
                self.type_name()
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
            Value::Float(x) => format_float(*x),
            Value::Complex(re, im) => format_complex(*re, *im),
            Value::Str(s) => (**s).clone(),
            Value::Bytes(b) => python_repr_bytes(b),
            // CPython renders both `str(ba)` and `repr(ba)` as
            // `bytearray(b'...')`.
            Value::ByteArray(b) => format!("bytearray({})", python_repr_bytes(&b.borrow())),
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
            Value::Dict(d) => {
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                let is_frozen = matches!(d.borrow().get(&frozen_key), Some(Value::Bool(true)));
                let d = d.borrow();
                let mut s = String::new();
                if is_frozen {
                    s.push_str("mappingproxy({");
                } else {
                    s.push('{');
                }
                let mut emitted = 0usize;
                for (k, v) in d.iter() {
                    if matches!(k, HashKey::Str(name) if name.as_str() == "__typhon_frozen__") {
                        continue;
                    }
                    if emitted > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&k.clone().into_value().py_repr());
                    s.push_str(": ");
                    s.push_str(&v.py_repr());
                    emitted += 1;
                }
                if is_frozen {
                    s.push_str("})");
                } else {
                    s.push('}');
                }
                s
            }
            Value::Set(set) => {
                let s = set.borrow();
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                let is_frozen = s.contains(&frozen_key);
                // Filter the synthetic `__typhon_frozen__` sentinel
                // that `deep_freeze_value` inserts to mark the set
                // immutable (review thread codex / copilot on PR
                // #147 — without this the sentinel leaks into
                // user-visible repr).
                //
                // The backing store is a Rust `HashSet`, whose iteration
                // order is non-deterministic and differs from CPython.
                // Sort by the collision-safe `canonical_sort_key` so the
                // repr is stable across runs and matches CPython for the
                // common all-numeric / all-string cases (FINDINGS H4).
                let mut keys: Vec<&HashKey> = s.iter().filter(|k| **k != frozen_key).collect();
                keys.sort_by_key(|k| k.canonical_sort_key());
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
            Value::Native(n) => format!("<built-in function {}>", n.name),
            Value::Function(func) => format!("<function {}>", func.name),
            Value::BoundMethod { function, .. } => format!("<bound method {}>", function.name),
            Value::Class(c) => format!("<class '{}'>", c.name),
            // `str(exc)` differs from `repr(exc)` for a field-less exception
            // instance: the message/args, not `ClassName('msg')`.
            Value::Instance(i) => match exception_instance_args(i) {
                Some(args) => exception_instance_str(i, &args),
                // A `model X:` lowers to a `pydantic.BaseModel` subclass,
                // and pydantic's `__str__` is NOT its `__repr__`: it is the
                // space-separated `field=value` pair list with no class name
                // (`id=1 name='a'`). The dataclass rendering below would
                // print the `U(...)` constructor form for both.
                None if class_is_pydantic_model(&i.class) => pydantic_instance_str(i),
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
            Value::Module(m) => format!("<module '{}'>", m.name),
            Value::Coroutine(c) => format!("<coroutine object {}>", c.function.name),
            Value::Exception {
                kind,
                message,
                args,
            } => match args.len() {
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
                _ => {
                    if message.is_empty() {
                        format!("{kind}()")
                    } else {
                        (**message).clone()
                    }
                }
            },
            Value::Iter(_) => "<iterator>".into(),
            Value::DictView { kind, items } => {
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
            // `repr(exc)` keeps the `ClassName('msg')` form even though
            // `str(exc)` (py_str) renders just the message.
            Value::Instance(i) => instance_repr(i),
            // `repr(ValueError("boom"))` is `ValueError('boom')` (and
            // `KeyError('k')`, `ValueError()` for no args) — the constructor
            // form, not the `str()` message.
            Value::Exception { kind, args, .. } => {
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
fn python_repr_str(s: &str) -> String {
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str("\\x");
                let hex = b"0123456789abcdef";
                out.push(hex[(c as u32 as usize >> 4) & 0xf] as char);
                out.push(hex[c as u32 as usize & 0xf] as char);
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
/// Whether `class` derives (directly or transitively) from `pydantic`'s
/// `BaseModel` — i.e. it is the lowering of a Typhon `model X:`. The VM
/// models `BaseModel` as a marker `Value::Class` with that exact name, so
/// walking the base chain for the name is the whole test.
///
/// Drives two pydantic-specific behaviours the dataclass defaults get
/// wrong: the `str()` rendering (space-separated `field=value` pairs, not
/// the `Class(...)` constructor form) and the exclusion of the
/// compiler-synthesised `model_config` from every user-visible surface.
pub fn class_is_pydantic_model(class: &Class) -> bool {
    class.name == "BaseModel" || class.bases.iter().any(|b| class_is_pydantic_model(b))
}

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

/// pydantic's `__str__`: `field=value` pairs (values rendered with `repr`)
/// joined by a single space, with no surrounding class name or parens. Only
/// declared model fields appear — never the compiler-synthesised
/// `model_config`, which pydantic keeps off both `str()` and `repr()`.
fn pydantic_instance_str(inst: &Instance) -> String {
    let fields = inst.fields.borrow();
    let parts: Vec<String> = inst
        .class
        .fields
        .iter()
        .filter(|cf| cf.name != "model_config")
        .filter_map(|cf| {
            fields
                .get(&cf.name)
                .map(|v| format!("{}={}", cf.name, v.py_repr()))
        })
        .collect();
    parts.join(" ")
}

fn instance_repr_inner(inst: &Instance) -> String {
    let fields = inst.fields.borrow();
    // Enum members repr as `<Class.NAME: value>` (CPython default), not as
    // their backing dataclass fields.
    if class_is_enum(&inst.class) {
        if let (Some(Value::Str(name)), Some(val)) = (fields.get("_name_"), fields.get("_value_")) {
            return format!("<{}.{}: {}>", inst.class.name, name, val.py_repr());
        }
    }
    let mut parts: Vec<String> = Vec::with_capacity(fields.len());
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for cf in &inst.class.fields {
        if let Some(v) = fields.get(&cf.name) {
            parts.push(format!("{}={}", cf.name, v.py_repr()));
            seen.insert(cf.name.as_str());
        }
    }
    // Any extra attributes not declared as class fields, in name order.
    let mut extras: Vec<(&String, &Value)> = fields
        .iter()
        .filter(|(k, _)| !seen.contains(k.as_str()))
        .collect();
    extras.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in extras {
        parts.push(format!("{}={}", k, v.py_repr()));
    }
    format!("{}({})", inst.class.name, parts.join(", "))
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
    let mag = x.abs();
    let exp10 = mag.log10().floor() as i32;

    if !(-4..16).contains(&exp10) {
        return format_float_scientific(x);
    }

    let s = format!("{}", x);
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{}.0", s)
    }
}

/// Format a float in CPython's scientific-notation style: shortest
/// round-tripping mantissa, `e+NN` / `e-NN` exponent with at least two
/// digits.
fn format_float_scientific(x: f64) -> String {
    // Rust's `{:e}` gives a shortest mantissa with a base-10 exponent but
    // formats the exponent without a sign or zero-padding (`1e20`,
    // `1.5e-5`). Reformat the exponent to CPython's `e+NN` / `e-NN`.
    let raw = format!("{:e}", x);
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

    fn mk_class(name: &str, field_names: &[&str]) -> Rc<Class> {
        Rc::new(Class {
            name: name.to_owned(),
            methods: RefCell::new(HashMap::new()),
            fields: field_names
                .iter()
                .map(|n| ClassField {
                    name: (*n).to_owned(),
                    default: Option::None,
                })
                .collect(),
            class_attrs: RefCell::new(HashMap::new()),
            bases: vec![],
            properties: RefCell::new(std::collections::HashSet::new()),
            classmethods: RefCell::new(std::collections::HashSet::new()),
            is_exception: false,
        })
    }

    fn mk_instance(class: &Rc<Class>, fields: &[(&str, Value)]) -> Value {
        let mut map: HashMap<String, Value> = HashMap::new();
        for (k, v) in fields {
            map.insert((*k).to_owned(), v.clone());
        }
        Value::Instance(Rc::new(Instance {
            class: class.clone(),
            fields: RefCell::new(map),
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
        use std::collections::HashSet;
        let mut s1 = HashSet::new();
        s1.insert(HashKey::Int(1.into()));
        s1.insert(HashKey::Int(2.into()));
        s1.insert(HashKey::Int(3.into()));
        let mut s2 = HashSet::new();
        s2.insert(HashKey::Int(3.into()));
        s2.insert(HashKey::Int(2.into()));
        s2.insert(HashKey::Int(1.into()));
        let a = Value::Set(Rc::new(RefCell::new(s1)));
        let b = Value::Set(Rc::new(RefCell::new(s2)));
        assert!(a.py_eq(&b));
    }

    #[test]
    fn set_repr_is_sorted_and_deterministic() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        for n in [5, 3, 1, 4, 2, 0, 7, 6] {
            s.insert(HashKey::Int(n.into()));
        }
        let v = Value::Set(Rc::new(RefCell::new(s)));
        assert_eq!(v.py_str(), "{0, 1, 2, 3, 4, 5, 6, 7}");
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
