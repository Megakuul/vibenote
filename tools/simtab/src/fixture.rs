//! Stroke file → ink fixture, emulating what the app receives through libinput + the compositor.
//!
//! Emulated libinput behaviour (measured in Phase 0, `docs/spikes/input-results.md` §1, and
//! libinput `src/evdev-tablet.c`):
//! - device pressure is quantised to 8,192 levels (simtab's uinput device);
//! - tip contact starts at raw pressure ≥ 5 % and ends at ≤ 1 %;
//! - reported pressure is rescaled so the 1 % lower threshold is 0;
//! - barrel buttons arrive as separate button events (BTN_STYLUS 331, BTN_STYLUS2 332).

use crate::stroke::{Strokes, Tool};
use vibenote_ink::input::{
    FIXTURE_FORMAT, FIXTURE_VERSION, Fixture, FixtureHeader, PenEvent, PenEventKind, PenTool, Surface,
};

pub const TIP_DOWN: f64 = 0.05;
pub const TIP_UP: f64 = 0.01;
pub const PRESSURE_LEVELS: f64 = 8191.0;

fn quantise(p: f64) -> f64 {
    (p.clamp(0.0, 1.0) * PRESSURE_LEVELS).round() / PRESSURE_LEVELS
}

/// Lossy by design: f64 → f32 for axis values the protocol carries as fixed-point anyway.
#[allow(clippy::cast_possible_truncation)]
fn f32_of(x: f64) -> f32 {
    x as f32
}

