use std::io;
use std::path::PathBuf;

use tachylite::Error;

fn watch(errno: i32) -> String {
    Error::Watch {
        path: PathBuf::from("vault"),
        source: io::Error::from_raw_os_error(errno),
    }
    .to_string()
}

#[test]
fn watch_hint_names_the_limit_that_was_hit() {
    // ENOSPC is the per-user watch limit; EMFILE is the per-user instance limit.
    assert!(
        watch(28).contains("fs.inotify.max_user_watches"),
        "{}",
        watch(28)
    );
    assert!(
        watch(24).contains("fs.inotify.max_user_instances"),
        "{}",
        watch(24)
    );
    assert!(!watch(13).contains("fs.inotify"), "{}", watch(13));
}
