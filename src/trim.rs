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
    // The crate's only `unsafe`: glibc's `int malloc_trim(size_t)`. It locks each arena,
    // the Linux man page marks it MT-Safe, and any argument is valid, so the call is safe.
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

/// Of the C libraries Tachylite builds against, only glibc has `malloc_trim`.
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }

    /// The lint denies `unsafe`, so each exception must name the lint.
    #[test]
    fn this_file_holds_the_only_exception_to_the_unsafe_lint() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let lint = concat!("unsafe", "_code");
        let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(manifest.contains(&format!("{lint} = \"deny\"")));
        let mut files = Vec::new();
        for dir in ["src", "tests", "examples", "benches"] {
            sources(&root.join(dir), &mut files);
        }
        let named: Vec<(PathBuf, usize)> = files
            .into_iter()
            .filter_map(|path| {
                let count = fs::read_to_string(&path).unwrap().matches(lint).count();
                (count > 0).then_some((path, count))
            })
            .collect();
        assert_eq!(named, [(root.join("src/trim.rs"), 1)]);
    }

    /// `make size` runs this in the static build of each release target.
    #[cfg(target_env = "gnu")]
    #[test]
    fn a_heap_past_the_slack_is_trimmed_once() {
        let live = vec![1u8; 2 * super::SLACK];
        let floor = super::settle(0);
        assert!(floor > live.len());
        assert_eq!(super::settle(floor), floor);
    }
}