pub fn to_fixture(s: &Strokes, surface: Surface, note: &str) -> Fixture {
    let mut events = Vec::with_capacity(s.samples.len());
    let mut tool: Option<Tool> = None;
    let mut contact = false;
    let (mut b1, mut b2) = (false, false);
    let push = |events: &mut Vec<PenEvent>, t: u64, kind, tl: Tool, x, y, p, tx, ty, d, button| {
        let tool = match tl {
            Tool::Pen => PenTool::Pen,
            Tool::Eraser => PenTool::Eraser,
        };
        events.push(PenEvent {
            t_us: t,
            kind,
            tool,
            x,
            y,
            p: f32_of(p),
            tx: f32_of(tx),
            ty: f32_of(ty),
            d: f32_of(d),
            button,
        });
    };
    let mut last = (0.0, 0.0, 0.0, 0.0);
    for smp in &s.samples {
        let (x, y) = (
            smp.x.clamp(0.0, 1.0) * surface.width,
            smp.y.clamp(0.0, 1.0) * surface.height,
        );
        // tool change or leaving proximity
        if let Some(prev) = tool
            && smp.tool != Some(prev)
        {
            if contact {
                push(
                    &mut events,
                    smp.t_us,
                    PenEventKind::Up,
                    prev,
                    last.0,
                    last.1,
                    0.0,
                    last.2,
                    last.3,
                    0.0,
                    None,
                );
                contact = false;
            }
            push(
                &mut events,
                smp.t_us,
                PenEventKind::ProximityOut,
                prev,
                last.0,
                last.1,
                0.0,
                last.2,
                last.3,
                1.0,
                None,
            );
            tool = None;
            (b1, b2) = (false, false);
        }
        let Some(tl) = smp.tool else { continue };
        let raw = quantise(smp.p);
        let p = ((raw - TIP_UP) / (1.0 - TIP_UP)).clamp(0.0, 1.0);
        if tool.is_none() {
            push(
                &mut events,
                smp.t_us,
                PenEventKind::ProximityIn,
                tl,
                x,
                y,
                0.0,
                smp.tx,
                smp.ty,
                smp.d,
                None,
            );
            tool = Some(tl);
        }
        for (now, prev_b, code) in [(smp.b1, &mut b1, 331u32), (smp.b2, &mut b2, 332u32)] {
            if now != *prev_b {
                let k = if now {
                    PenEventKind::ButtonDown
                } else {
                    PenEventKind::ButtonUp
                };
                push(
                    &mut events,
                    smp.t_us,
                    k,
                    tl,
                    x,
                    y,
                    if contact { p } else { 0.0 },
                    smp.tx,
                    smp.ty,
                    smp.d,
                    Some(code),
                );
                *prev_b = now;
            }
        }
        let kind = if !contact && raw >= TIP_DOWN {
            contact = true;
            PenEventKind::Down
        } else if contact && raw <= TIP_UP {
            contact = false;
            PenEventKind::Up
        } else {
            PenEventKind::Motion
        };
        let d = if contact { 0.0 } else { smp.d.clamp(0.0, 1.0) };
        push(
            &mut events,
            smp.t_us,
            kind,
            tl,
            x,
            y,
            if contact || kind == PenEventKind::Up { p } else { 0.0 },
            smp.tx,
            smp.ty,
            d,
            None,
        );
        last = (x, y, smp.tx, smp.ty);
    }
    if let Some(tl) = tool {
        let t = events.last().map_or(0, |e| e.t_us);
        if contact {
            push(
                &mut events,
                t,
                PenEventKind::Up,
                tl,
                last.0,
                last.1,
                0.0,
                last.2,
                last.3,
                0.0,
                None,
            );
        }
        push(
            &mut events,
            t,
            PenEventKind::ProximityOut,
            tl,
            last.0,
            last.1,
            0.0,
            last.2,
            last.3,
            1.0,
            None,
        );
    }
    Fixture {
        header: FixtureHeader {
            format: FIXTURE_FORMAT.into(),
            version: FIXTURE_VERSION,
            source: "simtab-generator".into(),
            note: note.into(),
            surface,
            labels: None,
        },
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{GenOpts, Pattern, generate};

    fn count(f: &Fixture, k: PenEventKind) -> usize {
        f.events.iter().filter(|e| e.kind == k).count()
    }

    #[test]
    fn libinput_thresholds_and_balance() {
        let surf = Surface {
            width: 1200.0,
            height: 800.0,
            scale: 1.0,
        };
        let s = generate(&GenOpts {
            pattern: Pattern::PressureRamp,
            strokes: 2,
            ..Default::default()
        })
        .unwrap();
        let f = to_fixture(&s, surf, "t");
        assert_eq!(count(&f, PenEventKind::Down), 2);
        assert_eq!(count(&f, PenEventKind::Up), 2);
        assert_eq!(count(&f, PenEventKind::ProximityIn), 2);
        assert_eq!(count(&f, PenEventKind::ProximityOut), 2);
        // first down happens at ≥ 5 % raw pressure, reported pressure is rescaled (≥ ~4 %)
        let d = f.events.iter().find(|e| e.kind == PenEventKind::Down).unwrap();
        assert!(d.p > 0.03 && d.p < 0.1, "{d:?}");
        for e in &f.events {
            e.validate().unwrap();
            assert!(e.x <= 1200.0 && e.y <= 800.0);
        }
    }

    #[test]
    fn eraser_flip_produces_eraser_tool_and_buttons_produce_codes() {
        let surf = Surface {
            width: 100.0,
            height: 100.0,
            scale: 1.0,
        };
        let f = to_fixture(
            &generate(&GenOpts {
                pattern: Pattern::EraserFlip,
                strokes: 2,
                ..Default::default()
            })
            .unwrap(),
            surf,
            "t",
        );
        assert!(
            f.events
                .iter()
                .any(|e| e.tool == PenTool::Eraser && e.kind == PenEventKind::Down)
        );
        let f = to_fixture(
            &generate(&GenOpts {
                pattern: Pattern::Buttons,
                strokes: 1,
                ..Default::default()
            })
            .unwrap(),
            surf,
            "t",
        );
        assert!(f.events.iter().any(|e| e.button == Some(331)));
        assert!(f.events.iter().any(|e| e.button == Some(332)));
    }
}
