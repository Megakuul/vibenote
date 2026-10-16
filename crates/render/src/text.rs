//! Canvas text (ADR 0002 decision 3, R-TEXT-001): text boxes and link labels laid out by
//! cosmic-text and drawn by glyphon, zoomed with the canvas. Only the bundled Inter fonts are
//! loaded (no system fonts), so pictures are the same on every machine.
//!
//! Formatting comes from the model's marks: bold, italic, colour and size per character;
//! headings, lists and checkboxes per line (a list or checkbox line gets a prefix). Underline
//! and strike-through become thin vector strokes from cosmic-text's decoration spans (glyphon
//! draws glyphs only).

use glyphon::cosmic_text::UnderlineStyle;
use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, ColorMode, Family, FontSystem, Metrics, Shaping, Style, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Weight, Wrap,
};
use std::collections::HashMap;
use vibenote_geom::Point;
use vibenote_model::{Color, TextMark, TextStyle};

use crate::fonts::{INTER, INTER_ITALIC, MONO};

/// Line height relative to the font size.
const LINE: f32 = 1.3;
/// Below this on-screen size (px per em) text is not drawn (unreadable; saves the atlas).
const MIN_PX: f64 = 2.0;
/// Above this the glyph cache would hold huge bitmaps: text is drawn at this size, clipped.
const MAX_PX: f64 = 512.0;

/// A text box or link label to draw (canvas units).
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    /// Identifies the content and formatting (same key, same layout).
    pub key: u64,
    pub origin: Point,
    /// Wrap width.
    pub width: f64,
    /// Font size per em.
    pub size: f32,
    pub content: String,
    pub marks: Vec<TextMark>,
    pub color: Color,
    /// Monospace (vim mode, R-VIM-001); part of `key`.
    pub mono: bool,
}

/// The font family of an item.
fn family(item: &TextItem) -> Family<'static> {
    family_of(item.mono)
}

/// The monospace family, or the text family.
fn family_of(mono: bool) -> Family<'static> {
    if mono {
        Family::Name("JetBrains Mono NL")
    } else {
        Family::Name("Inter Variable")
    }
}

fn gcolor(c: Color) -> GColor {
    let [r, g, b, a] = c.0.to_be_bytes();
    GColor::rgba(r, g, b, a)
}

/// Per-character formatting after applying the marks (paragraph styles per line).
#[derive(Clone, Copy, Debug, PartialEq)]
struct CharStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<Color>,
    size: f32,
    /// Code (R-TEXT-007): monospace.
    mono: bool,
}

/// Per-character styles from the character marks, by a sweep over mark boundaries
/// (O((n + m) log m); walking every mark's range was O(n · m)). Flags are on while any mark
/// sets them; for colour and size the last mark (in mark order) wins.
fn char_styles(n: usize, marks: &[TextMark], base: CharStyle) -> Vec<CharStyle> {
    use std::collections::BTreeMap;
    // (position, mark index, starts) events.
    let mut events: Vec<(usize, usize, bool)> = Vec::new();
    for (k, m) in marks.iter().enumerate() {
        if m.style.is_paragraph() {
            continue;
        }
        let a = usize::try_from(m.start).unwrap_or(usize::MAX).min(n);
        let b = usize::try_from(m.end).unwrap_or(usize::MAX).min(n);
        if a < b {
            events.push((a, k, true));
            events.push((b, k, false));
        }
    }
    events.sort_unstable();
    let mut flags = [0usize; 4];
    let mut colors: BTreeMap<usize, Color> = BTreeMap::new();
    let mut sizes: BTreeMap<usize, f32> = BTreeMap::new();
    let mut out = Vec::with_capacity(n);
    let mut ev = events.iter().peekable();
    for pos in 0..n {
        while let Some((p, k, starts)) = ev.next_if(|e| e.0 <= pos) {
            let _ = p;
            let Some(m) = marks.get(*k) else { continue };
            let flag = match m.style {
                TextStyle::Bold => Some(0),
                TextStyle::Italic => Some(1),
                TextStyle::Underline => Some(2),
                TextStyle::Strike => Some(3),
                TextStyle::Color { color } => {
                    if *starts {
                        colors.insert(*k, color);
                    } else {
                        colors.remove(k);
                    }
                    None
                }
                TextStyle::Size { size } => {
                    if *starts {
                        sizes.insert(*k, size);
                    } else {
                        sizes.remove(k);
                    }
                    None
                }
                _ => None,
            };
            if let Some(f) = flag.and_then(|f| flags.get_mut(f)) {
                *f = if *starts {
                    f.saturating_add(1)
                } else {
                    f.saturating_sub(1)
                };
            }
        }
        out.push(CharStyle {
            bold: flags[0] > 0,
            italic: flags[1] > 0,
            underline: flags[2] > 0,
            strike: flags[3] > 0,
            color: colors.last_key_value().map(|(_, c)| *c),
            size: sizes.last_key_value().map_or(base.size, |(_, s)| *s),
            mono: false,
        });
    }
    out
}

