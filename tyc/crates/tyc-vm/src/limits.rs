//! Size limits that CPython reports as exceptions and Rust would turn into an
//! abort or a panic.
//!
//! An allocation Rust cannot satisfy aborts the process ("memory allocation
//! of … bytes failed", exit 134), a capacity above `isize::MAX` panics
//! ("capacity overflow"), and `format!("{:.*}", p, x)` panics for a precision
//! above `u16::MAX`. CPython raises `MemoryError` / `OverflowError` /
//! `ValueError` in the same places, and a program may catch them, so every
//! size a program controls goes through one of these helpers first.

use std::cell::Cell;

use crate::error::Unwind;
use crate::error::VmException;
use crate::value::{Value, VmInt};

/// CPython's bare `MemoryError` (it carries no message).
pub fn memory_error() -> Unwind {
    Unwind::Exception(VmException::new("MemoryError", ""))
}

/// Allocations at least this large are probed with `try_reserve` before the
/// real buffer is built. Smaller ones cannot realistically fail, and probing
/// them would double the work of every ordinary `ljust` / `format`.
const PROBE_FROM: usize = 1 << 24;

/// Make sure a buffer of `bytes` bytes can be allocated: `MemoryError` when
/// it cannot (as CPython's `PyMem_Malloc` failure), instead of the abort the
/// real allocation would cause.
pub fn ensure_alloc(bytes: usize) -> Result<(), Unwind> {
    if bytes > isize::MAX as usize {
        return Err(memory_error());
    }
    if bytes >= PROBE_FROM {
        let mut probe: Vec<u8> = Vec::new();
        probe.try_reserve_exact(bytes).map_err(|_| memory_error())?;
    }
    Ok(())
}

/// [`ensure_alloc`] for `count` items of `item_bytes` bytes each.
pub fn ensure_alloc_items(count: usize, item_bytes: usize) -> Result<(), Unwind> {
    ensure_alloc(count.checked_mul(item_bytes).ok_or_else(memory_error)?)
}

/// `s` repeated `n` times, raising `MemoryError` when the result cannot be
/// allocated (the length was already checked against the index range).
pub fn try_repeat_str(s: &str, n: usize) -> Result<String, Unwind> {
    let total = s.len().checked_mul(n).ok_or_else(memory_error)?;
    let mut out = String::new();
    out.try_reserve_exact(total).map_err(|_| memory_error())?;
    for _ in 0..n {
        out.push_str(s);
    }
    Ok(out)
}

/// `b` repeated `n` times; see [`try_repeat_str`].
pub fn try_repeat_bytes(b: &[u8], n: usize) -> Result<Vec<u8>, Unwind> {
    let total = b.len().checked_mul(n).ok_or_else(memory_error)?;
    let mut out = Vec::new();
    out.try_reserve_exact(total).map_err(|_| memory_error())?;
    for _ in 0..n {
        out.extend_from_slice(b);
    }
    Ok(out)
}

/// `v.push(x)`, raising `MemoryError` when the vector cannot grow instead
/// of aborting. Growth stays amortised: `try_reserve` doubles like `push`.
#[inline]
pub fn try_push<T>(v: &mut Vec<T>, x: T) -> Result<(), Unwind> {
    if v.len() == v.capacity() {
        v.try_reserve(1).map_err(|_| memory_error())?;
    }
    v.push(x);
    Ok(())
}

/// Room for `extra` more elements, or `MemoryError`.
pub fn try_reserve_items<T>(v: &mut Vec<T>, extra: usize) -> Result<(), Unwind> {
    v.try_reserve_exact(extra).map_err(|_| memory_error())
}

/// Room for one more member of `s`, or `MemoryError` (see
/// [`PySet::try_reserve_one`](crate::pyset::PySet::try_reserve_one)).
#[inline]
pub(crate) fn set_room(s: &mut crate::pyset::PySet) -> Result<(), Unwind> {
    if s.try_reserve_one() {
        Ok(())
    } else {
        Err(memory_error())
    }
}

/// Room for one more entry of `d`, or `MemoryError`.
#[inline]
pub(crate) fn dict_room(d: &mut crate::pydict::PyDict) -> Result<(), Unwind> {
    if d.try_reserve_one() {
        Ok(())
    } else {
        Err(memory_error())
    }
}

