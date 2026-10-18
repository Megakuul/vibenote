//! Google Drive client (Drive API v3, scope `drive.file`) and the OAuth sign-in for desktop
//! apps with PKCE and a loopback redirect (ADR 0012 §8).

use crate::config::DriveConfig;
use crate::http::{self, form};
use crate::{BackupError, Obj, Remote, Result};
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Exchanges a refresh token for an access token.
fn access_token(agent: &ureq::Agent, cfg: &DriveConfig) -> Result<String> {
    let refresh = cfg
        .refresh_token
        .as_deref()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| BackupError::Config("Google Drive is not connected".into()))?;
    let body = form(&[
        ("client_id", &cfg.client_id),
        ("client_secret", &cfg.client_secret),
        ("refresh_token", refresh),
        ("grant_type", "refresh_token"),
    ]);
    let what = "Google sign-in";
    let resp = agent
        .post(cfg.token())
        .content_type("application/x-www-form-urlencoded")
        .send(body)
        .map_err(|e| http::net(what, &e))?;
    let v = http::json(what, &mut http::ok(what, resp)?)?;
    v.get("access_token")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| BackupError::Remote(format!("{what}: no access token in the answer")))
}

pub(crate) struct Drive<'a> {
    agent: ureq::Agent,
    cfg: DriveConfig,
    bearer: String,
    stem: String,
    db_name: String,
    /// Cached folder and file ids (`TargetState::drive_ids`).
    ids: &'a mut BTreeMap<String, String>,
}

/// A Drive query string literal.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

