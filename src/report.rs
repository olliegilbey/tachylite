//! What the binary prints to stderr.

use crate::vault::{SkipReason, Skipped};

/// Prints one line per skipped path.
pub fn skipped(list: &[Skipped]) {
    for skipped in list {
        let reason = match skipped.reason {
            SkipReason::Symlink => "symlink".to_string(),
            SkipReason::NonUtf8Name => "non-UTF-8 name".to_string(),
            SkipReason::UrlClash => "URL clash".to_string(),
            SkipReason::Unreadable(kind) => format!("unreadable ({kind})"),
        };
        eprintln!("skipped {}: {reason}", skipped.path.display());
    }
}
