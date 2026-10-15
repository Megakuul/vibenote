//! vibenote-cli — headless access to the engine (AGENTS.md §2 Phase 3 item 3).
//!
//!   vibenote-cli commands                      list commands (id, reqs, implemented, params schema)
//!   vibenote-cli run <cmd> [json-args]...      run commands in sequence on a fresh in-memory session;
//!                                              args come in pairs: `run a '{..}' b '{..}'`
//!   vibenote-cli script [file|-]               JSON-lines control requests → responses (stdout)
//!   vibenote-cli serve --socket <path>         serve the control channel on a Unix socket
//!   vibenote-cli inspect [pointer] [--script f] run a script, then print state (optionally a JSON pointer)
//!   vibenote-cli replay <fixture>              replay an ink fixture into a new note, print the state summary
//!   vibenote-cli render-png <fixture> <out.png> [--size WxH] [--origin X,Y] [--zoom Z]
//!                                              [--background KIND[:SPACING]] [--run CMD ARGS]…
//!                                              replay + headless render of the note
//! Persistence (workspace paths) arrives with the storage milestone.

mod headless;

use std::io::{BufReader, Write};
use std::process::ExitCode;
use vibenote_control::{NoExt, serve_lines, serve_unix};
use vibenote_engine::Engine;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vibenote-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn out(s: &str) -> Result<(), String> {
    let mut o = std::io::stdout().lock();
    writeln!(o, "{s}").map_err(|e| e.to_string())
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Global option: --workspace <db> (persistent; otherwise in-memory).
    let mut engine = match args.iter().position(|a| a == "--workspace") {
        Some(i) => {
            let p = args
                .get(i.saturating_add(1))
                .cloned()
                .ok_or("--workspace needs a path")?;
            args.drain(i..i.saturating_add(2));
            // Tools see the whole workspace (the app loads canvases lazily, R-PERF-004). The
            // crash harness writes like the app does: lazily, into a partially loaded note.
            let mut e = Engine::open(std::path::Path::new(&p)).map_err(|e| e.to_string())?;
            if args.first().map(String::as_str) != Some("crash-writer") {
                e.load_all().map_err(|e| e.to_string())?;
            }
            e
        }
        None => Engine::new().map_err(|e| e.to_string())?,
    };
    let mut it = args.iter().map(String::as_str);
    let r = match it.next() {
        Some("commands") => out(&serde_json::to_string_pretty(&engine.registry.describe()).map_err(|e| e.to_string())?),
        Some("run") => {
            let rest: Vec<&str> = it.collect();
            let mut i = 0;
            while let Some(cmd) = rest.get(i) {
                let (params, step) = match rest.get(i.saturating_add(1)) {
                    Some(a) if a.trim_start().starts_with('{') || *a == "null" => {
                        (serde_json::from_str(a).map_err(|e| format!("args for {cmd}: {e}"))?, 2)
                    }
                    _ => (serde_json::Value::Null, 1),
                };
                let r = engine.execute(cmd, params).map_err(|e| e.to_string())?;
                out(&r.to_string())?;
                i = i.saturating_add(step);
            }
            Ok(())
        }
        Some("script") => {
            let stdout = std::io::stdout();
            match it.next() {
                None | Some("-") => serve_lines(&mut engine, &mut NoExt, std::io::stdin().lock(), stdout.lock()),
                Some(f) => {
                    let file = std::fs::File::open(f).map_err(|e| format!("{f}: {e}"))?;
                    serve_lines(&mut engine, &mut NoExt, BufReader::new(file), stdout.lock())
                }
            }
            .map_err(|e| e.to_string())
        }
        Some("serve") => match (it.next(), it.next()) {
            (Some("--socket"), Some(p)) => {
                serve_unix(&mut engine, &mut NoExt, std::path::Path::new(p)).map_err(|e| e.to_string())
            }
            _ => Err("usage: serve --socket <path>".into()),
        },
        Some("inspect") => {
            let mut pointer = None;
            let mut script = None;
            while let Some(a) = it.next() {
                if a == "--script" {
                    script = it.next();
                } else {
                    pointer = Some(a);
                }
            }
            if let Some(f) = script {
                let file = std::fs::File::open(f).map_err(|e| format!("{f}: {e}"))?;
                serve_lines(&mut engine, &mut NoExt, BufReader::new(file), std::io::sink())
                    .map_err(|e| e.to_string())?;
            }
            let v = engine.inspect(pointer).map_err(|e| e.to_string())?;
            out(&serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?)
        }
        Some("replay") => {
            let f = headless::read_fixture(std::path::Path::new(it.next().ok_or("usage: replay <fixture>")?))?;
            let (e, note) = headless::replay_fixture(&f)?;
            let objs = e
                .inspect(Some(&format!("/ws/canvases/{note}/objects")))
                .map_err(|x| x.to_string())?;
            let n = objs.as_object().map_or(0, serde_json::Map::len);
            out(&format!(
                "{{\"note\":{note},\"objects\":{n},\"events\":{}}}",
                f.events.len()
            ))
        }
        // Features of writing/drawing blocks (`{"strokes": [[[x, y], …], …]}` per line) for the
        // classifier trainer in tools/hwrtrain: one JSON array per line (null for empty blocks).
        Some("hwr-features") => {
            let path = it.next().ok_or("usage: hwr-features <blocks.jsonl>")?;
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            let mut lines_out = String::new();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let v: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
                let strokes: Vec<Vec<(f64, f64)>> =
                    serde_json::from_value(v.get("strokes").cloned().unwrap_or_default()).map_err(|e| e.to_string())?;
                let f = vibenote_hwr::features::features(&strokes);
                lines_out.push_str(&serde_json::to_string(&f.map(|a| a.to_vec())).map_err(|e| e.to_string())?);
                lines_out.push('\n');
            }
            out(lines_out.trim_end())
        }
        Some("render-png") => {
            let fixture = it
                .next()
                .ok_or("usage: render-png <fixture> <out.png> [--size WxH] [--origin X,Y] [--zoom Z] [--background KIND[:SPACING]]")?;
            let outp = it.next().ok_or("missing <out.png>")?;
            let mut vp = vibenote_render::Viewport {
                origin_x: 0.0,
                origin_y: 0.0,
                zoom: 1.0,
                width: 1200,
                height: 800,
            };
            let mut background = None;
            let mut extra: Vec<(String, serde_json::Value)> = Vec::new();
            while let Some(a) = it.next() {
                if a == "--run" {
                    let cmd = it.next().ok_or("--run needs CMD ARGS")?;
                    let args = it.next().ok_or("--run needs CMD ARGS")?;
                    let args = serde_json::from_str(args).map_err(|e| format!("--run {cmd}: {e}"))?;
                    extra.push((cmd.to_string(), args));
                    continue;
                }
                let v = it.next().ok_or(format!("{a} needs a value"))?;
                match a {
                    "--size" => {
                        let (w, h) = v.split_once('x').ok_or("size WxH")?;
                        vp.width = w.parse().map_err(|_| "bad width")?;
                        vp.height = h.parse().map_err(|_| "bad height")?;
                    }
                    "--origin" => {
                        let (x, y) = v.split_once(',').ok_or("origin X,Y")?;
                        vp.origin_x = x.parse().map_err(|_| "bad x")?;
                        vp.origin_y = y.parse().map_err(|_| "bad y")?;
                    }
                    "--zoom" => vp.zoom = v.parse().map_err(|_| "bad zoom")?,
                    "--background" => {
                        let (kind, spacing) = v.split_once(':').map_or((v, None), |(k, s)| (k, Some(s)));
                        let mut args = serde_json::json!({ "kind": kind });
                        if let Some(s) = spacing {
                            let s: f64 = s.parse().map_err(|_| "bad spacing")?;
                            args = serde_json::json!({ "kind": kind, "spacing": s });
                        }
                        background = Some(args);
                    }
                    _ => return Err(format!("unknown option {a}")),
                }
            }
            let f = headless::read_fixture(std::path::Path::new(fixture))?;
            let (mut e, note) = headless::replay_fixture(&f)?;
            if let Some(args) = background {
                e.execute("canvas.set_background", args).map_err(|x| x.to_string())?;
            }
            for (cmd, args) in extra {
                e.execute(&cmd, args).map_err(|x| format!("{cmd}: {x}"))?;
            }
            let mut r = vibenote_render::Headless::new().map_err(|x| x.to_string())?;
            let img = headless::render_note(&mut r, &e, note, &vp)?;
            std::fs::write(outp, img.encode_png().map_err(|x| x.to_string())?).map_err(|x| format!("{outp}: {x}"))?;
            out(&format!(
                "wrote {outp} ({}x{}, adapter {})",
                img.width, img.height, r.adapter
            ))
        }
        // Test helper (crash harness): commits strokes forever on --workspace, printing
        // `ACK <id>` only after each commit is durable (`sync`).
        Some("crash-writer") => {
            let note = match engine.session.ws.notes.keys().next() {
                Some(n) => n.0,
                None => engine
                    .execute("workspace.note.create", serde_json::json!({"name": "crash"}))
                    .map_err(|e| e.to_string())?
                    .get("id")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("no id")?,
            };
            // A small first viewport: most of a reopened note streams in while we write.
            engine
                .execute("note.open", serde_json::json!({"id": note, "viewport": [50.0, 50.0]}))
                .map_err(|e| e.to_string())?;
            let mut k = 0u64;
            loop {
                k = k.wrapping_add(1);
                let x = f64::from(u32::try_from(k % 1000).unwrap_or(0));
                let r = engine
                    .execute("ink.stroke.add", serde_json::json!({"note": note, "samples": [{"x": x, "y": 1.5, "p": 0.4}, {"x": x + 7.25, "y": 9.0, "p": 0.8, "t_us": 4000}]}))
                    .map_err(|e| e.to_string())?;
                let id = r.get("id").and_then(serde_json::Value::as_u64).ok_or("no id")?;
                engine.sync().map_err(|e| e.to_string())?;
                out(&format!("ACK {id}"))?;
            }
        }
        _ => {
            Err("usage: vibenote-cli [--workspace <db>] <commands|run|script|serve|inspect|replay|render-png> …".into())
        }
    };
    // Everything the commands changed is on disk before we report success.
    let synced = engine.sync().map_err(|e| e.to_string());
    r.and(synced)
}
