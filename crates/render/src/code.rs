//! Code blocks in text boxes (R-TEXT-007): lines between a fence of three backticks (with an
//! optional language name) and a closing fence are code: monospace, on a tinted background,
//! with syntax highlighting. Display only; the stored text keeps its fences.
//!
//! The highlighter is a small tokenizer per language (keywords, strings, comments, numbers,
//! types, function names, keys of config formats), not a parser: it never fails, and anything
//! it does not know stays plain.

use vibenote_model::Color;

/// What a highlighted piece of code is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Keyword,
    Str,
    Comment,
    Number,
    Type,
    Function,
    /// A key in JSON/TOML/YAML, an attribute or tag name in HTML/XML, a property in CSS.
    Key,
    /// A fence line (```lang).
    Fence,
}

impl Token {
    /// Colours on the paper background.
    pub fn color(self) -> Color {
        Color(match self {
            Token::Keyword => 0x8f2f_a8ff,
            Token::Str => 0x2e7d_32ff,
            Token::Comment => 0x8a8f_98ff,
            Token::Number => 0xb359_00ff,
            Token::Type => 0x1f6f_b2ff,
            Token::Function => 0x0d5a_8cff,
            Token::Key => 0xa6_3a1fff,
            Token::Fence => 0x9a9e_a6ff,
        })
    }
}

/// The tint behind code block lines.
pub const BACKGROUND: Color = Color(0xe9ec_f1ff);

/// A code block: its lines `[first, last]` (the fences included) and its language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub first: usize,
    pub last: usize,
    pub lang: String,
}

fn fence(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("```").map(str::trim)
}

/// The code blocks of a text (by line index). An unclosed block runs to the end.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut open: Option<(usize, String)> = None;
    let mut n = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        n = i;
        match (&open, fence(line)) {
            (None, Some(lang)) => open = Some((i, lang.to_lowercase())),
            (Some((first, lang)), Some("")) => {
                out.push(Block {
                    first: *first,
                    last: i,
                    lang: lang.clone(),
                });
                open = None;
            }
            _ => {}
        }
    }
    if let Some((first, lang)) = open {
        out.push(Block { first, last: n, lang });
    }
    out
}

/// A language's rules.
struct Lang {
    keywords: &'static [&'static str],
    line_comment: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Capitalised identifiers are types.
    types: bool,
    /// `key:` / `key =` at the start of a line are keys (config formats).
    keys: bool,
    /// Tag and attribute names (`<a href=…>`).
    markup: bool,
}

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok",
    "Err",
];
const PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not", "or",
    "pass", "raise", "return", "True", "try", "while", "with", "yield", "self",
];
const JS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "of",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
    "interface",
    "type",
    "enum",
    "implements",
    "private",
    "public",
    "readonly",
    "as",
];
const C: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "if",
    "include",
    "define",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "nullptr",
    "private",
    "public",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "true",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "NULL",
];
const GO: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const JAVA: &[&str] = &[
    "abstract",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "throws",
    "true",
    "try",
    "var",
    "void",
    "while",
];
const SHELL: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case", "esac", "function",
    "return", "local", "export", "echo", "set", "unset", "exit", "cd", "source",
];
const LUA: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil",
    "not", "or", "repeat", "return", "then", "true", "until", "while",
];
const SQL: &[&str] = &[
    "select",
    "from",
    "where",
    "insert",
    "into",
    "values",
    "update",
    "set",
    "delete",
    "create",
    "table",
    "drop",
    "alter",
    "index",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "on",
    "group",
    "by",
    "order",
    "having",
    "limit",
    "and",
    "or",
    "not",
    "null",
    "as",
    "distinct",
    "union",
    "primary",
    "key",
    "references",
    "integer",
    "text",
    "with",
    "case",
    "when",
    "then",
    "else",
    "end",
    "is",
    "in",
    "like",
    "begin",
    "commit",
];
const NIX: &[&str] = &[
    "let", "in", "with", "rec", "inherit", "if", "then", "else", "assert", "import", "true", "false", "null",
];
const CONFIG: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

