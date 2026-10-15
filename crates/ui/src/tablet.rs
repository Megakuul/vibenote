//! `zwp_tablet_tool_v2` event translation (ADR 0001): protocol events are accumulated per tool
//! and turned into [`PenEvent`]s on each `frame`, in a fixed order:
//! proximity-in → button changes → down | motion → up → proximity-out. This matches the ink
//! fixture format v1 (which models libinput frames, `tools/simtab` `fixture`).
//!
//! Pure (no Wayland types) so it is testable without a compositor. The Wayland dispatch code
//! converts protocol events into [`ToolEvent`] and passes the receive time (CLOCK_MONOTONIC µs).
//!
//! Every axis change (position, pressure, tilt, distance) in a frame yields one sample, so no
//! sample delivered by the compositor is dropped (R-PEN-004), including pressure-only updates.

use vibenote_ink::input::{PenEvent, PenEventKind, PenTool};

/// One `zwp_tablet_tool_v2` event, with protocol units already decoded by the caller only where
/// the protocol itself is typed (wl_fixed → f64). Raw integer axes stay raw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToolEvent {
    /// `type`: physical tool type (pen-like types map to [`PenTool::Pen`]).
    Type(PenTool),
    ProximityIn,
    ProximityOut,
    Down,
    Up,
    /// Surface-local logical pixels.
    Motion {
        x: f64,
        y: f64,
    },
    /// 0..=65535.
    Pressure(u32),
    /// 0..=65535.
    Distance(u32),
    /// Degrees.
    Tilt {
        x: f64,
        y: f64,
    },
    Button {
        button: u32,
        pressed: bool,
    },
    /// End of a logical group; `time_ms` is the protocol timestamp (diagnostics only).
    Frame {
        time_ms: u32,
    },
    /// The tool was removed (unplugged); ends any contact.
    Removed,
}

const AXIS_MAX: f32 = 65535.0;

#[derive(Clone, Debug, Default)]
struct Pending {
    proximity_in: bool,
    proximity_out: bool,
    down: bool,
    up: bool,
    axes: bool,
    buttons: Vec<(u32, bool)>,
}

/// Per-tool translator state.
#[derive(Clone, Debug)]
pub struct ToolTranslator {
    tool: PenTool,
    x: f64,
    y: f64,
    p: f32,
    tx: f32,
    ty: f32,
    d: f32,
    in_contact: bool,
    in_proximity: bool,
    pending: Pending,
    /// Last protocol frame time (diagnostics; e.g. comparing with libinput timestamps).
    pub last_frame_ms: Option<u32>,
}

impl Default for ToolTranslator {
    fn default() -> Self {
        ToolTranslator::new(PenTool::Pen)
    }
}

/// Maximum button changes kept per frame (hostile/broken compositor bound).
const MAX_BUTTONS_PER_FRAME: usize = 32;

fn unit(v: u32) -> f32 {
    // u32 → f32 loses precision only above 2^24; axis values are ≤ 65535 after the clamp.
    #[allow(clippy::cast_precision_loss)]
    let f = v.min(65535) as f32;
    (f / AXIS_MAX).clamp(0.0, 1.0)
}

fn degrees(v: f64) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let f = v.clamp(-90.0, 90.0) as f32;
    f
}

impl ToolTranslator {
    pub fn new(tool: PenTool) -> Self {
        ToolTranslator {
            tool,
            x: 0.0,
            y: 0.0,
            p: 0.0,
            tx: 0.0,
            ty: 0.0,
            d: 0.0,
            in_contact: false,
            in_proximity: false,
            pending: Pending::default(),
            last_frame_ms: None,
        }
    }

    pub fn tool(&self) -> PenTool {
        self.tool
    }

    pub fn in_contact(&self) -> bool {
        self.in_contact
    }

    /// Feeds one protocol event. Returns the pen events completed by it (non-empty only on
    /// `Frame` and `Removed`). `now_us` is the receive time used as the events' timestamp.
    pub fn feed(&mut self, ev: ToolEvent, now_us: u64) -> Vec<PenEvent> {
        match ev {
            ToolEvent::Type(t) => self.tool = t,
            ToolEvent::ProximityIn => self.pending.proximity_in = true,
            ToolEvent::ProximityOut => self.pending.proximity_out = true,
            ToolEvent::Down => self.pending.down = true,
            ToolEvent::Up => self.pending.up = true,
            ToolEvent::Motion { x, y } => {
                if x.is_finite() && y.is_finite() {
                    self.x = x;
                    self.y = y;
                    self.pending.axes = true;
                }
            }
            ToolEvent::Pressure(v) => {
                self.p = unit(v);
                self.pending.axes = true;
            }
            ToolEvent::Distance(v) => {
                self.d = unit(v);
                self.pending.axes = true;
            }
            ToolEvent::Tilt { x, y } => {
                self.tx = degrees(x);
                self.ty = degrees(y);
                self.pending.axes = true;
            }
            ToolEvent::Button { button, pressed } => {
                if self.pending.buttons.len() < MAX_BUTTONS_PER_FRAME {
                    self.pending.buttons.push((button, pressed));
                }
            }
            ToolEvent::Frame { time_ms } => {
                self.last_frame_ms = Some(time_ms);
                return self.frame(now_us);
            }
            ToolEvent::Removed => {
                self.pending.proximity_out = true;
                return self.frame(now_us);
            }
        }
        Vec::new()
    }

