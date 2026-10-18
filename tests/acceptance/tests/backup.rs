#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! R-BACKUP-001..004 (SPEC §13a) against fake S3 and Google Drive servers on loopback.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use vibenote_acceptance::*;
use vibenote_engine::Engine;

// ---------------------------------------------------------------- a tiny HTTP/1.1 server

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn resp(status: u16, body: impl Into<Vec<u8>>) -> Resp {
    Resp {
        status,
        headers: vec![],
        body: body.into(),
    }
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn parse_query(q: &str) -> BTreeMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (dec(k), dec(v)))
        .collect()
}

fn read_req(stream: &TcpStream) -> Option<Req> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    let len: usize = headers.get("content-length").map_or(0, |v| v.parse().unwrap());
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(Req {
        method,
        path: dec(path),
        query: parse_query(query),
        headers,
        body,
    })
}

/// Serves `handler` on 127.0.0.1 (one connection at a time, `Connection: close`); returns the
/// base URL.
fn serve(mut handler: impl FnMut(&Req, &str) -> Resp + Send + 'static) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let b = base.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let Some(req) = read_req(&s) else { continue };
            let r = handler(&req, &b);
            let mut head = format!(
                "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                r.status,
                r.body.len()
            );
            for (k, v) in &r.headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&r.body);
        }
    });
    base
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------- fake S3

#[derive(Default)]
struct S3State {
    objects: BTreeMap<String, Vec<u8>>,
    puts: Vec<String>,
    refuse: bool,
}

fn fake_s3() -> (String, Arc<Mutex<S3State>>) {
    let st = Arc::new(Mutex::new(S3State::default()));
    let s = st.clone();
    let base = serve(move |r, _| {
        let mut s = s.lock().unwrap();
        let auth = r.headers.get("authorization").cloned().unwrap_or_default();
        if !auth.starts_with("AWS4-HMAC-SHA256 Credential=AKTEST/") || !auth.contains("Signature=") {
            return resp(403, "bad auth");
        }
        match r.method.as_str() {
            "PUT" if s.refuse => resp(403, "<Error><Code>AccessDenied</Code></Error>"),
            "PUT" => {
                if r.headers.get("x-amz-content-sha256") != Some(&sha256_hex(&r.body)) {
                    return resp(400, "payload hash mismatch");
                }
                s.puts.push(r.path.clone());
                s.objects.insert(r.path.clone(), r.body.clone());
                resp(200, "")
            }
            "GET" => match s.objects.get(&r.path) {
                Some(b) => resp(200, b.clone()),
                None => resp(404, "<Error><Code>NoSuchKey</Code></Error>"),
            },
            _ => resp(405, ""),
        }
    });
    (base, st)
}

// ---------------------------------------------------------------- fake Google Drive

#[derive(Default, Debug)]
struct DriveFile {
    name: String,
    parent: String,
    folder: bool,
    content: Vec<u8>,
}

#[derive(Default)]
struct DriveState {
    files: BTreeMap<String, DriveFile>,
    sessions: BTreeMap<String, (Option<String>, String, String)>,
    next: u32,
    /// code_challenge sent to the consent page (PKCE).
    challenge: Option<String>,
}

impl DriveState {
    fn id(&mut self) -> String {
        self.next += 1;
        format!("id{}", self.next)
    }
    /// The file at `path` (names from My Drive down).
    fn at(&self, path: &[&str]) -> Option<&DriveFile> {
        let mut parent = "root".to_string();
        let mut found = None;
        for name in path {
            let (id, f) = self.files.iter().find(|(_, f)| f.parent == parent && f.name == *name)?;
            parent = id.clone();
            found = Some(f);
        }
        found
    }
}

/// The quoted literals of a Drive query, in order.
fn literals(q: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Option<String> = None;
    let mut esc = false;
    for c in q.chars() {
        match (&mut cur, c) {
            (Some(s), _) if esc => {
                s.push(c);
                esc = false;
            }
            (Some(_), '\\') => esc = true,
            (Some(s), '\'') => {
                out.push(std::mem::take(s));
                cur = None;
            }
            (Some(s), c) => s.push(c),
            (None, '\'') => cur = Some(String::new()),
            (None, _) => {}
        }
    }
    out
}