fn lang(name: &str) -> Lang {
    let base = Lang {
        keywords: &[],
        line_comment: &[],
        block_comment: None,
        types: false,
        keys: false,
        markup: false,
    };
    match name {
        "rust" | "rs" => Lang {
            keywords: RUST,
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            types: true,
            ..base
        },
        "python" | "py" => Lang {
            keywords: PYTHON,
            line_comment: &["#"],
            types: true,
            ..base
        },
        "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx" | "mjs" => Lang {
            keywords: JS,
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            types: true,
            ..base
        },
        "c" | "cpp" | "c++" | "cc" | "h" | "hpp" | "cxx" => Lang {
            keywords: C,
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            types: true,
            ..base
        },
        "go" | "golang" => Lang {
            keywords: GO,
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            types: true,
            ..base
        },
        "java" | "kotlin" | "kt" => Lang {
            keywords: JAVA,
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            types: true,
            ..base
        },
        "bash" | "sh" | "shell" | "zsh" | "fish" => Lang {
            keywords: SHELL,
            line_comment: &["#"],
            ..base
        },
        "lua" => Lang {
            keywords: LUA,
            line_comment: &["--"],
            ..base
        },
        "sql" => Lang {
            keywords: SQL,
            line_comment: &["--"],
            block_comment: Some(("/*", "*/")),
            ..base
        },
        "nix" => Lang {
            keywords: NIX,
            line_comment: &["#"],
            block_comment: Some(("/*", "*/")),
            keys: true,
            ..base
        },
        "json" | "jsonc" => Lang {
            keywords: CONFIG,
            line_comment: &["//"],
            keys: true,
            ..base
        },
        "toml" | "ini" | "conf" => Lang {
            keywords: CONFIG,
            line_comment: &["#", ";"],
            keys: true,
            ..base
        },
        "yaml" | "yml" => Lang {
            keywords: CONFIG,
            line_comment: &["#"],
            keys: true,
            ..base
        },
        "html" | "xml" | "svg" | "vue" => Lang {
            block_comment: Some(("<!--", "-->")),
            markup: true,
            ..base
        },
        "css" | "scss" => Lang {
            block_comment: Some(("/*", "*/")),
            keys: true,
            ..base
        },
        // Unknown or no language: strings, numbers and the usual comments.
        _ => Lang {
            line_comment: &["//", "#"],
            block_comment: Some(("/*", "*/")),
            ..base
        },
    }
}

fn starts(chars: &[char], i: usize, pat: &str) -> bool {
    let mut k = i;
    for p in pat.chars() {
        if chars.get(k) != Some(&p) {
            return false;
        }
        k = k.saturating_add(1);
    }
    true
}

