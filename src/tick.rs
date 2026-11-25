//! A tick: the pure half. A token names a task's box by its line in the note's file and a
//! hash of that line. The renderer writes tokens and the write path finds a token's box, both
//! through `find`, so a token exists only for a box the write path can find.

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

/// A task's box: the 1-based line of the note's file it was rendered from, and that line's
/// hash. On a page it reads `<line>-<hash>`, the hash as 8 hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub line: usize,
    pub hash: u32,
}

impl Token {
    /// The token of the box on line `number`, and the box's character.
    pub fn of(number: usize, line: &[u8]) -> Option<(Token, u8)> {
        let at = find(line)?;
        let hash = hash(line, at);
        Some((Token { line: number, hash }, line[at]))
    }

    pub fn parse(text: &str) -> Option<Token> {
        let (line, hash) = text.split_once('-')?;
        let digits = |s: &str, radix| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
        if !digits(line, 10) || hash.len() != 8 || !digits(hash, 16) {
            return None;
        }
        Some(Token {
            line: line.parse().ok()?,
            hash: u32::from_str_radix(hash, 16).ok()?,
        })
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{:08x}", self.line, self.hash)
    }
}

/// The offset in `text` of the box's character that `token` names. Its own line, when that
/// holds a box and has the hash. Else the line moved: the one line that holds a box and has
/// the hash. `None` when no line does, or several do.
pub fn locate(text: &[u8], token: Token) -> Option<usize> {
    let boxed = |(start, line): (usize, &[u8])| {
        let at = find(line)?;
        (hash(line, at) == token.hash).then_some(start + at)
    };
    let own = token.line.checked_sub(1).and_then(|n| lines(text).nth(n));
    if let Some(at) = own.and_then(boxed) {
        return Some(at);
    }
    let mut found = lines(text).filter_map(boxed);
    let at = found.next()?;
    found.next().is_none().then_some(at)
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
        let token = Token {
            line: 12,
            hash: 0x00ab_cdef,
        };
        assert_eq!(token.to_string(), "12-00abcdef");
        assert_eq!(Token::parse("12-00abcdef"), Some(token));
        for bad in [
            "",
            "12",
            "12-",
            "-00abcdef",
            "12-abcdef",
            "12-00abcdefa",
            "+12-00abcdef",
            "12-00abcdeg",
            "1 2-00abcdef",
            "12-+0abcdef",
            "99999999999999999999999-00abcdef",
        ] {
            assert_eq!(Token::parse(bad), None, "{bad:?}");
        }
    }

    fn token(text: &str, line: usize) -> Token {
        let raw = lines(text.as_bytes()).nth(line - 1).unwrap().1;
        Token::of(line, raw).unwrap().0
    }

    #[test]
    fn a_token_finds_its_line_or_the_one_line_that_moved() {
        let note = "# n\n- [ ] one\n- [ ] two\n";
        let two = token(note, 3);
        assert_eq!(
            locate(note.as_bytes(), two),
            Some(note.find("] two").unwrap() - 1)
        );
        // Lines added above: the line moved, and its text is still the only one.
        let moved = format!("intro\n\n{note}");
        assert_eq!(
            locate(moved.as_bytes(), two),
            Some(moved.find("] two").unwrap() - 1)
        );
        // Ticked since: the same task.
        let ticked = note.replace("- [ ] two", "- [x] two");
        assert_eq!(
            locate(ticked.as_bytes(), two),
            Some(ticked.find("] two").unwrap() - 1)
        );
        // Reworded, or gone.
        assert_eq!(locate(b"# n\n- [ ] one\n- [ ] 2\n", two), None);
        assert_eq!(locate(b"", two), None);
        // No longer a box, though the hash's bytes are there.
        assert_eq!(locate(b"# n\n- [ ] one\n- [ ] two\\\n", two), None);
    }

    #[test]
    fn the_line_number_picks_between_two_lines_of_one_text() {
        let note = "- [ ] same\n- [ ] same\n";
        assert_eq!(token(note, 1).hash, token(note, 2).hash);
        assert_eq!(locate(note.as_bytes(), token(note, 1)), Some(3));
        assert_eq!(locate(note.as_bytes(), token(note, 2)), Some(14));
        // Moved, no line number says which.
        let moved = format!("\n\n\n{note}");
        assert_eq!(locate(moved.as_bytes(), token(note, 1)), None);
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
        /// A plan changes the box of the line it names, by one byte, to the status asked for.
        #[test]
        fn a_plan_changes_only_the_box_it_names(
            picks in prop::collection::vec(prop::sample::select(LINES), 1..12),
            crlf in any::<bool>(),
            pick in any::<prop::sample::Index>(),
            done in any::<bool>(),
        ) {
            let text = picks.join(if crlf { "\r\n" } else { "\n" });
            let number = pick.index(picks.len()) + 1;
            // An empty last line is no line.
            let Some((start, raw)) = lines(text.as_bytes()).nth(number - 1) else {
                return Ok(());
            };
            let Some((token, now)) = Token::of(number, raw) else {
                return Ok(());
            };
            let to = if done { To::Done } else { To::Todo };
            let at = start + find(raw).unwrap();
            match plan(text.as_bytes(), token, to) {
                Ok(Some((offset, byte))) => {
                    prop_assert_eq!(offset, at);
                    prop_assert_ne!(byte, now);
                    prop_assert_eq!(byte, if done { b'x' } else { b' ' });
                }
                Ok(None) => prop_assert_eq!(done, matches!(now, b'x' | b'X')),
                Err(Conflict) => prop_assert!(done && now == b'-'),
            }
        }
    }
}
