//! MD5, SHA-1, the SHA-2 family and unkeyed BLAKE2 — the `hashlib` digests the VM models,
//! implemented from the specifications (RFC 1321, FIPS 180-4) with no
//! external crate. The SHA-2 round constants and initial states are the
//! fractional parts of cube / square roots of the first primes; they are
//! derived here with exact integer roots rather than transcribed.

use std::sync::OnceLock;

use num_bigint::BigUint;
use num_traits::ToPrimitive;

fn first_primes(n: usize) -> Vec<u64> {
    let mut primes = Vec::with_capacity(n);
    let mut candidate = 2u64;
    while primes.len() < n {
        if primes.iter().all(|p| !candidate.is_multiple_of(*p)) {
            primes.push(candidate);
        }
        candidate += 1;
    }
    primes
}

/// The first `bits` bits of the fractional part of the `root`-th root of
/// `p`: `floor(p^(1/root) * 2^bits) mod 2^bits`, computed exactly.
fn frac_root_bits(p: u64, root: u32, bits: u32) -> u64 {
    let scaled = BigUint::from(p) << (bits * root);
    let r = scaled.nth_root(root);
    let mask = (BigUint::from(1u8) << bits) - BigUint::from(1u8);
    (r & mask).to_u64().unwrap_or(0)
}

fn sha256_tables() -> &'static (Vec<u32>, Vec<u32>) {
    static T: OnceLock<(Vec<u32>, Vec<u32>)> = OnceLock::new();
    T.get_or_init(|| {
        let primes = first_primes(64);
        let h: Vec<u32> = primes[..8]
            .iter()
            .map(|&p| frac_root_bits(p, 2, 32) as u32)
            .collect();
        let k: Vec<u32> = primes
            .iter()
            .map(|&p| frac_root_bits(p, 3, 32) as u32)
            .collect();
        (h, k)
    })
}

fn sha512_tables() -> &'static (Vec<u64>, Vec<u64>) {
    static T: OnceLock<(Vec<u64>, Vec<u64>)> = OnceLock::new();
    T.get_or_init(|| {
        let primes = first_primes(80);
        let h: Vec<u64> = primes[..8]
            .iter()
            .map(|&p| frac_root_bits(p, 2, 64))
            .collect();
        let k: Vec<u64> = primes.iter().map(|&p| frac_root_bits(p, 3, 64)).collect();
        (h, k)
    })
}

fn md5_table() -> &'static [u32; 64] {
    static T: OnceLock<[u32; 64]> = OnceLock::new();
    T.get_or_init(|| {
        let mut k = [0u32; 64];
        for (i, slot) in k.iter_mut().enumerate() {
            *slot = (((i + 1) as f64).sin().abs() * 4_294_967_296.0) as u32;
        }
        k
    })
}

/// Merkle–Damgård padding: a 0x80 byte, zeros, then the bit length in
/// `len_bytes` big- or little-endian bytes, to a multiple of `block`.
fn pad(data: &[u8], block: usize, len_bytes: usize, big_endian: bool) -> Vec<u8> {
    let mut out = data.to_vec();
    out.push(0x80);
    while out.len() % block != block - len_bytes {
        out.push(0);
    }
    let bit_len = (data.len() as u128) * 8;
    let mut len = vec![0u8; len_bytes];
    for i in 0..len_bytes.min(16) {
        let byte = ((bit_len >> (8 * i)) & 0xff) as u8;
        if big_endian {
            len[len_bytes - 1 - i] = byte;
        } else {
            len[i] = byte;
        }
    }
    out.extend_from_slice(&len);
    out
}

pub fn md5(data: &[u8]) -> Vec<u8> {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k = md5_table();
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for chunk in pad(data, 64, 8, false).chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
    state.iter().flat_map(|w| w.to_le_bytes()).collect()
}

pub fn sha1(data: &[u8]) -> Vec<u8> {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    for chunk in pad(data, 64, 8, true).chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            *word = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5a82_7999u32),
                1 => (b ^ c ^ d, 0x6ed9_eba1),
                2 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }
    state.iter().flat_map(|w| w.to_be_bytes()).collect()
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    sha256_with(data, *sha256_iv())
}

/// SHA-224: the SHA-256 compression from a different initial state, with the
/// last word of the result dropped.
pub fn sha224(data: &[u8]) -> Vec<u8> {
    const IV: [u32; 8] = [
        0xc105_9ed8,
        0x367c_d507,
        0x3070_dd17,
        0xf70e_5939,
        0xffc0_0b31,
        0x6858_1511,
        0x64f9_8fa7,
        0xbefa_4fa4,
    ];
    let mut out = sha256_with(data, IV);
    out.truncate(28);
    out
}

fn sha256_iv() -> &'static [u32; 8] {
    static IV: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    &IV
}

