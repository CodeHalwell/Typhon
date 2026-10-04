//! Character-indexed access to `str` values without rescanning them.
//!
//! A VM string is UTF-8, so its length in characters and the byte offset of
//! character `i` cost a scan. CPython answers both in O(1), and a loop such
//! as `while i < len(s): … s[i] …` over a long string was quadratic under
//! `tyc run` (392× slower than CPython for 200k characters). Strings are
//! immutable, so the scan's result can be kept: a small per-thread cache
//! remembers, for the last few long strings indexed, whether they are ASCII
//! (then byte and character offsets coincide), their character count, and —
//! built on the first index into a non-ASCII string — the byte offset of
//! every character.
//!
//! An entry is identified by the string's allocation, held through a `Weak`:
//! while the `Weak` lives the allocation cannot be reused, so a matching
//! address with a live strong count is the same string.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// Strings shorter than this are scanned directly; caching them would cost
/// more than the scan.
const CACHE_FROM: usize = 64;
const SLOTS: usize = 4;

struct Entry {
    s: Weak<String>,
    chars: usize,
    ascii: bool,
    /// Byte offset of each character, plus the string's length at the end;
    /// built on demand for a non-ASCII string.
    offsets: Option<Rc<[u32]>>,
}

thread_local! {
    static CACHE: RefCell<(usize, Vec<Entry>)> = const { RefCell::new((0, Vec::new())) };
}

fn with_entry<T>(s: &Rc<String>, f: impl FnOnce(&mut Entry) -> T) -> T {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let (next, entries) = &mut *cache;
        let hit = entries
            .iter()
            .position(|e| e.s.as_ptr() == Rc::as_ptr(s) && e.s.strong_count() > 0);
        let at = match hit {
            Some(at) => at,
            None => {
                let ascii = s.is_ascii();
                let entry = Entry {
                    s: Rc::downgrade(s),
                    chars: if ascii { s.len() } else { s.chars().count() },
                    ascii,
                    offsets: None,
                };
                if entries.len() < SLOTS {
                    entries.push(entry);
                    entries.len() - 1
                } else {
                    let at = *next;
                    *next = (*next + 1) % SLOTS;
                    entries[at] = entry;
                    at
                }
            }
        };
        f(&mut entries[at])
    })
}

/// The byte offsets of `s`'s characters (and its end), for a non-ASCII
/// string; `None` when byte and character offsets coincide.
fn offsets(s: &Rc<String>) -> Option<Rc<[u32]>> {
    with_entry(s, |e| {
        if e.ascii {
            return None;
        }
        if e.offsets.is_none() && s.len() <= u32::MAX as usize {
            let mut table: Vec<u32> = s.char_indices().map(|(i, _)| i as u32).collect();
            table.push(s.len() as u32);
            e.offsets = Some(table.into());
        }
        e.offsets.clone()
    })
}

/// `len(s)`: the number of characters.
pub fn char_len(s: &Rc<String>) -> usize {
    if s.len() < CACHE_FROM {
        return s.chars().count();
    }
    with_entry(s, |e| e.chars)
}

/// `s[i]` for an in-range character index.
pub fn char_at(s: &Rc<String>, i: usize) -> Option<char> {
    if s.len() < CACHE_FROM {
        return s.chars().nth(i);
    }
    match offsets(s) {
        None => s.as_bytes().get(i).map(|b| *b as char),
        Some(table) => {
            let start = *table.get(i)? as usize;
            s[start..].chars().next()
        }
    }
}

/// `s[start:stop]` for character indices with `start <= stop <= len(s)`.
pub fn char_range(s: &Rc<String>, start: usize, stop: usize) -> String {
    if s.len() < CACHE_FROM {
        return s
            .chars()
            .skip(start)
            .take(stop.saturating_sub(start))
            .collect();
    }
    match offsets(s) {
        None => s[start.min(s.len())..stop.min(s.len())].to_owned(),
        Some(table) => {
            let at = |i: usize| table.get(i).copied().unwrap_or(s.len() as u32) as usize;
            s[at(start)..at(stop)].to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_ascii_and_non_ascii_strings() {
        let ascii = Rc::new("abcdefghij".repeat(10));
        assert_eq!(char_len(&ascii), 100);
        assert_eq!(char_at(&ascii, 13), Some('d'));
        assert_eq!(char_range(&ascii, 8, 12), "ijab");
        let wide = Rc::new("aé€😀".repeat(30));
        assert_eq!(char_len(&wide), 120);
        assert_eq!(char_at(&wide, 3), Some('😀'));
        assert_eq!(char_at(&wide, 6), Some('€'));
        assert_eq!(char_range(&wide, 2, 5), "€😀a");
        assert_eq!(char_at(&wide, 120), None);
        assert_eq!(char_range(&wide, 118, 120), "€😀");
    }
}
