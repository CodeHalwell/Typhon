//! The operating system's cryptographically secure random source.
//!
//! One function wide, so the `getrandom` crate is named in exactly one
//! place. This backs `os.urandom`, which CPython documents as "suitable for
//! cryptographic use": a program that derives a key or a session token from
//! it must get the same quality of bytes under `tyc run` as under
//! `tyc build && python`.

/// Fill `dest` with bytes from the OS CSPRNG (`getrandom(2)`,
/// `getentropy`, `BCryptGenRandom`, …). Never falls back to a weaker
/// source: on failure the error is returned and the caller raises.
pub(crate) fn fill(dest: &mut [u8]) -> Result<(), String> {
    getrandom::fill(dest).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn fills_the_buffer_with_fresh_bytes_each_call() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        super::fill(&mut a).unwrap();
        super::fill(&mut b).unwrap();
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, b);
    }
}
