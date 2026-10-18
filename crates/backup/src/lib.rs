//! One-way cloud backup of a workspace to S3 or Google Drive (ADR 0012, R-BACKUP-001..004).
//!
//! A run takes a consistent snapshot of the database (`VACUUM INTO` + `quick_check`), uploads
//! the blobs not on the target yet, then the snapshot as the latest copy and, once a day, as
//! that weekday's copy. Everything blocks; the engine runs it on the [`Worker`] thread. This is
//! the only crate with networking (R-PLAT-003).

pub mod clock;
pub mod config;
mod drive;
mod http;
mod s3;

pub use config::{Config, ConfigFile, DriveConfig, Provider, S3Config, StateFile, TargetState};
pub use drive::{Consent, begin_consent, finish_consent};

use std::path::{Path, PathBuf};
use std::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("{0}")]
    Config(String),
    #[error("snapshot: {0}")]
    Snapshot(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Remote(String),
    #[error("{0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, BackupError>;

/// A remote object, by role.
pub(crate) enum Obj<'a> {
    Latest,
    Daily(&'a str),
    Blob(&'a str),
}

/// A backup target.
pub(crate) trait Remote {
    fn put(&mut self, obj: &Obj, file: &Path) -> Result<()>;
    /// Downloads into `dest` (created); returns the size.
    fn get(&mut self, obj: &Obj, dest: &Path) -> Result<u64>;
}

fn remote<'a>(
    cfg: &Config,
    db_name: &str,
    drive_ids: &'a mut std::collections::BTreeMap<String, String>,
) -> Result<Box<dyn Remote + 'a>> {
    Ok(match cfg.provider {
        Provider::None => return Err(BackupError::Config("no backup target chosen".into())),
        Provider::S3 => Box::new(s3::S3::new(&cfg.s3, db_name)?),
        Provider::Drive => Box::new(drive::Drive::new(&cfg.drive, db_name, drive_ids)?),
    })
}

/// What a run uploaded.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Report {
    /// Blobs uploaded.
    pub blobs: usize,
    /// Bytes uploaded (database copies and blobs).
    pub bytes: u64,
    /// The weekday copy written, if this was the day's first backup.
    pub daily: Option<String>,
}

/// `path` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn file_name(db: &Path) -> Result<String> {
    db.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| BackupError::Config(format!("not a database file: {}", db.display())))
}

