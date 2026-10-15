//! Pen events and the ink fixture format (v1).
//!
//! A fixture records what the app receives from the Wayland tablet protocol: one JSON object per
//! line. Line 1 is a [`FixtureHeader`]; every further line is a [`PenEvent`]. Coordinates are
//! surface-local logical pixels; `t_us` is microseconds since the first event (monotonic).
//! Replaying a fixture into the engine must be deterministic (same fixture → same state).

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

pub const FIXTURE_FORMAT: &str = "vibenote-ink-fixture";
pub const FIXTURE_VERSION: u32 = 1;
/// Hostile-input bounds for fixture files.
pub const MAX_FIXTURE_EVENTS: usize = 10_000_000;
pub const MAX_LINE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PenTool {
    Pen,
    Eraser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PenEventKind {
    ProximityIn,
    ProximityOut,
    /// Tip contact begins (position/axes are those of this frame).
    Down,
    /// Axes changed (hover or contact).
    Motion,
    /// Tip contact ends.
    Up,
    ButtonDown,
    ButtonUp,
}

/// One tablet-tool frame as received by the app.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PenEvent {
    pub t_us: u64,
    pub kind: PenEventKind,
    pub tool: PenTool,
    /// Surface-local logical pixels.
    pub x: f64,
    pub y: f64,
    /// 0..1
    #[serde(default)]
    pub p: f32,
    /// Degrees.
    #[serde(default)]
    pub tx: f32,
    #[serde(default)]
    pub ty: f32,
    /// 0..1 hover distance.
    #[serde(default)]
    pub d: f32,
    /// Linux button code for button events (BTN_STYLUS = 331, BTN_STYLUS2 = 332).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<u32>,
}

impl PenEvent {
    /// Validates ranges (hostile input).
    pub fn validate(&self) -> Result<(), FixtureError> {
        let ok = self.x.is_finite()
            && self.y.is_finite()
            && self.x.abs() < 1.0e7
            && self.y.abs() < 1.0e7
            && (0.0..=1.0).contains(&self.p)
            && (-90.0..=90.0).contains(&self.tx)
            && (-90.0..=90.0).contains(&self.ty)
            && (0.0..=1.0).contains(&self.d);
        if ok {
            Ok(())
        } else {
            Err(FixtureError::Invalid(format!(
                "event out of range at t_us={}",
                self.t_us
            )))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixtureHeader {
    pub format: String,
    pub version: u32,
    /// Where the events came from: "simtab-generator", "recorder", …
    pub source: String,
    /// Free-form description (pattern, parameters, device).
    #[serde(default)]
    pub note: String,
    /// Surface size in logical pixels and the buffer scale at recording time.
    pub surface: Surface,
    /// Optional labels for eval sets: per stroke index "writing" / "drawing", ground-truth text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fixture {
    pub header: FixtureHeader,
    pub events: Vec<PenEvent>,
}

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {msg}")]
    Parse { line: usize, msg: String },
    #[error("invalid fixture: {0}")]
    Invalid(String),
}

impl Fixture {
    pub fn read(r: impl BufRead) -> Result<Fixture, FixtureError> {
        let mut header: Option<FixtureHeader> = None;
        let mut events = Vec::new();
        let mut last_t = 0u64;
        for (i, line) in r.lines().enumerate() {
            let line = line?;
            let lno = i.saturating_add(1);
            if line.len() > MAX_LINE {
                return Err(FixtureError::Parse {
                    line: lno,
                    msg: "line too long".into(),
                });
            }
            if line.trim().is_empty() {
                continue;
            }
            match &header {
                None => {
                    let h: FixtureHeader = serde_json::from_str(&line).map_err(|e| FixtureError::Parse {
                        line: lno,
                        msg: e.to_string(),
                    })?;
                    if h.format != FIXTURE_FORMAT || h.version != FIXTURE_VERSION {
                        return Err(FixtureError::Invalid(format!(
                            "unsupported {} v{}",
                            h.format, h.version
                        )));
                    }
                    header = Some(h);
                }
                Some(_) => {
                    let e: PenEvent = serde_json::from_str(&line).map_err(|e| FixtureError::Parse {
                        line: lno,
                        msg: e.to_string(),
                    })?;
                    e.validate()?;
                    if e.t_us < last_t {
                        return Err(FixtureError::Parse {
                            line: lno,
                            msg: "t_us decreases".into(),
                        });
                    }
                    last_t = e.t_us;
                    if events.len() >= MAX_FIXTURE_EVENTS {
                        return Err(FixtureError::Invalid("too many events".into()));
                    }
                    events.push(e);
                }
            }
        }
        let header = header.ok_or_else(|| FixtureError::Invalid("empty fixture".into()))?;
        Ok(Fixture { header, events })
    }

    pub fn write(&self, mut w: impl Write) -> Result<(), FixtureError> {
        let h = serde_json::to_string(&self.header).map_err(|e| FixtureError::Invalid(e.to_string()))?;
        writeln!(w, "{h}")?;
        for e in &self.events {
            let l = serde_json::to_string(e).map_err(|e| FixtureError::Invalid(e.to_string()))?;
            writeln!(w, "{l}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: u64, kind: PenEventKind, x: f64) -> PenEvent {
        PenEvent {
            t_us: t,
            kind,
            tool: PenTool::Pen,
            x,
            y: 1.0,
            p: 0.5,
            tx: 0.0,
            ty: 0.0,
            d: 0.0,
            button: None,
        }
    }

    #[test]
    fn roundtrip() {
        let f = Fixture {
            header: FixtureHeader {
                format: FIXTURE_FORMAT.into(),
                version: 1,
                source: "test".into(),
                note: String::new(),
                surface: Surface {
                    width: 100.0,
                    height: 50.0,
                    scale: 1.0,
                },
                labels: None,
            },
            events: vec![
                ev(0, PenEventKind::Down, 1.0),
                ev(5000, PenEventKind::Motion, 2.123456789012345),
                ev(10000, PenEventKind::Up, 3.0),
            ],
        };
        let mut buf = Vec::new();
        f.write(&mut buf).unwrap();
        assert_eq!(Fixture::read(buf.as_slice()).unwrap(), f);
    }

    #[test]
    fn rejects_hostile_input() {
        let h =
            r#"{"format":"vibenote-ink-fixture","version":1,"source":"t","surface":{"width":1,"height":1,"scale":1}}"#;
        for bad in [
            format!("{h}\n{{\"t_us\":0,\"kind\":\"down\",\"tool\":\"pen\",\"x\":1e300,\"y\":0}}"),
            format!(
                "{h}\n{{\"t_us\":5,\"kind\":\"down\",\"tool\":\"pen\",\"x\":0,\"y\":0}}\n{{\"t_us\":1,\"kind\":\"up\",\"tool\":\"pen\",\"x\":0,\"y\":0}}"
            ),
            format!("{h}\n{{\"t_us\":0,\"kind\":\"down\",\"tool\":\"pen\",\"x\":0,\"y\":0,\"p\":7}}"),
            r#"{"format":"other","version":1,"source":"t","surface":{"width":1,"height":1,"scale":1}}"#.to_string(),
            String::new(),
            "x".repeat(MAX_LINE + 1),
        ] {
            assert!(Fixture::read(bad.as_bytes()).is_err(), "{bad:.80}");
        }
    }
}
