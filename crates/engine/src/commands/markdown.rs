//! Markdown in text boxes (R-TEXT-001): typing markdown syntax formats as you go, and markdown
//! files become formatted text. The result is the box's rich text (content + marks); the syntax
//! characters are removed, so saved boxes stay plain rich text.
//!
//! Line starts: `# `, `## `, `### ` (headings), `- `, `* `, `+ ` (bullets), `1. ` (numbered),
//! `[ ] `, `[x] `, `- [ ] `, `- [x] ` (checkboxes). Inline: `**bold**`, `__bold__`,
//! `*italic*`, `_italic_`, `***bold italic***`, `~~strike~~`.
//! Positions are character offsets.

use vibenote_model::{TextMark, TextStyle};

/// How far back (characters) a closing delimiter looks for its opening one.
const MAX_SPAN: usize = 2_000;

/// The paragraph style a line-start prefix (without the trailing space) asks for.
pub(crate) fn block_prefix(prefix: &str) -> Option<TextStyle> {
    Some(match prefix {
        "#" => TextStyle::Heading1,
        "##" => TextStyle::Heading2,
        "###" => TextStyle::Heading3,
        "-" | "*" | "+" => TextStyle::BulletList,
        "[]" | "[ ]" | "- [ ]" | "* [ ]" => TextStyle::Checkbox { checked: false },
        "[x]" | "[X]" | "- [x]" | "- [X]" | "* [x]" => TextStyle::Checkbox { checked: true },
        p if p.len() >= 2
            && p.len() <= 10
            && p.ends_with(['.', ')'])
            && p.get(..p.len().saturating_sub(1))
                .is_some_and(|d| d.bytes().all(|b| b.is_ascii_digit())) =>
        {
            TextStyle::NumberedList
        }
        _ => return None,
    })
}

/// Inline delimiters, longest first (a longer one must win over its prefix).
const INLINE: [(&str, &[TextStyle]); 6] = [
    ("***", &[TextStyle::Bold, TextStyle::Italic]),
    ("**", &[TextStyle::Bold]),
    ("__", &[TextStyle::Bold]),
    ("~~", &[TextStyle::Strike]),
    ("*", &[TextStyle::Italic]),
    ("_", &[TextStyle::Italic]),
];

/// An inline span that the character just typed at `line[end - 1]` closed: the opening
/// delimiter's start, the delimiter length and the styles. `line` is the current line's
/// characters up to the caret's line end; `end` is the caret within it.
pub(crate) fn inline_close(line: &[char], end: usize) -> Option<(usize, usize, &'static [TextStyle])> {
    let at = |i: usize| line.get(i).copied();
    for (delim, styles) in INLINE {
        let d: Vec<char> = delim.chars().collect();
        let n = d.len();
        let dc = *d.first()?;
        let Some(close) = end.checked_sub(n) else { continue };
        if line.get(close..end) != Some(d.as_slice()) {
            continue;
        }
        // The closing run is exactly this delimiter, right after non-space text.
        let before_close = close.checked_sub(1).and_then(at);
        if before_close.is_none_or(|c| c.is_whitespace() || c == dc) {
            continue;
        }
        let lo = close.saturating_sub(MAX_SPAN);
        let mut open = close.checked_sub(n.saturating_add(1));
        while let Some(o) = open {
            if o < lo {
                break;
            }
            let fits = line.get(o..o.saturating_add(n)) == Some(d.as_slice())
                && o.checked_sub(1).and_then(at) != Some(dc)
                && at(o.saturating_add(n)).is_some_and(|c| !c.is_whitespace() && c != dc)
                // A single `*` or `_` only opens at a word start (snake_case and 2*3*4 stay).
                && (n > 1 || o.checked_sub(1).and_then(at).is_none_or(|c| !c.is_alphanumeric()));
            if fits {
                return Some((o, n, styles));
            }
            open = o.checked_sub(1);
        }
    }
    None
}

