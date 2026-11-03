use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use tachylite::build::build;
use tachylite::vault::SkipReason;

const USAGE: &str = "usage: tachylite build <vault> <out>";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [cmd, vault, out] if cmd == "build" => run_build(Path::new(vault), Path::new(out)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_build(vault: &Path, out: &Path) -> ExitCode {
    match build(vault, out) {
        Ok(summary) => {
            for skipped in &summary.skipped {
                let reason = match skipped.reason {
                    SkipReason::Symlink => "symlink".to_string(),
                    SkipReason::NonUtf8Name => "non-UTF-8 name".to_string(),
                    SkipReason::UrlClash => "URL clash".to_string(),
                    SkipReason::Unreadable(kind) => format!("unreadable ({kind})"),
                };
                eprintln!("skipped {}: {reason}", skipped.path.display());
            }
            eprintln!(
                "built {} notes, {} files, {} skipped",
                summary.notes,
                summary.files,
                summary.skipped.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
