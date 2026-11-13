use proptest::prelude::*;
use tachylite::links::{Index, Resolved, key, resolve, split};

const FIXTURE: [&str; 6] = [
    "people/Jane Doe.md",
    "notes/README.md",
    "README.md",
    "notes/deep/README.md",
    "maps/route.html",
    "img/dot.png",
];

fn fixture() -> Index {
    Index::new(FIXTURE)
}

fn found(path: &str) -> Resolved {
    Resolved::Found(path.into())
}

#[test]
fn relative_paths_resolve_from_the_linking_note() {
    let index = fixture();
    assert_eq!(
        resolve(&index, "notes/a.md", "../people/Jane Doe"),
        found("people/Jane Doe.md")
    );
    assert_eq!(
        resolve(&index, "notes/a.md", "./deep/README"),
        found("notes/deep/README.md")
    );
}

#[test]
fn a_downward_path_without_dot_slash_falls_back_to_the_linking_note() {
    let index = Index::new(["proj/sub/detail.md", "sub/other.md", "notes/README.md"]);
    assert_eq!(
        resolve(&index, "proj/plan.md", "sub/detail"),
        found("proj/sub/detail.md")
    );
    assert_eq!(
        resolve(&index, "proj/plan.md", "Sub/Detail.md"),
        found("proj/sub/detail.md")
    );
    // The root lookup comes first.
    let index = Index::new(["proj/notes/README.md", "notes/README.md"]);
    assert_eq!(
        resolve(&index, "proj/plan.md", "notes/README"),
        found("notes/README.md")
    );
}

#[test]
fn paths_with_a_slash_resolve_from_the_root_in_any_case() {
    let index = fixture();
    for from in ["a.md", "notes/deep/b.md"] {
        assert_eq!(
            resolve(&index, from, "people/Jane Doe"),
            found("people/Jane Doe.md")
        );
        assert_eq!(
            resolve(&index, from, "People/jane doe"),
            found("people/Jane Doe.md")
        );
        assert_eq!(
            resolve(&index, from, "people/Jane Doe.md"),
            found("people/Jane Doe.md")
        );
    }
}

#[test]
fn a_bare_name_finds_the_nearest_match() {
    let index = fixture();
    assert_eq!(
        resolve(&index, "notes/a.md", "README"),
        found("notes/README.md")
    );
    assert_eq!(resolve(&index, "x/a.md", "README"), found("README.md"));
    assert_eq!(
        resolve(&index, "notes/deep/b.md", "README"),
        found("notes/deep/README.md")
    );
    assert_eq!(
        resolve(&index, "a.md", "jane doe"),
        found("people/Jane Doe.md")
    );
}

#[test]
fn a_tie_at_the_nearest_distance_is_ambiguous() {
    let index = Index::new(["a/README.md", "b/README.md"]);
    assert_eq!(resolve(&index, "c/x.md", "README"), Resolved::Ambiguous(2));
    assert_eq!(resolve(&index, "a/x.md", "README"), found("a/README.md"));
}

#[test]
fn only_notes_drop_their_extension() {
    let index = fixture();
    assert_eq!(
        resolve(&index, "a.md", "route.html"),
        found("maps/route.html")
    );
    assert_eq!(resolve(&index, "a.md", "route"), Resolved::Missing);
    assert_eq!(resolve(&index, "a.md", "dot.png"), found("img/dot.png"));
    assert_eq!(resolve(&index, "a.md", "Dot.PNG"), found("img/dot.png"));
}

#[test]
fn climbing_above_the_root_is_missing() {
    let index = fixture();
    assert_eq!(
        resolve(&index, "notes/a.md", "../../people/Jane Doe"),
        Resolved::Missing
    );
    assert_eq!(resolve(&index, "a.md", "../README"), Resolved::Missing);
    assert_eq!(
        resolve(&index, "a.md", "people/../../README"),
        Resolved::Missing
    );
    assert_eq!(resolve(&index, "a.md", ""), Resolved::Missing);
}

