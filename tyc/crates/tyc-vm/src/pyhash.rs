//! CPython's hash algorithms, reproduced bit-for-bit so `hash(x)` under
//! `tyc run` prints what `tyc build && python3.13` prints. `str` / `bytes`
//! use SipHash-1-3 with the all-zero key CPython installs for
//! `PYTHONHASHSEED=0` (the differential harness's setting; a randomised
//! seed makes those two values unpredictable on either surface).
//!
//! Sources: `Objects/longobject.c` (`long_hash`), `Python/pyhash.c`
//! (`_Py_HashDouble`, `siphash13`, `_Py_HashPointer`),
//! `Objects/tupleobject.c` (`tuplehash`), `Objects/setobject.c`
//! (`frozenset_hash`), `Objects/rangeobject.c` (`range_hash`) and
//! `Objects/complexobject.c` (`complex_hash`).

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};

/// `sys.hash_info.modulus`: the Mersenne prime 2**61 - 1.
pub const MODULUS: u64 = (1u64 << 61) - 1;
/// `sys.hash_info.inf`.
pub const INF: i64 = 314_159;
/// `sys.hash_info.imag`.
pub const IMAG: u64 = 1_000_003;
/// `hash(None)` — a fixed constant since Python 3.12.
pub const NONE: i64 = 0xFCA8_6420;

/// -1 is the C-level error sentinel, so no hash is ever -1.
fn fix(h: i64) -> i64 {
    if h == -1 {
        -2
    } else {
        h
    }
}

/// `hash(int)`: the value reduced modulo 2**61 - 1, sign kept.
pub fn int_hash(i: &BigInt) -> i64 {
    let r = (i.abs() % BigInt::from(MODULUS)).to_u64().unwrap_or(0) as i64;
    fix(if i.is_negative() { -r } else { r })
}

/// [`int_hash`] for a machine-sized int.
pub fn small_int_hash(i: i64) -> i64 {
    let r = ((i as i128).abs() % (MODULUS as i128)) as i64;
    fix(if i < 0 { -r } else { r })
}

/// C `frexp`: `v == m * 2**e` with `0.5 <= |m| < 1`.
fn frexp(v: f64) -> (f64, i32) {
    if v == 0.0 || !v.is_finite() {
        return (v, 0);
    }
    let bits = v.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // Subnormal: normalise through a power of two first.
        let (m, e) = frexp(v * (1u64 << 54) as f64);
        return (m, e - 54);
    }
    let mantissa_bits = (bits & !(0x7ffu64 << 52)) | (1022u64 << 52);
    (f64::from_bits(mantissa_bits), exp - 1022)
}

/// `hash(float)`: equal to the int hash for integral values, `inf` and
/// `nan` fixed (CPython keys a NaN on identity; 0 stands in).
pub fn float_hash(v: f64) -> i64 {
    if v.is_infinite() {
        return if v > 0.0 { INF } else { -INF };
    }
    if v.is_nan() {
        return 0;
    }
    let (mut m, mut e) = frexp(v);
    let sign: i64 = if m < 0.0 {
        m = -m;
        -1
    } else {
        1
    };
    let mut x: u64 = 0;
    while m != 0.0 {
        x = ((x << 28) & MODULUS) | (x >> (61 - 28));
        m *= 268_435_456.0; // 2**28
        e -= 28;
        let y = m as u64;
        m -= y as f64;
        x += y;
        if x >= MODULUS {
            x -= MODULUS;
        }
    }
    let e = if e >= 0 {
        e % 61
    } else {
        61 - 1 - ((-1 - e) % 61)
    };
    x = ((x << e) & MODULUS) | (x >> (61 - e));
    fix(x as i64 * sign)
}

/// `hash(complex)`: `hash(real) + 1000003 * hash(imag)` in unsigned
/// arithmetic.
pub fn complex_hash(re: f64, im: f64) -> i64 {
    let combined = (float_hash(re) as u64).wrapping_add(IMAG.wrapping_mul(float_hash(im) as u64));
    fix(combined as i64)
}

