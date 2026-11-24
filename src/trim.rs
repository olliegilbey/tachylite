//! Memory at rest. glibc keeps the pages a build frees, so the server would rest at its
//! build peak. `malloc_trim` returns them to the kernel.

/// How far resident memory may grow past the last trim before the next one. A trim takes
/// milliseconds, and a thread that allocates during one waits, so it stays rare.
#[cfg(target_env = "gnu")]
const SLACK: usize = 8 << 20;

/// Trims the heap once resident memory has grown `SLACK` past `floor`, the figure the last
/// trim left. Returns the new floor.
#[cfg(target_env = "gnu")]
pub fn settle(floor: usize) -> usize {
    // The crate's only `unsafe`. glibc declares `int malloc_trim(size_t)` and documents it
    // as safe from any thread with any argument, so the call itself is safe.
    #[expect(unsafe_code, reason = "std has no call that returns free heap pages")]
    unsafe extern "C" {
        safe fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }
    match resident() {
        Some(now) if now > floor.saturating_add(SLACK) => {
            malloc_trim(0);
            resident().unwrap_or(now)
        }
        _ => floor,
    }
}

/// Only glibc has `malloc_trim`.
#[cfg(not(target_env = "gnu"))]
pub fn settle(floor: usize) -> usize {
    floor
}

/// Resident memory in bytes, or `None` where `/proc` cannot be read.
#[cfg(target_env = "gnu")]
fn resident() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: usize = statm.split(' ').nth(1)?.parse().ok()?;
    Some(pages * rustix::param::page_size())
}