#[test]
fn insert_and_remove_change_what_resolves() {
    let mut index = Index::new(["a/README.md"]);
    assert_eq!(resolve(&index, "c/x.md", "README"), found("a/README.md"));
    index.insert("b/README.md");
    index.insert("b/README.md");
    assert_eq!(resolve(&index, "c/x.md", "README"), Resolved::Ambiguous(2));
    index.remove("a/README.md");
    assert_eq!(resolve(&index, "c/x.md", "README"), found("b/README.md"));
    assert_eq!(resolve(&index, "c/x.md", "a/README"), Resolved::Missing);
    index.remove("b/README.md");
    assert_eq!(resolve(&index, "c/x.md", "README"), Resolved::Missing);
    assert_eq!(index.get("b/README.md"), None);
}

#[test]
fn get_and_bucket_share_the_published_paths() {
    let index = Index::new(["a/README.md", "b/readme.md", "c.md"]);
    assert_eq!(index.get("a/README.md").as_deref(), Some("a/README.md"));
    assert_eq!(index.get("a/readme.md"), None, "get is exact");
    let mut bucket: Vec<&str> = index.bucket("readme").iter().map(|p| &**p).collect();
    bucket.sort();
    assert_eq!(bucket, ["a/README.md", "b/readme.md"]);
    assert!(index.bucket("nothing").is_empty());
}

#[test]
fn key_is_the_lowercase_basename_without_md() {
    assert_eq!(key("people/Jane Doe.md"), "jane doe");
    assert_eq!(key("Jane Doe"), "jane doe");
    assert_eq!(key("A.MD"), "a");
    assert_eq!(key("maps/route.html"), "route.html");
}

#[test]
fn split_separates_the_heading() {
    assert_eq!(split("note#Some Heading"), ("note", Some("Some Heading")));
    assert_eq!(split("#Top"), ("", Some("Top")));
    assert_eq!(split("note"), ("note", None));
}

fn folder(path: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts
}

fn distance(from: &str, to: &str) -> usize {
    let (a, b) = (folder(from), folder(to));
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    a.len() + b.len() - 2 * common
}

fn path() -> impl Strategy<Value = String> {
    let segment = prop::sample::select(vec!["a", "b", "notes", "deep"]);
    let stem = prop::sample::select(vec!["readme", "x", "jane doe"]);
    let ext = prop::sample::select(vec!["md", "html", "png"]);
    (prop::collection::vec(segment, 0..3), stem, ext).prop_map(|(dirs, stem, ext)| {
        let mut parts: Vec<String> = dirs.into_iter().map(String::from).collect();
        parts.push(format!("{stem}.{ext}"));
        parts.join("/")
    })
}

fn link_text(path: &str) -> String {
    path.strip_suffix(".md").unwrap_or(path).to_string()
}

proptest! {
    #[test]
    fn resolution_matches_the_spec(
        paths in prop::collection::btree_set(path(), 1..12),
        from in path(),
    ) {
        let index = Index::new(paths.iter().map(String::as_str));
        for p in &paths {
            if p.contains('/') {
                prop_assert_eq!(resolve(&index, &from, &link_text(p)), found(p));
                prop_assert_eq!(resolve(&index, &from, &p.to_uppercase()), found(p));
            }
            // From the linking note's folder: `../` steps up, then down to `p`.
            let (here, there) = (folder(&from), folder(p));
            let common = here.iter().zip(&there).take_while(|(x, y)| x == y).count();
            let mut steps = vec![".."; here.len() - common];
            steps.extend(&there[common..]);
            let name = link_text(p.rsplit('/').next().unwrap_or(p));
            steps.push(&name);
            let relative = steps.join("/");
            let dotted = if relative.starts_with("../") { relative.clone() } else { format!("./{relative}") };
            prop_assert_eq!(resolve(&index, &from, &dotted), found(p));
            // A downward path without `./` tries the root first.
            if !relative.starts_with("../") && relative.contains('/') {
                let root: Vec<&String> =
                    paths.iter().filter(|q| link_text(q) == relative).collect();
                let want = match root[..] {
                    [] => found(p),
                    [q] => found(q),
                    _ => Resolved::Ambiguous(root.len()),
                };
                prop_assert_eq!(resolve(&index, &from, &relative), want);
            }
            let bare = link_text(p.rsplit('/').next().unwrap_or(p));
            let candidates: Vec<&String> =
                paths.iter().filter(|q| key(q) == key(&bare)).collect();
            let nearest = candidates.iter().map(|q| distance(&from, q)).min();
            let tied = candidates
                .iter()
                .filter(|q| Some(distance(&from, q)) == nearest)
                .count();
            match resolve(&index, &from, &bare) {
                Resolved::Found(q) => {
                    prop_assert_eq!(key(&q), key(&bare));
                    prop_assert_eq!(Some(distance(&from, &q)), nearest);
                    prop_assert_eq!(tied, 1);
                }
                Resolved::Ambiguous(n) => {
                    prop_assert!(n >= 2);
                    prop_assert_eq!(n, tied);
                }
                Resolved::Missing => prop_assert!(false, "{bare} from {from} is missing"),
            }
        }
    }
}

