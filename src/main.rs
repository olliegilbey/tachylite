use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use tachylite::build::build;
use tachylite::serve::{DEFAULT_PORT, serve};

const USAGE: &str =
    "usage: tachylite build <vault> <out>\n       tachylite serve <vault> <out> [--port <port>]";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [cmd, vault, out] if cmd == "build" => run_build(Path::new(vault), Path::new(out)),
        [cmd, vault, out] if cmd == "serve" => run_serve(vault, out, DEFAULT_PORT),
        [cmd, vault, out, flag, port] if cmd == "serve" && flag == "--port" => {
            match port.to_str().and_then(|p| p.parse().ok()) {
                Some(port) => run_serve(vault, out, port),
                None => usage(),
            }
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn fail(e: tachylite::Error) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::from(1)
}

fn run_serve(vault: &OsString, out: &OsString, port: u16) -> ExitCode {
    match serve(Path::new(vault), Path::new(out), port) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn run_build(vault: &Path, out: &Path) -> ExitCode {
    match build(vault, out) {
        Ok(summary) => {
            tachylite::report::skipped(&summary.skipped);
            tachylite::report::broken(&summary.broken);
            eprintln!(
                "built {} notes, {} files, {} skipped",
                summary.notes,
                summary.files,
                summary.skipped.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}
