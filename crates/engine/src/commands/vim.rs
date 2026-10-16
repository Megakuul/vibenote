//! Vim mode in text boxes (R-VIM-001..004): Normal, Insert, Visual and Visual-line modes with
//! vim's motions, operators, text objects and unnamed register; `jk` leaves Insert; Space is
//! the leader key (Space f w / Space f f open the finders, also on the canvas via `vim.key`).
//!
//! Positions are character offsets into the box's content. In Normal mode the cursor sits on a
//! character (`caret == anchor`); in Visual modes `anchor` is where the selection started.

use super::text::{KeyP, TextEdit, content_of, edited, type_text};
use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `j` then `k` within this long (ms) leaves Insert mode (R-VIM-003).
pub const JK_MS: u64 = 300;
/// Leader keys typed in Insert mode: each within this long (ms) of the previous (R-VIM-004).
pub const INSERT_LEADER_MS: u64 = 400;
/// Leader sequences: each key within this long (ms) of the previous one (R-VIM-004).
pub const LEADER_MS: u64 = 1000;
/// Most keys waiting for a command to complete (a count and an operator fit easily).
const MAX_PENDING: usize = 16;
/// Largest count honoured (`999999x` is not a reason to loop for long).
const MAX_COUNT: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

/// Vim state of the box being edited.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VimState {
    pub mode: VimMode,
    /// Keys of a command not complete yet ("2d", "f", "g").
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pending: String,
    /// A `j` typed in Insert mode: when (ms, if known) and where, for `jk`.
    #[serde(skip)]
    pub(crate) last_j: Option<(Option<u64>, u32)>,
    /// The column `j`/`k` keep to.
    #[serde(skip)]
    pub(crate) col: Option<usize>,
    /// The last `f F t T` and its character (for `;`).
    #[serde(skip)]
    pub(crate) find: Option<(char, char)>,
    /// The note's undo depth when the box's editing began: `u` does not undo past it (nor
    /// the box's creation, nor unrelated changes to the note).
    #[serde(skip)]
    pub(crate) undo_floor: Option<usize>,
    /// The start of a line that got only automatic indent so far: leaving it (Esc, Enter)
    /// removes the indent again, like vim's 'autoindent'.
    #[serde(skip)]
    pub(crate) auto_indent: Option<usize>,
    /// A leader sequence typed in Insert mode: the keys so far (" ", " f"), when the last came
    /// and where the space went (R-VIM-004).
    #[serde(skip)]
    pub(crate) insert_leader: Option<(String, Option<u64>, u32)>,
}

