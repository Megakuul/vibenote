//! Cloud backup in the engine (ADR 0012, R-BACKUP-001..003): the workspace's backup config and
//! state, the schedule, and the backup thread. Nothing here runs unless the app or CLI passes
//! the config file explicitly ([`crate::Engine::use_backup_config`]), so tests and scratch
//! engines never touch the network.

use crate::EngineError;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use vibenote_backup::{Config, ConfigFile, Event, Job, Report, StateFile, TargetState, Worker};

/// How often the schedule looks at the workspace files (ms).
pub const CHECK_EVERY_MS: u64 = 60_000;
/// How long a browser sign-in may take (s).
const CONSENT_TIMEOUT_S: u64 = 300;

/// The app's config and state files: `$VIBENOTE_BACKUP_CONFIG` (state next to it, for tests and
/// scratch runs) or `$XDG_CONFIG_HOME/vibenote/backup.json` and
/// `$XDG_STATE_HOME/vibenote/backup-state.json`.
pub fn default_paths() -> Option<(PathBuf, PathBuf)> {
    if let Some(c) = std::env::var_os("VIBENOTE_BACKUP_CONFIG").filter(|c| !c.is_empty()) {
        let c = PathBuf::from(c);
        let s = with_suffix(&c, ".state");
        return Some((c, s));
    }
    Some((
        vibenote_backup::config::default_config_path()?,
        vibenote_backup::config::default_state_path()?,
    ))
}

/// The backup of the open workspace.
pub(crate) struct Backup {
    config_path: PathBuf,
    state_path: PathBuf,
    /// The workspace's key in both files and its database file.
    key: String,
    db: PathBuf,
    pub(crate) config: Config,
    pub(crate) state: TargetState,
    worker: Option<Worker>,
    running: bool,
    connecting: bool,
    consent_url: Option<String>,
    last_report: Option<Report>,
    last_check_ms: Option<u64>,
    /// When the last scheduled or manual run failed (retried one interval later).
    failed_at_ms: Option<u64>,
    /// When the running job started, and the database files' modification time then.
    started_ms: u64,
    started_mtime_ms: u64,
    /// Messages for the UI (failures), taken by [`crate::Engine::backup_tick`].
    notices: Vec<String>,
}

/// Session handle to the backup (shared, not part of the session's identity).
#[derive(Clone, Default)]
pub struct BackupHandle(pub(crate) Option<Arc<Mutex<Backup>>>);

impl std::fmt::Debug for BackupHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "BackupHandle(on)"
        } else {
            "BackupHandle(none)"
        })
    }
}

impl PartialEq for BackupHandle {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Model(format!("backup: {e}"))
}

impl BackupHandle {
    /// Runs `f` on the backup; an error if this session has none (in-memory or no config file).
    pub(crate) fn with<T>(&self, f: impl FnOnce(&mut Backup) -> Result<T, EngineError>) -> Result<T, EngineError> {
        let m = self
            .0
            .as_ref()
            .ok_or_else(|| EngineError::Model("backup is not available in this session".into()))?;
        let mut b = m.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut b)
    }
}