/// The paragraph styles of each line (lines split at '\n', `bounds` = [start, end) in chars),
/// in mark order, by a sweep over lines: a mark touches a contiguous run of lines (a non-empty
/// line when it overlaps it, an empty line when it contains its position).
fn line_styles(bounds: &[(usize, usize)], marks: &[TextMark]) -> Vec<Vec<TextStyle>> {
    use std::collections::BTreeMap;
    let touched = |li: usize, a: usize, b: usize| {
        bounds
            .get(li)
            .is_some_and(|&(s, e)| if s == e { a <= s && b > s } else { a < e && b > s })
    };
    // (line, mark index, starts) events.
    let mut events: Vec<(usize, usize, bool)> = Vec::new();
    for (k, m) in marks.iter().enumerate() {
        if !m.style.is_paragraph() {
            continue;
        }
        let a = usize::try_from(m.start).unwrap_or(usize::MAX);
        let b = usize::try_from(m.end).unwrap_or(usize::MAX);
        // The line containing `a`, or the next one.
        let k0 = bounds.partition_point(|&(s, _)| s <= a).saturating_sub(1);
        let l0 = if touched(k0, a, b) { k0 } else { k0.saturating_add(1) };
        let l1 = bounds.partition_point(|&(s, _)| s < b).checked_sub(1);
        match l1 {
            Some(l1) if l1 >= l0 && touched(l0, a, b) => {
                events.push((l0, k, true));
                events.push((l1.saturating_add(1), k, false));
            }
            _ => {}
        }
    }
    events.sort_unstable();
    let mut active: BTreeMap<usize, TextStyle> = BTreeMap::new();
    let mut ev = events.iter().peekable();
    let mut out = Vec::with_capacity(bounds.len());
    for li in 0..bounds.len() {
        while let Some((_, k, starts)) = ev.next_if(|e| e.0 <= li) {
            if *starts {
                if let Some(m) = marks.get(*k) {
                    active.insert(*k, m.style);
                }
            } else {
                active.remove(k);
            }
        }
        // Distinct styles in mark order (enough to decide headings, lists and checkboxes).
        let mut v: Vec<TextStyle> = Vec::new();
        for st in active.values() {
            let kind = std::mem::discriminant(st);
            if !v.iter().any(|x| std::mem::discriminant(x) == kind) {
                v.push(*st);
            }
            if v.len() >= 6 {
                break;
            }
        }
        out.push(v);
    }
    out
}

