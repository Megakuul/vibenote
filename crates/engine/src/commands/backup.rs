//! Cloud backup commands (R-BACKUP-001..003, ADR 0012): status, configure, back up now, and the
//! Google Drive sign-in. They work only in sessions with a backup config file
//! ([`crate::Engine::use_backup_config`]).

use crate::command::{Registry, Spec, Typed};
use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use vibenote_backup::Provider;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

/// Changes to the S3 settings; an empty string clears an optional field.
#[derive(Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct S3Patch {
    /// Custom endpoint (`https://…`; `http://` for a local service); empty: AWS.
    endpoint: Option<String>,
    bucket: Option<String>,
    region: Option<String>,
    /// Key prefix (default `vibenote`).
    prefix: Option<String>,
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
    session_token: Option<String>,
    /// A profile in `~/.aws/credentials`, used when no key pair is set.
    profile: Option<String>,
}

/// Changes to the Google Drive settings; an empty string clears an optional field.
#[derive(Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct DrivePatch {
    /// OAuth client ("Desktop app") from the owner's Google Cloud project.
    client_id: Option<String>,
    client_secret: Option<String>,
    /// Top folder in "My Drive" (default `vibenote-backup`).
    folder: Option<String>,
    /// Endpoint overrides (tests).
    api_base: Option<String>,
    upload_base: Option<String>,
    token_url: Option<String>,
    auth_url: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Configure {
    /// Back up this workspace (needs a complete target).
    enabled: Option<bool>,
    /// Minutes between backups of a changed workspace (1–1440; default 10).
    interval_min: Option<u32>,
    /// `none`, `s3` or `drive`.
    provider: Option<String>,
    s3: Option<S3Patch>,
    drive: Option<DrivePatch>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Connect {
    /// Open the consent page in the browser (default true).
    open: Option<bool>,
}

/// `Some("")` clears, `Some(v)` sets.
fn opt(field: &mut Option<String>, v: Option<String>) {
    if let Some(v) = v {
        let v = v.trim().to_string();
        *field = (!v.is_empty()).then_some(v);
    }
}

fn req(field: &mut String, v: Option<String>) {
    if let Some(v) = v {
        *field = v.trim().to_string();
    }
}

fn now_ms() -> u64 {
    vibenote_backup::clock::unix_ms(std::time::SystemTime::now())
}

pub fn register(r: &mut Registry) -> Result<(), EngineError> {
    r.register(Box::new(Typed::new(
        Spec {
            id: "backup.status",
            doc: "Cloud backup of this workspace: target settings (never secrets), whether it runs, the last success (Unix ms) and error.",
            reqs: &["R-BACKUP-001", "R-BACKUP-003"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| -> Result<Value, EngineError> {
            if s.backup.0.is_none() {
                return Ok(json!({"available": false}));
            }
            s.backup.with(|b| Ok(b.status()))
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "backup.configure",
            doc: "Change this workspace's backup settings (saved outside the workspace, mode 0600). Enabling needs a complete target.",
            reqs: &["R-BACKUP-001"],
            implemented: true,
        },
        |s: &mut Session, p: Configure| -> Result<Value, EngineError> {
            s.backup.with(|b| {
                let mut c = b.config.clone();
                if let Some(e) = p.enabled {
                    c.enabled = e;
                }
                if let Some(m) = p.interval_min {
                    if !vibenote_backup::config::INTERVAL_RANGE.contains(&m) {
                        return Err(EngineError::Model("interval_min must be 1–1440".into()));
                    }
                    c.interval_min = Some(m);
                }
                if let Some(pr) = p.provider {
                    c.provider = match pr.as_str() {
                        "none" => Provider::None,
                        "s3" => Provider::S3,
                        "drive" => Provider::Drive,
                        other => return Err(EngineError::Model(format!("unknown provider {other}"))),
                    };
                }
                if let Some(x) = p.s3 {
                    opt(&mut c.s3.endpoint, x.endpoint);
                    req(&mut c.s3.bucket, x.bucket);
                    req(&mut c.s3.region, x.region);
                    opt(&mut c.s3.prefix, x.prefix);
                    opt(&mut c.s3.access_key_id, x.access_key_id);
                    opt(&mut c.s3.secret_access_key, x.secret_access_key);
                    opt(&mut c.s3.session_token, x.session_token);
                    opt(&mut c.s3.profile, x.profile);
                }
                if let Some(x) = p.drive {
                    // Another client cannot use the old sign-in.
                    if x.client_id.as_deref().is_some_and(|id| id.trim() != c.drive.client_id) {
                        c.drive.refresh_token = None;
                    }
                    req(&mut c.drive.client_id, x.client_id);
                    req(&mut c.drive.client_secret, x.client_secret);
                    opt(&mut c.drive.folder, x.folder);
                    opt(&mut c.drive.api_base, x.api_base);
                    opt(&mut c.drive.upload_base, x.upload_base);
                    opt(&mut c.drive.token_url, x.token_url);
                    opt(&mut c.drive.auth_url, x.auth_url);
                }
                if c.enabled
                    && let Some(problem) = c.problem()
                {
                    return Err(EngineError::Model(format!("backup cannot be enabled: {problem}")));
                }
                b.config = c;
                b.config_changed();
                b.save_config()?;
                Ok(b.status())
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "backup.now",
            doc: "Start a backup of this workspace now (in the background; see backup.status).",
            reqs: &["R-BACKUP-002"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| -> Result<Value, EngineError> {
            s.backup.with(|b| {
                let started = b.start(now_ms())?;
                Ok(json!({"started": started, "running": true}))
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "backup.drive_connect",
            doc: "Sign in to Google Drive in the browser (OAuth for desktop apps, scope drive.file); returns the consent URL. The sign-in finishes in the background.",
            reqs: &["R-BACKUP-001"],
            implemented: true,
        },
        |s: &mut Session, p: Connect| -> Result<Value, EngineError> {
            s.backup.with(|b| {
                let url = b.connect(p.open.unwrap_or(true))?;
                Ok(json!({"url": url}))
            })
        },
    )))?;
    r.register(Box::new(Typed::new(
        Spec {
            id: "backup.drive_disconnect",
            doc: "Forget the Google Drive sign-in (and turn backup off if Drive is the target).",
            reqs: &["R-BACKUP-001"],
            implemented: true,
        },
        |s: &mut Session, _: NoParams| -> Result<Value, EngineError> {
            s.backup.with(|b| {
                b.config.drive.refresh_token = None;
                if b.config.provider == Provider::Drive {
                    b.config.enabled = false;
                }
                b.config_changed();
                b.save_config()?;
                Ok(b.status())
            })
        },
    )))?;
    Ok(())
}