/// `hash(tuple)` over the element hashes (the xxHash-derived `tuplehash`).
pub fn tuple_hash(items: &[i64]) -> i64 {
    const P1: u64 = 11_400_714_785_074_694_791;
    const P2: u64 = 14_029_467_366_897_019_727;
    const P5: u64 = 2_870_177_450_012_600_261;
    let mut acc = P5;
    for &h in items {
        acc = acc.wrapping_add((h as u64).wrapping_mul(P2));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(P1);
    }
    acc = acc.wrapping_add((items.len() as u64) ^ (P5 ^ 3_527_539));
    if acc == u64::MAX {
        return 1_546_275_796;
    }
    acc as i64
}

/// `hash(frozenset)` over the member hashes (order-independent).
pub fn frozenset_hash(members: &[i64]) -> i64 {
    fn shuffle(h: u64) -> u64 {
        ((h ^ 89_869_747) ^ (h << 16)).wrapping_mul(3_644_798_167)
    }
    let mut hash: u64 = 0;
    for &h in members {
        hash ^= shuffle(h as u64);
    }
    hash ^= ((members.len() as u64) + 1).wrapping_mul(1_927_868_237);
    hash ^= (hash >> 11) ^ (hash >> 25);
    hash = hash.wrapping_mul(69_069).wrapping_add(907_133_923);
    if hash == u64::MAX {
        hash = 590_923_713;
    }
    hash as i64
}

/// `hash(range)`: the tuple hash of `(len, start, step)`, with `None`
/// standing in for the parts an empty / single-element range ignores.
pub fn range_hash(len: i64, start: i64, step: i64) -> i64 {
    let items = if len == 0 {
        [small_int_hash(0), NONE, NONE]
    } else if len == 1 {
        [small_int_hash(1), small_int_hash(start), NONE]
    } else {
        [
            small_int_hash(len),
            small_int_hash(start),
            small_int_hash(step),
        ]
    };
    tuple_hash(&items)
}

/// `object.__hash__`: the address rotated right by four bits.
pub fn pointer_hash(p: usize) -> i64 {
    let y = (p as u64).rotate_right(4);
    fix(y as i64)
}

fn siphash13(k0: u64, k1: u64, data: &[u8]) -> u64 {
    let mut v0 = k0 ^ 0x736f_6d65_7073_6575;
    let mut v1 = k1 ^ 0x646f_7261_6e64_6f6d;
    let mut v2 = k0 ^ 0x6c79_6765_6e65_7261;
    let mut v3 = k1 ^ 0x7465_6462_7974_6573;
    let mut b: u64 = (data.len() as u64) << 56;
    macro_rules! round {
        () => {
            v0 = v0.wrapping_add(v1);
            v1 = v1.rotate_left(13);
            v1 ^= v0;
            v0 = v0.rotate_left(32);
            v2 = v2.wrapping_add(v3);
            v3 = v3.rotate_left(16);
            v3 ^= v2;
            v0 = v0.wrapping_add(v3);
            v3 = v3.rotate_left(21);
            v3 ^= v0;
            v2 = v2.wrapping_add(v1);
            v1 = v1.rotate_left(17);
            v1 ^= v2;
            v2 = v2.rotate_left(32);
        };
    }
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        let mi = u64::from_le_bytes(word);
        v3 ^= mi;
        round!();
        v0 ^= mi;
    }
    let mut t: u64 = 0;
    for (i, &byte) in chunks.remainder().iter().enumerate() {
        t |= (byte as u64) << (8 * i);
    }
    b |= t;
    v3 ^= b;
    round!();
    v0 ^= b;
    v2 ^= 0xff;
    round!();
    round!();
    round!();
    (v0 ^ v1) ^ (v2 ^ v3)
}

