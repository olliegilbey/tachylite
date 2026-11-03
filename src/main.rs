use std::ffi::OsString;
use std::process::ExitCode;

const USAGE: &str = "usage: tachylite build <vault> <out>";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [cmd, _vault, _out] if cmd == "build" => ExitCode::SUCCESS,
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
