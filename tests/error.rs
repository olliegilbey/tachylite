use std::io;
use std::path::PathBuf;

use rustix::io::Errno;
use tachylite::Error;

fn watch(errno: Errno) -> String {
    Error::Watch {
        path: PathBuf::from("vault"),
        source: io::Error::from_raw_os_error(errno.raw_os_error()),
    }
    .to_string()
}

#[test]
fn watch_hint_names_the_limit_that_was_hit() {
    let full = watch(Errno::NOSPC);
    assert!(full.contains("fs.inotify.max_user_watches"), "{full}");
    let many = watch(Errno::MFILE);
    assert!(many.contains("fs.inotify.max_user_instances"), "{many}");
    assert!(many.contains("ulimit -n"), "{many}");
    let denied = watch(Errno::ACCESS);
    assert!(!denied.contains("raise"), "{denied}");
}