fn len(pat: &str) -> usize {
    pat.chars().count()
}

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Highlights the lines of one block's body (without fences), carrying a block comment across
/// lines. Returns per line `(start, end, token)` character ranges.
pub fn highlight(lang_name: &str, lines: &[&str]) -> Vec<Vec<(usize, usize, Token)>> {
    let l = lang(lang_name);
    let mut in_block = false;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        let n = chars.len();
        let mut toks: Vec<(usize, usize, Token)> = Vec::new();
        let mut i = 0usize;
        // A config key: the first word of the line before `:` or `=`.
        let mut key_done = !l.keys;
        while i < n {
            let Some(&c) = chars.get(i) else { break };
            if in_block {
                let close = l.block_comment.map_or("", |b| b.1);
                let start = i;
                while i < n && !(close.is_empty() || starts(&chars, i, close)) {
                    i = i.saturating_add(1);
                }
                if i < n {
                    i = i.saturating_add(len(close));
                    in_block = false;
                }
                toks.push((start, i.min(n), Token::Comment));
                continue;
            }
            if let Some((open, _)) = l.block_comment
                && starts(&chars, i, open)
            {
                in_block = true;
                let start = i;
                i = i.saturating_add(len(open));
                let close = l.block_comment.map_or("", |b| b.1);
                while i < n && !starts(&chars, i, close) {
                    i = i.saturating_add(1);
                }
                if i < n {
                    i = i.saturating_add(len(close));
                    in_block = false;
                }
                toks.push((start, i.min(n), Token::Comment));
                continue;
            }
            if l.line_comment.iter().any(|p| starts(&chars, i, p)) {
                toks.push((i, n, Token::Comment));
                break;
            }
            if c == '"' || c == '\'' || c == '`' {
                let start = i;
                i = i.saturating_add(1);
                while i < n {
                    match chars.get(i) {
                        Some('\\') => i = i.saturating_add(2),
                        Some(&q) if q == c => {
                            i = i.saturating_add(1);
                            break;
                        }
                        _ => i = i.saturating_add(1),
                    }
                }
                let end = i.min(n);
                // A quoted key ("name": …) in a config format.
                let after = chars.get(end..).unwrap_or(&[]).iter().find(|x| !x.is_whitespace());
                let tok = if !key_done && after == Some(&':') {
                    Token::Key
                } else {
                    Token::Str
                };
                key_done = true;
                toks.push((start, end, tok));
                continue;
            }
            if c.is_ascii_digit() {
                let start = i;
                while chars
                    .get(i)
                    .is_some_and(|x| x.is_ascii_hexdigit() || matches!(x, 'x' | 'o' | 'b' | '_' | '.'))
                {
                    i = i.saturating_add(1);
                }
                toks.push((start, i, Token::Number));
                key_done = true;
                continue;
            }
            if l.markup && c == '<' {
                // A tag name, then attribute names.
                let mut k = i.saturating_add(1);
                if chars.get(k) == Some(&'/') {
                    k = k.saturating_add(1);
                }
                let s0 = k;
                while chars.get(k).is_some_and(|x| ident(*x) || *x == '-' || *x == ':') {
                    k = k.saturating_add(1);
                }
                if k > s0 {
                    toks.push((s0, k, Token::Keyword));
                }
                i = k.max(i.saturating_add(1));
                continue;
            }
            if ident(c) && !c.is_ascii_digit() {
                let start = i;
                while chars.get(i).is_some_and(|x| ident(*x) || (l.keys && *x == '-')) {
                    i = i.saturating_add(1);
                }
                let word: String = chars.get(start..i).unwrap_or(&[]).iter().collect();
                let next = chars
                    .get(i..)
                    .unwrap_or(&[])
                    .iter()
                    .find(|x| !x.is_whitespace())
                    .copied();
                let tok = if (!key_done && matches!(next, Some(':' | '='))) || (l.markup && next == Some('=')) {
                    Some(Token::Key)
                } else if l.keywords.iter().any(|k| *k == word)
                    || (lang_name == "sql" && l.keywords.iter().any(|k| k.eq_ignore_ascii_case(&word)))
                {
                    Some(Token::Keyword)
                } else if next == Some('(') {
                    Some(Token::Function)
                } else if l.types && word.chars().next().is_some_and(char::is_uppercase) {
                    Some(Token::Type)
                } else {
                    None
                };
                key_done = true;
                if let Some(t) = tok {
                    toks.push((start, i, t));
                }
                continue;
            }
            if !c.is_whitespace() {
                key_done = true;
            }
            i = i.saturating_add(1);
        }
        out.push(toks);
    }
    out
}

