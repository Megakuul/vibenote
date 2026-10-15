//! Search normalisation (R-SEARCH-003, ADR 0005 decision 5) and text extents, shared by the
//! storage index and the engine.

use crate::{TextMark, TextStyle};
use vibenote_geom::{Point, Rect};

/// The two searchable forms of a text: umlauts spelled out (`ä` → `ae`, `ß` → `ss`) and
/// stripped (`ä` → `a`, `ß` → `ss`); both lower-case without accents.
pub fn forms(s: &str) -> (String, String) {
    let mut a = String::with_capacity(s.len());
    let mut b = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let (x, y): (&str, &str) = match c {
            'ä' => ("ae", "a"),
            'ö' => ("oe", "o"),
            'ü' => ("ue", "u"),
            'ß' => ("ss", "ss"),
            'à' | 'á' | 'â' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => ("a", "a"),
            'æ' => ("ae", "ae"),
            'ç' | 'ć' | 'č' => ("c", "c"),
            'ď' | 'đ' => ("d", "d"),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => ("e", "e"),
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => ("i", "i"),
            'ł' | 'ľ' | 'ĺ' => ("l", "l"),
            'ñ' | 'ń' | 'ň' => ("n", "n"),
            'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'ō' | 'ő' => ("o", "o"),
            'œ' => ("oe", "oe"),
            'ř' | 'ŕ' => ("r", "r"),
            'ś' | 'š' | 'ş' => ("s", "s"),
            'ť' | 'ţ' => ("t", "t"),
            'ù' | 'ú' | 'û' | 'ū' | 'ů' | 'ű' | 'ų' => ("u", "u"),
            'ý' | 'ÿ' => ("y", "y"),
            'ź' | 'ż' | 'ž' => ("z", "z"),
            c if c.is_whitespace() => (" ", " "),
            _ => {
                a.push(c);
                b.push(c);
                continue;
            }
        };
        // Collapse runs of whitespace (line breaks in PDFs split words otherwise).
        if x == " " && a.ends_with(' ') {
            continue;
        }
        a.push_str(x);
        b.push_str(y);
    }
    (a, b)
}

/// A generous box for a text box (no layout here): its width, and a height from the number of
/// lines it can wrap into at the largest size it uses.
pub fn text_box(origin: Point, width: f64, size: f32, content: &str, marks: &[TextMark]) -> Rect {
    let biggest = marks
        .iter()
        .map(|m| match m.style {
            TextStyle::Size { size } => size,
            TextStyle::Heading1 => size * 2.0,
            _ => size,
        })
        .fold(size, f32::max);
    let em = f64::from(biggest.max(1.0));
    let per_line = (width / (0.45 * em)).max(1.0);
    let mut lines = 0.0;
    for l in content.split('\n') {
        #[allow(clippy::cast_precision_loss)] // a line count estimate
        let n = l.chars().count() as f64;
        lines += (n / per_line).ceil().max(1.0);
    }
    Rect {
        min: origin,
        max: Point::new(origin.x + width, origin.y + lines * 1.3 * em + em),
    }
}
