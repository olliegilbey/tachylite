//! A tick: the pure half. A token names a task's box by a hash of its line, and by the
//! line's place among the lines of the note's file with that hash. The renderer writes tokens
//! and the write path finds a token's box, both through `boxes`, so a token exists only for
//! a box the write path can find, and both count the lines of one hash alike.

use std::collections::HashMap;
use std::fmt;
use std::hash::{DefaultHasher, Hasher};

/// The offset of the box's character in a raw line. The box follows any run of spaces, tabs
/// and `>`, then one or more list markers, each followed by spaces or tabs. It is `[c]`
/// followed by a space, a tab or the line's end.
pub fn find(line: &[u8]) -> Option<usize> {
    let run = |from: usize, of: fn(&u8) -> bool| line[from..].iter().take_while(|b| of(b)).count();
    let mut at = run(0, |b| matches!(b, b' ' | b'\t' | b'>'));
    let mut markers = 0;
    loop {
        let digits = run(at, u8::is_ascii_digit);
        let end = match line.get(at + digits) {
            Some(b'.' | b')') if (1..=9).contains(&digits) => at + digits + 1,
            Some(b'-' | b'+' | b'*') if digits == 0 => at + 1,
            _ => break,
        };
        let gap = run(end, |b| matches!(b, b' ' | b'\t'));
        if gap == 0 {
            return None;
        }
        at = end + gap;
        markers += 1;
    }
    let boxed = markers > 0
        && line.get(at) == Some(&b'[')
        && line.get(at + 2) == Some(&b']')
        && matches!(line.get(at + 3), None | Some(b' ' | b'\t'));
    boxed.then_some(at + 1)
}

/// A line's hash with the character at `at`, its box, read as a space: a tick leaves it as
/// it was. A note renders from its text with invalid UTF-8 replaced, so the hash reads the
/// line so too. Only ASCII comes before a box, so its offset holds.
pub fn hash(line: &[u8], at: usize) -> u32 {
    let text = String::from_utf8_lossy(line);
    let line = text.as_bytes();
    let mut hasher = DefaultHasher::new();
    hasher.write(&line[..at]);
    hasher.write(b" ");
    hasher.write(&line[at + 1..]);
    (hasher.finish() & 0xffff_ffff) as u32
}

/// Each line of `text` with the offset of its start, without its `\n` or `\r\n`.
pub fn lines(text: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut next = 0;
    text.split_inclusive(|b| *b == b'\n').map(move |line| {
        let start = next;
        next += line.len();
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        (start, line.strip_suffix(b"\r").unwrap_or(line))
    })
}

/// True when every `\r` in `text` starts a `\r\n`. A lone one ends a line for the Markdown
/// parser alone, so its line numbers would not be `lines`' numbers.
pub fn numbered(text: &[u8]) -> bool {
    !text
        .iter()
        .enumerate()
        .any(|(at, b)| *b == b'\r' && text.get(at + 1) != Some(&b'\n'))
}

/// A task's box: its line's hash, and the line's place among the lines of the note's file
/// that hold a box and have that hash: it is line `nth` of `of`, counted from 0. On a page
/// it reads `<hash>`, 8 hex digits, when the line is the only one, else `<hash>.<nth>.<of>`.
/// It holds no line number, so lines added or removed elsewhere leave it as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub hash: u32,
    pub nth: usize,
    pub of: usize,
}

impl Token {
    pub fn parse(text: &str) -> Option<Token> {
        let digits = |s: &str, radix| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
        let mut parts = text.split('.');
        let hash = parts.next().filter(|h| h.len() == 8 && digits(h, 16))?;
        let hash = u32::from_str_radix(hash, 16).ok()?;
        let (nth, of) = match (parts.next(), parts.next(), parts.next()) {
            (None, ..) => (0, 1),
            (Some(nth), Some(of), None) if digits(nth, 10) && digits(of, 10) => {
                (nth.parse().ok()?, of.parse().ok()?)
            }
            _ => return None,
        };
        let token = Token { hash, nth, of };
        // Each token reads one way: the way it is written.
        (nth < of && token.to_string() == text).then_some(token)
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.hash)?;
        if self.of != 1 {
            write!(f, ".{}.{}", self.nth, self.of)?;
        }
        Ok(())
    }
}

