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
