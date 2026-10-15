//! Links (R-LINK-001..003): to a note (optionally at a view), a folder or a web URL. Targets
//! are stable ids, so renaming or moving them keeps links working; a deleted or trashed target
//! makes a link broken, and activating it is an error.

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Scope, Session, View};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibenote_geom::Point;
use vibenote_model::{CanvasObject, FolderId, LinkTarget, LinkView, NoteId, ObjectId, ObjectKind, Op};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewP {
    x: f64,
    y: f64,
    zoom: f64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TargetP {
    note: Option<u64>,
    folder: Option<u64>,
    url: Option<String>,
    /// With `note`: open it at this view (canvas origin and zoom).
    view: Option<ViewP>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateP {
    at: [f64; 2],
    target: TargetP,
    note: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdP {
    id: u64,
    /// `link.activate` with a URL: report the opener instead of running it.
    #[serde(default)]
    dry_run: bool,
    note: Option<u64>,
}

#[derive(Serialize)]
struct Created {
    id: u64,
}

#[derive(Serialize)]
struct Status {
    status: &'static str,
    target: LinkTarget,
}

#[derive(Serialize, Default)]
struct Activated {
    #[serde(skip_serializing_if = "Option::is_none")]
    opened: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    opener: Option<[String; 2]>,
}

fn target(p: TargetP) -> Result<LinkTarget, EngineError> {
    match (p.note, p.folder, p.url) {
        (Some(n), None, None) => Ok(LinkTarget::Note {
            note: NoteId(n),
            view: p.view.map(|v| LinkView {
                x: v.x,
                y: v.y,
                zoom: v.zoom,
            }),
        }),
        (None, Some(f), None) if p.view.is_none() => Ok(LinkTarget::Folder { folder: FolderId(f) }),
        (None, None, Some(url)) if p.view.is_none() => Ok(LinkTarget::Url { url }),
        _ => Err(EngineError::Model(
            "a link targets exactly one of note (with optional view), folder, url".into(),
        )),
    }
}

/// True if the target exists and is not in the trash.
fn alive(s: &Session, t: &LinkTarget) -> bool {
    match t {
        LinkTarget::Note { note, .. } => {
            s.ws.notes
                .get(note)
                .is_some_and(|n| !crate::quickopen::trashed(&s.ws, n))
        }
        LinkTarget::Folder { folder } => s.ws.folders.contains_key(folder) && !s.ws.folder_in_trash(*folder),
        LinkTarget::Url { .. } => true,
    }
}

fn link(s: &mut Session, note: Option<u64>, id: u64) -> Result<LinkTarget, EngineError> {
    let note = super::ink::target_note(s, note)?;
    match super::pdf::object(s, note, id)?.kind {
        ObjectKind::Link { target, .. } => Ok(target),
        _ => Err(EngineError::Model(format!("object {id} is not a link"))),
    }
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "link.create",
            doc: "Create a link to a note (optionally at a view), a folder, or an http/https/mailto URL (one undo step).",
            reqs: &["R-LINK-001", "R-LINK-003", "R-CANVAS-003"],
            implemented: true,
        },
        |s: &mut Session, p: CreateP| {
            let note = super::ink::target_note(s, p.note)?;
            let t = target(p.target)?;
            if !alive(s, &t) {
                return Err(EngineError::Model("the link target does not exist".into()));
            }
            let z = s.next_z(note)?;
            let id = ObjectId(s.ws.alloc_id()?);
            let object = CanvasObject {
                id,
                z,
                kind: ObjectKind::Link {
                    origin: Point::new(p.at[0], p.at[1]),
                    target: t,
                },
            };
            s.commit(Scope::Note(note), "link", vec![Op::InsertObject { note, object }])?;
            Ok(Created { id: id.0 })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "link.status",
            doc: "Whether a link's target exists (`ok`) or was deleted or trashed (`broken`).",
            reqs: &["R-LINK-002", "R-LINK-003"],
            implemented: true,
        },
        |s: &mut Session, p: IdP| {
            let t = link(s, p.note, p.id)?;
            Ok(Status {
                status: if alive(s, &t) { "ok" } else { "broken" },
                target: t,
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "link.activate",
            doc: "Follow a link: open the note (at its view), reveal the folder, or open the URL with xdg-open. Broken links are an error.",
            reqs: &["R-LINK-001", "R-LINK-002"],
            implemented: true,
        },
        |s: &mut Session, p: IdP| {
            let t = link(s, p.note, p.id)?;
            if !alive(s, &t) {
                return Err(EngineError::Model("the link is broken: its target was deleted".into()));
            }
            let mut out = Activated::default();
            match t {
                LinkTarget::Note { note, view } => {
                    if let Some(v) = view {
                        let zoom = v.zoom.clamp(super::view::ZOOM_MIN, super::view::ZOOM_MAX);
                        s.view = View {
                            origin_x: v.x,
                            origin_y: v.y,
                            zoom,
                        };
                    }
                    super::workspace::open_note(s, note.0, None)?;
                    out.opened = Some(note.0);
                }
                LinkTarget::Folder { folder } => {
                    s.ui.reveal_folder = Some(folder.0);
                    out.folder = Some(folder.0);
                }
                LinkTarget::Url { url } => {
                    if !vibenote_model::url_ok(&url) {
                        return Err(EngineError::Model("refusing to open this URL".into()));
                    }
                    if !p.dry_run {
                        let mut child = std::process::Command::new("xdg-open")
                            .arg(&url)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .map_err(|e| EngineError::Model(format!("xdg-open: {e}")))?;
                        // Reaped in the background (no zombie; xdg-open returns quickly).
                        let _ = std::thread::Builder::new()
                            .name("vibenote-xdg-open".into())
                            .spawn(move || {
                                let _ = child.wait();
                            });
                    }
                    out.opener = Some(["xdg-open".into(), url]);
                }
            }
            Ok(out)
        },
    )))?;
    Ok(())
}