/// A line of a note's file that holds a box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boxed {
    /// The line's number in the file, from 1.
    pub line: usize,
    /// The offset in the file of the line's start, and of the box's character.
    pub start: usize,
    pub at: usize,
    pub token: Token,
}

/// Every line of `text`, a note's whole file, that holds a box, in line order, each with
/// its token. A line in the frontmatter, in a code block or in a comment counts like any
/// other: the write path reads the file without parsing it, so the renderer counts so too.
pub fn boxes(text: &[u8]) -> Vec<Boxed> {
    let mut found: Vec<Boxed> = lines(text)
        .enumerate()
        .filter_map(|(number, (start, line))| {
            let at = find(line)?;
            let hash = hash(line, at);
            Some(Boxed {
                line: number + 1,
                start,
                at: start + at,
                token: Token {
                    hash,
                    nth: 0,
                    of: 0,
                },
            })
        })
        .collect();
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for boxed in &mut found {
        let count = counts.entry(boxed.token.hash).or_default();
        boxed.token.nth = *count;
        *count += 1;
    }
    for boxed in &mut found {
        boxed.token.of = counts[&boxed.token.hash];
    }
    found
}

/// The offset in `text` of the box's character that `token` names: the box of line `nth`
/// among the lines that hold a box and have its hash, when there are `of` of them. `None`
/// when their count is any other: a line of that text came or went since the token was
/// written, so its place no longer says which line it was.
pub fn locate(text: &[u8], token: Token) -> Option<usize> {
    boxes(text)
        .into_iter()
        .find(|boxed| boxed.token == token)
        .map(|boxed| boxed.at)
}

/// What a tick asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum To {
    Done,
    Todo,
}

impl To {
    pub fn parse(text: &str) -> Option<To> {
        match text {
            "done" => Some(To::Done),
            "todo" => Some(To::Todo),
            _ => None,
        }
    }
}

/// The task changed since its page was rendered, so the tick no longer names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict;

/// The character a tick writes over a box that holds `now`, or `None` when the task is
/// already so. Done takes a todo or a doing task. A cancelled task, or a box this does not
/// know, is a conflict. Todo takes a done task, and any other is not done already.
pub fn decide(to: To, now: u8) -> Result<Option<u8>, Conflict> {
    match (to, now) {
        (To::Done, b' ' | b'/') => Ok(Some(b'x')),
        (To::Done, b'x' | b'X') => Ok(None),
        (To::Done, _) => Err(Conflict),
        (To::Todo, b'x' | b'X') => Ok(Some(b' ')),
        (To::Todo, _) => Ok(None),
    }
}