fn sha256_with(data: &[u8], iv: [u32; 8]) -> Vec<u8> {
    let (_h0, k) = sha256_tables();
    let mut state: [u32; 8] = iv;
    for chunk in pad(data, 64, 8, true).chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            *word = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
    state.iter().flat_map(|w| w.to_be_bytes()).collect()
}

pub fn sha512(data: &[u8]) -> Vec<u8> {
    let (h0, _) = sha512_tables();
    let mut iv = [0u64; 8];
    iv.copy_from_slice(h0);
    sha512_with(data, iv)
}

/// SHA-384: the SHA-512 compression from a different initial state, truncated
/// to 48 bytes.
pub fn sha384(data: &[u8]) -> Vec<u8> {
    const IV: [u64; 8] = [
        0xcbbb_9d5d_c105_9ed8,
        0x629a_292a_367c_d507,
        0x9159_015a_3070_dd17,
        0x152f_ecd8_f70e_5939,
        0x6733_2667_ffc0_0b31,
        0x8eb4_4a87_6858_1511,
        0xdb0c_2e0d_64f9_8fa7,
        0x47b5_481d_befa_4fa4,
    ];
    let mut out = sha512_with(data, IV);
    out.truncate(48);
    out
}

fn sha512_with(data: &[u8], iv: [u64; 8]) -> Vec<u8> {
    let (_h0, k) = sha512_tables();
    let mut state: [u64; 8] = iv;
    for chunk in pad(data, 128, 16, true).chunks_exact(128) {
        let mut w = [0u64; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&chunk[8 * i..8 * i + 8]);
            *word = u64::from_be_bytes(bytes);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
    state.iter().flat_map(|w| w.to_be_bytes()).collect()
}

/// Digest by `hashlib` name, or `None` for an algorithm the VM lacks.
pub fn digest(name: &str, data: &[u8]) -> Option<Vec<u8>> {
    Some(match name {
        "md5" => md5(data),
        "sha1" => sha1(data),
        "sha224" => sha224(data),
        "sha256" => sha256(data),
        "sha384" => sha384(data),
        "sha512" => sha512(data),
        "blake2b" => blake2b(data, 64),
        "blake2s" => blake2s(data, 32),
        _ => return None,
    })
}

/// `(digest_size, block_size)` by `hashlib` name.
pub fn sizes(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "md5" => (16, 64),
        "sha1" => (20, 64),
        "sha224" => (28, 64),
        "sha256" => (32, 64),
        "sha384" => (48, 128),
        "sha512" => (64, 128),
        "blake2b" => (64, 128),
        "blake2s" => (32, 64),
        _ => return None,
    })
}

/// BLAKE2's message-word permutation, the same ten rows for both variants.
#[rustfmt::skip]
const BLAKE2_SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

/// Unkeyed BLAKE2b with a `digest_size`-byte digest (RFC 7693).
pub fn blake2b(data: &[u8], digest_size: usize) -> Vec<u8> {
    let (iv_words, _) = sha512_tables();
    let mut h = [0u64; 8];
    h.copy_from_slice(iv_words);
    // Parameter block: digest length, key length 0, fanout and depth 1.
    h[0] ^= 0x0101_0000 ^ (digest_size as u64);
    let mut counter: u128 = 0;
    let blocks = data.len() / 128;
    for i in 0..blocks {
        let last = i + 1 == blocks && data.len().is_multiple_of(128);
        counter += 128;
        blake2b_compress(&mut h, &data[i * 128..i * 128 + 128], counter, last);
    }
    if !data.len().is_multiple_of(128) || data.is_empty() {
        let rest = &data[blocks * 128..];
        let mut block = [0u8; 128];
        block[..rest.len()].copy_from_slice(rest);
        counter += rest.len() as u128;
        blake2b_compress(&mut h, &block, counter, true);
    }
    h.iter()
        .flat_map(|w| w.to_le_bytes())
        .take(digest_size)
        .collect()
}

