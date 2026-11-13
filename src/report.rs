//! What the binary prints to stderr.

use crate::render::Broken;
use crate::vault::{SkipReason, Skipped};

/// Prints one line per skipped path.
pub fn skipped(list: &[Skipped]) {
    for skipped in list {
        eprintln!(
            "skipped {}: {}",
            skipped.path.display(),
            reason(skipped.reason)
        );
    }
}

/// Why a path was skipped, in plain words.
pub fn reason(reason: SkipReason) -> String {
    match reason {
        SkipReason::Symlink => "symlink".to_string(),
        SkipReason::NonUtf8Name => "non-UTF-8 name".to_string(),
        SkipReason::UrlClash => "URL clash".to_string(),
        SkipReason::NameTooLong => "name too long for its page".to_string(),
        SkipReason::TooLarge => format!(
            "larger than the {} MB note limit",
            crate::vault::NOTE_MAX / 1024 / 1024
        ),
        SkipReason::Unreadable(kind) => format!("unreadable ({kind})"),
    }
}

/// Prints one line per exclude pattern that matched no path on a full scan.
pub fn unmatched<S: AsRef<str>>(patterns: &[S]) {
    for pattern in patterns {
        let pattern = pattern.as_ref();
        eprintln!("exclude {pattern:?} matched no published path; check it for a typo");
    }
}

/// Prints one line per broken or ambiguous link.
pub fn broken(list: &[Broken]) {
    for link in list {
        match link.matches {
            0 => eprintln!("broken link in {}: {}", link.from, link.target),
            n => eprintln!(
                "ambiguous link in {}: {} matches {n} files",
                link.from, link.target
            ),
        }
    }
}
