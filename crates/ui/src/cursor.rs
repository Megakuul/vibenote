//! System cursors (`wp_cursor_shape_v1`) for the mouse and the pen: what the engine says the
//! pointer would do ([`CursorHint`]) and what the chrome asks for become a cursor shape, or no
//! cursor at all where the canvas draws the tool's footprint itself (ink, eraser; R-PEN-011).

use vibenote_engine::CursorHint;
use vibenote_engine::gesture::Corner;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

/// The cursor to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Shape(Shape),
    /// No system cursor: the canvas draws the footprint.
    Hidden,
}

/// The cursor over the canvas for an engine hint. `space` is Space held (Space+Drag pans).
pub fn for_hint(h: CursorHint, space: bool) -> Cursor {
    if space && h != CursorHint::Grabbing {
        return Cursor::Shape(Shape::Grab);
    }
    Cursor::Shape(match h {
        CursorHint::Ink | CursorHint::Erase => return Cursor::Hidden,
        CursorHint::Default => Shape::Default,
        CursorHint::Lasso | CursorHint::Shape => Shape::Crosshair,
        CursorHint::Text => Shape::Text,
        CursorHint::Move => Shape::Move,
        CursorHint::Scale(Corner::TopLeft | Corner::BottomRight) => Shape::NwseResize,
        CursorHint::Scale(Corner::TopRight | Corner::BottomLeft) => Shape::NeswResize,
        CursorHint::Rotate => Shape::Grab,
        CursorHint::ResizeWidth => Shape::EwResize,
        CursorHint::Grabbing => Shape::Grabbing,
    })
}

/// The cursor over the chrome.
pub fn for_egui(c: egui::CursorIcon) -> Cursor {
    use egui::CursorIcon as C;
    Cursor::Shape(match c {
        C::None => return Cursor::Hidden,
        C::PointingHand => Shape::Pointer,
        C::Text => Shape::Text,
        C::VerticalText => Shape::VerticalText,
        C::Crosshair => Shape::Crosshair,
        C::Move => Shape::Move,
        C::Grab => Shape::Grab,
        C::Grabbing => Shape::Grabbing,
        C::NotAllowed | C::NoDrop => Shape::NotAllowed,
        C::Wait => Shape::Wait,
        C::Progress => Shape::Progress,
        C::Help => Shape::Help,
        C::ContextMenu => Shape::ContextMenu,
        C::Cell => Shape::Cell,
        C::Copy => Shape::Copy,
        C::Alias => Shape::Alias,
        C::AllScroll => Shape::AllScroll,
        C::ZoomIn => Shape::ZoomIn,
        C::ZoomOut => Shape::ZoomOut,
        C::ResizeHorizontal | C::ResizeColumn => Shape::EwResize,
        C::ResizeVertical | C::ResizeRow => Shape::NsResize,
        C::ResizeNeSw => Shape::NeswResize,
        C::ResizeNwSe => Shape::NwseResize,
        C::ResizeEast => Shape::EResize,
        C::ResizeWest => Shape::WResize,
        C::ResizeNorth => Shape::NResize,
        C::ResizeSouth => Shape::SResize,
        C::ResizeNorthEast => Shape::NeResize,
        C::ResizeNorthWest => Shape::NwResize,
        C::ResizeSouthEast => Shape::SeResize,
        C::ResizeSouthWest => Shape::SwResize,
        C::Default => Shape::Default,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_and_eraser_hide_the_system_cursor_for_the_footprint() {
        assert_eq!(for_hint(CursorHint::Ink, false), Cursor::Hidden);
        assert_eq!(for_hint(CursorHint::Erase, false), Cursor::Hidden);
        assert_eq!(for_hint(CursorHint::Text, false), Cursor::Shape(Shape::Text));
        assert_eq!(for_hint(CursorHint::Move, false), Cursor::Shape(Shape::Move));
        assert_eq!(for_hint(CursorHint::Ink, true), Cursor::Shape(Shape::Grab));
        assert_eq!(for_hint(CursorHint::Grabbing, true), Cursor::Shape(Shape::Grabbing));
    }
}
