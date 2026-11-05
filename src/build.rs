use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::Error;
use crate::output::{self, Writer};
use crate::site::{Change, Site};
use crate::vault::{VaultDir, scan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub notes: usize,
    pub files: usize,
    pub skipped: Vec<crate::vault::Skipped>,
    pub broken: Vec<crate::render::Broken>,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Checks the paths and opens the vault: the vault is a directory, and the output lives
/// outside it. Returns the vault and the resolved output path.
pub fn open(vault: &Path, out: &Path) -> Result<(VaultDir, PathBuf), Error> {
    if !vault.is_dir() {
        return Err(Error::VaultNotDir(vault.to_path_buf()));
    }
    let vault_path = vault.canonicalize().map_err(io_err(vault))?;
    let out = resolve_out(out)?;
    if overlaps(&vault_path, &out) {
        return Err(Error::Overlap {
            vault: vault_path,
            out,
        });
    }
    Ok((VaultDir::open(&vault_path)?, out))
}

/// Renders every note and copies every html page and asset into `out`: the site applied to
/// every file. Never writes inside the vault.
pub fn build(vault: &Path, out: &Path) -> Result<Summary, Error> {
    let (dir, out) = open(vault, out)?;
    let config = crate::config::load(&dir)?;
    let scan = scan(dir.root(), &config.exclude)?;
    output::prepare(&out)?;

    let mut writer = Writer::new(&out, &dir, |_| {});
    let report = Site::new(config.exclude).apply_to(
        vec![Change::Sync {
            under: String::new(),
            entries: scan.entries,
        }],
        &mut &dir,
        &mut writer,
    );
    let (notes, copies) = (writer.pages, writer.copies);
    let failed = writer.finish()?;
    let mut skipped = scan.skipped;
    skipped.extend(report.skipped);
    skipped.extend_from_slice(&failed);
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Summary {
        notes,
        files: copies - failed.len(),
        skipped,
        broken: report.broken,
    })
}

/// Makes `out` absolute without creating it: the deepest existing ancestor is canonicalised,
/// and the missing tail must be plain names, so the overlap check sees where it really leads.
fn resolve_out(out: &Path) -> Result<PathBuf, Error> {
    let abs = std::path::absolute(out).map_err(io_err(out))?;
    let components: Vec<Component> = abs.components().collect();
    let mut split = components.len();
    while split > 0 && !components[..split].iter().collect::<PathBuf>().exists() {
        split -= 1;
    }
    let existing: PathBuf = components[..split].iter().collect();
    let mut resolved = existing.canonicalize().map_err(io_err(&existing))?;
    for component in &components[split..] {
        match component {
            Component::Normal(name) => resolved.push(name),
            _ => return Err(Error::OutNotClean(out.to_path_buf())),
        }
    }
    Ok(resolved)
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// True if either path lies inside the other: as text, or by device and inode along the
/// ancestors, which catches an alias of the vault directory such as a bind mount of it.
/// It does not see a vault mounted somewhere below the output.
fn overlaps(vault: &Path, out: &Path) -> bool {
    if out.starts_with(vault) || vault.starts_with(out) {
        return true;
    }
    let inside = |inner: &Path, outer: &Path| {
        identity(outer).is_some_and(|id| inner.ancestors().any(|a| identity(a) == Some(id)))
    };
    inside(out, vault) || inside(vault, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_found_by_identity_not_path_text() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::create_dir(dir.path().join("other")).unwrap();
        std::os::unix::fs::symlink(&vault, dir.path().join("alias")).unwrap();
        // `alias` names the vault's directory under a different path, as a bind mount or a
        // case-insensitive filesystem would.
        assert!(overlaps(&vault, &dir.path().join("alias/site")));
        assert!(overlaps(&dir.path().join("alias"), &vault.join("site")));
        assert!(overlaps(&vault.join("deep"), dir.path()));
        assert!(!overlaps(&vault, &dir.path().join("other/site")));
    }
}