#[test]
fn a_link_to_a_folder_is_missing() {
    let index = fixture();
    for target in ["people/..", "notes/deep/..", "notes/", "./", "notes/."] {
        assert_eq!(
            resolve(&index, "notes/a.md", target),
            Resolved::Missing,
            "{target}"
        );
    }
}

fn with_alias(paths: &[&str], aliases: &[(&str, &[&str])]) -> Index {
    let mut index = Index::new(paths.iter().copied());
    for (path, names) in aliases {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        index.set_aliases(path, &names);
    }
    index
}

#[test]
fn alias_resolves_bare_name() {
    let index = with_alias(&["people/Jane Doe.md"], &[("people/Jane Doe.md", &["JD"])]);
    assert_eq!(resolve(&index, "a.md", "jd"), found("people/Jane Doe.md"));
    assert_eq!(
        resolve(&index, "a.md", "JD.md"),
        found("people/Jane Doe.md")
    );
    assert_eq!(index.aliases("people/Jane Doe.md"), ["JD"]);
}

#[test]
fn basename_beats_alias() {
    let index = with_alias(
        &["people/Jane Doe.md", "jd.md"],
        &[("people/Jane Doe.md", &["JD"])],
    );
    assert_eq!(resolve(&index, "a.md", "JD"), found("jd.md"));
}

#[test]
fn alias_needs_bare_name() {
    let index = with_alias(&["people/Jane Doe.md"], &[("people/Jane Doe.md", &["JD"])]);
    assert_eq!(resolve(&index, "a.md", "people/JD"), Resolved::Missing);
    assert_eq!(resolve(&index, "a.md", "./JD"), Resolved::Missing);
}

#[test]
fn alias_tie_is_ambiguous() {
    let index = with_alias(
        &["x/a.md", "y/b.md"],
        &[("x/a.md", &["same"]), ("y/b.md", &["Same"])],
    );
    assert_eq!(resolve(&index, "c.md", "same"), Resolved::Ambiguous(2));
}

#[test]
fn nearest_alias_wins() {
    let index = with_alias(
        &["x/a.md", "y/b.md"],
        &[("x/a.md", &["same"]), ("y/b.md", &["same"])],
    );
    assert_eq!(resolve(&index, "y/c.md", "same"), found("y/b.md"));
}

#[test]
fn replacing_and_removing_aliases() {
    let mut index = with_alias(&["a.md"], &[("a.md", &["one"])]);
    index.set_aliases("a.md", &["two".to_string()]);
    assert_eq!(resolve(&index, "c.md", "one"), Resolved::Missing);
    assert_eq!(resolve(&index, "c.md", "two"), found("a.md"));
    assert_eq!(index.alias_bucket("two").len(), 1);
    index.remove("a.md");
    assert_eq!(resolve(&index, "c.md", "two"), Resolved::Missing);
    assert!(index.alias_bucket("two").is_empty());
    assert!(index.aliases("a.md").is_empty());
}