/// The displayed text (with list/checkbox prefixes) and its styled spans.
fn spans(item: &TextItem) -> Vec<(String, CharStyle)> {
    let chars: Vec<char> = item.content.chars().collect();
    let base = CharStyle {
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        color: None,
        size: item.size,
        mono: false,
    };
    let styles = char_styles(chars.len(), &item.marks, base);
    // Code blocks (R-TEXT-007): monospace and highlighted (an explicit colour mark wins).
    let code = if item.content.contains("```") {
        crate::code::line_tokens(&item.content)
    } else {
        Vec::new()
    };
    let mut out: Vec<(String, CharStyle)> = Vec::new();
    let push = |out: &mut Vec<(String, CharStyle)>, c: char, st: CharStyle| match out.last_mut() {
        Some((s, last)) if *last == st => s.push(c),
        _ => out.push((c.to_string(), st)),
    };
    let mut number = 0u32;
    let lines: Vec<&[char]> = chars.split(|c| *c == '\n').collect();
    let count = lines.len();
    let mut bounds = Vec::with_capacity(count);
    let mut start = 0usize;
    for line in &lines {
        let end = start.saturating_add(line.len());
        bounds.push((start, end));
        start = end.saturating_add(1);
    }
    let paras = line_styles(&bounds, &item.marks);
    for (li, line) in lines.into_iter().enumerate() {
        let start = bounds.get(li).map_or(0, |b| b.0);
        let para: &[TextStyle] = paras.get(li).map_or(&[], Vec::as_slice);
        let heading = para.iter().find_map(|p| match p {
            TextStyle::Heading1 => Some(2.0),
            TextStyle::Heading2 => Some(1.5),
            TextStyle::Heading3 => Some(1.25),
            _ => None,
        });
        let mut prefix = String::new();
        if para.contains(&TextStyle::NumberedList) {
            number = number.saturating_add(1);
            prefix = format!("{number}. ");
        } else {
            number = 0;
            if para.contains(&TextStyle::BulletList) {
                prefix = "• ".into();
            }
        }
        if let Some(TextStyle::Checkbox { checked }) = para.iter().find(|p| matches!(p, TextStyle::Checkbox { .. })) {
            prefix.push_str(if *checked { "☑ " } else { "☐ " });
        }
        let line_style = |s: CharStyle| match heading {
            Some(k) => CharStyle {
                bold: true,
                size: s.size * k,
                ..s
            },
            None => s,
        };
        let first = styles.get(start).copied().unwrap_or(base);
        for c in prefix.chars() {
            push(
                &mut out,
                c,
                line_style(CharStyle {
                    underline: false,
                    strike: false,
                    ..first
                }),
            );
        }
        let toks = code.get(li).and_then(Option::as_ref);
        for (k, c) in line.iter().enumerate() {
            let mut st = styles.get(start.saturating_add(k)).copied().unwrap_or(base);
            if let Some(toks) = toks {
                st.mono = true;
                if let Some(&(_, _, t)) = toks.iter().find(|&&(a, b, _)| a <= k && k < b) {
                    st.color = st.color.or(Some(t.color()));
                    if t == crate::code::Token::Comment {
                        st.italic = true;
                    }
                }
            }
            push(&mut out, *c, line_style(st));
        }
        if li.saturating_add(1) < count {
            push(&mut out, '\n', line_style(first));
        }
    }
    out
}

pub(crate) struct TextState {
    fonts: FontSystem,
    swash: SwashCache,
    cache: Option<Cache>,
    gpu: Option<(wgpu::TextureFormat, TextAtlas, TextRenderer, glyphon::Viewport)>,
    /// Laid-out buffers by item key, with the frame they were last used.
    buffers: HashMap<u64, (Buffer, u64)>,
    frame: u64,
}

impl TextState {
    pub(crate) fn new() -> TextState {
        let fonts = FontSystem::new_with_fonts(
            [INTER, INTER_ITALIC]
                .into_iter()
                .chain(MONO)
                .map(|f| glyphon::fontdb::Source::Binary(std::sync::Arc::new(f))),
        );
        TextState {
            fonts,
            swash: SwashCache::new(),
            cache: None,
            gpu: None,
            buffers: HashMap::new(),
            frame: 0,
        }
    }