impl<'a> Drive<'a> {
    pub(crate) fn new(cfg: &DriveConfig, db_name: &str, ids: &'a mut BTreeMap<String, String>) -> Result<Drive<'a>> {
        if cfg.client_id.trim().is_empty() {
            return Err(BackupError::Config("Google Drive: no OAuth client id".into()));
        }
        let agent = http::agent();
        let token = access_token(&agent, cfg)?;
        let stem = Path::new(db_name)
            .file_stem()
            .map_or_else(|| db_name.to_string(), |s| s.to_string_lossy().into_owned());
        Ok(Drive {
            agent,
            cfg: cfg.clone(),
            bearer: format!("Bearer {token}"),
            stem,
            db_name: db_name.to_string(),
            ids,
        })
    }

    /// The id of the first file named `name` in `parent` (not trashed).
    fn find(&self, name: &str, parent: &str, folder: bool) -> Result<Option<String>> {
        let mut q = format!(
            "name = {} and {} in parents and trashed = false",
            lit(name),
            lit(parent)
        );
        if folder {
            q.push_str(&format!(" and mimeType = {}", lit(FOLDER_MIME)));
        }
        let what = format!("looking up {name}");
        let resp = self
            .agent
            .get(format!("{}/files", self.cfg.api()))
            .query("q", &q)
            .query("fields", "files(id,name)")
            .query("spaces", "drive")
            .query("pageSize", "10")
            .header("authorization", &self.bearer)
            .call()
            .map_err(|e| http::net(&what, &e))?;
        let v = http::json(&what, &mut http::ok(&what, resp)?)?;
        Ok(v.get("files")
            .and_then(Value::as_array)
            .and_then(|f| f.first())
            .and_then(|f| f.get("id"))
            .and_then(Value::as_str)
            .map(String::from))
    }

    /// The folder `name` in `parent` (cached as `key`), created if missing.
    fn folder(&mut self, key: &str, name: &str, parent: &str) -> Result<String> {
        if let Some(id) = self.ids.get(key) {
            return Ok(id.clone());
        }
        let id = match self.find(name, parent, true)? {
            Some(id) => id,
            None => {
                let what = format!("creating folder {name}");
                let resp = self
                    .agent
                    .post(format!("{}/files", self.cfg.api()))
                    .query("fields", "id")
                    .header("authorization", &self.bearer)
                    .content_type("application/json; charset=UTF-8")
                    .send(json!({"name": name, "mimeType": FOLDER_MIME, "parents": [parent]}).to_string())
                    .map_err(|e| http::net(&what, &e))?;
                let v = http::json(&what, &mut http::ok(&what, resp)?)?;
                v.get("id")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .ok_or_else(|| BackupError::Remote(format!("{what}: no id in the answer")))?
            }
        };
        self.ids.insert(key.into(), id.clone());
        Ok(id)
    }

    /// The parent folder id, file name and cache key of `obj`.
    fn place(&mut self, obj: &Obj) -> Result<(String, String, Option<String>)> {
        let top = self.cfg.folder().to_string();
        let top = self.folder("dir:top", &top, "root")?;
        let stem = self.stem.clone();
        let ws = self.folder("dir:ws", &stem, &top)?;
        Ok(match obj {
            Obj::Latest => (ws, self.db_name.clone(), Some("file:latest".into())),
            Obj::Daily(day) => {
                let d = self.folder("dir:daily", "daily", &ws)?;
                (d, format!("{day}.sqlite"), Some(format!("file:daily/{day}")))
            }
            Obj::Blob(h) => (self.folder("dir:blobs", "blobs", &ws)?, (*h).to_string(), None),
        })
    }

    fn put_once(&mut self, obj: &Obj, file: &Path) -> Result<()> {
        let (parent, name, key) = self.place(obj)?;
        let existing = match &key {
            Some(k) => match self.ids.get(k) {
                Some(id) => Some(id.clone()),
                None => self.find(&name, &parent, false)?,
            },
            None => None,
        };
        let len = std::fs::metadata(file)
            .map_err(|e| BackupError::Io(format!("{}: {e}", file.display())))?
            .len()
            .to_string();
        let what = format!("uploading {name}");
        // A resumable session (works for any size), then the whole file in one PUT.
        let start = match &existing {
            Some(id) => self
                .agent
                .patch(format!("{}/files/{id}", self.cfg.upload()))
                .query("uploadType", "resumable")
                .header("authorization", &self.bearer)
                .header("x-upload-content-type", "application/octet-stream")
                .header("x-upload-content-length", &len)
                .content_type("application/json; charset=UTF-8")
                .send("{}"),
            None => self
                .agent
                .post(format!("{}/files", self.cfg.upload()))
                .query("uploadType", "resumable")
                .header("authorization", &self.bearer)
                .header("x-upload-content-type", "application/octet-stream")
                .header("x-upload-content-length", &len)
                .content_type("application/json; charset=UTF-8")
                .send(json!({"name": name, "parents": [parent]}).to_string()),
        }
        .map_err(|e| http::net(&what, &e))?;
        let start = http::ok(&what, start)?;
        let session = start
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or_else(|| BackupError::Remote(format!("{what}: no upload session")))?;
        let f = std::fs::File::open(file).map_err(|e| BackupError::Io(format!("{}: {e}", file.display())))?;
        let resp = self
            .agent
            .put(session)
            .header("authorization", &self.bearer)
            .header("content-type", "application/octet-stream")
            .send(f)
            .map_err(|e| http::net(&what, &e))?;
        let v = http::json(&what, &mut http::ok(&what, resp)?)?;
        if let (Some(k), Some(id)) = (key, v.get("id").and_then(Value::as_str)) {
            self.ids.insert(k, id.to_string());
        }
        Ok(())
    }

    fn get_once(&mut self, obj: &Obj, dest: &Path) -> Result<u64> {
        let (parent, name, _) = self.place(obj)?;
        let id = self
            .find(&name, &parent, false)?
            .ok_or_else(|| BackupError::NotFound(format!("{name} is not in the backup")))?;
        let what = format!("downloading {name}");
        let resp = self
            .agent
            .get(format!("{}/files/{id}", self.cfg.api()))
            .query("alt", "media")
            .header("authorization", &self.bearer)
            .call()
            .map_err(|e| http::net(&what, &e))?;
        http::download(&what, http::ok(&what, resp)?, dest)
    }
}

impl Remote for Drive<'_> {
    /// A cached id that no longer exists (folder or file deleted in Drive) drops the cache and
    /// tries once more.
    fn put(&mut self, obj: &Obj, file: &Path) -> Result<()> {
        match self.put_once(obj, file) {
            Err(BackupError::NotFound(_)) if !self.ids.is_empty() => {
                self.ids.clear();
                self.put_once(obj, file)
            }
            r => r,
        }
    }

    fn get(&mut self, obj: &Obj, dest: &Path) -> Result<u64> {
        match self.get_once(obj, dest) {
            Err(BackupError::NotFound(_)) if !self.ids.is_empty() => {
                self.ids.clear();
                self.get_once(obj, dest)
            }
            r => r,
        }
    }
}