/// The one byte a tick changes in a note's `text`, as its offset and new value, or `None`
/// when the task is already so.
pub fn plan(text: &[u8], token: Token, to: To) -> Result<Option<(usize, u8)>, Conflict> {
    let at = locate(text, token).ok_or(Conflict)?;
    Ok(decide(to, text[at])?.map(|byte| (at, byte)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_box_follows_list_markers() {
        for (line, at) in [
            ("- [ ] a", Some(3)),
            ("- [x] a", Some(3)),
            ("* [/] a", Some(3)),
            ("+ [-] a", Some(3)),
            ("- [ ]", Some(3)),
            ("- [ ]\ta", Some(3)),
            ("-\t[ ] a", Some(3)),
            ("  - [ ] a", Some(5)),
            ("\t- [ ] a", Some(4)),
            ("1. [ ] a", Some(4)),
            ("12) [ ] a", Some(5)),
            ("123456789. [ ] a", Some(12)),
            ("> - [ ] a", Some(5)),
            (">> - [ ] a", Some(6)),
            ("> > - [ ] a", Some(7)),
            ("- - [ ] a", Some(5)),
            ("1. - [ ] a", Some(6)),
            ("- [ ] - [x] a", Some(3)),
            ("[ ] a", None),
            ("> [ ] a", None),
            ("-[ ] a", None),
            ("- [ ]a", None),
            ("- [] a", None),
            ("- [  ] a", None),
            ("- [ a", None),
            ("- a [ ] b", None),
            ("1234567890. [ ] a", None),
            ("1 [ ] a", None),
            ("- > - [ ] a", None),
            ("a - [ ] b", None),
            ("\u{FEFF}- [ ] a", None),
            ("-", None),
            ("- ", None),
            ("", None),
        ] {
            assert_eq!(find(line.as_bytes()), at, "{line:?}");
        }
    }

    #[test]
    fn a_tick_keeps_the_hash() {
        let hash_of = |line: &str| hash(line.as_bytes(), find(line.as_bytes()).unwrap());
        let todo = hash_of("- [ ] call [[people/jane-doe|Jane]]");
        assert_eq!(todo, hash_of("- [x] call [[people/jane-doe|Jane]]"));
        assert_eq!(todo, hash_of("- [/] call [[people/jane-doe|Jane]]"));
        assert_ne!(todo, hash_of("- [ ] call [[people/john-doe|Jane]]"));
        assert_ne!(todo, hash_of(" - [ ] call [[people/jane-doe|Jane]]"));
        // The same bytes whether hashed in one piece or three.
        let mut whole = DefaultHasher::new();
        whole.write(b"- [ ] a");
        assert_eq!(
            hash(b"- [x] a", 3),
            (whole.finish() & 0xffff_ffff) as u32,
            "the hash reads the line as one run of bytes"
        );
    }

    #[test]
    fn a_hash_reads_invalid_utf8_as_the_renderer_does() {
        let raw = b"- [ ] caf\xe9";
        let text = String::from_utf8_lossy(raw);
        assert_eq!(hash(raw, 3), hash(text.as_bytes(), 3));
    }

    #[test]
    fn lines_end_at_a_newline() {
        let got: Vec<(usize, &[u8])> = lines(b"a\r\nbc\n\nd").collect();
        let want: [(usize, &[u8]); 4] = [(0, b"a"), (3, b"bc"), (6, b""), (7, b"d")];
        assert_eq!(got, want);
        assert_eq!(lines(b"a\n").count(), 1);
        assert_eq!(lines(b"").count(), 0);
        assert!(numbered(b"a\r\nb\n"));
        assert!(!numbered(b"a\rb\n"));
        assert!(!numbered(b"a\r"));
    }

    #[test]
    fn a_token_reads_as_it_is_written() {
        let only = Token {
            hash: 0x00ab_cdef,
            nth: 0,
            of: 1,
        };
        assert_eq!(only.to_string(), "00abcdef");
        assert_eq!(Token::parse("00abcdef"), Some(only));
        let second = Token {
            hash: 0x00ab_cdef,
            nth: 1,
            of: 12,
        };
        assert_eq!(second.to_string(), "00abcdef.1.12");
        assert_eq!(Token::parse("00abcdef.1.12"), Some(second));
        for bad in [
            "",
            "12",
            "abcdef",
            "00abcdefa",
            "00abcdeg",
            "+0abcdef",
            "00ABCDEF",
            "3-00abcdef",
            "00abcdef.",
            "00abcdef.1",
            "00abcdef.1.",
            "00abcdef..2",
            "00abcdef.1.2.3",
            "00abcdef.0.1",
            "00abcdef.0.0",
            "00abcdef.2.2",
            "00abcdef.3.2",
            "00abcdef.01.2",
            "00abcdef.+1.2",
            "00abcdef.1.-2",
            "00abcdef.1 .2",
            "00abcdef.1.99999999999999999999999",
        ] {
            assert_eq!(Token::parse(bad), None, "{bad:?}");
        }
    }

    /// The token of the box on line `line` of `text`, counted from 1.
    fn token(text: &str, line: usize) -> Token {
        let found = boxes(text.as_bytes());
        found.iter().find(|b| b.line == line).unwrap().token
    }

    /// The offset of the box before `text`, which starts at the box's `]`.
    fn box_of(note: &str, text: &str) -> usize {
        note.find(text).unwrap() - 1
    }

    #[test]
    fn every_line_with_a_box_counts_wherever_it_is() {
        let note = "---\ntags:\n  - [ ] a\n---\n- [ ] a\n```\n- [x] a\n```\ntext\n- [/] b\n";
        let found: Vec<(usize, usize, String)> = boxes(note.as_bytes())
            .iter()
            .map(|b| (b.line, b.at - b.start, b.token.to_string()))
            .collect();
        let a = format!("{:08x}", hash(b"- [ ] a", 3));
        let b = format!("{:08x}", hash(b"- [ ] b", 3));
        assert_eq!(
            found,
            [
                // Indented, so another hash.
                (3, 5, format!("{:08x}", hash(b"  - [ ] a", 5))),
                (5, 3, format!("{a}.0.2")),
                (7, 3, format!("{a}.1.2")),
                (10, 3, b),
            ]
        );
        assert_eq!(boxes(b""), []);
        assert_eq!(boxes(b"text\n\n- [] a\n"), []);
    }

    #[test]
    fn a_token_finds_its_line_wherever_it_moved() {
        let note = "# n\n- [ ] one\n- [ ] two\n";
        let two = token(note, 3);
        assert_eq!(two.to_string().len(), 8);
        assert_eq!(locate(note.as_bytes(), two), Some(box_of(note, "] two")));
        // Lines added above: the line moved, and its text is still the only one.
        let moved = format!("intro\n\n{note}");
        assert_eq!(locate(moved.as_bytes(), two), Some(box_of(&moved, "] two")));
        // Ticked since: the same task.
        let ticked = note.replace("- [ ] two", "- [x] two");
        assert_eq!(
            locate(ticked.as_bytes(), two),
            Some(box_of(&ticked, "] two"))
        );
        // Reworded, or gone.
        assert_eq!(locate(b"# n\n- [ ] one\n- [ ] 2\n", two), None);
        assert_eq!(locate(b"", two), None);
        // No longer a box, though the hash's bytes are there.
        assert_eq!(locate(b"# n\n- [ ] one\n- [ ] two\\\n", two), None);
        // A second line of its text: the token says there was one.
        assert_eq!(locate(b"- [ ] two\n# n\n- [ ] two\n", two), None);
    }

    #[test]
    fn a_place_picks_between_lines_of_one_text() {
        let note = "# a\n- [ ] ship\n# b\n- [ ] ship\n";
        let (first, second) = (token(note, 2), token(note, 4));
        assert_eq!(first.to_string(), format!("{:08x}.0.2", first.hash));
        assert_eq!(second.to_string(), format!("{:08x}.1.2", first.hash));
        assert_eq!(locate(note.as_bytes(), first), Some(7));
        assert_eq!(locate(note.as_bytes(), second), Some(22));
        // Three lines added above: each token still names its own line.
        let moved = format!("one\ntwo\nthree\n{note}");
        assert_eq!(locate(moved.as_bytes(), first), Some(14 + 7));
        assert_eq!(locate(moved.as_bytes(), second), Some(14 + 22));
        // Either ticked: the other keeps its place.
        let ticked = note.replacen("- [ ] ship", "- [x] ship", 1);
        assert_eq!(locate(ticked.as_bytes(), second), Some(22));
        // One of the two removed, or a third added: no place says which line is which.
        for changed in [
            "# a\n# b\n- [ ] ship\n".to_string(),
            "# a\n- [ ] ship\n# b\n".to_string(),
            format!("- [ ] ship\n{note}"),
            format!("{note}- [x] ship\n"),
        ] {
            assert_eq!(locate(changed.as_bytes(), first), None, "{changed:?}");
            assert_eq!(locate(changed.as_bytes(), second), None, "{changed:?}");
        }
    }

    #[test]
    fn a_task_deleted_beside_its_text_in_a_code_block_is_a_conflict() {
        let note = "- [ ] ship\n\n```\n- [ ] ship\n```\n";
        let task = token(note, 1);
        assert_eq!(task.to_string(), format!("{:08x}.0.2", task.hash));
        assert_eq!(locate(note.as_bytes(), task), Some(3));
        // The task is gone, and the code block keeps a line of its text.
        assert_eq!(locate(b"\n```\n- [ ] ship\n```\n", task), None);
        assert_eq!(
            plan(b"\n```\n- [ ] ship\n```\n", task, To::Done),
            Err(Conflict)
        );
    }

    #[test]
    fn a_tick_decides_one_byte() {
        for (to, now, want) in [
            (To::Done, b' ', Ok(Some(b'x'))),
            (To::Done, b'/', Ok(Some(b'x'))),
            (To::Done, b'x', Ok(None)),
            (To::Done, b'X', Ok(None)),
            (To::Done, b'-', Err(Conflict)),
            (To::Done, b'?', Err(Conflict)),
            (To::Todo, b'x', Ok(Some(b' '))),
            (To::Todo, b'X', Ok(Some(b' '))),
            (To::Todo, b' ', Ok(None)),
            (To::Todo, b'/', Ok(None)),
            (To::Todo, b'-', Ok(None)),
        ] {
            assert_eq!(decide(to, now), want, "{to:?} over {:?}", now as char);
        }
        assert_eq!(To::parse("done"), Some(To::Done));
        assert_eq!(To::parse("todo"), Some(To::Todo));
        assert_eq!(To::parse("Done"), None);
        assert_eq!(To::parse("doing"), None);
    }

    /// Lines that are tasks, near-tasks and plain text.
    const LINES: &[&str] = &[
        "- [ ] a",
        "- [ ] a",
        "- [x] a",
        "- [/] b",
        "- [-] c",
        "  * [ ] nested",
        "> - [X] quoted",
        "1. [ ] first",
        "- [ ]",
        "-[ ] no",
        "text",
        "",
        "- [ ] caf\u{e9}",
    ];

    proptest! {
        /// A plan changes the box of the line it names, by one byte, to the status asked for,
        /// whichever lines share its text and wherever the note's other lines moved it.
        #[test]
        fn a_plan_changes_only_the_box_it_names(
            picks in prop::collection::vec(prop::sample::select(LINES), 1..12),
            crlf in any::<bool>(),
            pick in any::<prop::sample::Index>(),
            done in any::<bool>(),
            above in 0..4usize,
        ) {
            let end = if crlf { "\r\n" } else { "\n" };
            let text = picks.join(end);
            let found = boxes(text.as_bytes());
            prop_assume!(!found.is_empty());
            let Boxed { at, token, .. } = found[pick.index(found.len())];
            let now = text.as_bytes()[at];
            let to = if done { To::Done } else { To::Todo };
            // The page is older than the note: lines that hold no box came in above.
            let shift = format!("text{end}").repeat(above);
            let text = format!("{shift}{text}");
            match plan(text.as_bytes(), token, to) {
                Ok(Some((offset, byte))) => {
                    prop_assert_eq!(offset, shift.len() + at);
                    prop_assert_ne!(byte, now);
                    prop_assert_eq!(byte, if done { b'x' } else { b' ' });
                }
                Ok(None) => prop_assert_eq!(done, matches!(now, b'x' | b'X')),
                Err(Conflict) => prop_assert!(done && now == b'-'),
            }
        }

        /// A token names one box of its note, and each box has its own.
        #[test]
        fn each_box_has_its_own_token(
            picks in prop::collection::vec(prop::sample::select(LINES), 0..12),
        ) {
            let text = picks.join("\n");
            let found = boxes(text.as_bytes());
            for boxed in &found {
                prop_assert_eq!(locate(text.as_bytes(), boxed.token), Some(boxed.at));
                prop_assert_eq!(Token::parse(&boxed.token.to_string()), Some(boxed.token));
            }
            let mut tokens: Vec<String> = found.iter().map(|b| b.token.to_string()).collect();
            tokens.sort();
            tokens.dedup();
            prop_assert_eq!(tokens.len(), found.len());
        }
    }
}
