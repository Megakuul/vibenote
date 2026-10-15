//! JSON-lines control channel (AGENTS.md §2 Phase 3 item 3).
//!
//! One request per line, one response per line:
//! ```text
//! → {"id": 1, "method": "engine.execute", "params": {"cmd": "workspace.note.create", "args": {"name": "n"}}}
//! ← {"id": 1, "ok": true, "result": {"id": 1}}
//! ← {"id": 2, "ok": false, "error": {"error": "unknown_command", "detail": "x"}, "message": "unknown command `x`"}
//! ```
//! Methods: `ping`, `engine.commands`, `engine.execute`, `state.inspect`, plus methods provided
//! by upper layers through [`Extension`] (e.g. `ui.screenshot`, `render.png`).
//! Programmatic calls never open dialogs (commands cannot).

use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use vibenote_engine::{Engine, EngineError};

/// Maximum request line length (hostile-input bound).
pub const MAX_LINE: usize = 64 * 1024 * 1024;

/// Methods contributed by layers above the engine (UI screenshots, headless rendering…).
pub trait Extension {
    /// Returns `None` if the method is not handled by this extension.
    fn call(&mut self, engine: &mut Engine, method: &str, params: &Value) -> Option<Result<Value, EngineError>>;
    fn methods(&self) -> Vec<&'static str>;
}

/// No extensions (headless engine only).
pub struct NoExt;
impl Extension for NoExt {
    fn call(&mut self, _: &mut Engine, _: &str, _: &Value) -> Option<Result<Value, EngineError>> {
        None
    }
    fn methods(&self) -> Vec<&'static str> {
        vec![]
    }
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
struct ExecParams {
    cmd: String,
    #[serde(default)]
    args: Value,
}

#[derive(Deserialize, Default)]
struct InspectParams {
    pointer: Option<String>,
}

const CORE_METHODS: &[&str] = &[
    "ping",
    "engine.commands",
    "engine.execute",
    "state.inspect",
    "control.methods",
];

/// Handles one request line and returns the response line (without newline).
pub fn handle_line(engine: &mut Engine, ext: &mut dyn Extension, line: &str) -> String {
    let resp = match serde_json::from_str::<Request>(line) {
        Err(e) => respond(
            Value::Null,
            Err(EngineError::InvalidParams {
                cmd: "<request>".into(),
                msg: e.to_string(),
            }),
        ),
        Ok(req) => {
            let r = dispatch(engine, ext, &req.method, req.params);
            respond(req.id, r)
        }
    };
    resp.to_string()
}

fn dispatch(engine: &mut Engine, ext: &mut dyn Extension, method: &str, params: Value) -> Result<Value, EngineError> {
    let bad = |m: &str, e: serde_json::Error| EngineError::InvalidParams {
        cmd: m.to_string(),
        msg: e.to_string(),
    };
    match method {
        "ping" => Ok(json!("pong")),
        "engine.commands" => Ok(engine.registry.describe()),
        "control.methods" => {
            let mut m: Vec<&str> = CORE_METHODS.to_vec();
            m.extend(ext.methods());
            Ok(json!(m))
        }
        "engine.execute" => {
            let p: ExecParams = serde_json::from_value(params).map_err(|e| bad(method, e))?;
            engine.execute(&p.cmd, p.args)
        }
        "state.inspect" => {
            let p: InspectParams = if params.is_null() {
                InspectParams::default()
            } else {
                serde_json::from_value(params).map_err(|e| bad(method, e))?
            };
            engine.inspect(p.pointer.as_deref())
        }
        other => ext
            .call(engine, other, &params)
            .unwrap_or_else(|| Err(EngineError::UnknownCommand(format!("method {other}")))),
    }
}

fn respond(id: Value, r: Result<Value, EngineError>) -> Value {
    match r {
        Ok(result) => json!({"id": id, "ok": true, "result": result}),
        Err(e) => {
            json!({"id": id, "ok": false, "error": serde_json::to_value(&e).unwrap_or(Value::Null), "message": e.to_string()})
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Serves requests from `input` until EOF. Blank lines are ignored. Over-long lines get an error
/// response and are skipped.
pub fn serve_lines(
    engine: &mut Engine,
    ext: &mut dyn Extension,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), ServeError> {
    let mut reader = input.take(u64::MAX);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Read at most MAX_LINE + 1 bytes for this line.
        let n = {
            let mut limited = (&mut reader).take(u64::try_from(MAX_LINE).unwrap_or(u64::MAX).saturating_add(1));
            limited.read_until(b'\n', &mut buf)?
        };
        if n == 0 {
            return Ok(());
        }
        let resp = if buf.len() > MAX_LINE {
            // Skip the rest of the over-long line.
            let mut sink = Vec::new();
            if buf.last() != Some(&b'\n') {
                reader.read_until(b'\n', &mut sink)?;
            }
            respond(
                Value::Null,
                Err(EngineError::InvalidParams {
                    cmd: "<request>".into(),
                    msg: "line too long".into(),
                }),
            )
            .to_string()
        } else {
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            handle_line(engine, ext, line)
        };
        output.write_all(resp.as_bytes())?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}

/// Serves a Unix socket; connections are handled one after another (single-threaded engine).
pub fn serve_unix(engine: &mut Engine, ext: &mut dyn Extension, path: &std::path::Path) -> Result<(), ServeError> {
    let _ = std::fs::remove_file(path);
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    for conn in listener.incoming() {
        let conn = conn?;
        let reader = std::io::BufReader::new(conn.try_clone()?);
        serve_lines(engine, ext, reader, conn)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_over_lines() {
        let mut e = Engine::new().unwrap();
        let input = concat!(
            r#"{"id":1,"method":"engine.execute","params":{"cmd":"workspace.note.create","args":{"name":"n"}}}"#,
            "\n",
            "\n",
            r#"{"id":2,"method":"state.inspect","params":{"pointer":"/ws/notes/1/name"}}"#,
            "\n",
            r#"{"id":3,"method":"nope"}"#,
            "\n",
            "not json\n",
        );
        let mut out = Vec::new();
        serve_lines(&mut e, &mut NoExt, input.as_bytes(), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["result"]["id"], 1);
        assert_eq!(lines[1]["result"], "n");
        assert_eq!(lines[2]["ok"], false);
        assert_eq!(lines[3]["ok"], false);
    }
}
