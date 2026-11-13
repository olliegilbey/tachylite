//! The seeded budget vault: about 2,000 synthetic notes and 20 MB of text, for the budgets.
//! A change here moves every budget number; say so in the PR that makes one.

use std::fs;
use std::io;
use std::path::Path;

const SYLLABLES: [&str; 24] = [
    "ka", "lo", "mi", "ne", "ra", "tu", "shi", "vo", "pe", "zan", "dor", "fel", "quin", "bar",
    "ost", "ely", "mar", "ten", "sa", "gri", "um", "pho", "lik", "dez",
];
const README_FOLDERS: usize = 12;
const DAILY_PERCENT: usize = 15;
const MIN_NOTE: usize = 2_000;
const SPAN_NOTE: usize = 24_000;

/// splitmix64. Fixed here, unlike `rand`, so the vault never changes between dependency releases.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn word(&mut self) -> String {
        (0..1 + self.below(3))
            .map(|_| SYLLABLES[self.below(SYLLABLES.len())])
            .collect()
    }

    /// A tag from a vault-sized vocabulary: 24 tags, each with up to 24 nested under it. It
    /// draws as `word` does, so the rest of the vault stays as it was.
    fn tag(&mut self) -> String {
        let levels: Vec<&str> = (0..1 + self.below(3))
            .map(|_| SYLLABLES[self.below(SYLLABLES.len())])
            .collect();
        levels[..levels.len().min(2)].join("/")
    }
}

/// Every note as a vault-relative path and its Markdown, sorted by path.
pub fn generate(seed: u64, count: usize) -> Vec<(String, String)> {
    let mut rng = Rng(seed);
    let paths = note_paths(&mut rng, count);
    let mut notes: Vec<(String, String)> = paths
        .iter()
        .map(|path| (path.clone(), note(&mut rng, path, &paths)))
        .collect();
    notes.sort_by(|a, b| a.0.cmp(&b.0));
    notes
}

/// Writes the vault into `dir`.
pub fn write(dir: &Path, seed: u64, count: usize) -> io::Result<()> {
    for (path, content) in generate(seed, count) {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(file, content)?;
    }
    Ok(())
}

fn folders(rng: &mut Rng) -> Vec<String> {
    let mut folders: Vec<String> = Vec::new();
    while folders.len() < 50 {
        let parent = match rng.below(3) {
            0 => None,
            _ if folders.is_empty() => None,
            _ => {
                let p = folders[rng.below(folders.len())].clone();
                (p.matches('/').count() < 2).then_some(p)
            }
        };
        let name = format!("{}-{}", rng.word(), folders.len());
        folders.push(match parent {
            Some(p) => format!("{p}/{name}"),
            None => name,
        });
    }
    folders
}

