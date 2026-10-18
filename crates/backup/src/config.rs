//! Backup configuration (`backup.json`, secrets included, mode 0600) and local state
//! (`backup-state.json`), both keyed by the workspace's path (ADR 0012 §6).

use crate::{BackupError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Default minutes between backups of a changed workspace.
pub const DEFAULT_INTERVAL_MIN: u32 = 10;
/// Allowed interval (minutes).
pub const INTERVAL_RANGE: std::ops::RangeInclusive<u32> = 1..=1440;
/// Largest config or state file read (bytes).
const MAX_FILE: u64 = 16 << 20;

/// The backup of one workspace. Both providers' settings are kept; `provider` picks one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// Minutes between backups of a changed workspace (`None`: the default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_min: Option<u32>,
    pub provider: Provider,
    pub s3: S3Config,
    pub drive: DriveConfig,
}

impl Config {
    pub fn interval_min(&self) -> u32 {
        self.interval_min
            .filter(|m| INTERVAL_RANGE.contains(m))
            .unwrap_or(DEFAULT_INTERVAL_MIN)
    }

    /// What identifies the remote place: state (uploaded blobs, file ids) belongs to it and is
    /// dropped when it changes.
    pub fn identity(&self) -> String {
        match self.provider {
            Provider::None => String::new(),
            Provider::S3 => format!(
                "s3|{}|{}|{}|{}",
                self.s3.endpoint.as_deref().unwrap_or(""),
                self.s3.region.trim(),
                self.s3.bucket.trim(),
                self.s3.prefix()
            ),
            Provider::Drive => format!(
                "drive|{}|{}|{}",
                self.drive.client_id.trim(),
                self.drive.folder(),
                self.drive.api()
            ),
        }
    }

    /// Why a backup with this config cannot run (`None`: it can try).
    pub fn problem(&self) -> Option<String> {
        match self.provider {
            Provider::None => Some("no backup target chosen".into()),
            Provider::S3 if self.s3.bucket.trim().is_empty() => Some("S3: no bucket".into()),
            Provider::S3 if self.s3.region.trim().is_empty() => Some("S3: no region".into()),
            Provider::S3 => crate::s3::creds(&self.s3).err().map(|e| e.to_string()),
            Provider::Drive if self.drive.client_id.trim().is_empty() || self.drive.client_secret.trim().is_empty() => {
                Some("Google Drive: no OAuth client id and secret".into())
            }
            Provider::Drive if self.drive.refresh_token.as_deref().is_none_or(str::is_empty) => {
                Some("Google Drive is not connected".into())
            }
            Provider::Drive => None,
        }
    }
}

/// The backup service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    None,
    S3,
    Drive,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::None => "none",
            Provider::S3 => "s3",
            Provider::Drive => "drive",
        }
    }
}

/// An S3 bucket (AWS or any S3-compatible service).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct S3Config {
    /// Custom endpoint (`https://…`, or `http://` for a local service); `None`: AWS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub bucket: String,
    pub region: String,
    /// Key prefix (default `vibenote`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_access_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    /// A profile in `~/.aws/credentials` used when no key pair is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl S3Config {
    pub fn prefix(&self) -> &str {
        self.prefix
            .as_deref()
            .map(|p| p.trim_matches('/'))
            .filter(|p| !p.is_empty())
            .unwrap_or("vibenote")
    }
}

/// A Google Drive account, through the owner's OAuth client ("Desktop app").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DriveConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Set by "Connect" (the browser sign-in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Top folder in "My Drive" (default `vibenote-backup`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Endpoint overrides (tests): API, upload, token and consent URLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
}

impl DriveConfig {
    pub fn folder(&self) -> &str {
        self.folder
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .unwrap_or("vibenote-backup")
    }
    pub(crate) fn api(&self) -> &str {
        self.api_base
            .as_deref()
            .unwrap_or("https://www.googleapis.com/drive/v3")
    }
    pub(crate) fn upload(&self) -> &str {
        self.upload_base
            .as_deref()
            .unwrap_or("https://www.googleapis.com/upload/drive/v3")
    }
    pub(crate) fn token(&self) -> &str {
        self.token_url
            .as_deref()
            .unwrap_or("https://oauth2.googleapis.com/token")
    }
    pub(crate) fn auth(&self) -> &str {
        self.auth_url
            .as_deref()
            .unwrap_or("https://accounts.google.com/o/oauth2/v2/auth")
    }
}

/// `backup.json`: one config per workspace path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    pub workspaces: BTreeMap<String, Config>,
}

/// What the last runs did for one workspace and target.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetState {
    /// [`Config::identity`] this state belongs to.
    pub target: String,
    /// When the last successful backup began (ms since the Unix epoch).
    pub last_success_ms: Option<u64>,
    /// Newest modification time (Unix ms) of the database files when the last successful
    /// backup began: later changes make the workspace due again (independent of clock jumps).
    pub source_mtime_ms: Option<u64>,
    /// UTC day (`YYYY-MM-DD`) of the last daily copy.
    pub last_daily: Option<String>,
    /// Blob hashes known to be on the target.
    pub uploaded: BTreeSet<String>,
    /// Google Drive file and folder ids by name (`folder`, `daily`, `blobs`, `latest`, …).
    pub drive_ids: BTreeMap<String, String>,
    /// The last failure (cleared by a success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl TargetState {
    /// This state if it belongs to `config`'s target, else a fresh one for it.
    pub fn for_config(self, config: &Config) -> TargetState {
        let id = config.identity();
        if self.target == id {
            self
        } else {
            TargetState {
                target: id,
                ..TargetState::default()
            }
        }
    }
}

/// `backup-state.json`: one state per workspace path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StateFile {
    pub workspaces: BTreeMap<String, TargetState>,
}

/// The key of a workspace in both files: its canonical path (the path itself if it cannot be
/// resolved).
pub fn workspace_key(db: &Path) -> String {
    std::fs::canonicalize(db)
        .unwrap_or_else(|_| db.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The config of workspace `db` in the config file at `path` (default: off).
pub fn load_config(path: &Path, db: &Path) -> Result<Config> {
    let mut file: ConfigFile = read_json(path)?;
    Ok(file.workspaces.remove(&workspace_key(db)).unwrap_or_default())
}

/// `$XDG_CONFIG_HOME/vibenote/backup.json` (or `~/.config/…`).
pub fn default_config_path() -> Option<PathBuf> {
    xdg("XDG_CONFIG_HOME", ".config").map(|d| d.join("vibenote").join("backup.json"))
}

/// `$XDG_STATE_HOME/vibenote/backup-state.json` (or `~/.local/state/…`).
pub fn default_state_path() -> Option<PathBuf> {
    xdg("XDG_STATE_HOME", ".local/state").map(|d| d.join("vibenote").join("backup-state.json"))
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)),
    }
}

/// Reads a JSON file; a missing file is the default value.
pub fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(BackupError::Io(format!("{}: {e}", path.display()))),
    };
    if meta.len() > MAX_FILE {
        return Err(BackupError::Config(format!("{} is too large", path.display())));
    }
    let text = std::fs::read_to_string(path).map_err(|e| BackupError::Io(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| BackupError::Config(format!("{}: {e}", path.display())))
}

/// Writes a JSON file atomically (temp file, fsync, rename), readable by the owner only.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let io = |e: std::io::Error| BackupError::Io(format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(|e| BackupError::Config(e.to_string()))?;
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(io)?;
    // An existing temp file keeps its mode; make sure.
    std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600)).map_err(io)?;
    f.write_all(&json).map_err(io)?;
    f.sync_all().map_err(io)?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(io)?;
    if let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
}
