use std::fmt;
use std::io;
use std::path::PathBuf;

use rustix::io::Errno;

#[derive(Debug)]
pub enum Error {
    Io { path: PathBuf, source: io::Error },
    VaultNotDir(PathBuf),
    Overlap { vault: PathBuf, out: PathBuf },
    OutNotOurs(PathBuf),
    OutNotClean(PathBuf),
    NoOpenat2,
    Bind { port: u16, source: io::Error },
    Watch { path: PathBuf, source: io::Error },
    VaultGone(PathBuf),
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
            Error::NoOpenat2 => write!(
                f,
                "this kernel has no openat2 (Linux 5.6+); Tachylite needs it to read the vault safely"
            ),
            Error::Bind { port, source } => {
                write!(f, "cannot serve on 127.0.0.1:{port}: {source}")
            }
            Error::Watch { path, source } => {
                write!(f, "cannot watch {}: {source}", path.display())?;
                let errno = source.raw_os_error().map(Errno::from_raw_os_error);
                match errno {
                    Some(Errno::NOSPC) => f.write_str(" (raise fs.inotify.max_user_watches)"),
                    Some(Errno::MFILE) => f.write_str(" (raise fs.inotify.max_user_instances)"),
                    _ => Ok(()),
                }
            }
            Error::VaultGone(path) => {
                write!(f, "vault {} was moved or deleted; stopping", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } | Error::Bind { source, .. } | Error::Watch { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}