/// Converts markdown source to text with marks.
pub(crate) fn parse(src: &str) -> (String, Vec<TextMark>) {
    let mut out = String::with_capacity(src.len());
    let mut marks = Vec::new();
    let mut base = 0u32; // characters in `out`
    for (i, raw) in src.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            base = base.saturating_add(1);
        }
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        // A block prefix: up to the first space, or "- [ ]"-style (two words).
        let mut body = raw;
        let mut para = None;
        for words in [2usize, 1] {
            let mut it = raw.splitn(words.saturating_add(1), ' ');
            let parts: Vec<&str> = it.by_ref().take(words).collect();
            if parts.len() == words
                && let Some(rest) = it.next()
                && let Some(st) = block_prefix(&parts.join(" "))
            {
                para = Some(st);
                body = rest;
                break;
            }
        }
        let mut line: Vec<char> = Vec::with_capacity(body.len());
        let mut inline: Vec<(usize, usize, TextStyle)> = Vec::new();
        for c in body.chars() {
            line.push(c);
            if let Some((o, n, styles)) = inline_close(&line, line.len()) {
                let close = line.len().saturating_sub(n);
                line.drain(close..);
                line.drain(o..o.saturating_add(n));
                let shift = |x: usize| {
                    if x >= close {
                        x.saturating_sub(n.saturating_mul(2))
                    } else if x > o {
                        x.saturating_sub(n)
                    } else {
                        x
                    }
                };
                for m in &mut inline {
                    m.0 = shift(m.0);
                    m.1 = shift(m.1);
                }
                for st in styles {
                    inline.push((o, close.saturating_sub(n), *st));
                }
            }
        }
        let len = u32::try_from(line.len()).unwrap_or(u32::MAX);
        for (a, b, st) in inline {
            let a = u32::try_from(a).unwrap_or(u32::MAX);
            let b = u32::try_from(b).unwrap_or(u32::MAX);
            marks.push(TextMark {
                start: base.saturating_add(a),
                end: base.saturating_add(b),
                style: st,
            });
        }
        if let Some(st) = para
            && len > 0
        {
            marks.push(TextMark {
                start: base,
                end: base.saturating_add(len),
                style: st,
            });
        }
        out.extend(line);
        base = base.saturating_add(len);
    }
    marks.retain(|m| m.start < m.end);
    (out, marks)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn m(start: u32, end: u32, style: TextStyle) -> TextMark {
        TextMark { start, end, style }
    }

    fn close(s: &str) -> Option<(usize, usize, &'static [TextStyle])> {
        let c: Vec<char> = s.chars().collect();
        inline_close(&c, c.len())
    }

    #[test]
    fn prefixes() {
        assert_eq!(block_prefix("#"), Some(TextStyle::Heading1));
        assert_eq!(block_prefix("###"), Some(TextStyle::Heading3));
        assert_eq!(block_prefix("####"), None);
        assert_eq!(block_prefix("-"), Some(TextStyle::BulletList));
        assert_eq!(block_prefix("12."), Some(TextStyle::NumberedList));
        assert_eq!(block_prefix("."), None);
        assert_eq!(block_prefix("- [x]"), Some(TextStyle::Checkbox { checked: true }));
    }

    #[test]
    fn inline_spans_close_only_on_their_own_delimiter() {
        assert_eq!(close("a **bold**").map(|x| (x.0, x.1)), Some((2, 2)));
        assert_eq!(close("a **bold*"), None, "half a closing ** is no italic");
        assert_eq!(close("*it*").map(|x| x.2), Some(&[TextStyle::Italic][..]));
        assert_eq!(
            close("***both***").map(|x| x.2),
            Some(&[TextStyle::Bold, TextStyle::Italic][..])
        );
        assert_eq!(close("2 * 3 *"), None, "spaces inside the delimiters");
        assert_eq!(close("snake_case_"), None);
        assert_eq!(close("2*3*"), None);
        assert_eq!(close("a**b**").map(|x| x.0), Some(1), "bold works inside words");
        assert_eq!(close("~~gone~~").map(|x| x.2), Some(&[TextStyle::Strike][..]));
        assert_eq!(close("**"), None);
    }

    #[test]
    fn parse_markdown_to_marks() {
        let (t, marks) = parse("# Title\nsome **bold** and *it*\n- one\n2. two\n- [x] done\nplain_snake_case");
        assert_eq!(t, "Title\nsome bold and it\none\ntwo\ndone\nplain_snake_case");
        assert_eq!(
            marks,
            vec![
                m(0, 5, TextStyle::Heading1),
                m(11, 15, TextStyle::Bold),
                m(20, 22, TextStyle::Italic),
                m(23, 26, TextStyle::BulletList),
                m(27, 30, TextStyle::NumberedList),
                m(31, 35, TextStyle::Checkbox { checked: true }),
            ]
        );
        let (t, marks) = parse("***x*** ~~y~~\r\n");
        assert_eq!(t, "x y\n");
        assert_eq!(
            marks,
            vec![
                m(0, 1, TextStyle::Bold),
                m(0, 1, TextStyle::Italic),
                m(2, 3, TextStyle::Strike)
            ]
        );
    }
}
