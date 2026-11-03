//! Writes the seeded budget vault: `cargo run --release --example budget_vault -- <dir> [seed=1] [count=2000]`.

use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let dir = PathBuf::from(
        args.next()
            .ok_or("usage: budget_vault <dir> [seed] [count]")?,
    );
    let seed: u64 = args
        .next()
        .map(|s| s.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(1);
    let count: usize = args
        .next()
        .map(|s| s.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(2000);
    if dir.exists() && std::fs::read_dir(&dir)?.next().is_some() {
        return Err(format!("{} is not empty", dir.display()).into());
    }
    tachylite::budget_vault::write(&dir, seed, count)?;
    eprintln!("wrote {count} notes to {} (seed {seed})", dir.display());
    Ok(())
}