    fn layout(&mut self, item: &TextItem) {
        if let Some((_, used)) = self.buffers.get_mut(&item.key) {
            *used = self.frame;
            return;
        }
        let size = item.size.max(1.0);
        let mut buf = Buffer::new(&mut self.fonts, Metrics::new(size, size * LINE));
        #[allow(clippy::cast_possible_truncation)] // a wrap width in canvas units
        buf.set_size(Some(item.width.max(1.0) as f32), None);
        buf.set_wrap(Wrap::WordOrGlyph);
        let sp = spans(item);
        let attrs: Vec<(&str, Attrs<'_>)> = sp
            .iter()
            .map(|(t, st)| {
                let mut a = Attrs::new()
                    .family(family_of(item.mono || st.mono))
                    .weight(if st.bold { Weight::BOLD } else { Weight::NORMAL })
                    .style(if st.italic { Style::Italic } else { Style::Normal })
                    .metrics(Metrics::new(st.size, st.size * LINE));
                if let Some(c) = st.color {
                    a = a.color(gcolor(c));
                }
                if st.underline {
                    a = a.underline(UnderlineStyle::Single);
                }
                if st.strike {
                    a = a.strikethrough();
                }
                (t.as_str(), a)
            })
            .collect();
        buf.set_rich_text(attrs, &Attrs::new().family(family(item)), Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.fonts, false);
        self.buffers.insert(item.key, (buf, self.frame));
    }

    /// The displayed lines of `item` (list/checkbox prefixes included) and, per line, how
    /// many prefix characters come before the content.
    fn display_lines(item: &TextItem) -> (Vec<String>, Vec<usize>) {
        let disp: String = spans(item).into_iter().map(|(t, _)| t).collect();
        let lines: Vec<String> = disp.split('\n').map(String::from).collect();
        let prefix = item
            .content
            .split('\n')
            .zip(&lines)
            .map(|(c, d)| d.chars().count().saturating_sub(c.chars().count()))
            .collect();
        (lines, prefix)
    }

    /// Content offset (chars) → layout cursor (line, byte index in the displayed line).
    fn cursor_of(item: &TextItem, offset: u32) -> glyphon::Cursor {
        let (lines, prefix) = Self::display_lines(item);
        let mut left = usize::try_from(offset).unwrap_or(usize::MAX);
        let content: Vec<&str> = item.content.split('\n').collect();
        let last = content.len().saturating_sub(1);
        for (li, c) in content.iter().enumerate() {
            let n = c.chars().count();
            if left <= n || li == last {
                let col = prefix.get(li).copied().unwrap_or(0).saturating_add(left.min(n));
                let line = lines.get(li).map_or("", String::as_str);
                let byte = line.char_indices().nth(col).map_or(line.len(), |(b, _)| b);
                return glyphon::Cursor::new(li, byte);
            }
            left = left.saturating_sub(n.saturating_add(1));
        }
        glyphon::Cursor::new(0, 0)
    }

    /// Layout cursor → content offset (chars); positions inside a prefix go to the line start.
    fn offset_of(item: &TextItem, c: glyphon::Cursor) -> u32 {
        let (lines, prefix) = Self::display_lines(item);
        let mut off = 0usize;
        for (li, content) in item.content.split('\n').enumerate() {
            let n = content.chars().count();
            if li == c.line {
                let line = lines.get(li).map_or("", String::as_str);
                let col = line.get(..c.index.min(line.len())).map_or(0, |s| s.chars().count());
                let k = col.saturating_sub(prefix.get(li).copied().unwrap_or(0)).min(n);
                return u32::try_from(off.saturating_add(k)).unwrap_or(u32::MAX);
            }
            off = off.saturating_add(n).saturating_add(1);
        }
        u32::try_from(item.content.chars().count()).unwrap_or(u32::MAX)
    }

    /// The laid-out extent of `item`: (widest line, total height) in canvas units; an empty
    /// item is one line high and has no width.
    pub(crate) fn extent(&mut self, item: &TextItem) -> (f64, f64) {
        self.layout(item);
        let line = f64::from(item.size.max(1.0) * LINE);
        let Some((buf, _)) = self.buffers.get(&item.key) else {
            return (0.0, line);
        };
        let (mut w, mut h) = (0.0f64, 0.0f64);
        for r in buf.layout_runs() {
            w = w.max(f64::from(r.line_w));
            h = h.max(f64::from(r.line_top + r.line_height));
        }
        (w, h.max(line))
    }

    /// The caret before content offset `offset`: (x, top, height) in canvas units.
    pub(crate) fn caret(&mut self, item: &TextItem, offset: u32) -> (f64, f64, f64) {
        self.layout(item);
        let fallback = (item.origin.x, item.origin.y, f64::from(item.size.max(1.0) * LINE));
        let Some((buf, _)) = self.buffers.get(&item.key) else {
            return fallback;
        };
        let cur = Self::cursor_of(item, offset);
        let found = buf
            .layout_runs()
            .filter(|r| r.line_i == cur.line)
            .find_map(|r| r.cursor_position(&cur).map(|x| (x, r.line_top, r.line_height)));
        match found {
            Some((x, top, h)) => (
                item.origin.x + f64::from(x),
                item.origin.y + f64::from(top),
                f64::from(h),
            ),
            None => fallback,
        }
    }

    /// The content offset nearest to canvas point `p`.
    pub(crate) fn hit(&mut self, item: &TextItem, p: (f64, f64)) -> u32 {
        self.layout(item);
        let Some((buf, _)) = self.buffers.get(&item.key) else {
            return 0;
        };
        #[allow(clippy::cast_possible_truncation)] // canvas units inside a text box
        let (x, y) = ((p.0 - item.origin.x) as f32, (p.1 - item.origin.y) as f32);
        match buf.hit(x, y) {
            Some(c) => Self::offset_of(item, c),
            // Below the last line: the end.
            None if y > 0.0 => u32::try_from(item.content.chars().count()).unwrap_or(u32::MAX),
            None => 0,
        }
    }

    /// Boxes covering content range `[a, b)`, one per laid-out line: [x0, y0, x1, y1] in
    /// canvas units.
    pub(crate) fn ranges(&mut self, item: &TextItem, a: u32, b: u32) -> Vec<[f64; 4]> {
        self.layout(item);
        let Some((buf, _)) = self.buffers.get(&item.key) else {
            return Vec::new();
        };
        let (ca, cb) = (Self::cursor_of(item, a.min(b)), Self::cursor_of(item, a.max(b)));
        let mut out = Vec::new();
        for r in buf.layout_runs() {
            if r.line_i < ca.line || r.line_i > cb.line {
                continue;
            }
            let mut any = false;
            for (x, w) in r.highlight(ca, cb) {
                any = true;
                out.push([
                    item.origin.x + f64::from(x),
                    item.origin.y + f64::from(r.line_top),
                    item.origin.x + f64::from(x + w),
                    item.origin.y + f64::from(r.line_top + r.line_height),
                ]);
            }
            // An empty line inside the range has no glyphs to highlight: an empty rectangle
            // marks its row.
            if !any && r.glyphs.is_empty() && (r.line_i > ca.line || ca.index == 0) && r.line_i < cb.line {
                let top = item.origin.y + f64::from(r.line_top);
                out.push([
                    item.origin.x,
                    top,
                    item.origin.x,
                    item.origin.y + f64::from(r.line_top + r.line_height),
                ]);
            }
        }
        out
    }

    /// The tinted backgrounds of code blocks (R-TEXT-007) of laid-out items: per laid-out row
    /// of a block, `[x0, y0, x1, y1]` in canvas units across the box's wrap width.
    pub(crate) fn code_backgrounds(&mut self, items: &[TextItem]) -> Vec<[f64; 4]> {
        let mut out = Vec::new();
        for item in items {
            if !item.content.contains("```") {
                continue;
            }
            let blocks = crate::code::blocks(&item.content);
            if blocks.is_empty() {
                continue;
            }
            self.layout(item);
            let Some((buf, _)) = self.buffers.get(&item.key) else {
                continue;
            };
            let pad = f64::from(item.size.max(1.0)) * 0.3;
            for r in buf.layout_runs() {
                if blocks.iter().any(|b| b.first <= r.line_i && r.line_i <= b.last) {
                    out.push([
                        item.origin.x - pad,
                        item.origin.y + f64::from(r.line_top),
                        item.origin.x + item.width + pad,
                        item.origin.y + f64::from(r.line_top + r.line_height),
                    ]);
                }
            }
        }
        out
    }

    /// Underline and strike-through lines of laid-out items, as (from, to, thickness, colour)
    /// in canvas units.
    pub(crate) fn decorations(&mut self, items: &[TextItem]) -> Vec<(Point, Point, f32, Color)> {
        let mut out = Vec::new();
        for item in items {
            self.layout(item);
            let Some((buf, _)) = self.buffers.get(&item.key) else {
                continue;
            };
            for run in buf.layout_runs() {
                for d in run.decorations {
                    let (Some(g0), Some(g1)) = (
                        run.glyphs.get(d.glyph_range.start),
                        run.glyphs.get(d.glyph_range.end.saturating_sub(1)),
                    ) else {
                        continue;
                    };
                    let color = d
                        .color_opt
                        .map_or(item.color, |c| Color(u32::from_be_bytes([c.r(), c.g(), c.b(), c.a()])));
                    let (x0, x1) = (f64::from(g0.x), f64::from(g1.x + g1.w));
                    let td = &d.data.text_decoration;
                    let mut line = |m: &glyphon::cosmic_text::DecorationMetrics| {
                        let y = f64::from(run.line_y - m.offset * d.font_size);
                        let t = (m.thickness * d.font_size).max(0.5);
                        out.push((
                            Point::new(item.origin.x + x0, item.origin.y + y),
                            Point::new(item.origin.x + x1, item.origin.y + y),
                            t,
                            color,
                        ));
                    };
                    if td.underline != UnderlineStyle::None {
                        line(&d.data.underline_metrics);
                    }
                    if td.strikethrough {
                        line(&d.data.strikethrough_metrics);
                    }
                }
            }
        }
        out
    }

    /// Lays out and uploads the glyphs of `items` for a frame at `vp` into `format` targets.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        items: &[TextItem],
        vp: &crate::Viewport,
    ) -> bool {
        self.frame = self.frame.wrapping_add(1);
        let cache = self.cache.get_or_insert_with(|| Cache::new(device));
        if self.gpu.as_ref().is_none_or(|(f, ..)| *f != format) {
            let mode = if format.is_srgb() {
                ColorMode::Accurate
            } else {
                ColorMode::Web
            };
            let mut atlas = TextAtlas::with_color_mode(device, queue, cache, format, mode);
            let renderer = TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
            let viewport = glyphon::Viewport::new(device, cache);
            self.gpu = Some((format, atlas, renderer, viewport));
        }
        let visible: Vec<&TextItem> = items.iter().filter(|i| f64::from(i.size) * vp.zoom >= MIN_PX).collect();
        for i in &visible {
            self.layout(i);
        }
        // Forget layouts not used for a while.
        let frame = self.frame;
        self.buffers.retain(|_, (_, used)| frame.wrapping_sub(*used) < 600);
        let Some((_, atlas, renderer, viewport)) = self.gpu.as_mut() else {
            return false;
        };
        viewport.update(
            queue,
            glyphon::Resolution {
                width: vp.width,
                height: vp.height,
            },
        );
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: i32::try_from(vp.width).unwrap_or(i32::MAX),
            bottom: i32::try_from(vp.height).unwrap_or(i32::MAX),
        };
        #[allow(clippy::cast_possible_truncation)] // screen positions, bounded by the viewport check
        let areas: Vec<TextArea<'_>> = visible
            .iter()
            .filter_map(|i| {
                let (buf, _) = self.buffers.get(&i.key)?;
                let scale = vp.zoom.min(MAX_PX / f64::from(i.size.max(1.0)));
                Some(TextArea {
                    buffer: buf,
                    left: ((i.origin.x - vp.origin_x) * vp.zoom) as f32,
                    top: ((i.origin.y - vp.origin_y) * vp.zoom) as f32,
                    scale: scale as f32,
                    bounds,
                    default_color: gcolor(i.color),
                    custom_glyphs: &[],
                })
            })
            .collect();
        if areas.is_empty() {
            return false;
        }
        renderer
            .prepare(device, queue, &mut self.fonts, atlas, viewport, areas, &mut self.swash)
            .is_ok()
    }

    /// Draws what `prepare` uploaded.
    pub(crate) fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        if let Some((_, atlas, renderer, viewport)) = self.gpu.as_ref() {
            let _ = renderer.render(atlas, viewport, pass);
        }
    }

    /// Frees atlas space not used by the last frame.
    pub(crate) fn trim(&mut self) {
        if let Some((_, atlas, ..)) = self.gpu.as_mut() {
            atlas.trim();
        }
    }
}