/// `a + b` for two strings, `MemoryError` when the result cannot be held.
pub fn try_concat_str(a: &str, b: &str) -> Result<String, Unwind> {
    let total = a.len().checked_add(b.len()).ok_or_else(memory_error)?;
    let mut out = String::new();
    out.try_reserve_exact(total).map_err(|_| memory_error())?;
    out.push_str(a);
    out.push_str(b);
    Ok(out)
}

/// `a + b` for two byte strings; see [`try_concat_str`].
pub fn try_concat_bytes(a: &[u8], b: &[u8]) -> Result<Vec<u8>, Unwind> {
    let total = a.len().checked_add(b.len()).ok_or_else(memory_error)?;
    let mut out = Vec::new();
    out.try_reserve_exact(total).map_err(|_| memory_error())?;
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    Ok(out)
}

/// `a + b` for two element sequences (list / tuple concatenation).
pub fn try_concat_items<T: Clone>(a: &[T], b: &[T]) -> Result<Vec<T>, Unwind> {
    let total = a.len().checked_add(b.len()).ok_or_else(memory_error)?;
    let mut out = Vec::new();
    out.try_reserve_exact(total).map_err(|_| memory_error())?;
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    Ok(out)
}

/// A fallible `slice.to_vec()`.
pub fn try_clone_items<T: Clone>(a: &[T]) -> Result<Vec<T>, Unwind> {
    let mut out = Vec::new();
    out.try_reserve_exact(a.len()).map_err(|_| memory_error())?;
    out.extend_from_slice(a);
    Ok(out)
}

/// `sep.join(parts)` with the total length checked and reserved up front.
pub fn try_join_str(
    sep: &str,
    parts: &[impl std::ops::Deref<Target = String>],
) -> Result<String, Unwind> {
    let mut total = sep
        .len()
        .checked_mul(parts.len().saturating_sub(1))
        .ok_or_else(memory_error)?;
    for p in parts {
        total = total.checked_add(p.len()).ok_or_else(memory_error)?;
    }
    let mut out = String::new();
    out.try_reserve_exact(total).map_err(|_| memory_error())?;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        out.push_str(p);
    }
    Ok(out)
}

/// `sep.join(parts)` for bytes; see [`try_join_str`].
pub fn try_join_bytes(
    sep: &[u8],
    parts: &[impl std::ops::Deref<Target = Vec<u8>>],
) -> Result<Vec<u8>, Unwind> {
    let mut total = sep
        .len()
        .checked_mul(parts.len().saturating_sub(1))
        .ok_or_else(memory_error)?;
    for p in parts {
        total = total.checked_add(p.len()).ok_or_else(memory_error)?;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(total).map_err(|_| memory_error())?;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(sep);
        }
        out.extend_from_slice(p);
    }
    Ok(out)
}

/// Make sure an integer of `bits` bits can be held (`MemoryError`
/// otherwise), before `num_bigint` allocates it and aborts on failure.
/// `scratch` asks for that much again, for an operation that needs working
/// space the size of its result (multiplication, exponentiation, radix
/// conversion).
pub fn ensure_int_bits(bits: u128, scratch: bool) -> Result<(), Unwind> {
    let bytes = bits / 8 + 8;
    let bytes = if scratch {
        bytes.saturating_mul(2)
    } else {
        bytes
    };
    ensure_alloc(usize::try_from(bytes).map_err(|_| memory_error())?)
}

/// Bytes one piece of a split costs beyond its text: the `Value`, the
/// shared string header and the allocator's own bookkeeping.
const PIECE_OVERHEAD: usize = 96;

