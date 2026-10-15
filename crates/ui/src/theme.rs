//! Theme tokens (AGENTS.md §4, ADR 0002 decision 5): the chrome's colours, spacing and fonts in
//! one place. Fonts are the bundled Inter only (owner decision: no egui default fonts), plus a
//! subset of the Lucide icon font (ISC; ATTRIBUTION.md) for the toolbar and the tree.

use egui::{Color32, FontData, FontDefinitions, FontFamily};

/// Model colours are 0xRRGGBBAA.
pub fn rgba(c: u32) -> Color32 {
    let [r, g, b, a] = c.to_be_bytes();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

pub const PANEL: Color32 = Color32::from_rgb(0xf3, 0xf1, 0xec);
pub const PANEL_EDGE: Color32 = Color32::from_rgb(0xd9, 0xd5, 0xcc);
pub const TEXT: Color32 = Color32::from_rgb(0x2b, 0x2d, 0x33);
pub const ACCENT: Color32 = Color32::from_rgb(0x2a, 0x6f, 0xdb);
pub const NO_COLOR: Color32 = Color32::from_rgb(0xc8, 0xc4, 0xbb);
/// Secondary text and icons (tree icons without a colour, hints).
pub const MUTED: Color32 = Color32::from_rgb(0x7a, 0x7d, 0x86);
/// Row hover and the selected row / pressed tool.
pub const HOVER: Color32 = Color32::from_rgb(0xe8, 0xe5, 0xde);
pub const SELECTED: Color32 = Color32::from_rgb(0xdc, 0xe6, 0xf7);
/// Indentation guides in the tree.
pub const GUIDE: Color32 = Color32::from_rgb(0xdd, 0xd9, 0xd0);
/// The canvas chip that offers prettify (suggest mode).
pub const CHIP: Color32 = Color32::from_rgb(0xff, 0xfd, 0xf7);
/// Icon size in the toolbar and in tree rows (logical px).
pub const ICON: f32 = 18.0;
pub const TREE_ICON: f32 = 15.0;
/// Find highlights over the canvas (translucent).
pub const HIGHLIGHT: Color32 = Color32::from_rgba_premultiplied(0x7a, 0x66, 0x00, 0x7f);
/// Folder and note colours offered in the menu (0xRRGGBBAA).
pub const PALETTE: [u32; 8] = [
    0xd660_4dff,
    0xe09a_3aff,
    0xc8b2_3aff,
    0x6eaa_64ff,
    0x3f9a_9aff,
    0x5a8c_d2ff,
    0x8a6c_c8ff,
    0x9a9a_9aff,
];

/// The Lucide icons the chrome uses (subset: `ATTRIBUTION.md` lists the command that made it;
/// a code point not in the subset draws nothing).
static ICONS: &[u8] = include_bytes!("../../../assets/fonts/LucideIcons-subset.ttf");

/// Lucide code points (private use area), drawn with any chrome text.
pub mod icon {
    pub const SIDEBAR: &str = "\u{e12a}";
    pub const PEN: &str = "\u{e130}";
    pub const HIGHLIGHTER: &str = "\u{e0f4}";
    pub const ERASER: &str = "\u{e28f}";
    pub const LASSO: &str = "\u{e1ce}";
    pub const SHAPES: &str = "\u{e4b3}";
    pub const TEXT: &str = "\u{e198}";
    pub const UNDO: &str = "\u{e2a1}";
    pub const REDO: &str = "\u{e2a0}";
    pub const ZOOM_IN: &str = "\u{e1b6}";
    pub const ZOOM_OUT: &str = "\u{e1b7}";
    pub const FIT: &str = "\u{e257}";
    pub const MIC: &str = "\u{e118}";
    pub const STOP: &str = "\u{e167}";
    pub const SEARCH: &str = "\u{e151}";
    pub const SETTINGS: &str = "\u{e154}";
    pub const LINE: &str = "\u{e11c}";
    pub const ARROW: &str = "\u{e04d}";
    pub const RECTANGLE: &str = "\u{e167}";
    pub const ELLIPSE: &str = "\u{e076}";
    pub const TRIANGLE: &str = "\u{e192}";
    pub const POLYGON: &str = "\u{e52b}";
    pub const BOLD: &str = "\u{e05d}";
    pub const ITALIC: &str = "\u{e0fb}";
    pub const UNDERLINE: &str = "\u{e19a}";
    pub const STRIKE: &str = "\u{e177}";
    pub const H1: &str = "\u{e385}";
    pub const H2: &str = "\u{e386}";
    pub const H3: &str = "\u{e387}";
    pub const BULLETS: &str = "\u{e106}";
    pub const NUMBERS: &str = "\u{e1d1}";
    pub const CHECKS: &str = "\u{e1d0}";
    pub const FOLDER: &str = "\u{e0d7}";
    pub const FOLDER_OPEN: &str = "\u{e247}";
    pub const NOTE: &str = "\u{e0cc}";
    pub const CHEVRON_RIGHT: &str = "\u{e06f}";
    pub const CHEVRON_DOWN: &str = "\u{e06d}";
    pub const NEW_FOLDER: &str = "\u{e0d9}";
    pub const NEW_NOTE: &str = "\u{e0c9}";
    pub const TRASH: &str = "\u{e18e}";
    pub const SPARKLES: &str = "\u{e412}";
    pub const CLOSE: &str = "\u{e1b2}";
    pub const CHECK: &str = "\u{e06c}";
    pub const COPY: &str = "\u{e09e}";
    pub const CUT: &str = "\u{e14e}";
    pub const INSERT_IMAGE: &str = "\u{e1f7}";
    pub const PASTE: &str = "\u{e3e8}";
}

/// The bundled fonts (shared with the canvas text renderer) and the icons.
fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::empty();
    f.font_data.insert(
        "Inter".into(),
        std::sync::Arc::new(FontData::from_static(vibenote_render::fonts::INTER)),
    );
    f.font_data
        .insert("Lucide".into(), std::sync::Arc::new(FontData::from_static(ICONS)));
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        // Icons first: the subset has only private-use code points, where Inter has glyphs
        // of its own.
        let list = f.families.entry(fam).or_default();
        list.push("Lucide".into());
        list.push("Inter".into());
    }
    f
}

/// Applies fonts and visuals to a context.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Light);
    ctx.style_mut_of(egui::Theme::Light, |s| {
        let v = &mut s.visuals;
        *v = egui::Visuals::light();
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.override_text_color = Some(TEXT);
        // Opaque fills only: translucent ones came out dark on the window surface.
        v.selection.bg_fill = SELECTED;
        v.selection.stroke.color = ACCENT;
        v.widgets.noninteractive.bg_stroke.color = PANEL_EDGE;
        v.widgets.inactive.weak_bg_fill = PANEL;
        v.widgets.inactive.bg_fill = PANEL;
        v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        v.widgets.hovered.weak_bg_fill = HOVER;
        v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
        v.widgets.active.weak_bg_fill = SELECTED;
        v.widgets.active.bg_stroke = egui::Stroke::NONE;
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = egui::CornerRadius::same(6);
        }
        v.window_corner_radius = egui::CornerRadius::same(10);
        v.window_stroke = egui::Stroke::new(1.0, PANEL_EDGE);
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
    });
}
