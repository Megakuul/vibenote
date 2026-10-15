//! Handwriting commands (R-HWR-001/002/003/007/008): packs, block segmentation, the
//! writing-vs-drawing classifier, recognition (now or as a background job), evaluation.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vibenote_hwr::blocks::TimedStroke;
use vibenote_model::{NoteId, ObjectId, ObjectKind};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PacksP {
    /// Extra directories to search (each a pack or a directory of packs).
    #[serde(default)]
    dirs: Vec<PathBuf>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SegmentP {
    /// An ink fixture file to segment (its pen strokes, in order).
    fixture: Option<PathBuf>,
    /// Idle threshold in ms (default: the setting).
    idle_ms: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StrokesP {
    strokes: Vec<u64>,
    /// `hwr.recognize_block`: queue as a job instead of waiting.
    #[serde(default)]
    background: bool,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TidyP {
    /// Show the selected handwriting as written again.
    #[serde(default)]
    revert: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SuggestionP {
    block: u64,
    /// Apply it (otherwise dismiss it).
    #[serde(default)]
    apply: bool,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PrettifyP {
    strokes: Vec<u64>,
    /// Skip the mode and the two gates (gallery, tests).
    #[serde(default)]
    force: bool,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RevertP {
    /// The prettify object (`hwr.prettify_block`'s `block`).
    block: u64,
    note: Option<u64>,
}

/// Gate B (ADR 0007): the recogniser reads the block confidently as text.
pub(crate) const GATE_B_CONF: f32 = 0.85;

pub(crate) fn gate_b(r: &crate::hwr::BlockText) -> bool {
    let chars: Vec<char> = r.text.chars().filter(|c| !c.is_whitespace()).collect();
    let letters = chars.iter().filter(|c| c.is_alphabetic()).count();
    #[allow(clippy::cast_precision_loss)] // character counts
    let share = if chars.is_empty() {
        0.0
    } else {
        letters as f64 / chars.len() as f64
    };
    r.conf >= GATE_B_CONF && letters >= 2 && share >= 0.6
}

/// Prettifies a block (strokes of `note`), one undo step; returns the block id (its first stroke).
pub(crate) fn prettify(
    s: &mut Session,
    note: NoteId,
    ids: &[u64],
) -> Result<(u64, vibenote_hwr::prettify::Prettified), EngineError> {
    let pts = stroke_points(s, note, ids)?;
    let p = vibenote_hwr::prettify::stage1(&pts).ok_or_else(|| EngineError::Model("nothing to prettify".into()))?;
    let block = ObjectId(*ids.first().ok_or_else(|| EngineError::Model("no strokes".into()))?);
    let after = vibenote_model::Prettify {
        strokes: ids.iter().map(|i| ObjectId(*i)).collect(),
        transforms: p.transforms.clone(),
    };
    s.commit(
        crate::Scope::Note(note),
        "prettify",
        vec![vibenote_model::Op::SetPrettify {
            note,
            block,
            after: Some(after),
        }],
    )?;
    Ok((block.0, p))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetP {
    /// Eval set directory.
    set: PathBuf,
    /// At most this many items per language (recognition).
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JobP {
    id: u64,
}

#[derive(Serialize)]
struct Blocks {
    blocks: Vec<BlockOut>,
}

#[derive(Serialize)]
struct BlockOut {
    /// Inclusive stroke index range.
    strokes: [usize; 2],
}

#[derive(Serialize)]
struct Class {
    class: &'static str,
    p: f64,
    threshold: f64,
}

#[derive(Serialize)]
struct ClassEval {
    drawing_false_positives: usize,
    drawings: usize,
    writing_recall: f64,
    writings: usize,
    threshold: f64,
}

#[derive(Serialize)]
struct RecEval {
    cer_en: f64,
    cer_de: f64,
    n_en: usize,
    n_de: usize,
    pack: String,
}

/// The pen strokes of an ink fixture, with their times (segmentation input).
fn fixture_strokes(path: &PathBuf) -> Result<Vec<TimedStroke>, EngineError> {
    use vibenote_ink::input::{Fixture, PenEventKind};
    let meta = std::fs::metadata(path).map_err(|e| EngineError::Model(format!("{}: {e}", path.display())))?;
    if meta.len() > 256 << 20 {
        return Err(EngineError::Model("fixture too large".into()));
    }
    let f = std::fs::File::open(path).map_err(|e| EngineError::Model(format!("{}: {e}", path.display())))?;
    let fx = Fixture::read(std::io::BufReader::new(f)).map_err(|e| EngineError::Model(e.to_string()))?;
    let mut out = Vec::new();
    let mut cur: Option<TimedStroke> = None;
    for e in &fx.events {
        match e.kind {
            PenEventKind::Down => {
                cur = Some(TimedStroke {
                    start_us: e.t_us,
                    end_us: e.t_us,
                    points: vec![(e.x, e.y)],
                });
            }
            PenEventKind::Motion => {
                if let Some(c) = cur.as_mut() {
                    c.points.push((e.x, e.y));
                    c.end_us = e.t_us;
                }
            }
            PenEventKind::Up | PenEventKind::ProximityOut => {
                if let Some(mut c) = cur.take() {
                    c.end_us = c.end_us.max(e.t_us);
                    out.push(c);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The points of the given strokes of a note (ids in the order given; missing ids are errors).
pub(crate) fn stroke_points(s: &mut Session, note: NoteId, ids: &[u64]) -> Result<Vec<Vec<(f64, f64)>>, EngineError> {
    if ids.is_empty() || ids.len() > crate::MAX_IDS {
        return Err(EngineError::Model("give 1.. stroke ids".into()));
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        match super::pdf::object(s, note, *id)?.kind {
            ObjectKind::Stroke(st) => out.push(st.samples.iter().map(|p| (p.x, p.y)).collect()),
            _ => return Err(EngineError::Model(format!("object {id} is not a stroke"))),
        }
    }
    Ok(out)
}

/// Line images of a block, top to bottom.
pub(crate) fn block_lines(strokes: &[Vec<(f64, f64)>]) -> Vec<vibenote_hwr::raster::Gray> {
    vibenote_hwr::raster::lines(strokes)
        .iter()
        .filter_map(|line| {
            let refs: Vec<&Vec<(f64, f64)>> = line.iter().filter_map(|k| strokes.get(*k)).collect();
            vibenote_hwr::raster::render(&refs)
        })
        .collect()
}

/// The bounding box of a block's points.
pub(crate) fn region(strokes: &[Vec<(f64, f64)>]) -> vibenote_geom::Rect {
    let mut r = vibenote_geom::Rect::EMPTY;
    for p in strokes.iter().flatten() {
        r.include(vibenote_geom::Point::new(p.0, p.1));
    }
    r
}

/// Levenshtein distance over characters (bounded inputs).
fn edits(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len().saturating_add(1)];
    for (i, ca) in a.iter().enumerate() {
        if let Some(c0) = cur.first_mut() {
            *c0 = i.saturating_add(1);
        }
        for (j, cb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0).saturating_add(usize::from(ca != cb));
            let del = prev.get(j.saturating_add(1)).copied().unwrap_or(0).saturating_add(1);
            let ins = cur.get(j).copied().unwrap_or(0).saturating_add(1);
            if let Some(slot) = cur.get_mut(j.saturating_add(1)) {
                *slot = sub.min(del).min(ins);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev.get(b.len()).copied().unwrap_or(0)
}

/// CER of the recogniser on an eval directory with `gt.tsv` (`file<TAB>text`).
fn cer(s: &Session, dir: &std::path::Path, limit: usize) -> Result<(f64, usize), EngineError> {
    let gt = std::fs::read_to_string(dir.join("gt.tsv"))
        .map_err(|e| EngineError::Model(format!("{}: {e}", dir.display())))?;
    let (mut errs, mut total, mut n) = (0usize, 0usize, 0usize);
    for line in gt.lines().take(limit) {
        let Some((name, text)) = line.split_once('\t') else {
            continue;
        };
        if name.contains('/') || name.contains("..") {
            continue;
        }
        let bytes =
            std::fs::read(dir.join(format!("{name}.png"))).map_err(|e| EngineError::Model(format!("{name}: {e}")))?;
        let img = crate::hwr::line_from_image(&bytes)?;
        let got = s.hwr.recognize_now(std::slice::from_ref(&img))?.text;
        let (g, t): (Vec<char>, Vec<char>) = (got.chars().collect(), text.trim().chars().collect());
        errs = errs.saturating_add(edits(&g, &t));
        total = total.saturating_add(t.len());
        n = n.saturating_add(1);
    }
    #[allow(clippy::cast_precision_loss)] // character counts
    let c = if total == 0 { 0.0 } else { errs as f64 / total as f64 };
    Ok((c, n))
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.packs",
            doc: "Language packs found in the bundled directory and the given ones (R-HWR-007).",
            reqs: &["R-HWR-007"],
            implemented: true,
        },
        |_s: &mut Session, p: PacksP| {
            let mut dirs = vec![crate::hwr::packs_dir()];
            dirs.extend(p.dirs);
            let (packs, errors) = vibenote_hwr::pack::discover(&dirs);
            let list: Vec<serde_json::Value> = packs
                .iter()
                .map(|p| {
                    serde_json::json!({"lang": p.manifest.lang, "name": p.manifest.name, "dir": p.dir, "recognises": p.manifest.model.is_some(), "tag": p.tag()})
                })
                .collect();
            Ok(serde_json::json!({ "packs": list, "errors": errors }))
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.segment_blocks",
            doc: "Split strokes into writing blocks by idle time (0.5–10 s, default the setting) and spatial jumps (R-HWR-001).",
            reqs: &["R-HWR-001"],
            implemented: true,
        },
        |s: &mut Session, p: SegmentP| {
            let idle = p.idle_ms.unwrap_or(s.settings.block_idle_ms);
            let (lo, hi) = vibenote_model::settings::bounds::BLOCK_IDLE_MS;
            if !(lo..=hi).contains(&idle) {
                return Err(EngineError::Model(format!("idle_ms must be {lo}..={hi}")));
            }
            let strokes = match &p.fixture {
                Some(f) => fixture_strokes(f)?,
                None => return Err(EngineError::Model("give a fixture".into())),
            };
            let blocks = vibenote_hwr::blocks::segment(&strokes, idle)
                .into_iter()
                .map(|b| BlockOut { strokes: b })
                .collect();
            Ok(Blocks { blocks })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.classify_block",
            doc: "Writing, drawing or uncertain for a block of strokes (gate A, ADR 0007).",
            reqs: &["R-HWR-001", "R-HWR-002"],
            implemented: true,
        },
        |s: &mut Session, p: StrokesP| {
            let note = super::ink::target_note(s, p.note)?;
            let pts = stroke_points(s, note, &p.strokes)?;
            let m = s.hwr.classifier()?;
            let f = vibenote_hwr::features::features(&pts).ok_or_else(|| EngineError::Model("empty block".into()))?;
            let prob = m.predict(&f);
            let class = if prob >= m.threshold {
                "writing"
            } else if prob <= 0.2 {
                "drawing"
            } else {
                "uncertain"
            };
            Ok(Class {
                class,
                p: prob,
                threshold: m.threshold,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.eval_classifier",
            doc: "Classifier on an eval set (`blocks.jsonl`): drawing false positives at the threshold and writing recall (R-HWR-002).",
            reqs: &["R-HWR-002"],
            implemented: true,
        },
        |s: &mut Session, p: SetP| {
            let m = s.hwr.classifier()?;
            let text = std::fs::read_to_string(p.set.join("blocks.jsonl")).map_err(|e| EngineError::Model(format!("{}: {e}", p.set.display())))?;
            let (mut fp, mut drawings, mut hits, mut writings) = (0usize, 0usize, 0usize, 0usize);
            for line in text.lines() {
                let v: serde_json::Value = serde_json::from_str(line).map_err(|e| EngineError::Model(e.to_string()))?;
                let strokes: Vec<Vec<(f64, f64)>> = serde_json::from_value(v.get("strokes").cloned().unwrap_or_default()).map_err(|e| EngineError::Model(e.to_string()))?;
                let Some(f) = vibenote_hwr::features::features(&strokes) else { continue };
                let writing = m.predict(&f) >= m.threshold;
                match v.get("label").and_then(serde_json::Value::as_str) {
                    Some("drawing") => {
                        drawings = drawings.saturating_add(1);
                        fp = fp.saturating_add(usize::from(writing));
                    }
                    Some("writing") => {
                        writings = writings.saturating_add(1);
                        hits = hits.saturating_add(usize::from(writing));
                    }
                    _ => {}
                }
            }
            #[allow(clippy::cast_precision_loss)] // counts
            let recall = if writings == 0 { 0.0 } else { hits as f64 / writings as f64 };
            Ok(ClassEval {
                drawing_false_positives: fp,
                drawings,
                writing_recall: recall,
                writings,
                threshold: m.threshold,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.recognize_block",
            doc: "Recognise a block of strokes (lines → recogniser); `background: true` queues a cancellable job. The text joins search.",
            reqs: &["R-HWR-003", "R-HWR-007", "R-HWR-008"],
            implemented: true,
        },
        |s: &mut Session, p: StrokesP| {
            let note = super::ink::target_note(s, p.note)?;
            let pts = stroke_points(s, note, &p.strokes)?;
            let block = ObjectId(*p.strokes.first().unwrap_or(&0));
            let reg = region(&pts);
            if p.background {
                let job = s.hwr.submit(note, block, reg, pts);
                return Ok(serde_json::json!({ "job": job }));
            }
            let r = s.hwr.recognize_now(&block_lines(&pts))?;
            s.set_ink_text(note, block, reg, &r)?;
            serde_json::to_value(r).map_err(|e| EngineError::Internal(e.to_string()))
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.eval_recognition",
            doc: "Character error rate of the recogniser on an eval set (`en/`, `de/` with gt.tsv) (R-HWR-003).",
            reqs: &["R-HWR-003"],
            implemented: true,
        },
        |s: &mut Session, p: SetP| {
            let limit = p.limit.unwrap_or(usize::MAX);
            let (cer_en, n_en) = cer(s, &p.set.join("en"), limit)?;
            let (cer_de, n_de) = cer(s, &p.set.join("de"), limit)?;
            Ok(RecEval {
                cer_en,
                cer_de,
                n_en,
                n_de,
                pack: s.hwr.recognizer()?.tag.clone(),
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "selection.tidy",
            doc: "Prettify the handwriting in the selection on request (`revert: true`: show it as written again). The selected strokes (with any already prettified block they belong to) become one block; one undo step. Strokes never change (display transforms, ADR 0008).",
            reqs: &["R-HWR-004", "R-HWR-005"],
            implemented: true,
        },
        |s: &mut Session, p: TidyP| {
            let note = s
                .selection
                .note
                .ok_or_else(|| EngineError::Model("nothing is selected".into()))?;
            let strokes: Vec<u64> = {
                let c = s.ws.canvas(note)?;
                s.selection
                    .ids
                    .iter()
                    .copied()
                    .filter(|id| matches!(c.objects.get(&ObjectId(*id)).map(|o| &o.kind), Some(ObjectKind::Stroke(_))))
                    .collect()
            };
            if strokes.is_empty() {
                return Err(EngineError::Model("no handwriting selected".into()));
            }
            // Prettified blocks the selection touches come off (and join the new block).
            let touched: Vec<(ObjectId, vibenote_model::Prettify)> = s
                .ws
                .prettified
                .get(&note)
                .into_iter()
                .flat_map(|m| m.iter())
                .filter(|(_, b)| b.strokes.iter().any(|id| strokes.contains(&id.0)))
                .map(|(k, b)| (*k, b.clone()))
                .collect();
            let mut ops: Vec<vibenote_model::Op> = touched
                .iter()
                .map(|(block, _)| vibenote_model::Op::SetPrettify {
                    note,
                    block: *block,
                    after: None,
                })
                .collect();
            if !p.revert {
                let mut ids = strokes;
                for (_, b) in &touched {
                    ids.extend(b.strokes.iter().map(|i| i.0));
                }
                ids.sort_unstable();
                ids.dedup();
                let pts = stroke_points(s, note, &ids)?;
                let pr = vibenote_hwr::prettify::stage1(&pts)
                    .ok_or_else(|| EngineError::Model("nothing to tidy".into()))?;
                let block = ObjectId(*ids.first().ok_or_else(|| EngineError::Model("no strokes".into()))?);
                ops.push(vibenote_model::Op::SetPrettify {
                    note,
                    block,
                    after: Some(vibenote_model::Prettify {
                        strokes: ids.iter().map(|i| ObjectId(*i)).collect(),
                        transforms: pr.transforms,
                    }),
                });
            }
            if ops.is_empty() {
                return Ok(());
            }
            s.commit(crate::Scope::Note(note), if p.revert { "revert tidy" } else { "tidy" }, ops)
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.suggestion",
            doc: "Accept (`apply: true`, one undo step) or dismiss a prettify suggestion (mode \"suggest\") for a writing block.",
            reqs: &["R-HWR-004", "R-HWR-006"],
            implemented: true,
        },
        |s: &mut Session, p: SuggestionP| {
            let note = super::ink::target_note(s, p.note)?;
            let ids = s
                .take_suggestion(note, p.block)
                .ok_or_else(|| EngineError::Model(format!("no suggestion for block {}", p.block)))?;
            if p.apply {
                prettify(s, note, &ids)?;
            }
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.prettify_block",
            doc: "Straighten a writing block (rotation and word baselines; a display transform, the strokes never change). Without `force`: only when the mode allows it and both gates say writing.",
            reqs: &["R-HWR-004", "R-HWR-005", "R-HWR-006"],
            implemented: true,
        },
        |s: &mut Session, p: PrettifyP| {
            let note = super::ink::target_note(s, p.note)?;
            if !p.force {
                if s.settings.prettify_mode == vibenote_model::settings::PrettifyMode::Off {
                    return Ok(serde_json::json!({"applied": false, "reason": "prettify is off"}));
                }
                let pts = stroke_points(s, note, &p.strokes)?;
                let m = s.hwr.classifier()?;
                let f = vibenote_hwr::features::features(&pts).ok_or_else(|| EngineError::Model("empty block".into()))?;
                if m.predict(&f) < m.threshold {
                    return Ok(serde_json::json!({"applied": false, "reason": "gate A: not confidently writing"}));
                }
                let r = s.hwr.recognize_now(&block_lines(&pts))?;
                if !gate_b(&r) {
                    return Ok(serde_json::json!({"applied": false, "reason": "gate B: not read confidently as text"}));
                }
            }
            let (block, pr) = prettify(s, note, &p.strokes)?;
            Ok(serde_json::json!({
                "applied": true,
                "block": block,
                "angle_deg": pr.angle_deg,
                "words": pr.words,
                "max_shape_displacement_xh": pr.max_shape_displacement_xh,
            }))
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "hwr.revert_block",
            doc: "Show a prettified block as written again (one undo step; the strokes were never changed).",
            reqs: &["R-HWR-005"],
            implemented: true,
        },
        |s: &mut Session, p: RevertP| {
            let note = super::ink::target_note(s, p.note)?;
            let block = ObjectId(p.block);
            if !s.ws.prettified.get(&note).is_some_and(|m| m.contains_key(&block)) {
                return Err(EngineError::Model(format!("{} is not a prettified block", p.block)));
            }
            s.commit(
                crate::Scope::Note(note),
                "revert prettify",
                vec![vibenote_model::Op::SetPrettify {
                    note,
                    block,
                    after: None,
                }],
            )?;
            Ok(())
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "jobs.cancel",
            doc: "Cancel a background job (R-HWR-008).",
            reqs: &["R-HWR-008"],
            implemented: true,
        },
        |s: &mut Session, p: JobP| Ok(serde_json::json!({ "cancelled": s.hwr.cancel(p.id) })),
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "jobs.status",
            doc: "A background job's state (queued, running, done with the result, failed, cancelled).",
            reqs: &["R-HWR-008"],
            implemented: true,
        },
        |s: &mut Session, p: JobP| {
            let st = s
                .hwr
                .state(p.id)
                .ok_or_else(|| EngineError::Model(format!("no job {}", p.id)))?;
            serde_json::to_value(st).map_err(|e| EngineError::Internal(e.to_string()))
        },
    )))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[test]
    fn edit_distance() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(super::edits(&c("kitten"), &c("sitting")), 3);
        assert_eq!(super::edits(&c(""), &c("abc")), 3);
        assert_eq!(super::edits(&c("Größe"), &c("Grösse")), 2);
    }
}