/// Before a `str` / `bytes` method builds its result from a large receiver
/// of `len` bytes: `MemoryError` when the result cannot be allocated. The
/// methods that copy or re-case the text need about `len` bytes again;
/// `pieces()` (called only for a large receiver) counts what a split
/// produces. Small receivers return at once.
pub fn ensure_text_result(
    len: usize,
    name: &str,
    pieces: impl FnOnce() -> usize,
) -> Result<(), Unwind> {
    if len < PROBE_FROM {
        return Ok(());
    }
    match name {
        "split" | "rsplit" | "splitlines" => {
            let n = pieces();
            ensure_alloc(
                n.checked_mul(PIECE_OVERHEAD)
                    .and_then(|b| b.checked_add(len))
                    .ok_or_else(memory_error)?,
            )
        }
        "upper" | "lower" | "casefold" | "title" | "capitalize" | "swapcase" | "strip"
        | "lstrip" | "rstrip" | "encode" | "decode" | "translate" | "removeprefix"
        | "removesuffix" | "partition" | "rpartition" | "hex" | "format" | "format_map" => {
            // `hex` doubles the text; a case mapping can grow it slightly.
            ensure_alloc(len.checked_mul(2).ok_or_else(memory_error)?)
        }
        _ => Ok(()),
    }
}

/// The size of `hay.replace(from, to)` when `count` occurrences are
/// replaced, checked against memory before the result is built.
pub fn ensure_replace_result(
    hay_len: usize,
    from_len: usize,
    to_len: usize,
    count: usize,
) -> Result<(), Unwind> {
    let total = if to_len > from_len {
        count
            .checked_mul(to_len - from_len)
            .and_then(|grow| grow.checked_add(hay_len))
            .ok_or_else(memory_error)?
    } else {
        hay_len
    };
    ensure_alloc(total)
}

/// The number of items `range(start, stop, step)` yields from `current`.
pub fn range_len(current: i64, stop: i64, step: i64) -> u128 {
    let (lo, hi, step) = if step > 0 {
        (current as i128, stop as i128, step as i128)
    } else {
        (stop as i128, current as i128, -(step as i128))
    };
    if step == 0 || lo >= hi {
        0
    } else {
        ((hi - lo - 1) / step + 1) as u128
    }
}

/// CPython's error for a count that does not fit `Py_ssize_t`.
pub fn index_overflow() -> Unwind {
    Unwind::Exception(VmException::new(
        "OverflowError",
        "cannot fit 'int' into an index-sized integer",
    ))
}

/// An integer argument CPython converts to `Py_ssize_t` (a width, a count):
/// `OverflowError: Python int too large to convert to C ssize_t` outside the
/// `i64` range.
pub fn ssize_arg(v: &Value) -> Result<i64, Unwind> {
    match v {
        Value::Int(n) => n.to_i64().ok_or_else(|| {
            Unwind::Exception(VmException::new(
                "OverflowError",
                "Python int too large to convert to C ssize_t",
            ))
        }),
        Value::Bool(b) => Ok(i64::from(*b)),
        other => other.to_int(),
    }
}

/// An integer argument CPython converts to a C `int` (`expandtabs`'s
/// tab size): `OverflowError` outside the `i32` range.
pub fn c_int_arg(v: &Value) -> Result<i32, Unwind> {
    let too_large = || {
        Unwind::Exception(VmException::new(
            "OverflowError",
            "Python int too large to convert to C int",
        ))
    };
    match v {
        Value::Int(n) => n
            .to_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(too_large),
        Value::Bool(b) => Ok(i32::from(*b)),
        other => i32::try_from(other.to_int()?).map_err(|_| too_large()),
    }
}

// ── The int/str conversion length limit (CPython 3.11+) ───────────────

/// CPython's default `sys.get_int_max_str_digits()`.
pub const DEFAULT_INT_MAX_STR_DIGITS: usize = 4300;

thread_local! {
    /// `sys.set_int_max_str_digits(n)`; 0 disables the limit. One VM runs
    /// per thread, and `Interpreter::new` resets it.
    static INT_MAX_STR_DIGITS: Cell<usize> = const { Cell::new(DEFAULT_INT_MAX_STR_DIGITS) };
}

pub fn int_max_str_digits() -> usize {
    INT_MAX_STR_DIGITS.with(Cell::get)
}

pub fn set_int_max_str_digits(n: usize) {
    INT_MAX_STR_DIGITS.with(|c| c.set(n));
}