/// Modification time of `p` in Unix ms (0 if unknown).
fn mtime_ms(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .map(vibenote_backup::clock::unix_ms)
        .unwrap_or(0)
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

impl Backup {
    /// Loads the entries of workspace `db` from both files.
    pub(crate) fn open(db: &Path, config_path: &Path, state_path: &Path) -> Result<Backup, EngineError> {
        let key = vibenote_backup::config::workspace_key(db);
        let mut file: ConfigFile = vibenote_backup::config::read_json(config_path).map_err(err)?;
        let config = file.workspaces.remove(&key).unwrap_or_default();
        // An unreadable state file only costs re-uploads.
        let mut states: StateFile = vibenote_backup::config::read_json(state_path).unwrap_or_default();
        let state = states.workspaces.remove(&key).unwrap_or_default().for_config(&config);
        Ok(Backup {
            config_path: config_path.to_path_buf(),
            state_path: state_path.to_path_buf(),
            key,
            db: db.to_path_buf(),
            config,
            state,
            worker: None,
            running: false,
            connecting: false,
            consent_url: None,
            last_report: None,
            last_check_ms: None,
            failed_at_ms: None,
            started_ms: 0,
            started_mtime_ms: 0,
            notices: Vec::new(),
        })
    }

    /// Writes this workspace's config (other workspaces' entries are kept).
    pub(crate) fn save_config(&self) -> Result<(), EngineError> {
        let mut file: ConfigFile = vibenote_backup::config::read_json(&self.config_path).map_err(err)?;
        file.workspaces.insert(self.key.clone(), self.config.clone());
        vibenote_backup::config::write_json(&self.config_path, &file).map_err(err)
    }

    fn save_state(&mut self) {
        let mut file: StateFile = vibenote_backup::config::read_json(&self.state_path).unwrap_or_default();
        file.workspaces.insert(self.key.clone(), self.state.clone());
        if let Err(e) = vibenote_backup::config::write_json(&self.state_path, &file) {
            self.notices.push(format!("backup state not saved: {e}"));
        }
    }

    /// Re-keys the state after a config change (a new target starts afresh).
    pub(crate) fn config_changed(&mut self) {
        self.state = std::mem::take(&mut self.state).for_config(&self.config);
    }

    fn worker(&mut self) -> Result<&Worker, EngineError> {
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn().map_err(err)?);
        }
        self.worker.as_ref().ok_or_else(|| err("no backup thread"))
    }

    /// Starts a backup now (unless one is running). `Ok(false)`: one is already running.
    pub(crate) fn start(&mut self, now_ms: u64) -> Result<bool, EngineError> {
        if self.running {
            return Ok(false);
        }
        if let Some(p) = self.config.problem() {
            return Err(err(p));
        }
        let job = Job::Backup {
            db: self.db.clone(),
            config: Box::new(self.config.clone()),
            state: self.state.clone(),
            now_ms,
        };
        self.worker()?.send(job).map_err(err)?;
        self.running = true;
        self.started_ms = now_ms;
        self.started_mtime_ms = self.source_mtime();
        Ok(true)
    }

    /// Starts the Google sign-in; returns the consent URL (opened in the browser if `open`).
    pub(crate) fn connect(&mut self, open: bool) -> Result<String, EngineError> {
        let consent = vibenote_backup::begin_consent(&self.config.drive).map_err(err)?;
        let url = consent.url.clone();
        let job = Job::Connect {
            drive: self.config.drive.clone(),
            consent,
            timeout_s: CONSENT_TIMEOUT_S,
        };
        self.worker()?.send(job).map_err(err)?;
        self.connecting = true;
        self.consent_url = Some(url.clone());
        if open {
            crate::commands::link::xdg_open(&url)?;
        }
        Ok(url)
    }

    /// Newest modification time of the database and its WAL (Unix ms).
    fn source_mtime(&self) -> u64 {
        mtime_ms(&self.db).max(mtime_ms(&with_suffix(&self.db, "-wal")))
    }

    /// True if the workspace changed since the last successful backup began.
    fn changed(&self) -> bool {
        self.state.source_mtime_ms.is_none_or(|t| self.source_mtime() > t)
    }

    /// Takes finished work and starts a scheduled backup if one is due; returns messages for
    /// the UI.
    pub(crate) fn tick(&mut self, now_ms: u64) -> Vec<String> {
        while let Some(ev) = self.worker.as_ref().and_then(Worker::poll) {
            self.finished(ev);
        }
        let due_check = self
            .last_check_ms
            .is_none_or(|t| now_ms.saturating_sub(t) >= CHECK_EVERY_MS);
        if self.config.enabled && !self.running && due_check {
            self.last_check_ms = Some(now_ms);
            let interval = u64::from(self.config.interval_min()).saturating_mul(60_000);
            let since = |t: Option<u64>| t.is_none_or(|t| now_ms.saturating_sub(t) >= interval);
            if since(self.state.last_success_ms)
                && since(self.failed_at_ms)
                && self.changed()
                && let Err(e) = self.start(now_ms)
            {
                let msg = e.to_string();
                // The same problem once, not every minute.
                if self.state.last_error.as_deref() != Some(msg.as_str()) {
                    self.notices.push(msg.clone());
                    self.state.last_error = Some(msg);
                }
            }
        }
        std::mem::take(&mut self.notices)
    }

    fn finished(&mut self, ev: Event) {
        match ev {
            Event::BackedUp { result, state } => {
                self.running = false;
                // The run's state belongs to the config it ran with; keep it only if the target
                // is still the same.
                let still = state.target == self.config.identity();
                match result {
                    Ok(report) => {
                        if still {
                            self.state = state;
                            self.state.source_mtime_ms = Some(self.started_mtime_ms);
                        }
                        self.failed_at_ms = None;
                        self.last_report = Some(report);
                    }
                    Err(e) => {
                        if still {
                            self.state = state;
                        }
                        self.failed_at_ms = Some(self.started_ms);
                        let msg = format!("backup failed: {e}");
                        self.state.last_error = Some(msg.clone());
                        self.notices.push(msg);
                    }
                }
                self.save_state();
            }
            Event::Connected { result } => {
                self.connecting = false;
                self.consent_url = None;
                match result {
                    Ok(token) => {
                        self.config.drive.refresh_token = Some(token);
                        self.config_changed();
                        if let Err(e) = self.save_config() {
                            self.notices.push(e.to_string());
                        }
                    }
                    Err(e) => self.notices.push(format!("Google Drive: {e}")),
                }
            }
        }
    }

    /// True while a backup or a sign-in is in progress.
    pub(crate) fn busy(&self) -> bool {
        self.running || self.connecting
    }

    /// Waits for the running job (CLI, tests); `false` on timeout.
    pub(crate) fn wait(&mut self, timeout: std::time::Duration) -> bool {
        if !self.running && !self.connecting {
            return true;
        }
        match self.worker.as_ref().and_then(|w| w.wait(timeout)) {
            Some(ev) => {
                self.finished(ev);
                true
            }
            None => false,
        }
    }

    /// The status for `backup.status`; secrets are never included.
    pub(crate) fn status(&self) -> Value {
        let c = &self.config;
        let set = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.is_empty());
        json!({
            "available": true,
            "enabled": c.enabled,
            "provider": c.provider.name(),
            "interval_min": c.interval_min(),
            "s3": {
                "endpoint": c.s3.endpoint,
                "bucket": c.s3.bucket,
                "region": c.s3.region,
                "prefix": c.s3.prefix(),
                "profile": c.s3.profile,
                "access_key_id": c.s3.access_key_id,
                "has_secret": set(&c.s3.secret_access_key),
            },
            "drive": {
                "client_id": c.drive.client_id,
                "has_client_secret": !c.drive.client_secret.is_empty(),
                "connected": set(&c.drive.refresh_token),
                "folder": c.drive.folder(),
            },
            "problem": c.problem(),
            "running": self.running,
            "connecting": self.connecting,
            "consent_url": self.consent_url,
            "last_success_ms": self.state.last_success_ms,
            "last_error": self.state.last_error,
            "last_report": self.last_report,
        })
    }
}