/// Results of [`measure`] by item key, and the layout state behind it (no GPU).
struct Measurer {
    state: TextState,
    sizes: HashMap<u64, (f64, f64)>,
}

static MEASURER: std::sync::Mutex<Option<Measurer>> = std::sync::Mutex::new(None);

/// Most remembered measurements (and laid-out buffers) before they are dropped.
const MEASURE_CACHE: usize = 4096;

/// The laid-out extent of a text item, (widest line, height) in canvas units, from the same
/// layout the renderer draws (R-TEXT-004). Cached by the item key.
pub fn measure(item: &TextItem) -> (f64, f64) {
    let mut g = MEASURER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let m = g.get_or_insert_with(|| Measurer {
        state: TextState::new(),
        sizes: HashMap::new(),
    });
    if let Some(s) = m.sizes.get(&item.key) {
        return *s;
    }
    if m.sizes.len() >= MEASURE_CACHE {
        m.sizes.clear();
        m.state.buffers.clear();
    }
    let s = m.state.extent(item);
    // The measuring state keeps no layouts beyond what the cache needs.
    m.state.buffers.remove(&item.key);
    m.sizes.insert(item.key, s);
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// The previous O(n · m) implementation: the reference for the sweep.
    fn spans_reference(item: &TextItem) -> Vec<(String, CharStyle)> {
        let chars: Vec<char> = item.content.chars().collect();
        let base = CharStyle {
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            color: None,
            size: item.size,
            mono: false,
        };
        let mut styles = vec![base; chars.len()];
        for m in &item.marks {
            if m.style.is_paragraph() {
                continue;
            }
            let (a, b) = (
                usize::try_from(m.start).unwrap_or(0),
                usize::try_from(m.end).unwrap_or(0),
            );
            for s in styles.iter_mut().take(b).skip(a) {
                match m.style {
                    TextStyle::Bold => s.bold = true,
                    TextStyle::Italic => s.italic = true,
                    TextStyle::Underline => s.underline = true,
                    TextStyle::Strike => s.strike = true,
                    TextStyle::Color { color } => s.color = Some(color),
                    TextStyle::Size { size } => s.size = size,
                    _ => {}
                }
            }
        }
        let mut out: Vec<(String, CharStyle)> = Vec::new();
        let push = |out: &mut Vec<(String, CharStyle)>, c: char, st: CharStyle| match out.last_mut() {
            Some((s, last)) if *last == st => s.push(c),
            _ => out.push((c.to_string(), st)),
        };
        let mut number = 0u32;
        let mut start = 0usize;
        let lines: Vec<&[char]> = chars.split(|c| *c == '\n').collect();
        let count = lines.len();
        for (li, line) in lines.into_iter().enumerate() {
            let end = start.saturating_add(line.len());
            let touches = |m: &&TextMark| {
                let (a, b) = (
                    usize::try_from(m.start).unwrap_or(0),
                    usize::try_from(m.end).unwrap_or(0),
                );
                // A non-empty line [start, end) is touched by marks overlapping it; an empty line
                // by marks containing its position.
                if start == end {
                    a <= start && b > start
                } else {
                    a < end && b > start
                }
            };
            let para: Vec<TextStyle> = item
                .marks
                .iter()
                .filter(|m| m.style.is_paragraph())
                .filter(touches)
                .map(|m| m.style)
                .collect();
            let heading = para.iter().find_map(|p| match p {
                TextStyle::Heading1 => Some(2.0),
                TextStyle::Heading2 => Some(1.5),
                TextStyle::Heading3 => Some(1.25),
                _ => None,
            });
            let mut prefix = String::new();
            if para.contains(&TextStyle::NumberedList) {
                number = number.saturating_add(1);
                prefix = format!("{number}. ");
            } else {
                number = 0;
                if para.contains(&TextStyle::BulletList) {
                    prefix = "• ".into();
                }
            }
            if let Some(TextStyle::Checkbox { checked }) = para.iter().find(|p| matches!(p, TextStyle::Checkbox { .. }))
            {
                prefix.push_str(if *checked { "☑ " } else { "☐ " });
            }
            let line_style = |s: CharStyle| match heading {
                Some(k) => CharStyle {
                    bold: true,
                    size: s.size * k,
                    ..s
                },
                None => s,
            };
            let first = styles.get(start).copied().unwrap_or(base);
            for c in prefix.chars() {
                push(
                    &mut out,
                    c,
                    line_style(CharStyle {
                        underline: false,
                        strike: false,
                        ..first
                    }),
                );
            }
            for (k, c) in line.iter().enumerate() {
                let st = styles.get(start.saturating_add(k)).copied().unwrap_or(base);
                push(&mut out, *c, line_style(st));
            }
            if li.saturating_add(1) < count {
                push(&mut out, '\n', line_style(first));
            }
            start = end.saturating_add(1);
        }
        out
    }

    fn style_of(k: u8) -> TextStyle {
        match k % 13 {
            0 => TextStyle::Bold,
            1 => TextStyle::Italic,
            2 => TextStyle::Underline,
            3 => TextStyle::Strike,
            4 => TextStyle::Color {
                color: Color(0xff00_00ff),
            },
            5 => TextStyle::Color {
                color: Color(0x00ff_00ff),
            },
            6 => TextStyle::Size { size: 24.0 },
            7 => TextStyle::Heading1,
            8 => TextStyle::Heading2,
            9 => TextStyle::Heading3,
            10 => TextStyle::BulletList,
            11 => TextStyle::NumberedList,
            _ => TextStyle::Checkbox {
                checked: k.is_multiple_of(2),
            },
        }
    }

    proptest::proptest! {
        /// The sweep gives exactly the spans of the previous implementation.
        #[test]
        fn sweep_matches_the_reference(
            content in "[ab\\n]{0,40}",
            marks in proptest::collection::vec((0u32..45, 0u32..45, proptest::num::u8::ANY), 0..12),
        ) {
            let marks: Vec<TextMark> = marks
                .into_iter()
                .map(|(a, b, k)| TextMark { start: a.min(b), end: a.max(b), style: style_of(k) })
                .collect();
            let it = item(&content, marks);
            proptest::prop_assert_eq!(spans(&it), spans_reference(&it));
        }
    }

    #[test]
    fn many_marks_on_long_text_are_fast() {
        // Regression (M6–M9 review): 100k marks over 1M characters took ~1e11 steps.
        let content: String = "abcdefghi\n".repeat(100_000);
        let marks: Vec<TextMark> = (0..100_000u32)
            .map(|k| TextMark {
                start: k,
                end: 1_000_000,
                style: style_of(u8::try_from(k % 13).unwrap()),
            })
            .collect();
        let t = std::time::Instant::now();
        let s = spans(&item(&content, marks));
        assert!(!s.is_empty());
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    }

    fn item(content: &str, marks: Vec<TextMark>) -> TextItem {
        TextItem {
            key: 1,
            origin: Point::new(0.0, 0.0),
            width: 400.0,
            size: 16.0,
            content: content.into(),
            marks,
            color: Color::INK,
            mono: false,
        }
    }

    #[test]
    fn caret_hit_and_ranges_agree_with_the_layout() {
        let mut st = TextState::new();
        let it = TextItem {
            origin: Point::new(100.0, 50.0),
            width: 1000.0,
            ..item(
                "ab\ncd",
                vec![TextMark {
                    start: 3,
                    end: 5,
                    style: TextStyle::BulletList,
                }],
            )
        };
        let (x0, y0, h) = st.caret(&it, 0);
        assert!((x0 - 100.0).abs() < 1e-6 && (y0 - 50.0).abs() < 1e-6 && h > 10.0);
        let (x2, _, _) = st.caret(&it, 2);
        assert!(x2 > x0, "after two letters");
        // The second line has a bullet prefix: its first character is further right.
        let (x3, y3, _) = st.caret(&it, 3);
        assert!(x3 > x0 && y3 > y0, "{x3} {y3}");
        for off in 0..=5 {
            let (x, y, h) = st.caret(&it, off);
            assert_eq!(st.hit(&it, (x + 0.5, y + h / 2.0)), off, "offset {off}");
        }
        assert_eq!(st.hit(&it, (90.0, 51.0)), 0);
        assert_eq!(st.hit(&it, (2000.0, 500.0)), 5, "below: the end");
        let r = st.ranges(&it, 1, 4);
        assert_eq!(r.len(), 2, "one box per line: {r:?}");
    }

    #[test]
    fn a_selected_empty_line_has_a_row() {
        let mut st = TextState::new();
        let it = item("ab\n\ncd", Vec::new());
        // "b", the empty line and "c": three rows, the middle one empty.
        let r = st.ranges(&it, 1, 5);
        assert_eq!(r.len(), 3, "{r:?}");
        assert!(r[0][1] < r[1][1] && r[1][1] < r[2][1], "{r:?}");
        assert!((r[1][2] - r[1][0]).abs() < 1e-9, "{r:?}");
        // A selection ending at the empty line's start does not include it.
        assert_eq!(st.ranges(&it, 0, 3).len(), 1);
    }

    #[test]
    fn paragraph_styles_add_prefixes_per_line() {
        let m = |s, e, style| TextMark {
            start: s,
            end: e,
            style,
        };
        let t = item(
            "Titel\neins\nzwei\nkaufen",
            vec![
                m(0, 5, TextStyle::Heading1),
                m(6, 14, TextStyle::NumberedList),
                m(16, 22, TextStyle::Checkbox { checked: true }),
                m(6, 10, TextStyle::Bold),
            ],
        );
        let sp = spans(&t);
        let text: String = sp.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(text, "Titel\n1. eins\n2. zwei\n☑ kaufen");
        let titel = sp.iter().find(|(s, _)| s.starts_with("Titel")).unwrap();
        assert!(titel.1.bold && (titel.1.size - 32.0).abs() < 1e-6);
        assert!(sp.iter().any(|(s, st)| s.contains("eins") && st.bold));
    }

    #[test]
    fn underline_and_strike_become_lines() {
        let mut st = TextState::new();
        let m = |s, e, style| TextMark {
            start: s,
            end: e,
            style,
        };
        let lines = st.decorations(&[item(
            "unten durch",
            vec![m(0, 5, TextStyle::Underline), m(6, 11, TextStyle::Strike)],
        )]);
        assert_eq!(lines.len(), 2);
        let (u, s) = (&lines[0], &lines[1]);
        assert!(u.0.y > s.0.y, "the underline is below the strike-through");
        assert!(u.1.x < s.0.x + 1.0, "underline covers the first word only");
    }
}
