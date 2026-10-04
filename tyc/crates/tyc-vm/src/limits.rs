//! Size limits that CPython reports as exceptions and Rust would turn into an
//! abort or a panic.
//!
//! An allocation Rust cannot satisfy aborts the process ("memory allocation
//! of … bytes failed", exit 134), a capacity above `isize::MAX` panics
//! ("capacity overflow"), and `format!("{:.*}", p, x)` panics for a precision
//! above `u16::MAX`. CPython raises `MemoryError` / `OverflowError` /
//! `ValueError` in the same places, and a program may catch them, so every
//! size a program controls goes through one of these helpers first.

use crate::error::VmException;
use crate::error::Unwind;
use crate::value::Value;

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
