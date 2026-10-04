//! Native-stack headroom for the tree-walking interpreter.
//!
//! Every Typhon call costs the VM a real Rust stack frame chain (about
//! 7 KiB per call in a release build, 97 KiB in a debug build — measured in
//! W5-01), so a program that raises `sys.setrecursionlimit` and then
//! recurses tens of thousands of calls deep used to overflow the native
//! stack and abort the process (SIGABRT). CPython raises a catchable
//! `RecursionError` instead. The interpreter records the lowest usable
//! address of the stack of the thread it was created on, and every call
//! checks the remaining headroom against a safety margin before it
//! recurses, raising `RecursionError` well before the guard page.

/// The lowest address of the current thread's stack and its size, when the
/// platform can report them.
pub(crate) fn current_thread_stack() -> Option<(usize, usize)> {
    platform::current_thread_stack()
}

/// Approximate current stack pointer: the address of a local.
#[inline(always)]
pub(crate) fn stack_pointer() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

/// Headroom kept free below the guard: an eighth of the stack, at least
/// 1 MiB — room for the deepest native recursion that can run between two
/// call-boundary checks (nested expression evaluation, a builtin's own
/// recursion, the exception unwinding itself).
pub(crate) fn safety_margin(stack_size: usize) -> usize {
    (stack_size / 8).max(1024 * 1024)
}

#[cfg(target_os = "macos")]
mod platform {
    pub(super) fn current_thread_stack() -> Option<(usize, usize)> {
        // SAFETY: both calls only read the calling thread's own attributes.
        unsafe {
            let this = libc::pthread_self();
            let high = libc::pthread_get_stackaddr_np(this) as usize;
            let size = libc::pthread_get_stacksize_np(this);
            (high > size && size > 0).then(|| (high - size, size))
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    pub(super) fn current_thread_stack() -> Option<(usize, usize)> {
        // SAFETY: `attr` is initialised by `pthread_getattr_np` before it is
        // read and destroyed exactly once afterwards.
        unsafe {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
                return None;
            }
            let mut addr: *mut libc::c_void = std::ptr::null_mut();
            let mut size: libc::size_t = 0;
            let rc = libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
            libc::pthread_attr_destroy(&mut attr);
            (rc == 0 && size > 0).then(|| (addr as usize, size))
        }
    }
}

#[cfg(not(unix))]
mod platform {
    pub(super) fn current_thread_stack() -> Option<(usize, usize)> {
        None
    }
}