    fn event(&self, t_us: u64, kind: PenEventKind) -> PenEvent {
        PenEvent {
            t_us,
            kind,
            tool: self.tool,
            x: self.x,
            y: self.y,
            p: self.p,
            tx: self.tx,
            ty: self.ty,
            d: self.d,
            button: None,
        }
    }

    fn frame(&mut self, t: u64) -> Vec<PenEvent> {
        let pend = std::mem::take(&mut self.pending);
        let mut out = Vec::new();
        if pend.proximity_in && !self.in_proximity {
            self.in_proximity = true;
            out.push(self.event(t, PenEventKind::ProximityIn));
        }
        for (button, pressed) in pend.buttons {
            let kind = if pressed {
                PenEventKind::ButtonDown
            } else {
                PenEventKind::ButtonUp
            };
            out.push(PenEvent {
                button: Some(button),
                ..self.event(t, kind)
            });
        }
        if pend.down && !self.in_contact {
            self.in_contact = true;
            out.push(self.event(t, PenEventKind::Down));
        } else if pend.axes && !(pend.up && self.in_contact) {
            // Down and up frames carry their axes themselves; any other frame with axis
            // changes (hover, contact, proximity-in, button) yields one motion sample.
            out.push(self.event(t, PenEventKind::Motion));
        }
        // A proximity-out (or tool removal) while touching must never leave a stroke open.
        if (pend.up || pend.proximity_out) && self.in_contact {
            self.in_contact = false;
            out.push(self.event(t, PenEventKind::Up));
        }
        if pend.proximity_out && self.in_proximity {
            self.in_proximity = false;
            out.push(self.event(t, PenEventKind::ProximityOut));
        }
        out
    }
}

/// Inverse of [`ToolTranslator`] for tests and tools: the protocol frames a compositor would
/// send for a sequence of pen events (ink fixture format v1). A proximity-in or button event
/// followed by a motion with the same timestamp is one frame, as libinput delivers it.
pub fn protocol_frames(events: &[PenEvent]) -> Vec<(u64, Vec<ToolEvent>)> {
    let raw = |v: f32| -> u32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let r = (v.clamp(0.0, 1.0) * AXIS_MAX).round() as u32;
        r
    };
    let axes = |e: &PenEvent| {
        [
            ToolEvent::Motion { x: e.x, y: e.y },
            ToolEvent::Pressure(raw(e.p)),
            ToolEvent::Distance(raw(e.d)),
            ToolEvent::Tilt {
                x: f64::from(e.tx),
                y: f64::from(e.ty),
            },
        ]
    };
    let mut frames = Vec::new();
    let mut it = events.iter().peekable();
    while let Some(first) = it.next() {
        let t = first.t_us;
        let mut v = Vec::new();
        // Leading proximity-in / button changes, then at most one down | up | motion, all with
        // the same timestamp, form one frame.
        let mut e = Some(first);
        while let Some(x) = e {
            match x.kind {
                PenEventKind::ProximityIn => v.push(ToolEvent::ProximityIn),
                PenEventKind::ButtonDown | PenEventKind::ButtonUp => v.push(ToolEvent::Button {
                    button: x.button.unwrap_or(0),
                    pressed: x.kind == PenEventKind::ButtonDown,
                }),
                _ => break,
            }
            e = it.next_if(|n| n.t_us == t);
        }
        if let Some(x) = e {
            match x.kind {
                PenEventKind::ProximityOut => v.push(ToolEvent::ProximityOut),
                PenEventKind::Down => {
                    v.push(ToolEvent::Down);
                    v.extend(axes(x));
                }
                PenEventKind::Up => {
                    v.push(ToolEvent::Up);
                    v.extend(axes(x));
                }
                PenEventKind::Motion => v.extend(axes(x)),
                PenEventKind::ProximityIn | PenEventKind::ButtonDown | PenEventKind::ButtonUp => {}
            }
        }
        v.push(ToolEvent::Frame { time_ms: 0 });
        frames.push((t, v));
    }
    frames
}
