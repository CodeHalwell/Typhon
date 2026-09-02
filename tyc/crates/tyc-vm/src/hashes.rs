//! MD5, SHA-1, SHA-256 and SHA-512 — the `hashlib` digests the VM models,
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
    let (h0, k) = sha256_tables();
    let mut state: [u32; 8] = [0; 8];
    state.copy_from_slice(h0);
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
    let (h0, k) = sha512_tables();
    let mut state: [u64; 8] = [0; 8];
    state.copy_from_slice(h0);
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
        "sha256" => sha256(data),
        "sha512" => sha512(data),
        _ => return None,
    })
}

/// `(digest_size, block_size)` by `hashlib` name.
pub fn sizes(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "md5" => (16, 64),
        "sha1" => (20, 64),
        "sha256" => (32, 64),
        "sha512" => (64, 128),
        _ => return None,
    })
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
