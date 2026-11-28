//! Memory exhaustion as a catchable `MemoryError`.
//!
//! When the Rust allocator cannot satisfy a request, `std` aborts the
//! process ("memory allocation of N bytes failed", SIGABRT). CPython raises
//! `MemoryError` there, and a program may catch it. Two mechanisms close
//! the gap:
//!
//! - A single allocation whose size the program controls (`1 << 10**12`,
//!   `list(range(10**12))`, a growing list or table) is checked or reserved
//!   fallibly before it is made — see [`crate::limits`].
//! - Everything else — a loop that builds millions of small objects until
//!   memory runs out — is caught here. The interpreter holds an *emergency
//!   reserve*. When an ordinary allocation fails, the allocator releases
//!   the reserve, retries (which now succeeds) and records that memory ran
//!   out; the interpreter raises `MemoryError` at its next safe point (the
//!   next statement or iterator step), and unwinding frees what the program
//!   was building. Once memory is plentiful again the reserve is re-armed.
//!
//! The reserve is only armed by [`arm`] (called when an interpreter is
//! created), so a process that never runs the VM — `tyc build`, the
//! language server — allocates exactly as it did before: the wrapper then
//! forwards every call to the system allocator and nothing else.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::error::Unwind;

/// Size of the reserve: enough for the interpreter to reach its next safe
/// point, build the exception and unwind.
const RESERVE_BYTES: usize = 32 << 20;

const RESERVE_LAYOUT: Layout = match Layout::from_size_align(RESERVE_BYTES, 16) {
    Ok(layout) => layout,
    Err(_) => panic!("valid layout"),
};

/// The reserve block, null while it is released (or was never armed).
static RESERVE: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

/// Nothing to do: the reserve is armed, or the VM never armed one.
const IDLE: u8 = 0;
/// An allocation failed and took the reserve: raise `MemoryError`.
const PENDING: u8 = 1;
/// `MemoryError` was raised; the reserve has not been re-armed yet.
const DISARMED: u8 = 2;

thread_local! {
    /// Per thread, so the thread whose allocation failed is the one that
    /// raises (one VM runs per thread). A `Cell<u8>` with a constant
    /// initialiser needs no lazy setup and no destructor, which is what
    /// makes it usable from inside the allocator.
    static STATE: Cell<u8> = const { Cell::new(IDLE) };
}

fn state() -> u8 {
    STATE.try_with(Cell::get).unwrap_or(IDLE)
}

fn set_state(to: u8) {
    let _ = STATE.try_with(|s| s.set(to));
}

/// The system allocator plus the emergency reserve described in the module
/// docs.
pub struct ReserveAlloc;

/// Give the reserve back to the system so the failed request can be
/// retried. `false` when there is no reserve to give.
#[cold]
fn release_reserve() -> bool {
    let block = RESERVE.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if block.is_null() {
        return false;
    }
    // SAFETY: `block` came from `System.alloc(RESERVE_LAYOUT)` in `arm`
    // and the swap above made this the only owner.
    unsafe { System.dealloc(block, RESERVE_LAYOUT) };
    set_state(PENDING);
    true
}

// SAFETY: every method forwards to `System`, which upholds the
// `GlobalAlloc` contract; the only addition is a second attempt after
// freeing a block this module owns. Nothing here unwinds.
unsafe impl GlobalAlloc for ReserveAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded with the caller's layout.
        let p = unsafe { System.alloc(layout) };
        if p.is_null() && layout.size() <= RESERVE_BYTES && release_reserve() {
            // SAFETY: as above.
            return unsafe { System.alloc(layout) };
        }
        p
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded with the caller's pointer and layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded with the caller's layout.
        let p = unsafe { System.alloc_zeroed(layout) };
        if p.is_null() && layout.size() <= RESERVE_BYTES && release_reserve() {
            // SAFETY: as above.
            return unsafe { System.alloc_zeroed(layout) };
        }
        p
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded with the caller's pointer, layout and size.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() && new_size <= RESERVE_BYTES && release_reserve() {
            // SAFETY: a failed `realloc` leaves `ptr` valid.
            return unsafe { System.realloc(ptr, layout, new_size) };
        }
        p
    }
}

#[global_allocator]
static GLOBAL: ReserveAlloc = ReserveAlloc;

/// Allocate the reserve, if it is not held already. `false` when memory
/// for it cannot be had.
fn try_arm() -> bool {
    if !RESERVE.load(Ordering::Acquire).is_null() {
        return true;
    }
    // SAFETY: `RESERVE_LAYOUT` has a non-zero size.
    let block = unsafe { System.alloc(RESERVE_LAYOUT) };
    if block.is_null() {
        return false;
    }
    if RESERVE
        .compare_exchange(
            std::ptr::null_mut(),
            block,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        // Another thread armed it first.
        // SAFETY: `block` was just allocated with this layout and never shared.
        unsafe { System.dealloc(block, RESERVE_LAYOUT) };
    }
    true
}

/// Arm the reserve for a new interpreter. The pages are never touched, so
/// this costs address space, not memory.
pub fn arm() {
    if state() == IDLE {
        try_arm();
    }
}

/// The interpreter's safe point: `MemoryError` when an allocation since the
/// last one had to fall back on the reserve.
#[inline(always)]
pub fn check() -> Result<(), Unwind> {
    if state() == IDLE {
        Ok(())
    } else {
        check_slow()
    }
}

#[cold]
#[inline(never)]
fn check_slow() -> Result<(), Unwind> {
    if state() == PENDING {
        set_state(DISARMED);
        return Err(crate::limits::memory_error());
    }
    // `MemoryError` has been raised. Re-arm once the program has let go of
    // enough memory for the reserve *and* as much again: a handler that
    // still holds everything keeps the freed reserve as its own headroom.
    // SAFETY: `RESERVE_LAYOUT` has a non-zero size; the probe is freed with
    // the layout it was allocated with.
    unsafe {
        let headroom = System.alloc(RESERVE_LAYOUT);
        if !headroom.is_null() {
            if try_arm() {
                set_state(IDLE);
            }
            System.dealloc(headroom, RESERVE_LAYOUT);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole cycle, without exhausting anything: a released reserve is
    /// reported once as `MemoryError`, then re-armed.
    #[test]
    fn released_reserve_raises_once_then_rearms() {
        // The state is per thread; the reserve block is shared with the
        // other tests' threads, but none of them releases it.
        arm();
        assert!(check().is_ok());
        assert!(release_reserve(), "an armed reserve can be released");
        assert!(!release_reserve(), "and only once");
        match check() {
            Err(Unwind::Exception(e)) => assert_eq!(e.kind, "MemoryError"),
            _ => panic!("the safe point after a release raises MemoryError"),
        }
        // The next safe point finds memory available and re-arms.
        assert!(check().is_ok());
        assert!(!RESERVE.load(Ordering::Acquire).is_null());
        assert_eq!(state(), IDLE);
        assert!(check().is_ok());
    }
}