/// Removes a file when dropped.
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The blobs in a workspace's blob directory: (hash, path), sorted.
fn local_blobs(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let io = |e: std::io::Error| BackupError::Io(format!("{}: {e}", dir.display()));
    let mut out = Vec::new();
    let top = match std::fs::read_dir(dir) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(io(e)),
    };
    for sub in top {
        let sub = sub.map_err(io)?;
        if sub.file_name().len() != 2 || !sub.file_type().map_err(io)?.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(sub.path()).map_err(io)? {
            let f = f.map_err(io)?;
            let name = f.file_name().to_string_lossy().into_owned();
            if vibenote_storage::blobs::is_hash(&name) && f.file_type().map_err(io)?.is_file() {
                out.push((name, f.path()));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Backs up the workspace database `db` (and its blobs) to `cfg`'s target; `now_ms` is the wall
/// clock (Unix ms). `state` is updated as far as the run got, also when it fails, so blobs
/// uploaded before a failure are not uploaded again.
pub fn run(db: &Path, cfg: &Config, state: &mut TargetState, now_ms: u64) -> Result<Report> {
    let db_name = file_name(db)?;
    let tmp = TempFile(sibling(db, ".backup-tmp"));
    let _ = std::fs::remove_file(&tmp.0);
    let snap = |e: vibenote_storage::StorageError| BackupError::Snapshot(e.to_string());
    vibenote_storage::Store::open_reader(db)
        .map_err(snap)?
        .snapshot_into(&tmp.0)
        .map_err(snap)?;
    let mut report = Report::default();
    let TargetState {
        uploaded,
        drive_ids,
        last_daily,
        ..
    } = state;
    let mut remote = remote(cfg, &db_name, drive_ids)?;
    // Blobs first: a database on the target never references a blob that is not there.
    for (hash, path) in local_blobs(&vibenote_storage::BlobStore::dir_for(db))? {
        if uploaded.contains(&hash) {
            continue;
        }
        remote.put(&Obj::Blob(&hash), &path)?;
        uploaded.insert(hash);
        report.blobs = report.blobs.saturating_add(1);
        report.bytes = report.bytes.saturating_add(size(&path));
    }
    remote.put(&Obj::Latest, &tmp.0)?;
    report.bytes = report.bytes.saturating_add(size(&tmp.0));
    let today = clock::date(now_ms);
    if last_daily.as_deref() != Some(today.as_str()) {
        let day = clock::weekday(now_ms);
        remote.put(&Obj::Daily(day), &tmp.0)?;
        report.bytes = report.bytes.saturating_add(size(&tmp.0));
        report.daily = Some(day.to_string());
        *last_daily = Some(today);
    }
    drop(remote);
    state.last_success_ms = Some(now_ms);
    state.last_error = None;
    Ok(report)
}

/// What a restore wrote.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Restored {
    pub database: PathBuf,
    pub blobs: usize,
    pub bytes: u64,
}

/// Downloads the latest (or the `daily` weekday's) database named `db_name` and every blob it
/// references into `dir` (R-BACKUP-004). Refuses to overwrite an existing database there; blob
/// contents are verified against their hashes.
pub fn restore(
    cfg: &Config,
    state: &mut TargetState,
    db_name: &str,
    dir: &Path,
    daily: Option<&str>,
) -> Result<Restored> {
    if let Some(d) = daily
        && !clock::is_weekday(d)
    {
        return Err(BackupError::Config(format!("--daily takes mon … sun, not {d}")));
    }
    let io = |p: &Path, e: std::io::Error| BackupError::Io(format!("{}: {e}", p.display()));
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let db = dir.join(db_name);
    if db.exists() {
        return Err(BackupError::Config(format!(
            "{} exists; restore into an empty directory",
            db.display()
        )));
    }
    let mut remote = remote(cfg, db_name, &mut state.drive_ids)?;
    let part = TempFile(sibling(&db, ".part"));
    let obj = daily.map_or(Obj::Latest, Obj::Daily);
    let mut bytes = remote.get(&obj, &part.0)?;
    let refs = vibenote_storage::Store::open_reader(&part.0)
        .and_then(|s| s.blob_refs())
        .map_err(|e| BackupError::Snapshot(format!("the downloaded database does not open: {e}")))?;
    std::fs::rename(&part.0, &db).map_err(|e| io(&db, e))?;
    let blobs_dir = vibenote_storage::BlobStore::dir_for(&db);
    let mut refs: Vec<String> = refs
        .into_iter()
        .filter(|h| vibenote_storage::blobs::is_hash(h))
        .collect();
    refs.sort();
    for h in &refs {
        let sub = blobs_dir.join(h.get(..2).unwrap_or("00"));
        std::fs::create_dir_all(&sub).map_err(|e| io(&sub, e))?;
        let dest = sub.join(h);
        let part = TempFile(sibling(&dest, ".part"));
        bytes = bytes.saturating_add(remote.get(&Obj::Blob(h), &part.0)?);
        let got = std::fs::File::open(&part.0)
            .and_then(|f| {
                blake3::Hasher::new()
                    .update_reader(f)
                    .map(|h| h.finalize().to_hex().to_string())
            })
            .map_err(|e| io(&part.0, e))?;
        if got != *h {
            return Err(BackupError::Remote(format!("blob {h} in the backup is damaged")));
        }
        std::fs::rename(&part.0, &dest).map_err(|e| io(&dest, e))?;
    }
    Ok(Restored {
        database: db,
        blobs: refs.len(),
        bytes,
    })
}

/// Work for the backup thread.
#[derive(Debug)]
pub enum Job {
    Backup {
        db: PathBuf,
        config: Box<Config>,
        state: TargetState,
        now_ms: u64,
    },
    /// Waits for the browser sign-in to finish (`timeout_s`), then reports the refresh token.
    Connect {
        drive: DriveConfig,
        consent: Consent,
        timeout_s: u64,
    },
}

/// What the backup thread reports.
#[derive(Debug)]
pub enum Event {
    BackedUp {
        result: std::result::Result<Report, String>,
        state: TargetState,
    },
    Connected {
        result: std::result::Result<String, String>,
    },
}

/// The backup thread: jobs in, events out; it never touches the caller's state.
pub struct Worker {
    jobs: mpsc::Sender<Job>,
    events: mpsc::Receiver<Event>,
}

impl Worker {
    pub fn spawn() -> Result<Worker> {
        let (jobs, rx) = mpsc::channel::<Job>();
        let (tx, events) = mpsc::channel();
        std::thread::Builder::new()
            .name("vibenote-backup".into())
            .spawn(move || {
                for job in rx {
                    let ev = match job {
                        Job::Backup {
                            db,
                            config,
                            mut state,
                            now_ms,
                        } => {
                            let result = run(&db, &config, &mut state, now_ms).map_err(|e| e.to_string());
                            Event::BackedUp { result, state }
                        }
                        Job::Connect {
                            drive,
                            consent,
                            timeout_s,
                        } => Event::Connected {
                            result: finish_consent(&drive, consent, std::time::Duration::from_secs(timeout_s))
                                .map_err(|e| e.to_string()),
                        },
                    };
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| BackupError::Io(format!("starting the backup thread: {e}")))?;
        Ok(Worker { jobs, events })
    }

    pub fn send(&self, job: Job) -> Result<()> {
        self.jobs
            .send(job)
            .map_err(|_| BackupError::Io("the backup thread stopped".into()))
    }

    /// The next finished job, without waiting.
    pub fn poll(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// The next finished job, waiting up to `timeout` (CLI, tests).
    pub fn wait(&self, timeout: std::time::Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }
}