/// A browser sign-in in progress: the consent URL to open and the loopback listener that
/// receives Google's redirect.
#[derive(Debug)]
pub struct Consent {
    pub url: String,
    listener: TcpListener,
    redirect: String,
    verifier: String,
    state: String,
}

fn random_token(bytes: usize) -> Result<String> {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).map_err(|e| BackupError::Io(format!("random: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

/// Starts a sign-in: listens on `127.0.0.1:<free port>` and builds the consent URL.
pub fn begin_consent(cfg: &DriveConfig) -> Result<Consent> {
    use sha2::Digest as _;
    if cfg.client_id.trim().is_empty() || cfg.client_secret.trim().is_empty() {
        return Err(BackupError::Config(
            "Google Drive needs an OAuth client id and secret (Google Cloud console → Credentials → Desktop app)"
                .into(),
        ));
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| BackupError::Io(format!("loopback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| BackupError::Io(format!("loopback listener: {e}")))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = random_token(48)?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let state = random_token(16)?;
    let url = format!(
        "{}?{}",
        cfg.auth(),
        form(&[
            ("client_id", cfg.client_id.trim()),
            ("redirect_uri", &redirect),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ])
    );
    Ok(Consent {
        url,
        listener,
        redirect,
        verifier,
        state,
    })
}

/// Reads one request from the browser; returns its query parameters.
fn read_redirect(stream: &mut TcpStream) -> Option<BTreeMap<String, String>> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut line = String::new();
    let n = std::io::Read::take(BufReader::new(&*stream), 8192)
        .read_line(&mut line)
        .ok()?;
    if n == 0 {
        return None;
    }
    let target = line.split_whitespace().nth(1)?;
    let query = target.split_once('?').map_or("", |(_, q)| q);
    Some(
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (http::dec(k), http::dec(v)))
            .collect(),
    )
}

fn answer(stream: &mut TcpStream, status: &str, text: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>vibenote</title><body style=\"font:16px sans-serif;margin:3em\">{text}</body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Waits (up to `timeout`) for the browser's redirect, then exchanges the code for a refresh
/// token.
pub fn finish_consent(cfg: &DriveConfig, consent: Consent, timeout: Duration) -> Result<String> {
    let deadline = Instant::now().checked_add(timeout).unwrap_or_else(Instant::now);
    consent
        .listener
        .set_nonblocking(true)
        .map_err(|e| BackupError::Io(format!("loopback listener: {e}")))?;
    let code = loop {
        match consent.listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let Some(q) = read_redirect(&mut stream) else { continue };
                if let Some(err) = q.get("error") {
                    answer(&mut stream, "200 OK", "Sign-in cancelled. You can close this tab.");
                    return Err(BackupError::Remote(format!("Google sign-in: {err}")));
                }
                match (q.get("code"), q.get("state")) {
                    (Some(code), Some(state)) if *state == consent.state => {
                        answer(
                            &mut stream,
                            "200 OK",
                            "vibenote is connected to Google Drive. You can close this tab.",
                        );
                        break code.clone();
                    }
                    // Anything else (favicon, stale tabs) is not the redirect.
                    _ => answer(&mut stream, "404 Not Found", "Not found."),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(BackupError::Remote("Google sign-in timed out".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(BackupError::Io(format!("loopback listener: {e}"))),
        }
    };
    let body = form(&[
        ("code", &code),
        ("client_id", cfg.client_id.trim()),
        ("client_secret", cfg.client_secret.trim()),
        ("redirect_uri", &consent.redirect),
        ("grant_type", "authorization_code"),
        ("code_verifier", &consent.verifier),
    ]);
    let what = "Google sign-in";
    let resp = http::agent()
        .post(cfg.token())
        .content_type("application/x-www-form-urlencoded")
        .send(body)
        .map_err(|e| http::net(what, &e))?;
    let v = http::json(what, &mut http::ok(what, resp)?)?;
    v.get("refresh_token")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| BackupError::Remote(format!("{what}: no refresh token in the answer")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_literals_are_escaped() {
        assert_eq!(lit("a'b\\c"), "'a\\'b\\\\c'");
        assert_eq!(crate::http::enc("x y", false), "x%20y");
    }
}