impl VimState {
    pub fn new(mode: VimMode) -> VimState {
        VimState {
            mode,
            pending: String::new(),
            last_j: None,
            col: None,
            find: None,
            undo_floor: None,
            auto_indent: None,
            insert_leader: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Text helpers (all bounds-checked; positions are clamped to the text).

fn line_start(t: &[char], p: usize) -> usize {
    t.get(..p.min(t.len()))
        .and_then(|h| h.iter().rposition(|c| *c == '\n'))
        .map_or(0, |i| i.saturating_add(1))
}

/// The end of the line at `p`: the index of its '\n', or the text length.
fn line_end(t: &[char], p: usize) -> usize {
    let p = p.min(t.len());
    t.get(p..)
        .and_then(|r| r.iter().position(|c| *c == '\n'))
        .map_or(t.len(), |i| p.saturating_add(i))
}

/// The last position the Normal-mode cursor may take on the line of `p`.
fn clamp_normal(t: &[char], p: usize) -> usize {
    let (ls, le) = (line_start(t, p), line_end(t, p));
    p.min(le.saturating_sub(1)).max(ls)
}

fn first_non_blank(t: &[char], p: usize) -> usize {
    let (ls, le) = (line_start(t, p), line_end(t, p));
    (ls..le)
        .find(|&i| t.get(i).is_some_and(|c| *c != ' ' && *c != '\t'))
        .unwrap_or(le)
}

/// Line index of `p` and the start offsets of all lines.
fn lines(t: &[char]) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, c) in t.iter().enumerate() {
        if *c == '\n' {
            v.push(i.saturating_add(1));
        }
    }
    v
}

fn line_of(starts: &[usize], p: usize) -> usize {
    starts.partition_point(|&s| s <= p).saturating_sub(1)
}

/// Character class for word motions: 0 blank, 1 word (letters, digits, `_`), 2 punctuation;
/// for WORDs everything not blank is 1.
fn class(c: Option<char>, big: bool) -> u8 {
    match c {
        None => 0,
        Some(c) if c.is_whitespace() => 0,
        Some(_) if big => 1,
        Some(c) if c.is_alphanumeric() || c == '_' => 1,
        Some(_) => 2,
    }
}

fn word_forward(t: &[char], p: usize, big: bool) -> usize {
    let n = t.len();
    let mut i = p;
    let c = class(t.get(i).copied(), big);
    if c != 0 {
        while i < n && class(t.get(i).copied(), big) == c {
            i = i.saturating_add(1);
        }
    }
    // Blanks and line breaks; an empty line is a word of its own.
    while i < n && class(t.get(i).copied(), big) == 0 {
        if t.get(i) == Some(&'\n') && t.get(i.saturating_add(1)) == Some(&'\n') && i >= p {
            return i.saturating_add(1);
        }
        i = i.saturating_add(1);
    }
    i
}

fn word_end(t: &[char], p: usize, big: bool) -> usize {
    let n = t.len();
    let mut i = p.saturating_add(1);
    while i < n && class(t.get(i).copied(), big) == 0 {
        i = i.saturating_add(1);
    }
    let c = class(t.get(i).copied(), big);
    while i.saturating_add(1) < n && class(t.get(i.saturating_add(1)).copied(), big) == c {
        i = i.saturating_add(1);
    }
    i.min(n.saturating_sub(1))
}

fn word_back(t: &[char], p: usize, big: bool) -> usize {
    let mut i = p;
    while i > 0 && class(t.get(i.saturating_sub(1)).copied(), big) == 0 {
        i = i.saturating_sub(1);
    }
    if i == 0 {
        return 0;
    }
    let c = class(t.get(i.saturating_sub(1)).copied(), big);
    while i > 0 && class(t.get(i.saturating_sub(1)).copied(), big) == c {
        i = i.saturating_sub(1);
    }
    i
}

// ---------------------------------------------------------------------------------------------
// Commands.

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mot {
    Left,
    Right,
    Down,
    Up,
    Word(bool),
    Back(bool),
    End(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    FileStart,
    FileEnd,
    Find(char, char),
    Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Motion(Mot),
    /// `iw`/`aw` (first: around; second: WORD, `iW`/`aW`), `ip`/`ap`.
    Word(bool, bool),
    Para(bool),
    /// The operator doubled: `dd`, `cc`, `yy`, `>>`.
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cmd {
    Move(Mot),
    Op(char, Target),
    /// x X s S D C ~ J p P u
    Simple(char),
    Replace(char),
    Insert(char),
    Visual(bool),
    Redo,
}

enum Parse {
    More,
    Bad,
    Done(usize, Cmd),
}

fn motion_of(c: char) -> Option<Mot> {
    Some(match c {
        'h' => Mot::Left,
        'l' | ' ' => Mot::Right,
        'j' => Mot::Down,
        'k' => Mot::Up,
        'w' => Mot::Word(false),
        'W' => Mot::Word(true),
        'b' => Mot::Back(false),
        'B' => Mot::Back(true),
        'e' => Mot::End(false),
        'E' => Mot::End(true),
        '0' => Mot::LineStart,
        '^' => Mot::FirstNonBlank,
        '$' => Mot::LineEnd,
        'G' => Mot::FileEnd,
        ';' => Mot::Repeat,
        _ => return None,
    })
}

/// Parses pending keys: `[count] (motion | operator [count] target | action)`.
fn parse(keys: &[char]) -> Parse {
    let mut i = 0usize;
    let count = |keys: &[char], i: &mut usize| -> Option<usize> {
        let start = *i;
        while keys
            .get(*i)
            .is_some_and(|c| c.is_ascii_digit() && (*i > start || *c != '0'))
        {
            *i = i.saturating_add(1);
        }
        let s: String = keys.get(start..*i)?.iter().collect();
        if s.is_empty() { None } else { s.parse::<usize>().ok() }
    };
    let n1 = count(keys, &mut i);
    let Some(&c) = keys.get(i) else { return Parse::More };
    i = i.saturating_add(1);
    let n = n1.unwrap_or(1).min(MAX_COUNT);
    // A motion target after `g`, `f`… (shared by motions and operators).
    let target = |keys: &[char], i: usize, c: char| -> Option<Option<(Mot, usize)>> {
        match c {
            'g' => match keys.get(i) {
                None => Some(None),
                Some('g') => Some(Some((Mot::FileStart, i.saturating_add(1)))),
                Some(_) => None,
            },
            'f' | 'F' | 't' | 'T' => match keys.get(i) {
                None => Some(None),
                Some(&ch) => Some(Some((Mot::Find(c, ch), i.saturating_add(1)))),
            },
            _ => motion_of(c).map(|m| Some((m, i))),
        }
    };
    match c {
        'd' | 'c' | 'y' | '>' | '<' => {
            let mut j = i;
            let n2 = count(keys, &mut j);
            let n = n.saturating_mul(n2.unwrap_or(1)).min(MAX_COUNT);
            let Some(&m) = keys.get(j) else { return Parse::More };
            let j = j.saturating_add(1);
            if m == c {
                return Parse::Done(n, Cmd::Op(c, Target::Lines));
            }
            if m == 'i' || m == 'a' {
                return match keys.get(j) {
                    None => Parse::More,
                    Some(&w @ ('w' | 'W')) => Parse::Done(n, Cmd::Op(c, Target::Word(m == 'a', w == 'W'))),
                    Some('p') => Parse::Done(n, Cmd::Op(c, Target::Para(m == 'a'))),
                    Some(_) => Parse::Bad,
                };
            }
            match target(keys, j, m) {
                None => Parse::Bad,
                Some(None) => Parse::More,
                Some(Some((mot, _))) => Parse::Done(n, Cmd::Op(c, Target::Motion(mot))),
            }
        }
        'x' | 'X' | 's' | 'S' | 'D' | 'C' | '~' | 'J' | 'p' | 'P' | 'u' => Parse::Done(n, Cmd::Simple(c)),
        'r' => keys.get(i).map_or(Parse::More, |&ch| Parse::Done(n, Cmd::Replace(ch))),
        'i' | 'a' | 'I' | 'A' | 'o' | 'O' => Parse::Done(n, Cmd::Insert(c)),
        'v' => Parse::Done(n, Cmd::Visual(false)),
        'V' => Parse::Done(n, Cmd::Visual(true)),
        // `G` with a count goes to that line.
        'G' if n1.is_some() => Parse::Done(n, Cmd::Move(Mot::FileStart)),
        _ => match target(keys, i, c) {
            None => Parse::Bad,
            Some(None) => Parse::More,
            Some(Some((mot, _))) => Parse::Done(n, Cmd::Move(mot)),
        },
    }
}

/// Where motion `m` goes from `p` (`count` times), how operators treat it, and whether it
/// succeeded (a failed motion leaves the cursor and aborts an operator, like vim). `op`: an
/// operator uses it (`l` and `$` may reach the line end; `w` stops at the line end).
fn motion(t: &[char], st: &mut VimState, p: usize, m: Mot, count: usize, op: bool) -> (usize, Kind, bool) {
    let n = t.len();
    let starts = lines(t);
    let mut q = p.min(n);
    let col = |q: usize| q.saturating_sub(line_start(t, q));
    match m {
        Mot::Down | Mot::Up => {
            let c = st.col.unwrap_or_else(|| col(q));
            let li = line_of(&starts, q);
            let last = starts.len().saturating_sub(1);
            let to = if m == Mot::Down {
                li.saturating_add(count).min(last)
            } else {
                li.saturating_sub(count)
            };
            let ls = starts.get(to).copied().unwrap_or(0);
            let target = ls.saturating_add(c).min(line_end(t, ls));
            st.col = Some(c);
            let q = if op { target } else { clamp_normal(t, target) };
            return (q, Kind::Linewise, to != li);
        }
        Mot::FileStart | Mot::FileEnd => {
            // Neovim's 'nostartofline': the cursor keeps its column.
            let c = st.col.unwrap_or_else(|| col(q));
            let li = if m == Mot::FileEnd {
                starts.len().saturating_sub(1)
            } else {
                count.saturating_sub(1)
            };
            let ls = starts.get(li.min(starts.len().saturating_sub(1))).copied().unwrap_or(0);
            st.col = Some(c);
            return (ls.saturating_add(c).min(line_end(t, ls)), Kind::Linewise, true);
        }
        _ => {}
    }
    // `$` with a count goes down `count - 1` lines first; later `j`/`k` stay at line ends.
    if m == Mot::LineEnd {
        let li = line_of(&starts, q);
        let lastl = starts.len().saturating_sub(1);
        if count > 1 && li >= lastl {
            return (p, Kind::Inclusive, false);
        }
        let to = li.saturating_add(count.saturating_sub(1)).min(lastl);
        let ls = starts.get(to).copied().unwrap_or(0);
        let le = line_end(t, ls);
        st.col = Some(usize::MAX);
        let q = if op {
            le.saturating_sub(1).max(ls)
        } else {
            clamp_normal(t, le)
        };
        return (q, Kind::Inclusive, true);
    }
    st.col = None;
    let find = match m {
        Mot::Find(f, ch) => Some((f, ch)),
        Mot::Repeat => st.find,
        _ => None,
    };
    if find.is_some() {
        st.find = find;
    }
    let mut kind = Kind::Exclusive;
    let mut ok = true;
    let last = count.max(1).saturating_sub(1);
    for i in 0..count.max(1) {
        let (ls, le) = (line_start(t, q), line_end(t, q));
        q = match m {
            Mot::Left => q.saturating_sub(1).max(ls),
            Mot::Right => {
                let max = if op { le } else { le.saturating_sub(1).max(ls) };
                q.saturating_add(1).min(max)
            }
            Mot::Word(big) => {
                let w = word_forward(t, q, big);
                // An operator's last `w` stops at the end of the line.
                if op && i == last && w > le { le.max(q) } else { w }
            }
            Mot::Back(big) => word_back(t, q, big),
            Mot::End(big) => {
                kind = Kind::Inclusive;
                word_end(t, q, big)
            }
            Mot::LineStart => ls,
            Mot::FirstNonBlank => first_non_blank(t, q),
            Mot::LineEnd => {
                kind = Kind::Inclusive;
                le.saturating_sub(1).max(ls)
            }
            Mot::Find(..) | Mot::Repeat => {
                let Some((f, ch)) = find else {
                    ok = false;
                    break;
                };
                // `;` after `t`/`T` skips the character right next to the cursor (vim's cpo-;).
                let skip = usize::from(m == Mot::Repeat && matches!(f, 't' | 'T'));
                let found = match f {
                    'f' | 't' => (q.saturating_add(1).saturating_add(skip)..le)
                        .find(|&i| t.get(i) == Some(&ch))
                        .map(|i| if f == 't' { i.saturating_sub(1) } else { i }),
                    _ => (ls..q.saturating_sub(skip))
                        .rev()
                        .find(|&i| t.get(i) == Some(&ch))
                        .map(|i| if f == 'T' { i.saturating_add(1) } else { i }),
                };
                if matches!(f, 'f' | 't') {
                    kind = Kind::Inclusive;
                }
                let Some(found) = found else {
                    ok = false;
                    break;
                };
                found
            }
            _ => q,
        };
    }
    // Motions that cannot move fail (`h` at the line start, `b` at the top, `e` at the end…).
    // (`h`/`l` under an operator just give an empty range.)
    let moves = !matches!(
        m,
        Mot::LineStart | Mot::FirstNonBlank | Mot::LineEnd | Mot::Find(..) | Mot::Repeat
    ) && !(op && matches!(m, Mot::Left | Mot::Right));
    if !ok || (moves && q == p) {
        return (p, kind, false);
    }
    (q, kind, true)
}

/// `cw`: to the end of the word, staying on a word's last character (vim's special case).
fn change_word_end(t: &[char], p: usize, big: bool, count: usize) -> usize {
    let mut q = p;
    for i in 0..count.max(1) {
        let here = class(t.get(q).copied(), big);
        let next = class(t.get(q.saturating_add(1)).copied(), big);
        q = if i == 0 && here != next { q } else { word_end(t, q, big) };
    }
    q
}

/// The range an operator `op` acts on: `[a, b)` and whether it is whole lines; `None` when its
/// motion fails (the operator does nothing).
fn range_of(
    t: &[char],
    st: &mut VimState,
    p: usize,
    op: char,
    target: Target,
    count: usize,
) -> Option<(usize, usize, bool)> {
    let n = t.len();
    Some(match target {
        Target::Lines => {
            let starts = lines(t);
            let li = line_of(&starts, p);
            // `2dd` on the last line fails (vim moves down `count - 1` lines first).
            if count > 1 && li.saturating_add(1) >= starts.len() {
                return None;
            }
            let last = li
                .saturating_add(count.saturating_sub(1))
                .min(starts.len().saturating_sub(1));
            let a = starts.get(li).copied().unwrap_or(0);
            let b = line_end(t, starts.get(last).copied().unwrap_or(a));
            (a, b, true)
        }
        Target::Word(around, big) if count > 1 => {
            // Each further count takes the next word (or blank run) too.
            let (a, mut b, _) = range_of(t, st, p, op, Target::Word(around, big), 1)?;
            for _ in 1..count {
                if b >= n || t.get(b) == Some(&'\n') {
                    break;
                }
                let (_, b2, _) = range_of(t, st, b, op, Target::Word(around, big), 1)?;
                b = b2.max(b.saturating_add(1));
            }
            (a, b, false)
        }
        // `aw` on an empty line: its line break and the next line's first word.
        Target::Word(true, big) if line_start(t, p) == line_end(t, p) => {
            if p >= n {
                return None;
            }
            let mut b = p.saturating_add(1);
            while t.get(b).is_some_and(|c| *c == ' ' || *c == '\t') {
                b = b.saturating_add(1);
            }
            let k = class(t.get(b).copied(), big);
            while b < n && k != 0 && class(t.get(b).copied(), big) == k {
                b = b.saturating_add(1);
            }
            (p, b, false)
        }
        Target::Word(around, big) => {
            let c = class(t.get(p).copied(), big);
            let (mut a, mut b) = (p, p);
            while a > 0
                && class(t.get(a.saturating_sub(1)).copied(), big) == c
                && t.get(a.saturating_sub(1)) != Some(&'\n')
            {
                a = a.saturating_sub(1);
            }
            while b < n && class(t.get(b).copied(), big) == c && t.get(b) != Some(&'\n') {
                b = b.saturating_add(1);
            }
            if around {
                let blank = |i: usize| t.get(i).is_some_and(|c| *c == ' ' || *c == '\t');
                if c == 0 {
                    // `aw` on blanks: the blanks and the word after them.
                    let k = class(t.get(b).copied(), big);
                    while b < n && k != 0 && class(t.get(b).copied(), big) == k {
                        b = b.saturating_add(1);
                    }
                } else if blank(b) {
                    while blank(b) {
                        b = b.saturating_add(1);
                    }
                } else {
                    while a > 0 && blank(a.saturating_sub(1)) {
                        a = a.saturating_sub(1);
                    }
                }
            }
            (a, b, false)
        }
        Target::Para(around) => {
            let starts = lines(t);
            let empty = |li: usize| {
                let ls = starts.get(li).copied().unwrap_or(0);
                line_end(t, ls) == ls
            };
            let li = line_of(&starts, p);
            let (mut l0, mut l1) = (li, li);
            let e = empty(li);
            while l0 > 0 && empty(l0.saturating_sub(1)) == e {
                l0 = l0.saturating_sub(1);
            }
            while l1.saturating_add(1) < starts.len() && empty(l1.saturating_add(1)) == e {
                l1 = l1.saturating_add(1);
            }
            // Each count is one more block (`ap`: paragraph and the blank lines after it);
            // too few blocks fail.
            let blocks = if around { count.saturating_mul(2) } else { count };
            for _ in 1..blocks {
                if l1.saturating_add(1) >= starts.len() {
                    return None;
                }
                let k = empty(l1.saturating_add(1));
                while l1.saturating_add(1) < starts.len() && empty(l1.saturating_add(1)) == k {
                    l1 = l1.saturating_add(1);
                }
            }
            let a = starts.get(l0).copied().unwrap_or(0);
            let b = line_end(t, starts.get(l1).copied().unwrap_or(a));
            (a, b, true)
        }
        // `cw` on a word changes to its end (not the blank after it); on blanks it is `dw`.
        // `cw` on an empty line just starts typing there.
        Target::Motion(Mot::Word(_)) if op == 'c' && line_start(t, p) == line_end(t, p) => (p, p, false),
        Target::Motion(Mot::Word(big)) if op == 'c' && class(t.get(p).copied(), big) != 0 => {
            let q = change_word_end(t, p, big, count);
            (p, q.saturating_add(1).min(n), false)
        }
        Target::Motion(m) => {
            let (q, kind, ok) = motion(t, st, p, m, count, true);
            if !ok {
                return None;
            }
            match kind {
                Kind::Linewise => {
                    let (x, y) = (p.min(q), p.max(q));
                    (line_start(t, x), line_end(t, y), true)
                }
                Kind::Inclusive => (p.min(q), p.max(q).saturating_add(1).min(n), false),
                Kind::Exclusive => (p.min(q), p.max(q), false),
            }
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Editing through the text box (undo steps, formatting following the text).

/// Replaces `[a, b)` by `ins` (one undo step unless `typing`).
fn replace(
    s: &mut Session,
    mut ed: TextEdit,
    a: usize,
    b: usize,
    ins: &str,
    typing: bool,
) -> Result<TextEdit, EngineError> {
    let to = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    if !typing {
        s.coalesce = None;
    }
    if a == b && ins.is_empty() {
        return Ok(ed);
    }
    ed.anchor = to(a);
    ed.caret = to(b);
    let ed = type_text(s, ed, ins)?;
    if !typing {
        s.coalesce = None;
    }
    Ok(ed)
}

fn chars(s: &Session, ed: &TextEdit) -> Vec<char> {
    content_of(edited(s, ed).as_ref()).chars().collect()
}

fn set_cursor(ed: &mut TextEdit, p: usize) {
    ed.caret = u32::try_from(p).unwrap_or(u32::MAX);
    ed.anchor = ed.caret;
}

fn vim_mut(ed: &mut TextEdit) -> &mut VimState {
    ed.vim.get_or_insert_with(|| VimState::new(VimMode::Normal))
}

/// Text of `[a, b)` for the register; whole lines end with a line break.
fn yank(s: &mut Session, t: &[char], a: usize, b: usize, linewise: bool) {
    let mut txt: String = t.get(a..b.min(t.len())).unwrap_or(&[]).iter().collect();
    if linewise && !txt.ends_with('\n') {
        txt.push('\n');
    }
    s.register = Some((txt, linewise));
}

/// Deletes (or changes) `[a, b)`; whole lines take their line break with them (`change` keeps
/// one empty line to type into).
fn delete(
    s: &mut Session,
    ed: TextEdit,
    t: &[char],
    (a, b, linewise): (usize, usize, bool),
    change: bool,
) -> Result<TextEdit, EngineError> {
    yank(s, t, a, b, linewise);
    if !linewise || change {
        let mut ed = replace(s, ed, a, b, "", !change)?;
        set_cursor(&mut ed, a);
        return Ok(ed);
    }
    let n = t.len();
    let (x, y) = if b < n {
        (a, b.saturating_add(1)) // the lines and their final break
    } else if a > 0 {
        (a.saturating_sub(1), b) // the last line: the break before it
    } else {
        (a, b)
    };
    let mut ed = replace(s, ed, x, y, "", false)?;
    let t2 = chars(s, &ed);
    let at = if b < n {
        a
    } else {
        line_start(&t2, x.min(t2.len().saturating_sub(1)))
    };
    set_cursor(&mut ed, first_non_blank(&t2, at.min(t2.len())));
    Ok(ed)
}

/// The typing that follows a change (`c`, `s`, `S`, `C`) joins its undo step: one `u` undoes both.
fn continue_step(s: &mut Session, ed: TextEdit) -> TextEdit {
    s.coalesce = ed.id.map(|id| (ed.note, id));
    ed
}

fn to_insert(s: &mut Session, mut ed: TextEdit, p: usize) -> TextEdit {
    s.coalesce = None;
    set_cursor(&mut ed, p);
    let v = vim_mut(&mut ed);
    v.mode = VimMode::Insert;
    v.last_j = None;
    ed
}

fn to_normal(s: &mut Session, mut ed: TextEdit, t: &[char]) -> TextEdit {
    s.coalesce = None;
    let p = usize::try_from(ed.caret).unwrap_or(usize::MAX);
    let was_insert = ed.vim.as_ref().is_some_and(|v| v.mode == VimMode::Insert);
    // Leaving Insert, the cursor steps back onto the last typed character.
    let p = if was_insert && p > line_start(t, p) {
        p.saturating_sub(1)
    } else {
        p
    };
    set_cursor(&mut ed, clamp_normal(t, p));
    let v = vim_mut(&mut ed);
    v.mode = VimMode::Normal;
    v.pending.clear();
    v.last_j = None;
    ed
}

/// The cursor after undo/redo: where the text first differs.
fn changed_at(before: &[char], after: &[char]) -> usize {
    before
        .iter()
        .zip(after)
        .position(|(x, y)| x != y)
        .unwrap_or(before.len().min(after.len()))
}

/// The note's undo depth.
fn undo_depth(s: &mut Session, note: vibenote_model::NoteId) -> usize {
    let key = Scope::Note(note).key();
    s.ensure_loaded(&key);
    s.history.get(&key).map_or(0, |h| h.undo.len())
}

fn undo(s: &mut Session, ed: TextEdit, redo: bool) -> Result<Option<TextEdit>, EngineError> {
    let floor = ed.vim.as_ref().and_then(|v| v.undo_floor).unwrap_or(0);
    if !redo && undo_depth(s, ed.note) <= floor {
        return Ok(Some(ed)); // nothing of this editing to undo
    }
    let before = chars(s, &ed);
    let scope = Scope::Note(ed.note);
    let r = if redo { s.redo(scope) } else { s.undo(scope) };
    if let Err(e) = r {
        if matches!(e, EngineError::Model(_)) {
            return Ok(Some(ed)); // nothing to undo
        }
        return Err(e);
    }
    if ed.id.is_some() && edited(s, &ed).is_none() {
        return Ok(None); // the box itself was undone
    }
    let after = chars(s, &ed);
    let mut ed = ed;
    let mut at = changed_at(&before, &after);
    // Undoing inserted lines: the cursor goes to the line above them.
    if after.len() < before.len() && at > 0 && after.get(at.saturating_sub(1)) == Some(&'\n') {
        at = line_start(&after, at.saturating_sub(1));
    }
    set_cursor(&mut ed, clamp_normal(&after, at));
    Ok(Some(ed))
}

fn toggle_case(c: char) -> String {
    if c.is_uppercase() {
        c.to_lowercase().collect()
    } else {
        c.to_uppercase().collect()
    }
}

/// Runs a complete Normal-mode command.
fn run(s: &mut Session, mut ed: TextEdit, count: usize, cmd: Cmd) -> Result<Option<TextEdit>, EngineError> {
    let t = chars(s, &ed);
    let n = t.len();
    let p = usize::try_from(ed.caret).unwrap_or(usize::MAX).min(n);
    let (ls, le) = (line_start(&t, p), line_end(&t, p));
    let mut st = ed.vim.clone().unwrap_or_else(|| VimState::new(VimMode::Normal));
    let visual = matches!(st.mode, VimMode::Visual | VimMode::VisualLine);
    // The column linewise commands keep (Neovim's 'nostartofline'): `j`/`k`'s or the cursor's.
    let want = st.col.unwrap_or(p.saturating_sub(ls));
    if !matches!(
        cmd,
        Cmd::Move(Mot::Up | Mot::Down | Mot::FileStart | Mot::FileEnd | Mot::LineEnd)
    ) {
        st.col = None;
    }
    let out = match cmd {
        Cmd::Move(m) => {
            let (q, _, _) = motion(&t, &mut st, p, m, count, false);
            let q = if visual {
                q.min(n.saturating_sub(1))
            } else {
                clamp_normal(&t, q)
            };
            ed.vim = Some(st);
            ed.caret = u32::try_from(q).unwrap_or(u32::MAX);
            if !visual {
                ed.anchor = ed.caret;
            }
            return Ok(Some(ed));
        }
        Cmd::Op(op, target) if visual => {
            let _ = target;
            let (a, b, linewise) = visual_range(&t, &ed);
            ed.vim = Some(st);
            apply_op(s, ed, &t, op, (a, b, linewise), want)?
        }
        Cmd::Op(op, target) => {
            let Some(r) = range_of(&t, &mut st, p, op, target, count) else {
                ed.vim = Some(st);
                return Ok(Some(ed));
            };
            // The motion's column (`$`: line ends; `j`: the kept column).
            let want = st.col.unwrap_or(want);
            st.col = None;
            ed.vim = Some(st);
            apply_op(s, ed, &t, op, r, want)?
        }
        Cmd::Simple(c) if visual && matches!(c, 'x' | 'X' | 'D' | 's' | 'S' | 'C' | '~' | 'J') => {
            let (a, b, linewise) = visual_range(&t, &ed);
            ed.vim = Some(st);
            match c {
                // Joins the selected lines (at least two).
                'J' => {
                    let starts = lines(&t);
                    let (l0, l1) = (line_of(&starts, a), line_of(&starts, b.saturating_sub(1).max(a)));
                    let ed = join(s, ed, &t, a, l1.saturating_sub(l0).saturating_add(1).max(2))?;
                    to_normal_keep(ed)
                }
                '~' => {
                    let flipped: String = t.get(a..b).unwrap_or(&[]).iter().map(|c| toggle_case(*c)).collect();
                    let mut ed = replace(s, ed, a, b, &flipped, false)?;
                    set_cursor(&mut ed, a);
                    to_normal_keep(ed)
                }
                's' | 'S' | 'C' => apply_op(s, ed, &t, 'c', (a, b, linewise || c != 's'), want)?,
                _ => apply_op(s, ed, &t, 'd', (a, b, linewise || matches!(c, 'X' | 'D')), want)?,
            }
        }
        Cmd::Simple(c) => {
            ed.vim = Some(st);
            match c {
                'x' if le > ls => delete(s, ed, &t, (p, p.saturating_add(count).min(le), false), false)?,
                'X' if p > ls => {
                    let mut ed = delete(s, ed, &t, (p.saturating_sub(count).max(ls), p, false), false)?;
                    let t2 = chars(s, &ed);
                    let q = usize::try_from(ed.caret).unwrap_or(0);
                    set_cursor(&mut ed, clamp_normal(&t2, q));
                    ed
                }
                's' => {
                    let ed = delete(s, ed, &t, (p, p.saturating_add(count).min(le), false), true)?;
                    let ed = to_insert(s, ed, p);
                    return Ok(Some(continue_step(s, ed)));
                }
                'S' => {
                    let r = range_of(&t, &mut VimState::new(VimMode::Normal), p, 'c', Target::Lines, count)
                        .unwrap_or((ls, le, true));
                    apply_op(s, ed, &t, 'c', r, want)?
                }
                // To the end of the line, and `count - 1` more lines.
                'D' | 'C' => {
                    let starts = lines(&t);
                    let li = line_of(&starts, p).saturating_add(count.saturating_sub(1));
                    if li >= starts.len() && count > 1 && line_of(&starts, p).saturating_add(1) >= starts.len() {
                        return Ok(Some(ed)); // `2D` on the last line fails
                    }
                    let last = starts.get(li.min(starts.len().saturating_sub(1))).copied().unwrap_or(p);
                    let b = line_end(&t, last);
                    let op = if c == 'D' { 'd' } else { 'c' };
                    apply_op(s, ed, &t, op, (p, b, false), want)?
                }
                '~' if le > ls => {
                    let b = p.saturating_add(count).min(le);
                    let flipped: String = t.get(p..b).unwrap_or(&[]).iter().map(|c| toggle_case(*c)).collect();
                    let mut ed = replace(s, ed, p, b, &flipped, false)?;
                    set_cursor(&mut ed, clamp_normal(&t, b));
                    ed
                }
                'J' => join(s, ed, &t, p, count.max(2))?,
                'p' | 'P' => put(s, ed, &t, p, c == 'P', count)?,
                'u' => return undo(s, ed, false),
                _ => ed,
            }
        }
        Cmd::Redo => {
            ed.vim = Some(st);
            return undo(s, ed, true);
        }
        Cmd::Replace(ch) => {
            ed.vim = Some(st);
            let b = p.saturating_add(count);
            if le > ls && b <= le && ch != '\n' {
                let with: String = std::iter::repeat_n(ch, count).collect();
                let mut ed = replace(s, ed, p, b, &with, false)?;
                set_cursor(&mut ed, b.saturating_sub(1));
                ed
            } else {
                ed
            }
        }
        Cmd::Insert(c) => {
            ed.vim = Some(st);
            match c {
                'i' => to_insert(s, ed, p),
                'a' => to_insert(s, ed, if le > ls { p.saturating_add(1).min(le) } else { p }),
                'I' => to_insert(s, ed, first_non_blank(&t, p)),
                'A' => to_insert(s, ed, le),
                // A new line with the current line's indent ('autoindent').
                'o' => {
                    let ind = indent_of(&t, p);
                    let k = ind.chars().count();
                    let ed = to_insert(s, ed, le);
                    let mut ed = replace(s, ed, le, le, &format!("\n{ind}"), true)?;
                    set_cursor(&mut ed, le.saturating_add(1).saturating_add(k));
                    if k > 0 {
                        vim_mut(&mut ed).auto_indent = Some(le.saturating_add(1));
                    }
                    ed
                }
                _ => {
                    let ind = indent_of(&t, p);
                    let k = ind.chars().count();
                    let ed = to_insert(s, ed, ls);
                    let mut ed = replace(s, ed, ls, ls, &format!("{ind}\n"), true)?;
                    set_cursor(&mut ed, ls.saturating_add(k));
                    if k > 0 {
                        vim_mut(&mut ed).auto_indent = Some(ls);
                    }
                    ed
                }
            }
        }
        Cmd::Visual(line) => {
            let want = if line { VimMode::VisualLine } else { VimMode::Visual };
            if st.mode == want {
                st.mode = VimMode::Normal;
                ed.vim = Some(st);
                set_cursor(&mut ed, clamp_normal(&t, p));
            } else {
                if !visual {
                    ed.anchor = ed.caret;
                }
                st.mode = want;
                ed.vim = Some(st);
            }
            return Ok(Some(ed));
        }
    };
    Ok(Some(out))
}

/// The Visual selection as `[a, b)` and whether it is whole lines.
fn visual_range(t: &[char], ed: &TextEdit) -> (usize, usize, bool) {
    let (x, y) = (
        usize::try_from(ed.anchor.min(ed.caret)).unwrap_or(0),
        usize::try_from(ed.anchor.max(ed.caret)).unwrap_or(0),
    );
    if ed.vim.as_ref().is_some_and(|v| v.mode == VimMode::VisualLine) {
        (line_start(t, x), line_end(t, y), true)
    } else if ed.vim.as_ref().is_some_and(|v| v.col == Some(usize::MAX)) && ed.caret >= ed.anchor {
        // `v$`: up to and including the line break.
        (x.min(t.len()), line_end(t, y).saturating_add(1).min(t.len()), false)
    } else {
        (x.min(t.len()), y.saturating_add(1).min(t.len()), false)
    }
}

/// The position in the line at `p` closest to column `col` (Normal mode).
fn at_col(t: &[char], p: usize, col: usize) -> usize {
    let ls = line_start(t, p);
    clamp_normal(t, ls.saturating_add(col).min(line_end(t, ls)))
}

/// Applies operator `op` to a range; `want` is the column linewise operators keep.
fn apply_op(
    s: &mut Session,
    ed: TextEdit,
    t: &[char],
    op: char,
    (a, b, linewise): (usize, usize, bool),
    want: usize,
) -> Result<TextEdit, EngineError> {
    let mut ed = ed;
    match op {
        'y' if visual_op(&ed) => {
            yank(s, t, a, b, linewise);
            let start = usize::try_from(ed.anchor.min(ed.caret)).unwrap_or(0);
            set_cursor(&mut ed, clamp_normal(t, start));
            Ok(to_normal_keep(ed))
        }
        'y' => {
            yank(s, t, a, b, linewise);
            // The cursor goes to the start of the yanked text (whole lines: its first line, the
            // same column).
            let at = if linewise {
                at_col(t, a, want)
            } else {
                clamp_normal(t, a)
            };
            set_cursor(&mut ed, at);
            Ok(to_normal_keep(ed))
        }
        'd' => {
            // A multi-line delete from within the indent to a line end takes whole lines.
            let multi = t.get(a..b).is_some_and(|r| r.contains(&'\n'));
            let to_end = t
                .get(b..line_end(t, b))
                .is_some_and(|r| r.iter().all(|c| *c == ' ' || *c == '\t'));
            let linewise = linewise || (multi && to_end && a <= first_non_blank(t, a) && !visual_op(&ed));
            let (a, b) = if linewise {
                (line_start(t, a), line_end(t, b))
            } else {
                (a, b)
            };
            let ed = delete(s, ed, t, (a, b, linewise), false)?;
            let t2 = chars(s, &ed);
            let mut ed = to_normal_keep(ed);
            let q = usize::try_from(ed.caret).unwrap_or(0);
            let q = if linewise {
                at_col(&t2, q, want)
            } else {
                clamp_normal(&t2, q)
            };
            set_cursor(&mut ed, q);
            Ok(ed)
        }
        // Changing whole lines keeps the first one's indent ('autoindent').
        'c' if linewise => {
            let ind = indent_of(t, a);
            let k = ind.chars().count();
            let ed = delete(s, ed, t, (a, b, linewise), true)?;
            let ed = to_insert(s, ed, a);
            let mut ed = continue_step(s, ed);
            if k > 0 {
                ed = replace(s, ed, a, a, &ind, true)?;
                set_cursor(&mut ed, a.saturating_add(k));
                vim_mut(&mut ed).auto_indent = Some(a);
            }
            Ok(ed)
        }
        'c' => {
            let ed = delete(s, ed, t, (a, b, linewise), true)?;
            let ed = to_insert(s, ed, a);
            Ok(continue_step(s, ed))
        }
        '>' | '<' => {
            let starts = lines(t);
            let (l0, l1) = (line_of(&starts, a), line_of(&starts, b.saturating_sub(1).max(a)));
            // Bottom-up, so earlier offsets stay valid.
            for li in (l0..=l1).rev() {
                let ls = starts.get(li).copied().unwrap_or(0);
                if op == '>' {
                    if line_end(t, ls) > ls {
                        ed = replace(s, ed, ls, ls, &" ".repeat(super::text::INDENT), true)?;
                    }
                } else {
                    let k = (0..super::text::INDENT)
                        .take_while(|&i| t.get(ls.saturating_add(i)) == Some(&' '))
                        .count();
                    ed = replace(s, ed, ls, ls.saturating_add(k), "", true)?;
                }
            }
            s.coalesce = None;
            let t2 = chars(s, &ed);
            let first = starts.get(l0).copied().unwrap_or(0);
            set_cursor(&mut ed, at_col(&t2, first.min(t2.len()), want));
            Ok(to_normal_keep(ed))
        }
        _ => Ok(ed),
    }
}

/// True while the operator acts on a Visual selection.
fn visual_op(ed: &TextEdit) -> bool {
    ed.vim
        .as_ref()
        .is_some_and(|v| matches!(v.mode, VimMode::Visual | VimMode::VisualLine))
}

/// Back to Normal mode without moving the cursor (after an operator).
fn to_normal_keep(mut ed: TextEdit) -> TextEdit {
    let v = vim_mut(&mut ed);
    v.mode = VimMode::Normal;
    v.pending.clear();
    ed.anchor = ed.caret;
    ed
}

fn join(s: &mut Session, mut ed: TextEdit, t: &[char], p: usize, lines_n: usize) -> Result<TextEdit, EngineError> {
    let mut t = t.to_vec();
    let mut at = p;
    for _ in 1..lines_n {
        let le = line_end(&t, at);
        if le >= t.len() {
            break;
        }
        let mut k = le.saturating_add(1);
        while t.get(k).is_some_and(|c| *c == ' ' || *c == '\t') {
            k = k.saturating_add(1);
        }
        // No space after an empty line or one ending in a blank, before an empty line or `)`.
        let empty = le == line_start(&t, le);
        let sep =
            if empty || t.get(k).is_none_or(|c| *c == '\n' || *c == ')') || t.get(le.saturating_sub(1)) == Some(&' ') {
                ""
            } else {
                " "
            };
        ed = replace(s, ed, le, k, sep, true)?;
        t = chars(s, &ed);
        at = le;
        set_cursor(&mut ed, le);
    }
    s.coalesce = None;
    Ok(ed)
}

fn put(
    s: &mut Session,
    ed: TextEdit,
    t: &[char],
    p: usize,
    before: bool,
    count: usize,
) -> Result<TextEdit, EngineError> {
    let Some((txt, linewise)) = s.register.clone() else {
        return Ok(ed);
    };
    let txt = txt.repeat(count.clamp(1, 1000));
    if linewise {
        let (ls, le) = (line_start(t, p), line_end(t, p));
        let body = txt.strip_suffix('\n').unwrap_or(&txt);
        let (at, ins, start) = if before {
            (ls, format!("{body}\n"), ls)
        } else {
            (le, format!("\n{body}"), le.saturating_add(1))
        };
        let mut ed = replace(s, ed, at, at, &ins, false)?;
        let t2 = chars(s, &ed);
        set_cursor(&mut ed, first_non_blank(&t2, start.min(t2.len())));
        return Ok(ed);
    }
    let (ls, le) = (line_start(t, p), line_end(t, p));
    let at = if before || le == ls {
        p
    } else {
        p.saturating_add(1).min(le)
    };
    let k = txt.chars().count();
    let mut ed = replace(s, ed, at, at, &txt, false)?;
    set_cursor(&mut ed, at.saturating_add(k).saturating_sub(1));
    Ok(ed)
}

// ---------------------------------------------------------------------------------------------
// Input.

/// A click or drag in the box (R-TEXT-005, R-VIM-001): `[a, c)` is the field-like selection;
/// an empty one puts the Normal-mode cursor on the clicked character, otherwise Visual mode
/// covers it (`line`: Visual Line mode).
pub(crate) fn click(ed: &mut TextEdit, content: &str, a: u32, c: u32, line: bool) {
    let t: Vec<char> = content.chars().collect();
    let (a, c) = (usize::try_from(a).unwrap_or(0), usize::try_from(c).unwrap_or(0));
    let v = vim_mut(ed);
    v.pending.clear();
    v.col = None;
    if line {
        v.mode = VimMode::VisualLine;
        set_cursor(ed, a.min(t.len()));
    } else if a == c {
        if v.mode != VimMode::Insert {
            v.mode = VimMode::Normal;
            set_cursor(ed, clamp_normal(&t, c));
        } else {
            set_cursor(ed, c.min(t.len()));
        }
    } else {
        v.mode = VimMode::Visual;
        // Visual selections include the cursor's character.
        let (anchor, caret) = if c > a {
            (a, c.saturating_sub(1))
        } else {
            (a.saturating_sub(1), c)
        };
        ed.anchor = u32::try_from(anchor).unwrap_or(u32::MAX);
        ed.caret = u32::try_from(caret).unwrap_or(u32::MAX);
    }
}

/// A leader key (`" "`, `"f"`, `"w"`) at `t_ms`: true if it belongs to a leader sequence. Space
/// starts one; Space f w opens the text finder, Space f f the note finder (R-VIM-004).
pub(crate) fn leader_key(s: &mut Session, key: &str, t_ms: Option<u64>) -> bool {
    if !s.settings.vim {
        s.leader = None;
        return false;
    }
    if key == " " || key == "space" {
        s.leader = Some((" ".into(), t_ms));
        return true;
    }
    let Some((seq, last)) = s.leader.take() else {
        return false;
    };
    let late = matches!((last, t_ms), (Some(a), Some(b)) if b.saturating_sub(a) > LEADER_MS);
    if late {
        return false;
    }
    match (seq.as_str(), key) {
        (" ", "f") => {
            s.leader = Some((" f".into(), t_ms));
            true
        }
        (" f", "w") => {
            s.ui.panel = Some("finder_text");
            true
        }
        (" f", "f") => {
            s.ui.panel = Some("finder_notes");
            true
        }
        _ => false,
    }
}

/// The leading blanks of the line at `p`.
fn indent_of(t: &[char], p: usize) -> String {
    let ls = line_start(t, p);
    t.get(ls..line_end(t, p))
        .unwrap_or(&[])
        .iter()
        .take_while(|c| **c == ' ' || **c == '\t')
        .collect()
}

/// Removes the indent of a line that only got automatic indent (vim's 'autoindent').
fn drop_auto_indent(s: &mut Session, mut ed: TextEdit) -> Result<TextEdit, EngineError> {
    let Some(ls) = ed.vim.as_mut().and_then(|v| v.auto_indent.take()) else {
        return Ok(ed);
    };
    let t = chars(s, &ed);
    let le = line_end(&t, ls);
    let caret = usize::try_from(ed.caret).unwrap_or(0);
    let blank = t.get(ls..le).is_some_and(|r| r.iter().all(|c| *c == ' ' || *c == '\t'));
    if line_start(&t, caret) != ls || !blank || le == ls {
        return Ok(ed);
    }
    let mut ed = replace(s, ed, ls, le, "", true)?;
    set_cursor(&mut ed, ls);
    Ok(ed)
}

/// A key in Insert mode: Esc leaves it; Enter keeps the line's indent; Backspace in the
/// indent goes back one indent level; Tab indents (R-VIM-002).
fn insert_key(s: &mut Session, ed: TextEdit, k: KeyP, shift: bool) -> Result<Option<TextEdit>, EngineError> {
    use super::text::INDENT;
    match k {
        KeyP::Escape => {
            let ed = drop_auto_indent(s, ed)?;
            let t = chars(s, &ed);
            Ok(Some(to_normal(s, ed, &t)))
        }
        KeyP::Enter if ed.caret == ed.anchor => {
            // The new line gets the indent before the caret (also when that line keeps none);
            // blanks after the caret go.
            let t0 = chars(s, &ed);
            let p0 = usize::try_from(ed.caret).unwrap_or(0).min(t0.len());
            let ind: String = t0
                .get(line_start(&t0, p0)..p0)
                .unwrap_or(&[])
                .iter()
                .take_while(|c| **c == ' ' || **c == '\t')
                .collect();
            let ed = drop_auto_indent(s, ed)?;
            let t = chars(s, &ed);
            let p = usize::try_from(ed.caret).unwrap_or(0).min(t.len());
            let gap = t
                .get(p..line_end(&t, p))
                .unwrap_or(&[])
                .iter()
                .take_while(|c| **c == ' ' || **c == '\t')
                .count();
            let mut ed = replace(s, ed, p, p.saturating_add(gap), &format!("\n{ind}"), true)?;
            set_cursor(&mut ed, p.saturating_add(1).saturating_add(ind.chars().count()));
            if !ind.is_empty() {
                vim_mut(&mut ed).auto_indent = Some(p.saturating_add(1));
            }
            Ok(Some(ed))
        }
        KeyP::Backspace if ed.caret == ed.anchor => {
            let t = chars(s, &ed);
            let p = usize::try_from(ed.caret).unwrap_or(0).min(t.len());
            let ls = line_start(&t, p);
            // Spaces before the caret go back to the previous tab stop ('softtabstop'), but
            // not past other characters.
            let spaces = t
                .get(ls..p)
                .unwrap_or(&[])
                .iter()
                .rev()
                .take_while(|c| **c == ' ')
                .count();
            if spaces > 0 {
                let col = p.saturating_sub(ls);
                let stop = col
                    .saturating_sub(1)
                    .checked_div(INDENT)
                    .unwrap_or(0)
                    .saturating_mul(INDENT);
                let to = stop.max(col.saturating_sub(spaces));
                let mut ed = replace(s, ed, ls.saturating_add(to), p, "", true)?;
                set_cursor(&mut ed, ls.saturating_add(to));
                return Ok(Some(ed));
            }
            super::text::key(s, ed, k, shift)
        }
        KeyP::Tab => {
            let mut ed = ed;
            vim_mut(&mut ed).auto_indent = None; // typed indent stays
            super::text::key(s, ed, k, shift)
        }
        _ => super::text::key(s, ed, k, shift),
    }
}

/// Typed text or a key for a box edited in vim mode. `None`: editing ends.
pub(crate) fn input(
    s: &mut Session,
    mut ed: TextEdit,
    text: Option<&str>,
    key: Option<KeyP>,
    shift: bool,
    ctrl: bool,
    t_ms: Option<u64>,
) -> Result<Option<TextEdit>, EngineError> {
    let mode = ed.vim.as_ref().map_or(VimMode::Normal, |v| v.mode);
    if ed.id.is_some() && ed.vim.as_ref().is_some_and(|v| v.undo_floor.is_none()) {
        let depth = undo_depth(s, ed.note);
        vim_mut(&mut ed).undo_floor = Some(depth);
    }
    if mode == VimMode::Insert {
        if let Some(k) = key {
            if let Some(v) = ed.vim.as_mut() {
                v.last_j = None;
            }
            return insert_key(s, ed, k, shift);
        }
        let Some(text) = text else { return Ok(Some(ed)) };
        if ctrl {
            return Ok(Some(ed));
        }
        // `jk`: a `k` right after a `j` (within 300 ms) takes the `j` back and leaves Insert.
        let caret = ed.caret;
        if text == "k"
            && let Some((tj, pos)) = ed.vim.as_ref().and_then(|v| v.last_j)
            && pos.saturating_add(1) == caret
            && match (tj, t_ms) {
                (Some(a), Some(b)) => b.saturating_sub(a) <= JK_MS,
                _ => true,
            }
        {
            let t = chars(s, &ed);
            let at = usize::try_from(pos).unwrap_or(usize::MAX);
            if t.get(at) == Some(&'j') {
                let mut ed = replace(s, ed, at, at.saturating_add(1), "", true)?;
                set_cursor(&mut ed, at);
                let t = chars(s, &ed);
                return Ok(Some(to_normal(s, ed, &t)));
            }
        }
        // Space f w / Space f f typed quickly open the finder; the typed " f" is taken back.
        let pending = ed.vim.as_mut().and_then(|v| v.insert_leader.take());
        if let Some((seq, tl, at)) = pending
            && match (tl, t_ms) {
                (Some(a), Some(b)) => b.saturating_sub(a) <= INSERT_LEADER_MS,
                _ => true,
            }
            && at.saturating_add(u32::try_from(seq.chars().count()).unwrap_or(u32::MAX)) == caret
        {
            match (seq.as_str(), text) {
                (" ", "f") => {
                    let mut ed = type_text(s, ed, text)?;
                    vim_mut(&mut ed).insert_leader = Some((" f".into(), t_ms, at));
                    return Ok(Some(ed));
                }
                (" f", "w" | "f") => {
                    let t = chars(s, &ed);
                    let a = usize::try_from(at).unwrap_or(usize::MAX);
                    if t.get(a..a.saturating_add(2)) == Some(&[' ', 'f']) {
                        let mut ed = replace(s, ed, a, a.saturating_add(2), "", true)?;
                        set_cursor(&mut ed, a);
                        s.ui.panel = Some(if text == "w" { "finder_text" } else { "finder_notes" });
                        return Ok(Some(ed));
                    }
                }
                _ => {}
            }
        }
        let mut ed = type_text(s, ed, text)?;
        if let Some(v) = ed.vim.as_mut() {
            v.last_j = (text == "j").then_some((t_ms, caret));
            v.auto_indent = None;
            v.insert_leader = (text == " ").then(|| (" ".to_string(), t_ms, caret));
        }
        return Ok(Some(ed));
    }
    // Normal and Visual modes.
    if let Some(k) = key {
        let t = chars(s, &ed);
        if let Some(v) = ed.vim.as_mut()
            && !v.pending.is_empty()
        {
            v.pending.clear();
            if k == KeyP::Escape {
                return Ok(Some(ed));
            }
        }
        let m = match k {
            KeyP::Escape if matches!(mode, VimMode::Visual | VimMode::VisualLine) => {
                return Ok(Some(to_normal(s, ed, &t)));
            }
            KeyP::Escape => return Ok(None),
            KeyP::Left | KeyP::Backspace => Mot::Left,
            KeyP::Right => Mot::Right,
            KeyP::Up => Mot::Up,
            KeyP::Down | KeyP::Enter => Mot::Down,
            KeyP::Home => Mot::LineStart,
            KeyP::End => Mot::LineEnd,
            KeyP::Delete => return run(s, ed, 1, Cmd::Simple('x')),
            KeyP::SelectAll | KeyP::Tab => return Ok(Some(ed)),
        };
        return run(s, ed, 1, Cmd::Move(m));
    }
    let Some(text) = text else { return Ok(Some(ed)) };
    if ctrl {
        return if text.eq_ignore_ascii_case("r") {
            run(s, ed, 1, Cmd::Redo)
        } else {
            Ok(Some(ed))
        };
    }
    let mut cur = Some(ed);
    for c in text.chars() {
        let Some(mut ed) = cur.take() else { break };
        let pending_empty = ed.vim.as_ref().is_none_or(|v| v.pending.is_empty());
        // Leader keys: Space (with nothing pending) starts a sequence; its next keys go to it.
        let key = c.to_string();
        if (pending_empty && c == ' ') || (s.leader.is_some() && pending_empty) {
            if leader_key(s, &key, t_ms) {
                cur = Some(ed);
                continue;
            }
            s.leader = None;
        }
        let v = vim_mut(&mut ed);
        v.pending.push(c);
        if v.pending.chars().count() > MAX_PENDING {
            v.pending.clear();
            cur = Some(ed);
            continue;
        }
        let keys: Vec<char> = v.pending.chars().collect();
        let visual = matches!(v.mode, VimMode::Visual | VimMode::VisualLine);
        // In Visual mode an operator acts on the selection at once.
        let parsed = match keys.as_slice() {
            [op @ ('d' | 'c' | 'y' | '>' | '<')] if visual => Parse::Done(1, Cmd::Op(*op, Target::Lines)),
            _ => parse(&keys),
        };
        match parsed {
            Parse::More => cur = Some(ed),
            Parse::Bad => {
                vim_mut(&mut ed).pending.clear();
                cur = Some(ed);
            }
            Parse::Done(count, cmd) => {
                vim_mut(&mut ed).pending.clear();
                cur = run(s, ed, count, cmd)?;
            }
        }
    }
    Ok(cur)
}

/// The selection to draw while editing in vim mode: Visual modes cover the character under
/// the cursor (and whole lines in Visual-line mode); `None` outside Visual modes.
impl Session {
    pub fn vim_selection(&self) -> Option<(u32, u32)> {
        let ed = self.text_edit.as_ref()?;
        let v = ed.vim.as_ref()?;
        if !matches!(v.mode, VimMode::Visual | VimMode::VisualLine) {
            return None;
        }
        let t = chars(self, ed);
        let (a, b, _) = visual_range(&t, ed);
        Some((
            u32::try_from(a).unwrap_or(u32::MAX),
            u32::try_from(b).unwrap_or(u32::MAX),
        ))
    }

    /// True in Visual Line mode (the selection is drawn as whole rows, R-VIM-001).
    pub fn vim_linewise(&self) -> bool {
        self.text_edit
            .as_ref()
            .and_then(|e| e.vim.as_ref())
            .is_some_and(|v| v.mode == VimMode::VisualLine)
    }

    /// The character the Normal/Visual-mode block cursor covers (`None` in Insert mode or
    /// without vim).
    pub fn vim_block_cursor(&self) -> Option<u32> {
        let ed = self.text_edit.as_ref()?;
        let v = ed.vim.as_ref()?;
        (v.mode != VimMode::Insert).then_some(ed.caret)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    /// "space" or a single character.
    key: String,
    /// When the key was pressed (ms, any monotonic clock).
    t_ms: Option<u64>,
}

#[derive(Serialize)]
struct Consumed {
    consumed: bool,
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "vim.key",
            doc: "A key on the canvas with vim mode on (no text being typed): Space starts a leader sequence; Space f w opens the text finder, Space f f the note finder (each key within 1 s). With a single text box selected, any other character starts editing it in Normal mode as a vim command. Returns whether the key was used (else it is an ordinary shortcut).",
            reqs: &["R-VIM-004", "R-VIM-006"],
            implemented: true,
        },
        |s: &mut Session, p: KeyArgs| {
            if p.key.chars().count() > 8 {
                return Err(EngineError::Model("not a key".into()));
            }
            if leader_key(s, &p.key, p.t_ms) {
                return Ok(Consumed { consumed: true });
            }
            // A key with a single text box selected starts editing it in Normal mode and acts
            // as a vim command there (R-VIM-006).
            let mut cs = p.key.chars();
            if s.settings.vim
                && s.text_edit.is_none()
                && let (Some(c), None) = (cs.next(), cs.next())
                && !c.is_control()
                && c != ' '
                && let (Some((id, _)), Some(note)) = (s.single_text(), s.selection.note)
            {
                super::text::open_at(s, note, id, 0)?;
                if let Some(ed) = s.text_edit.clone() {
                    let out = input(s, ed, Some(&p.key), None, false, false, p.t_ms)?;
                    s.text_edit = out;
                }
                return Ok(Consumed { consumed: true });
            }
            Ok(Consumed { consumed: false })
        },
    )))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn c(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn word_motions() {
        let t = c("hello, world  foo\n\nbar");
        assert_eq!(word_forward(&t, 0, false), 5);
        assert_eq!(word_forward(&t, 5, false), 7);
        assert_eq!(word_forward(&t, 0, true), 7);
        assert_eq!(word_end(&t, 0, false), 4);
        assert_eq!(word_back(&t, 7, false), 5);
        assert_eq!(word_forward(&t, 14, false), 18, "an empty line is a word");
        assert_eq!(line_end(&t, 3), 17);
        assert_eq!(line_start(&t, 19), 19);
    }

    #[test]
    fn parsing() {
        assert!(matches!(parse(&c("2d")), Parse::More));
        assert!(matches!(
            parse(&c("2d3w")),
            Parse::Done(6, Cmd::Op('d', Target::Motion(Mot::Word(false))))
        ));
        assert!(matches!(parse(&c("dd")), Parse::Done(1, Cmd::Op('d', Target::Lines))));
        assert!(matches!(
            parse(&c("ciw")),
            Parse::Done(1, Cmd::Op('c', Target::Word(false, false)))
        ));
        // `cw` is resolved with the text (to the word's end, or like `dw` on blanks).
        assert!(matches!(
            parse(&c("cw")),
            Parse::Done(1, Cmd::Op('c', Target::Motion(Mot::Word(false))))
        ));
        assert!(matches!(parse(&c("gg")), Parse::Done(1, Cmd::Move(Mot::FileStart))));
        assert!(matches!(parse(&c("0")), Parse::Done(1, Cmd::Move(Mot::LineStart))));
        assert!(matches!(
            parse(&c("fx")),
            Parse::Done(1, Cmd::Move(Mot::Find('f', 'x')))
        ));
        assert!(matches!(parse(&c("dq")), Parse::Bad));
        assert!(matches!(parse(&c("Q")), Parse::Bad));
    }
}