/// Per line of `text`: `None` outside code, else the line's tokens (a fence line is one
/// [`Token::Fence`] range).
pub fn line_tokens(text: &str) -> Vec<Option<Vec<(usize, usize, Token)>>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Option<Vec<(usize, usize, Token)>>> = vec![None; lines.len()];
    for b in blocks(text) {
        if let Some(slot) = out.get_mut(b.first) {
            *slot = Some(vec![(
                0,
                lines.get(b.first).map_or(0, |l| l.chars().count()),
                Token::Fence,
            )]);
        }
        let closed = b.last > b.first && lines.get(b.last).and_then(|l| fence(l)).is_some_and(str::is_empty);
        let body_end = if closed { b.last } else { b.last.saturating_add(1) };
        let body: Vec<&str> = lines
            .get(b.first.saturating_add(1)..body_end.max(b.first.saturating_add(1)))
            .unwrap_or(&[])
            .to_vec();
        for (k, toks) in highlight(&b.lang, &body).into_iter().enumerate() {
            if let Some(slot) = out.get_mut(b.first.saturating_add(1).saturating_add(k)) {
                *slot = Some(toks);
            }
        }
        if closed && let Some(slot) = out.get_mut(b.last) {
            *slot = Some(vec![(
                0,
                lines.get(b.last).map_or(0, |l| l.chars().count()),
                Token::Fence,
            )]);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn tok(lang: &str, line: &str) -> Vec<(String, Token)> {
        let chars: Vec<char> = line.chars().collect();
        highlight(lang, &[line])[0]
            .iter()
            .map(|&(a, b, t)| (chars[a..b].iter().collect(), t))
            .collect()
    }

    fn has(v: &[(String, Token)], s: &str, t: Token) -> bool {
        v.iter().any(|(x, k)| x == s && *k == t)
    }

    #[test]
    fn blocks_are_found_between_fences() {
        let t = "intro\n```rust\nfn main() {}\n```\nafter\n```\nopen";
        assert_eq!(
            blocks(t),
            vec![
                Block {
                    first: 1,
                    last: 3,
                    lang: "rust".into()
                },
                Block {
                    first: 5,
                    last: 6,
                    lang: String::new()
                }
            ]
        );
        let lt = line_tokens(t);
        assert!(lt[0].is_none() && lt[4].is_none());
        assert_eq!(lt[1].as_deref(), Some(&[(0, 7, Token::Fence)][..]));
        assert!(lt[2].is_some() && lt[6].is_some(), "an unclosed block runs to the end");
    }

    /// One sample line per language: its keywords, strings, comments, numbers, types,
    /// functions and keys are found.
    #[test]
    fn every_language_highlights_its_sample() {
        let r = tok("rust", r#"pub fn add(a: i32) -> Vec<u8> { let s = "x"; 42 } // done"#);
        assert!(has(&r, "pub", Token::Keyword) && has(&r, "fn", Token::Keyword));
        assert!(has(&r, "add", Token::Function) && has(&r, "Vec", Token::Type));
        assert!(has(&r, "\"x\"", Token::Str) && has(&r, "42", Token::Number));
        assert!(has(&r, "// done", Token::Comment));
        let p = tok("python", "def f(x): return 'a'  # c");
        assert!(has(&p, "def", Token::Keyword) && has(&p, "f", Token::Function) && has(&p, "# c", Token::Comment));
        let j = tok("js", "const x = await fetch(`u`); /* c */");
        assert!(has(&j, "const", Token::Keyword) && has(&j, "fetch", Token::Function));
        assert!(has(&j, "`u`", Token::Str) && has(&j, "/* c */", Token::Comment));
        let c = tok("cpp", "int main(void) { return 0; }");
        assert!(has(&c, "int", Token::Keyword) && has(&c, "main", Token::Function) && has(&c, "0", Token::Number));
        let g = tok("go", "func main() { fmt.Println(\"hi\") }");
        assert!(has(&g, "func", Token::Keyword) && has(&g, "Println", Token::Function));
        let jv = tok("java", "public static void main(String[] a) {}");
        assert!(has(&jv, "public", Token::Keyword) && has(&jv, "String", Token::Type));
        let sh = tok("bash", "if [ -f x ]; then echo \"ok\"; fi # end");
        assert!(has(&sh, "if", Token::Keyword) && has(&sh, "\"ok\"", Token::Str) && has(&sh, "# end", Token::Comment));
        let l = tok("lua", "local x = 1 -- note");
        assert!(has(&l, "local", Token::Keyword) && has(&l, "-- note", Token::Comment));
        let q = tok("sql", "SELECT name FROM users WHERE id = 3");
        assert!(has(&q, "SELECT", Token::Keyword) && has(&q, "FROM", Token::Keyword) && has(&q, "3", Token::Number));
        let js = tok("json", r#""name": "vibenote", "n": 1"#);
        assert!(has(&js, "\"name\"", Token::Key) && has(&js, "\"vibenote\"", Token::Str));
        let tm = tok("toml", "version = \"1.0\" # x");
        assert!(has(&tm, "version", Token::Key) && has(&tm, "# x", Token::Comment));
        let y = tok("yaml", "enabled: true");
        assert!(has(&y, "enabled", Token::Key) && has(&y, "true", Token::Keyword));
        let nx = tok("nix", "let x = 1; in x");
        assert!(has(&nx, "let", Token::Keyword) && has(&nx, "in", Token::Keyword));
        let h = tok("html", "<a href=\"/\">x</a> <!-- c -->");
        assert!(has(&h, "a", Token::Keyword) && has(&h, "href", Token::Key) && has(&h, "<!-- c -->", Token::Comment));
        let cs = tok("css", "color: red; /* c */");
        assert!(has(&cs, "color", Token::Key) && has(&cs, "/* c */", Token::Comment));
        let unknown = tok("", "x = \"s\" // c");
        assert!(has(&unknown, "\"s\"", Token::Str) && has(&unknown, "// c", Token::Comment));
    }

    #[test]
    fn block_comments_span_lines_and_hostile_input_is_fine() {
        let v = highlight("c", &["a /* one", "two */ b"]);
        assert_eq!(v[0], vec![(2, 8, Token::Comment)]);
        assert_eq!(v[1], vec![(0, 6, Token::Comment)]);
        // Unterminated strings, lone backslashes, emoji: no panic, ranges inside the line.
        for line in ["\"abc\\", "'", "😀 fn x(", "\\\\\\", "<", "/*"] {
            let n = line.chars().count();
            for (a, b, _) in &highlight("rust", &[line])[0] {
                assert!(a <= b && *b <= n, "{line}");
            }
        }
    }
}