/// `ValueError` when converting `n` to decimal text would exceed the
/// limit. Only ints far beyond `i64` can, and most are decided from their
/// bit length without converting.
pub fn check_int_to_str(n: &VmInt) -> Result<(), Unwind> {
    let limit = int_max_str_digits();
    let VmInt::Big(b) = n else {
        return Ok(());
    };
    if limit == 0 {
        return Ok(());
    }
    let bits = b.bits();
    // `bits` binary digits hold between ⌊(bits-1)·log10 2⌋+1 and
    // ⌊bits·log10 2⌋+1 decimal digits.
    let at_least =
        ((bits.saturating_sub(1)) as f64 * std::f64::consts::LOG10_2).floor() as usize + 1;
    let at_most = (bits as f64 * std::f64::consts::LOG10_2).floor() as usize + 1;
    let too_long = if at_least > limit {
        true
    } else if at_most <= limit {
        false
    } else {
        b.magnitude().to_str_radix(10).len() > limit
    };
    if too_long {
        return Err(Unwind::Exception(VmException::new(
            "ValueError",
            format!(
                "Exceeds the limit ({limit} digits) for integer string conversion; \
                 use sys.set_int_max_str_digits() to increase the limit"
            ),
        )));
    }
    Ok(())
}

/// `ValueError` when an `int()` literal in a base that is not a power of two
/// has more digits than the limit.
pub fn check_str_to_int(digits: usize, base: u32) -> Result<(), Unwind> {
    let limit = int_max_str_digits();
    if limit == 0 || base.is_power_of_two() || digits <= limit {
        return Ok(());
    }
    Err(Unwind::Exception(VmException::new(
        "ValueError",
        format!(
            "Exceeds the limit ({limit} digits) for integer string conversion: \
             value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
        ),
    )))
}

/// A finite double's exact decimal expansion has at most 1074 digits after
/// the point (2⁻¹⁰⁷⁴); every digit past that is a zero.
const EXACT_FIXED_DIGITS: usize = 1100;
/// …and at most 767 significant digits.
const EXACT_SIG_DIGITS: usize = 800;

/// `format!("{:.*}", p, x)` for any precision. Rust's formatter panics for a
/// precision above `u16::MAX`; past the exact expansion only zeros follow.
pub fn fixed(x: f64, p: usize) -> String {
    if p <= EXACT_FIXED_DIGITS || !x.is_finite() {
        return format!("{:.*}", p.min(EXACT_FIXED_DIGITS), x);
    }
    let mut s = format!("{:.*}", EXACT_FIXED_DIGITS, x);
    s.extend(std::iter::repeat_n('0', p - EXACT_FIXED_DIGITS));
    s
}

/// `format!("{:.*e}", p, x)` for any precision (Rust's `3.5e2` exponent
/// form; callers normalise it to CPython's `e+02`).
pub fn scientific(x: f64, p: usize) -> String {
    if p <= EXACT_SIG_DIGITS || !x.is_finite() {
        return format!("{:.*e}", p.min(EXACT_SIG_DIGITS), x);
    }
    let s = format!("{:.*e}", EXACT_SIG_DIGITS, x);
    match s.find('e') {
        Some(at) => {
            let (mantissa, exp) = s.split_at(at);
            let mut out = String::with_capacity(s.len() + p - EXACT_SIG_DIGITS);
            out.push_str(mantissa);
            out.extend(std::iter::repeat_n('0', p - EXACT_SIG_DIGITS));
            out.push_str(exp);
            out
        }
        None => s,
    }
}

/// CPython refuses a float precision that does not fit a C `int`
/// (`format(1.5, ".2147483648f")`).
pub fn check_float_precision(p: usize) -> Result<(), Unwind> {
    if p > i32::MAX as usize {
        return Err(Unwind::Exception(VmException::new(
            "ValueError",
            "precision too big",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_precisions_pad_the_exact_expansion() {
        let s = fixed(1.5, 70_000);
        assert_eq!(s.len(), 2 + 70_000);
        assert!(s.starts_with("1.5000"));
        assert_eq!(fixed(0.1, 20), format!("{:.20}", 0.1));
        let e = scientific(2.5, 70_000);
        assert!(e.starts_with("2.5000") && e.ends_with("e0"));
        assert_eq!(e.len(), 2 + 70_000 + 2);
    }
}
