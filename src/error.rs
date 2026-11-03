use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    Io { path: PathBuf, source: io::Error },
    VaultNotDir(PathBuf),
    Overlap { vault: PathBuf, out: PathBuf },
    OutNotOurs(PathBuf),
    OutNotClean(PathBuf),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::VaultNotDir(path) => write!(f, "vault {} is not a directory", path.display()),
            Error::Overlap { vault, out } => write!(
                f,
                "output {} and vault {} overlap; the output must live outside the vault",
                out.display(),
                vault.display()
            ),
            Error::OutNotOurs(path) => write!(
                f,
                "output {} is not empty and has no .tachylite-output marker; refusing to write there",
                path.display()
            ),
            Error::OutNotClean(path) => write!(
                f,
                "output {} has . or .. after its last existing directory; give a plain path",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
