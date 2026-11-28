//! Bytes from the operating system's cryptographic random generator — what
//! CPython's `os.urandom` returns. One function wide, so the platform calls
//! stay in one place.
//!
//! `std` exposes no such source. `RandomState` (the hash-seed generator the
//! VM used here before) takes its keys from the OS once per thread and then
//! *increments* them: fine for seeding a hash map, not a source to draw
//! keys, tokens or temporary-file names from.

/// Fill `buf` from the OS generator (`getrandom(2)`, `getentropy`,
/// `ProcessPrng`, … — whatever the `getrandom` crate selects for the
/// platform). There is no weaker fallback: on failure the error is returned
/// and the caller raises, as CPython does.
pub fn fill(buf: &mut [u8]) -> std::io::Result<()> {
    getrandom::fill(buf).map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn fills_the_whole_buffer_differently_each_time() {
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        super::fill(&mut a).expect("OS random source");
        super::fill(&mut b).expect("OS random source");
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 64]);
    }
}