fn fake_drive() -> (String, Arc<Mutex<DriveState>>) {
    let st = Arc::new(Mutex::new(DriveState::default()));
    let s = st.clone();
    let base = serve(move |r, base| {
        let mut s = s.lock().unwrap();
        if r.path == "/token" {
            let f = parse_query(std::str::from_utf8(&r.body).unwrap());
            return match f.get("grant_type").map(String::as_str) {
                Some("authorization_code") => {
                    let verifier = f.get("code_verifier").cloned().unwrap_or_default();
                    use base64::Engine as _;
                    use sha2::Digest;
                    let ch = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(sha2::Sha256::digest(verifier.as_bytes()));
                    if f.get("code").map(String::as_str) != Some("CODE")
                        || s.challenge.as_deref() != Some(ch.as_str())
                        || f.get("client_secret").map(String::as_str) != Some("SECRET")
                    {
                        return resp(400, r#"{"error":"invalid_grant"}"#);
                    }
                    resp(200, r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#)
                }
                Some("refresh_token") if f.get("refresh_token").map(String::as_str) == Some("RT") => {
                    resp(200, r#"{"access_token":"AT","expires_in":3600}"#)
                }
                _ => resp(400, r#"{"error":"invalid_grant"}"#),
            };
        }
        // The upload session URL authorises by itself.
        if !r.path.starts_with("/session/") && r.headers.get("authorization").map(String::as_str) != Some("Bearer AT") {
            return resp(401, "{}");
        }
        let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
        match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/drive/v3/files") => {
                let q = r.query.get("q").cloned().unwrap_or_default();
                let lit = literals(&q);
                let folder_only = q.contains("mimeType");
                let files: Vec<Value> = s
                    .files
                    .iter()
                    .filter(|(_, f)| f.name == lit[0] && f.parent == lit[1] && (!folder_only || f.folder))
                    .map(|(id, f)| json!({"id": id, "name": f.name}))
                    .collect();
                resp(200, json!({"files": files}).to_string())
            }
            ("POST", "/drive/v3/files") => {
                let id = s.id();
                s.files.insert(
                    id.clone(),
                    DriveFile {
                        name: body["name"].as_str().unwrap().into(),
                        parent: body["parents"][0].as_str().unwrap().into(),
                        folder: body["mimeType"] == "application/vnd.google-apps.folder",
                        content: vec![],
                    },
                );
                resp(200, json!({"id": id}).to_string())
            }
            ("POST", "/upload/drive/v3/files") => {
                let sid = s.id();
                let parent = body["parents"][0].as_str().unwrap().to_string();
                if !s.files.contains_key(&parent) {
                    return resp(404, r#"{"error":"parent not found"}"#);
                }
                s.sessions
                    .insert(sid.clone(), (None, body["name"].as_str().unwrap().into(), parent));
                Resp {
                    status: 200,
                    headers: vec![("Location".into(), format!("{base}/session/{sid}"))],
                    body: vec![],
                }
            }
            ("PATCH", p) if p.starts_with("/upload/drive/v3/files/") => {
                let id = p.trim_start_matches("/upload/drive/v3/files/").to_string();
                if !s.files.contains_key(&id) {
                    return resp(404, r#"{"error":"not found"}"#);
                }
                let sid = s.id();
                s.sessions.insert(sid.clone(), (Some(id), String::new(), String::new()));
                Resp {
                    status: 200,
                    headers: vec![("Location".into(), format!("{base}/session/{sid}"))],
                    body: vec![],
                }
            }
            ("PUT", p) if p.starts_with("/session/") => {
                let sid = p.trim_start_matches("/session/");
                let Some((existing, name, parent)) = s.sessions.remove(sid) else {
                    return resp(404, "{}");
                };
                let id = match existing {
                    Some(id) => {
                        s.files.get_mut(&id).unwrap().content = r.body.clone();
                        id
                    }
                    None => {
                        let id = s.id();
                        s.files.insert(
                            id.clone(),
                            DriveFile {
                                name,
                                parent,
                                folder: false,
                                content: r.body.clone(),
                            },
                        );
                        id
                    }
                };
                resp(200, json!({"id": id}).to_string())
            }
            ("GET", p)
                if p.starts_with("/drive/v3/files/") && r.query.get("alt").map(String::as_str) == Some("media") =>
            {
                match s.files.get(p.trim_start_matches("/drive/v3/files/")) {
                    Some(f) => resp(200, f.content.clone()),
                    None => resp(404, "{}"),
                }
            }
            _ => resp(404, "{}"),
        }
    });
    (base, st)
}

// ---------------------------------------------------------------- workspace helpers

struct Ws {
    dir: tempfile::TempDir,
    e: Engine,
    note: u64,
}

impl Ws {
    fn db(&self) -> PathBuf {
        self.dir.path().join("ws.sqlite")
    }
    fn config(&self) -> PathBuf {
        self.dir.path().join("config/backup.json")
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state/backup-state.json")
    }
}

/// A workspace file with a note holding a stroke, an image and a PDF, and backup handling on.
fn workspace() -> Ws {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open(&dir.path().join("ws.sqlite")).unwrap();
    let note = id(&run(&mut e, "workspace.note.create", json!({"name": "Sicherung"})));
    run(&mut e, "note.open", json!({"id": note}));
    line(&mut e, note, (10.0, 10.0), (200.0, 80.0), 0.5);
    run(
        &mut e,
        "image.import",
        json!({"path": root().join("tests/golden/loops-4.png"), "at": [0.0, 200.0]}),
    );
    run(
        &mut e,
        "pdf.import",
        json!({"path": root().join("tests/fixtures/pdf/three-pages.pdf"), "at": [0.0, 1200.0]}),
    );
    e.sync().unwrap();
    let mut w = Ws { dir, e, note };
    let (c, s) = (w.config(), w.state());
    w.e.use_backup_config(&c, &s).unwrap();
    w
}

fn status(e: &mut Engine) -> Value {
    run(e, "backup.status", json!({}))
}

fn s3_config(base: &str) -> Value {
    json!({"provider": "s3", "s3": {
        "endpoint": base, "bucket": "notes", "region": "eu-central-2",
        "access_key_id": "AKTEST", "secret_access_key": "TOP-SECRET-KEY"}})
}

fn wait(e: &mut Engine) {
    assert!(e.backup_wait(Duration::from_secs(60)), "backup did not finish");
}

/// The notes and object counts of a workspace file.
fn summary(db: &Path) -> Vec<(String, usize)> {
    let mut e = Engine::open(db).unwrap();
    e.load_all().unwrap();
    let ws = &e.session.ws;
    let mut out: Vec<(String, usize)> = ws
        .notes
        .iter()
        .map(|(id, n)| (n.name.clone(), ws.canvas(*id).map_or(0, |c| c.objects.len())))
        .collect();
    out.sort();
    out
}

fn blob_refs(db: &Path) -> Vec<String> {
    let mut v: Vec<String> = vibenote_storage::Store::open_reader(db)
        .unwrap()
        .blob_refs()
        .unwrap()
        .into_iter()
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------- tests

/// R-BACKUP-001: off by default; settings round-trip; secrets stay out of the status; the file
/// is private; other workspaces are not affected; in-memory engines have no backup.
#[test]
fn r_backup_001_settings() {
    let (mut mem, _) = engine_with_note();
    assert_eq!(status(&mut mem)["available"], false);
    assert!(mem.execute("backup.now", json!({})).is_err());

    let mut w = workspace();
    let st = status(&mut w.e);
    assert_eq!(
        (st["enabled"].clone(), st["provider"].clone()),
        (json!(false), json!("none"))
    );
    assert!(
        w.e.execute("backup.configure", json!({"enabled": true})).is_err(),
        "no target yet"
    );
    let mut cfg = s3_config("https://s3.example.org");
    cfg["interval_min"] = json!(15);
    cfg["s3"]["prefix"] = json!("backups");
    run(&mut w.e, "backup.configure", cfg);
    let st = run(&mut w.e, "backup.configure", json!({"enabled": true}));
    assert_eq!(st["enabled"], true);
    assert_eq!(st["interval_min"], 15);
    assert_eq!(st["s3"]["bucket"], "notes");
    assert_eq!(st["s3"]["prefix"], "backups");
    assert_eq!(st["s3"]["has_secret"], true);
    assert!(!st.to_string().contains("TOP-SECRET-KEY"), "secret in the status: {st}");
    assert!(
        w.e.execute("backup.configure", json!({"interval_min": 0})).is_err(),
        "interval range"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(w.config()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // Switching to Drive keeps the S3 settings; a reopened engine reads them back.
    run(
        &mut w.e,
        "backup.configure",
        json!({"enabled": false, "provider": "drive", "drive": {"client_id": "c", "client_secret": "s"}}),
    );
    let mut again = Engine::open(&w.db()).unwrap();
    again.use_backup_config(&w.config(), &w.state()).unwrap();
    let st = status(&mut again);
    assert_eq!(st["provider"], "drive");
    assert_eq!(st["drive"]["connected"], false);
    assert_eq!(st["s3"]["bucket"], "notes");
    assert!(
        again.execute("backup.configure", json!({"enabled": true})).is_err(),
        "Drive is not connected"
    );
    // Another workspace with the same config file: off.
    let other = w.dir.path().join("other.sqlite");
    let mut o = Engine::open(&other).unwrap();
    o.use_backup_config(&w.config(), &w.state()).unwrap();
    assert_eq!(status(&mut o)["enabled"], false);
    assert_eq!(status(&mut o)["provider"], "none");
    run(&mut o, "backup.configure", s3_config("https://other.example.org"));
    // The first workspace's entry is untouched.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(w.config()).unwrap()).unwrap();
    assert_eq!(file["workspaces"].as_object().unwrap().len(), 2);
}

/// R-BACKUP-002: a scheduled backup uploads a consistent database and the blobs; nothing
/// without changes; after a change only the database again; a damaged database is never
/// uploaded.
#[test]
fn r_backup_002_scheduled_backup_to_s3() {
    let (base, fake) = fake_s3();
    let mut w = workspace();
    let mut cfg = s3_config(&base);
    cfg["enabled"] = json!(true);
    run(&mut w.e, "backup.configure", cfg);
    let t0 = SystemTime::now();
    assert!(w.e.backup_tick(t0).is_empty());
    assert_eq!(status(&mut w.e)["running"], true);
    wait(&mut w.e);
    let st = status(&mut w.e);
    assert_eq!(st["last_error"], Value::Null, "{st}");
    assert!(st["last_success_ms"].as_u64().is_some());
    let day = st["last_report"]["daily"].as_str().unwrap().to_string();
    let refs = blob_refs(&w.db());
    assert!(refs.len() >= 2, "image and PDF blobs: {refs:?}");
    {
        let f = fake.lock().unwrap();
        let latest = "/notes/vibenote/ws/ws.sqlite";
        assert!(
            f.objects.contains_key(latest),
            "{:?}",
            f.objects.keys().collect::<Vec<_>>()
        );
        assert!(
            f.objects
                .contains_key(&format!("/notes/vibenote/ws/daily/{day}.sqlite"))
        );
        for h in &refs {
            let k = format!("/notes/vibenote/ws/ws.sqlite.blobs/{}/{h}", &h[..2]);
            assert!(f.objects.contains_key(&k), "blob {h} missing");
        }
        // The uploaded database is the same workspace.
        let copy = w.dir.path().join("copy/ws.sqlite");
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(&copy, &f.objects[latest]).unwrap();
        assert_eq!(summary(&copy), summary(&w.db()));
    }
    let puts = fake.lock().unwrap().puts.len();
    // No changes: nothing happens, even long after.
    w.e.backup_tick(t0 + Duration::from_secs(11 * 60));
    assert_eq!(status(&mut w.e)["running"], false);
    assert_eq!(fake.lock().unwrap().puts.len(), puts);
    // A change: within the interval nothing yet, after it the database only (blobs are on the
    // target already).
    std::thread::sleep(Duration::from_millis(20));
    line(&mut w.e, w.note, (300.0, 10.0), (400.0, 90.0), 0.5);
    w.e.sync().unwrap();
    w.e.backup_tick(t0 + Duration::from_secs(11 * 60 + 30));
    assert_eq!(status(&mut w.e)["running"], false, "checked only once a minute");
    w.e.backup_tick(t0 + Duration::from_secs(12 * 60));
    assert_eq!(status(&mut w.e)["running"], true);
    wait(&mut w.e);
    let f = fake.lock().unwrap();
    let new: Vec<&String> = f.puts[puts..].iter().collect();
    assert!(new.iter().any(|p| p.ends_with("/ws/ws.sqlite")), "{new:?}");
    assert!(
        !new.iter().any(|p| p.contains(".blobs/")),
        "blobs uploaded again: {new:?}"
    );
    let puts = f.puts.len();
    drop(f);
    // Another change: not before the interval (10 min) since that backup.
    std::thread::sleep(Duration::from_millis(20));
    line(&mut w.e, w.note, (300.0, 200.0), (400.0, 290.0), 0.5);
    w.e.sync().unwrap();
    w.e.backup_tick(t0 + Duration::from_secs(16 * 60));
    assert_eq!(status(&mut w.e)["running"], false, "interval");
    w.e.backup_tick(t0 + Duration::from_secs(22 * 60));
    assert_eq!(status(&mut w.e)["running"], true);
    wait(&mut w.e);
    assert!(fake.lock().unwrap().puts.len() > puts);

    // A damaged database: the run fails and uploads nothing.
    let (base2, fake2) = fake_s3();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("bad.sqlite");
    {
        let mut e = Engine::open(&db).unwrap();
        let n = id(&run(&mut e, "workspace.note.create", json!({"name": "x"})));
        run(&mut e, "note.open", json!({"id": n}));
        for i in 0..300 {
            let y = f64::from(i) * 10.0;
            line(&mut e, n, (0.0, y), (500.0, y + 5.0), 0.5);
        }
        e.sync().unwrap();
    }
    // The last connection to close moves the WAL into the database file.
    drop(vibenote_storage::Store::open(&db).unwrap());
    let mut bytes = std::fs::read(&db).unwrap();
    assert!(bytes.len() > 64 * 1024, "{} bytes", bytes.len());
    let end = bytes.len() - 4096;
    for b in &mut bytes[8192..end] {
        *b = 0xA5;
    }
    std::fs::write(&db, bytes).unwrap();
    let _ = std::fs::remove_file(dir.path().join("bad.sqlite-wal"));
    let cfg: vibenote_backup::Config = serde_json::from_value(json!({
        "enabled": true, "provider": "s3",
        "s3": {"endpoint": base2, "bucket": "b", "region": "r", "access_key_id": "AKTEST", "secret_access_key": "k"}
    }))
    .unwrap();
    let mut state = vibenote_backup::TargetState::default();
    let r = vibenote_backup::run(&db, &cfg, &mut state, 0);
    assert!(r.is_err(), "{r:?}");
    assert!(fake2.lock().unwrap().puts.is_empty());
    assert!(!dir.path().join("bad.sqlite.backup-tmp").exists(), "temp file removed");
}

/// R-BACKUP-003: a refused upload is reported (status, notice) and retried one interval later.
#[test]
fn r_backup_003_failures_are_reported_and_retried() {
    let (base, fake) = fake_s3();
    fake.lock().unwrap().refuse = true;
    let mut w = workspace();
    let mut cfg = s3_config(&base);
    cfg["enabled"] = json!(true);
    run(&mut w.e, "backup.configure", cfg);
    let t0 = SystemTime::now();
    w.e.backup_tick(t0);
    wait(&mut w.e);
    let notices = w.e.backup_tick(t0 + Duration::from_secs(1));
    assert!(notices.iter().any(|n| n.contains("403")), "{notices:?}");
    let st = status(&mut w.e);
    assert!(st["last_error"].as_str().unwrap().contains("403"), "{st}");
    assert_eq!(st["last_success_ms"], Value::Null);
    // Not every minute: one interval later.
    fake.lock().unwrap().refuse = false;
    w.e.backup_tick(t0 + Duration::from_secs(2 * 60));
    assert_eq!(status(&mut w.e)["running"], false);
    w.e.backup_tick(t0 + Duration::from_secs(11 * 60));
    assert_eq!(status(&mut w.e)["running"], true);
    wait(&mut w.e);
    let st = status(&mut w.e);
    assert_eq!(st["last_error"], Value::Null, "{st}");
    assert!(st["last_success_ms"].as_u64().is_some());
}

/// Restores the configured target of `w` into a new directory and compares.
fn restore_and_compare(w: &Ws) {
    let cfg = vibenote_backup::config::load_config(&w.config(), &w.db()).unwrap();
    let out = w.dir.path().join("restored");
    let mut state = vibenote_backup::TargetState::default();
    let r = vibenote_backup::restore(&cfg, &mut state, "ws.sqlite", &out, None).unwrap();
    let refs = blob_refs(&w.db());
    assert_eq!(r.blobs, refs.len());
    assert_eq!(summary(&r.database), summary(&w.db()));
    let blobs = vibenote_storage::BlobStore::open(&vibenote_storage::BlobStore::dir_for(&r.database)).unwrap();
    for h in &refs {
        let b = blobs.get(h).unwrap();
        assert_eq!(vibenote_storage::blobs::hash(&b), *h);
    }
    // The restored workspace renders its image and PDF like the original.
    let mut e = Engine::open(&r.database).unwrap();
    e.load_all().unwrap();
    assert!(e.session.ws.notes.values().any(|n| n.name == "Sicherung"));
    // Never over an existing workspace.
    assert!(vibenote_backup::restore(&cfg, &mut state, "ws.sqlite", &out, None).is_err());
}

/// R-BACKUP-004: S3 round trip (latest and a weekday copy).
#[test]
fn r_backup_004_restore_from_s3() {
    let (base, _fake) = fake_s3();
    let mut w = workspace();
    let mut cfg = s3_config(&base);
    cfg["enabled"] = json!(true);
    run(&mut w.e, "backup.configure", cfg);
    run(&mut w.e, "backup.now", json!({}));
    wait(&mut w.e);
    let st = status(&mut w.e);
    assert_eq!(st["last_error"], Value::Null, "{st}");
    restore_and_compare(&w);
    let day = st["last_report"]["daily"].as_str().unwrap().to_string();
    let cfg = vibenote_backup::config::load_config(&w.config(), &w.db()).unwrap();
    let out = w.dir.path().join("daily");
    let mut state = vibenote_backup::TargetState::default();
    let r = vibenote_backup::restore(&cfg, &mut state, "ws.sqlite", &out, Some(&day)).unwrap();
    assert_eq!(summary(&r.database), summary(&w.db()));
}

/// Sends the browser's redirect to the loopback listener of a consent URL.
fn browser_redirect(url: &str, code: &str) {
    let q = parse_query(url.split_once('?').unwrap().1);
    let redirect = q["redirect_uri"].trim_start_matches("http://").to_string();
    let mut s = TcpStream::connect(&redirect).unwrap();
    write!(
        s,
        "GET /?state={}&code={code}&scope=x HTTP/1.1\r\nHost: {redirect}\r\n\r\n",
        q["state"]
    )
    .unwrap();
    let mut answer = String::new();
    s.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
}

/// R-BACKUP-001 (Drive sign-in with PKCE) and R-BACKUP-004: Google Drive round trip.
#[test]
fn r_backup_004_google_drive_sign_in_backup_and_restore() {
    let (base, fake) = fake_drive();
    let mut w = workspace();
    run(
        &mut w.e,
        "backup.configure",
        json!({"provider": "drive", "drive": {
            "client_id": "CLIENT", "client_secret": "SECRET",
            "api_base": format!("{base}/drive/v3"), "upload_base": format!("{base}/upload/drive/v3"),
            "token_url": format!("{base}/token"), "auth_url": format!("{base}/auth")}}),
    );
    let url = run(&mut w.e, "backup.drive_connect", json!({"open": false}))["url"]
        .as_str()
        .unwrap()
        .to_string();
    let q = parse_query(url.split_once('?').unwrap().1);
    assert!(url.starts_with(&format!("{base}/auth?")));
    assert_eq!(q["scope"], "https://www.googleapis.com/auth/drive.file");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(status(&mut w.e)["connecting"], true);
    fake.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    browser_redirect(&url, "CODE");
    wait(&mut w.e);
    let st = status(&mut w.e);
    assert_eq!(st["drive"]["connected"], true, "{st}");
    assert!(!st.to_string().contains("RT\""), "refresh token in the status");
    run(&mut w.e, "backup.configure", json!({"enabled": true}));
    w.e.backup_tick(SystemTime::now());
    wait(&mut w.e);
    let st = status(&mut w.e);
    assert_eq!(st["last_error"], Value::Null, "{st}");
    let day = st["last_report"]["daily"].as_str().unwrap().to_string();
    {
        let f = fake.lock().unwrap();
        assert!(f.at(&["vibenote-backup", "ws", "ws.sqlite"]).is_some(), "{:?}", f.files);
        assert!(
            f.at(&["vibenote-backup", "ws", "daily", &format!("{day}.sqlite")])
                .is_some()
        );
        for h in blob_refs(&w.db()) {
            assert!(f.at(&["vibenote-backup", "ws", "blobs", &h]).is_some(), "blob {h}");
        }
    }
    // A second backup updates the file in place (no duplicate).
    run(&mut w.e, "backup.now", json!({}));
    wait(&mut w.e);
    assert_eq!(status(&mut w.e)["last_error"], Value::Null);
    let f = fake.lock().unwrap();
    assert_eq!(f.files.values().filter(|x| x.name == "ws.sqlite").count(), 1);
    drop(f);
    restore_and_compare(&w);
}