fn date(day: usize) -> String {
    // Days since 2020-01-01 to a civil date, after Howard Hinnant's algorithm.
    let z = 18_262 + day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn note_paths(rng: &mut Rng, count: usize) -> Vec<String> {
    let folders = folders(rng);
    let daily = count * DAILY_PERCENT / 100;
    let readmes = README_FOLDERS.min(count.saturating_sub(daily));
    let mut paths: Vec<String> = (0..daily)
        .map(|d| format!("daily/{}.md", date(d * 2)))
        .collect();
    paths.extend(
        folders
            .iter()
            .take(readmes)
            .map(|f| format!("{f}/README.md")),
    );
    let mut i = 0;
    while paths.len() < count {
        let name = format!("{}-{}-{i}", rng.word(), rng.word());
        let path = match rng.below(10) {
            0 => format!("{name}.md"),
            _ => format!("{}/{name}.md", folders[rng.below(folders.len())]),
        };
        paths.push(path);
        i += 1;
    }
    paths
}

fn sentence(rng: &mut Rng, from: &str, paths: &[String], out: &mut String) {
    let words = 6 + rng.below(14);
    for w in 0..words {
        out.push(' ');
        match rng.below(100) {
            0..=3 => link(rng, from, paths, out),
            4 => {
                out.push_str(&format!("${}", 1 + rng.below(500)));
            }
            5 => {
                out.push_str(&format!("#{}", rng.tag()));
            }
            6 => {
                out.push_str(&format!("=={}==", rng.word()));
            }
            7 => {
                out.push_str(&format!("%%{} {}%%", rng.word(), rng.word()));
            }
            _ => out.push_str(&rng.word()),
        }
        if w + 1 == words {
            out.push('.');
        }
    }
}

fn link(rng: &mut Rng, from: &str, paths: &[String], out: &mut String) {
    if rng.chance(3) {
        out.push_str(&format!("[[missing-{}]]", rng.word()));
        return;
    }
    let target = &paths[rng.below(paths.len())];
    let target = target.strip_suffix(".md").unwrap_or(target);
    let mut text = match rng.below(3) {
        0 => format!("{}{target}", "../".repeat(from.matches('/').count())),
        1 if target.contains('/') => target.to_string(),
        _ => target.rsplit('/').next().unwrap_or(target).to_string(),
    };
    if rng.chance(15) {
        text.push_str(&format!("#{}", rng.word()));
    }
    if rng.chance(20) {
        text.push_str(&format!("|{} {}", rng.word(), rng.word()));
    }
    out.push_str(&format!("[[{text}]]"));
}

fn note(rng: &mut Rng, path: &str, paths: &[String]) -> String {
    let skew = rng.unit();
    let target = MIN_NOTE + (skew * skew * SPAN_NOTE as f64) as usize;
    let mut out = String::with_capacity(target + 512);
    if rng.chance(70) {
        out.push_str(&format!(
            "---\ntitle: {} {}\ntags: [{}, {}]\n",
            rng.word(),
            rng.word(),
            rng.tag(),
            rng.tag()
        ));
        if rng.chance(30) {
            out.push_str(&format!("aliases: [{}]\n", rng.word()));
        }
        out.push_str("---\n\n");
    }
    out.push_str(&format!("# {} {}\n\n", rng.word(), rng.word()));
    while out.len() < target {
        match rng.below(10) {
            0 => {
                out.push_str(&format!("## {} {}\n\n", rng.word(), rng.word()));
            }
            1 => {
                for _ in 0..3 + rng.below(4) {
                    let state = [" ", "/", "x", "-"][rng.below(4)];
                    out.push_str(&format!("- [{state}] {} {}\n", rng.word(), rng.word()));
                }
                out.push('\n');
            }
            2 => {
                out.push_str(&format!(
                    "```sh\necho {} ${}\n```\n\n",
                    rng.word(),
                    1 + rng.below(99)
                ));
            }
            3 => {
                out.push_str("| item | price |\n| --- | --- |\n");
                for _ in 0..2 + rng.below(4) {
                    out.push_str(&format!("| {} | ${} |\n", rng.word(), 1 + rng.below(999)));
                }
                out.push('\n');
            }
            4 => {
                out.push_str("> [!note]\n>");
                sentence(rng, path, paths, &mut out);
                out.push_str("\n\n");
            }
            _ => {
                for _ in 0..2 + rng.below(4) {
                    sentence(rng, path, paths, &mut out);
                }
                out.push_str("\n\n");
            }
        }
    }
    // A few notes carry raw HTML, so the sanitizer is under the budgets.
    if rng.chance(3) {
        out.push_str("<details><summary>More</summary>\n\nHidden <kbd>K</kbd>.\n\n</details>\n");
    }
    // And a few embed another note, so its edits re-render them.
    if rng.chance(2) {
        out.push_str("\n!");
        link(rng, path, paths, &mut out);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{Kind, classify};
    use std::collections::HashSet;

    fn vault() -> Vec<(String, String)> {
        generate(1, 2000)
    }

    fn all_text(v: &[(String, String)]) -> String {
        v.iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn same_seed_same_vault() {
        assert!(generate(1, 200) == generate(1, 200));
    }

    #[test]
    fn different_seed_differs() {
        assert!(generate(1, 200) != generate(2, 200));
    }

    #[test]
    fn budget_vault_size() {
        let v = vault();
        assert_eq!(v.len(), 2000);
        let total: usize = v.iter().map(|(_, c)| c.len()).sum();
        assert!((18_000_000..=22_000_000).contains(&total), "total {total}");
    }

    #[test]
    fn paths_unique_and_publishable() {
        let v = vault();
        let unique: HashSet<&str> = v.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(unique.len(), v.len());
        for (path, _) in &v {
            assert_eq!(
                classify(path, &crate::config::Exclude::default()),
                Some(Kind::Note),
                "{path}"
            );
        }
    }

    #[test]
    fn exercises_link_forms() {
        let v = vault();
        let text = all_text(&v);
        assert!(text.contains("[[../"));
        let links: Vec<&str> = text
            .split("[[")
            .skip(1)
            .filter_map(|s| s.split_once("]]").map(|(l, _)| l))
            .collect();
        let target = |l: &str| l.split(['|', '#']).next().unwrap_or("").to_string();
        assert!(
            links
                .iter()
                .any(|l| !l.starts_with('.') && target(l).contains('/')),
            "root path"
        );
        assert!(links.iter().any(|l| !target(l).contains('/')), "bare name");
        assert!(links.iter().any(|l| l.contains('|')), "label");
        assert!(links.iter().any(|l| l.contains('#')), "heading");
        let stems: HashSet<String> = v
            .iter()
            .map(|(p, _)| {
                p.rsplit('/')
                    .next()
                    .unwrap()
                    .trim_end_matches(".md")
                    .to_string()
            })
            .collect();
        assert!(
            links.iter().any(|l| {
                let t = target(l);
                !stems.contains(t.rsplit('/').next().unwrap())
            }),
            "missing target"
        );
    }

    #[test]
    fn exercises_syntax() {
        let v = vault();
        let readmes: HashSet<&str> = v
            .iter()
            .filter_map(|(p, _)| p.strip_suffix("/README.md"))
            .collect();
        assert!(readmes.len() >= 10, "{} README folders", readmes.len());
        let text = all_text(&v);
        for needle in [
            "- [ ] ",
            "- [/] ",
            "- [x] ",
            "- [-] ",
            "$",
            "```",
            "| --- |",
            "> [!note]",
            "==",
            "%%",
            " #",
            "aliases:",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
        let daily = v
            .iter()
            .filter(|(p, _)| {
                p.strip_prefix("daily/")
                    .and_then(|n| n.strip_suffix(".md"))
                    .is_some_and(|d| {
                        d.len() == 10 && d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-'
                    })
            })
            .count();
        assert!(daily >= 200, "{daily} daily notes");
    }
}