/// `hash(bytes)` (`PYTHONHASHSEED=0` key).
pub fn bytes_hash(data: &[u8]) -> i64 {
    if data.is_empty() {
        return 0;
    }
    fix(siphash13(0, 0, data) as i64)
}

/// `hash(str)` (`PYTHONHASHSEED=0` key): SipHash over the PEP 393 canonical
/// buffer — one byte per code point when every one is < 256, two when
/// every one is < 65536, four otherwise.
pub fn str_hash(s: &str) -> i64 {
    if s.is_empty() {
        return 0;
    }
    let max = s.chars().map(|c| c as u32).max().unwrap_or(0);
    let buffer: Vec<u8> = if max < 256 {
        s.chars().map(|c| c as u32 as u8).collect()
    } else if max < 65_536 {
        s.chars()
            .flat_map(|c| (c as u32 as u16).to_le_bytes())
            .collect()
    } else {
        s.chars().flat_map(|c| (c as u32).to_le_bytes()).collect()
    };
    fix(siphash13(0, 0, &buffer) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every expected value below was printed by `python3.13` with
    // `PYTHONHASHSEED=0`.
    #[test]
    fn ints_and_floats() {
        assert_eq!(small_int_hash(1), 1);
        assert_eq!(small_int_hash(-1), -2);
        assert_eq!(small_int_hash(-2), -2);
        assert_eq!(small_int_hash((1i64 << 61) - 1), 0);
        assert_eq!(small_int_hash(1i64 << 61), 1);
        assert_eq!(small_int_hash((1i64 << 61) - 2), 2305843009213693950);
        assert_eq!(int_hash(&BigInt::from(1u128 << 64)), 8);
        assert_eq!(int_hash(&BigInt::from(1u128 << 63)), 4);
        assert_eq!(int_hash(&-BigInt::from(1u128 << 63)), -4);
        assert_eq!(float_hash(1.5), 1152921504606846977);
        assert_eq!(float_hash(0.5), 1152921504606846976);
        assert_eq!(float_hash(-0.25), -576460752303423488);
        assert_eq!(float_hash(1e300), 1224995262755759164);
        assert_eq!(float_hash(5e-324), 16777216);
        assert_eq!(float_hash(-2.0), -2);
        assert_eq!(float_hash(f64::INFINITY), 314159);
        assert_eq!(complex_hash(1.0, 2.0), 2000007);
    }

    #[test]
    fn tuples_ranges_frozensets() {
        assert_eq!(tuple_hash(&[]), 5740354900026072187);
        assert_eq!(tuple_hash(&[1]), -6644214454873602895);
        assert_eq!(tuple_hash(&[1, 2]), -3550055125485641917);
        assert_eq!(range_hash(5, 0, 1), 5795932985296280846);
        assert_eq!(range_hash(0, 0, 1), 2676694398852732306);
        assert_eq!(range_hash(1, 1, 1), -1269592299258668772);
        assert_eq!(range_hash(3, 1, 3), -9160968616009628824);
        assert_eq!(frozenset_hash(&[]), 133146708735736);
        assert_eq!(frozenset_hash(&[1, 2]), -1826646154956904602);
    }

    #[test]
    fn strings_and_bytes() {
        assert_eq!(str_hash(""), 0);
        assert_eq!(bytes_hash(b""), 0);
        assert_eq!(str_hash("abc"), -4594863902769663758);
        assert_eq!(bytes_hash(b"abc"), -4594863902769663758);
        assert_eq!(str_hash("héllo"), 6395329678795984700);
        assert_eq!(str_hash("日本"), 6243316497235261705);
        assert_eq!(str_hash("😀"), -3536540696076613844);
        assert_eq!(str_hash("aaaaaaa"), 3743554345682611213);
        assert_eq!(str_hash("aaaaaaaa"), -4250921761727411054);
        assert_eq!(str_hash("aaaaaaaaa"), 1304380474379753940);
        assert_eq!(str_hash("abĀ"), 2281199826692479958);
    }
}