fn blake2b_compress(h: &mut [u64; 8], block: &[u8], counter: u128, last: bool) {
    let (iv_words, _) = sha512_tables();
    let mut v = [0u64; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(iv_words);
    v[12] ^= counter as u64;
    v[13] ^= (counter >> 64) as u64;
    if last {
        v[14] = !v[14];
    }
    let mut m = [0u64; 16];
    for (i, word) in m.iter_mut().enumerate() {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&block[i * 8..i * 8 + 8]);
        *word = u64::from_le_bytes(bytes);
    }
    for round in 0..12 {
        let s = BLAKE2_SIGMA[round % 10];
        blake2b_g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        blake2b_g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        blake2b_g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        blake2b_g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        blake2b_g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        blake2b_g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        blake2b_g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        blake2b_g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

fn blake2b_g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

/// Unkeyed BLAKE2s with a `digest_size`-byte digest (RFC 7693).
pub fn blake2s(data: &[u8], digest_size: usize) -> Vec<u8> {
    let mut h = *sha256_iv();
    h[0] ^= 0x0101_0000 ^ (digest_size as u32);
    let mut counter: u64 = 0;
    let blocks = data.len() / 64;
    for i in 0..blocks {
        let last = i + 1 == blocks && data.len().is_multiple_of(64);
        counter += 64;
        blake2s_compress(&mut h, &data[i * 64..i * 64 + 64], counter, last);
    }
    if !data.len().is_multiple_of(64) || data.is_empty() {
        let rest = &data[blocks * 64..];
        let mut block = [0u8; 64];
        block[..rest.len()].copy_from_slice(rest);
        counter += rest.len() as u64;
        blake2s_compress(&mut h, &block, counter, true);
    }
    h.iter()
        .flat_map(|w| w.to_le_bytes())
        .take(digest_size)
        .collect()
}

fn blake2s_compress(h: &mut [u32; 8], block: &[u8], counter: u64, last: bool) {
    let mut v = [0u32; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(sha256_iv());
    v[12] ^= counter as u32;
    v[13] ^= (counter >> 32) as u32;
    if last {
        v[14] = !v[14];
    }
    let mut m = [0u32; 16];
    for (i, word) in m.iter_mut().enumerate() {
        *word = u32::from_le_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for s in BLAKE2_SIGMA {
        blake2s_g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        blake2s_g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        blake2s_g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        blake2s_g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        blake2s_g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        blake2s_g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        blake2s_g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        blake2s_g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

fn blake2s_g(v: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(12);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(8);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(7);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // Expected digests printed by python3.13's hashlib.
    #[test]
    fn known_vectors() {
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    // Lengths straddling the padding boundaries of both block sizes, over
    // the byte ramp `bytes(range(256)) * 4`.
    #[test]
    fn padding_boundaries() {
        let ramp: Vec<u8> = (0..4).flat_map(|_| 0u8..=255).collect();
        let cases: [(usize, &str, &str, &str, &str); 6] = [
            (55, "6912ee65fff2d9f9ce2508cddf8bcda0", "8ae2d46729cfe68ff927af5eec9c7d1b66d65ac2", "463eb28e72f82e0a96c0a4cc53690c571281131f672aa229e0d45ae59b598b59", "6856647f269c2ee3d8128f0b25427659d880641ef343300dd3cd4679168f58d6527fda70b4ebc854e2065e172b7d58c1536992c0810599259ba84a2b40c65414"),
            (56, "51fdd1acda72405dfdfa03fcb85896d7", "636e2ec698dac903498e648bd2f3af641d3c88cb", "da2ae4d6b36748f2a318f23e7ab1dfdf45acdc9d049bd80e59de82a60895f562", "8b12b2f6fe400a51d29656e2b8c42a1bbfe6fcf3e425da430db05d1a2dda14790dee20fa8b22d8762afffe4988a5c98a4430d22a17e41e23d90fa61ab75671a9"),
            (64, "b2d3f56bc197fd985d5965079b5e7148", "c6138d514ffa2135bfce0ed0b8fac65669917ec7", "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108", "ee4320ebaf3fdb4f2c832b137200c08e235e0fa7bbd0eb1740c7063ba8a0d151da77e003398e1714a955d475b05e3e950b639503b452ec185de4229bc4873949"),
            (111, "4fad3ab7d8546851ec1bb63ea7e6f5a8", "bc544e24573d592290fdaff8ecf3f7f2b00cd483", "60780e9451bdc43cf4530ffc95cbb0c4eb24dae2c39f55f334d679e076c08065", "a1a111449b198d9b1f538bad7f3fc1022b3a5b1a5e90a0bc860de8512746cbc31599e6c834de3a3235327af0b51ff57bf7acf1974a73014d9c3953812edc7c8d"),
            (120, "b7ba1efc6022e9ed272f00b8831e26e6", "d3dbd653bd8597b7475321b60a36891278e6a04a", "f52b23db1fbb6ded89ef42a23ce0c8922c45f25c50b568a93bf1c075420bbb7c", "9636708964c5ff6600510319e07bf3fcfcb1f4058fec278efb677964ba1e140c1632505452f802e99bcf09da3d456dc3868d149a0788a730e49d239ce7415145"),
            (1000, "cbecbdb0fdd5cec1e242493b6008cc79", "af0b191c2de46fe13fe0908f5a6a4e90e0cafc46", "a8af099bf2e878609558dbf69d8f88f4a31040a8cf84b549a0cfa912f12ffc3f", "6cd2eda9bf9c0597129029b0054b81e433f6b8b7b499a75eb705efd74bac194149835b1d1a14c48be696e4d588456d512a22eae7aa1b57be2b56eae7d35e08cb"),
        ];
        for (n, m, s1, s256, s512) in cases {
            let data = &ramp[..n];
            assert_eq!(hex(&md5(data)), m, "md5 {n}");
            assert_eq!(hex(&sha1(data)), s1, "sha1 {n}");
            assert_eq!(hex(&sha256(data)), s256, "sha256 {n}");
            assert_eq!(hex(&sha512(data)), s512, "sha512 {n}");
        }
    }
}
