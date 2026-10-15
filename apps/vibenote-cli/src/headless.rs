//! Headless helpers shared by the CLI and its golden tests: replay a fixture, render a note.

use serde_json::json;
use vibenote_engine::Engine;
use vibenote_ink::input::Fixture;
use vibenote_model::NoteId;
use vibenote_render::{Headless, Image, Viewport};

/// Fresh engine with one note (opened), the fixture replayed into it. Returns (engine, note id).
pub fn replay_fixture(f: &Fixture) -> Result<(Engine, u64), String> {
    let mut e = Engine::new().map_err(|x| x.to_string())?;
    let n = e
        .execute("workspace.note.create", json!({"name": "fixture"}))
        .map_err(|x| x.to_string())?;
    let id = n.get("id").and_then(serde_json::Value::as_u64).ok_or("no note id")?;
    e.execute("note.open", json!({"id": id})).map_err(|x| x.to_string())?;
    for chunk in f.events.chunks(256) {
        e.execute("input.pen", json!({"events": chunk}))
            .map_err(|x| x.to_string())?;
    }
    Ok((e, id))
}

/// Runs `[cmd, args]` pairs (e.g. from a golden case) on the engine. A string `"$k"` anywhere
/// in the args stands for the `id` returned by the k-th command.
#[allow(dead_code)] // used by the golden tests, not by every binary including this module
pub fn run_commands(e: &mut Engine, cmds: &[serde_json::Value]) -> Result<(), String> {
    fn subst(v: &mut serde_json::Value, ids: &[Option<u64>]) -> Result<(), String> {
        match v {
            serde_json::Value::String(s) if s.starts_with('$') => {
                let k: usize = s
                    .get(1..)
                    .and_then(|k| k.parse().ok())
                    .ok_or(format!("bad reference {s}"))?;
                let id = ids
                    .get(k)
                    .copied()
                    .flatten()
                    .ok_or(format!("{s}: command {k} returned no id"))?;
                *v = serde_json::Value::from(id);
            }
            serde_json::Value::Array(a) => a.iter_mut().try_for_each(|x| subst(x, ids))?,
            serde_json::Value::Object(o) => o.values_mut().try_for_each(|x| subst(x, ids))?,
            _ => {}
        }
        Ok(())
    }
    let mut ids = Vec::new();
    for c in cmds {
        let (Some(cmd), Some(args)) = (c.get(0).and_then(serde_json::Value::as_str), c.get(1)) else {
            return Err(format!("bad command {c} (want [cmd, args])"));
        };
        let mut args = args.clone();
        subst(&mut args, &ids)?;
        let r = e.execute(cmd, args).map_err(|x| format!("{cmd}: {x}"))?;
        ids.push(r.get("id").and_then(serde_json::Value::as_u64));
    }
    Ok(())
}

/// Renders a note with its background, settled (the whole note, however large).
pub fn render_note(r: &mut Headless, e: &Engine, note: u64, vp: &Viewport) -> Result<Image, String> {
    let canvas = e.session.ws.canvas(NoteId(note)).map_err(|x| x.to_string())?;
    r.set_background(e.session.background_of(NoteId(note)));
    r.set_display(e.session.display_transforms(NoteId(note)));
    let (texts, vectors) = e.session.extras_in_view(
        (vp.origin_x, vp.origin_y),
        vp.zoom,
        (f64::from(vp.width), f64::from(vp.height)),
    );
    r.set_extras(texts, vectors);
    // PDF pages, rendered now (the picture must be complete).
    let pages = e.session.images_in_view(
        (vp.origin_x, vp.origin_y),
        vp.zoom,
        (f64::from(vp.width), f64::from(vp.height)),
        true,
    );
    r.set_images(
        pages
            .into_iter()
            .map(|p| vibenote_render::ImageQuad {
                key: p.key,
                rect: p.rect,
                width: p.width,
                height: p.height,
                pixels: p.pixels,
            })
            .collect(),
    );
    // The whole note, however large (rendering does bounded work per frame).
    r.render_settled(canvas, &[], vp, 10_000).map_err(|x| x.to_string())
}

pub fn read_fixture(path: &std::path::Path) -> Result<Fixture, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Fixture::read(std::io::BufReader::new(f)).map_err(|e| format!("{}: {e}", path.display()))
}
