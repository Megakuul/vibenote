//! S3 client: Signature V4, one `PUT`/`GET` per object (ADR 0012 §7).

use crate::config::S3Config;
use crate::http::{self, enc, hex};
use crate::{BackupError, Obj, Remote, Result};
use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};
use std::path::Path;

/// SHA-256 of an empty payload.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// What a request is signed with.
pub(crate) struct SignInput<'a> {
    pub method: &'a str,
    pub host: &'a str,
    /// Already encoded path.
    pub uri: &'a str,
    /// Raw query pairs (encoded and sorted here).
    pub query: &'a [(&'a str, &'a str)],
    /// Further headers to sign (lower-case names; `host` is added here).
    pub headers: &'a [(&'a str, &'a str)],
    pub payload_sha256: &'a str,
    pub amz_date: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    pub key_id: &'a str,
    pub secret: &'a str,
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    match Hmac::<Sha256>::new_from_slice(key) {
        Ok(mut m) => {
            m.update(data.as_bytes());
            m.finalize().into_bytes().to_vec()
        }
        // HMAC accepts keys of any length.
        Err(_) => Vec::new(),
    }
}

/// The canonical query string (pairs encoded, sorted).
fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (enc(k, false), enc(v, false))).collect();
    q.sort();
    q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// The `Authorization` header value for `r` (AWS Signature Version 4).
pub(crate) fn authorization(r: &SignInput) -> String {
    let mut headers: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    headers.push(("host".into(), r.host.to_string()));
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        r.method,
        r.uri,
        canonical_query(r.query),
        canonical_headers,
        signed,
        r.payload_sha256
    );
    let date = r.amz_date.get(..8).unwrap_or("");
    let scope = format!("{date}/{}/{}/aws4_request", r.region, r.service);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        r.amz_date,
        hex(&Sha256::digest(request.as_bytes()))
    );
    let k = hmac(format!("AWS4{}", r.secret).as_bytes(), date);
    let k = hmac(&k, r.region);
    let k = hmac(&k, r.service);
    let k = hmac(&k, "aws4_request");
    let sig = hex(&hmac(&k, &to_sign));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={sig}",
        r.key_id
    )
}

/// Access key, secret and optional session token.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Creds {
    pub key: String,
    pub secret: String,
    pub token: Option<String>,
}

/// Credentials: the config's key pair, else its profile, else `AWS_ACCESS_KEY_ID` /
/// `AWS_SECRET_ACCESS_KEY`, else the `default` profile (`$AWS_SHARED_CREDENTIALS_FILE` or
/// `~/.aws/credentials`).
pub(crate) fn creds(cfg: &S3Config) -> Result<Creds> {
    let some = |v: &Option<String>| v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    if let (Some(key), Some(secret)) = (some(&cfg.access_key_id), some(&cfg.secret_access_key)) {
        return Ok(Creds {
            key,
            secret,
            token: some(&cfg.session_token),
        });
    }
    let file = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".aws/credentials")));
    let read = |profile: &str| -> Option<Creds> {
        let text = std::fs::read_to_string(file.as_ref()?).ok()?;
        profile_creds(&text, profile)
    };
    if let Some(p) = some(&cfg.profile) {
        return read(&p).ok_or_else(|| BackupError::Config(format!("AWS profile '{p}' has no access key")));
    }
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let (Some(key), Some(secret)) = (env("AWS_ACCESS_KEY_ID"), env("AWS_SECRET_ACCESS_KEY")) {
        return Ok(Creds {
            key,
            secret,
            token: env("AWS_SESSION_TOKEN"),
        });
    }
    read("default").ok_or_else(|| BackupError::Config("S3: no access key configured".into()))
}

/// The key pair of `profile` in an AWS credentials file (INI).
pub(crate) fn profile_creds(text: &str, profile: &str) -> Option<Creds> {
    let mut in_profile = false;
    let (mut key, mut secret, mut token) = (None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_profile = name.trim() == profile;
            continue;
        }
        if !in_profile {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = Some(v.trim().to_string());
        match k.trim() {
            "aws_access_key_id" => key = v,
            "aws_secret_access_key" => secret = v,
            "aws_session_token" => token = v,
            _ => {}
        }
    }
    Some(Creds {
        key: key?,
        secret: secret?,
        token,
    })
}

pub(crate) struct S3 {
    agent: ureq::Agent,
    cfg: S3Config,
    creds: Creds,
    /// `scheme://authority`, host header, path before the key (encoded, no trailing `/`).
    origin: String,
    host: String,
    base: String,
    /// Key prefix up to the workspace (`<prefix>/<stem>`) and the database file name.
    root: String,
    db_name: String,
}

impl S3 {
    pub(crate) fn new(cfg: &S3Config, db_name: &str) -> Result<S3> {
        if cfg.bucket.trim().is_empty() {
            return Err(BackupError::Config("S3: no bucket".into()));
        }
        if cfg.region.trim().is_empty() {
            return Err(BackupError::Config("S3: no region".into()));
        }
        let creds = creds(cfg)?;
        let bucket = cfg.bucket.trim();
        let (origin, host, base) = match cfg.endpoint.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) => {
                let (scheme, rest) = e
                    .split_once("://")
                    .ok_or_else(|| BackupError::Config(format!("S3 endpoint needs http:// or https://: {e}")))?;
                if scheme != "https" && scheme != "http" {
                    return Err(BackupError::Config(format!("S3 endpoint scheme {scheme}")));
                }
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                let path = path.trim_matches('/');
                let base = if path.is_empty() {
                    format!("/{}", enc(bucket, false))
                } else {
                    format!("/{}/{}", enc(path, true), enc(bucket, false))
                };
                (format!("{scheme}://{authority}"), authority.to_string(), base)
            }
            // Virtual-hosted style on AWS, except for bucket names with dots (TLS wildcard).
            None if !bucket.contains('.') => {
                let host = format!("{bucket}.s3.{}.amazonaws.com", cfg.region.trim());
                (format!("https://{host}"), host, String::new())
            }
            None => {
                let host = format!("s3.{}.amazonaws.com", cfg.region.trim());
                (format!("https://{host}"), host, format!("/{}", enc(bucket, false)))
            }
        };
        let stem = Path::new(db_name)
            .file_stem()
            .map_or_else(|| db_name.to_string(), |s| s.to_string_lossy().into_owned());
        Ok(S3 {
            agent: http::agent(),
            cfg: cfg.clone(),
            creds,
            origin,
            host,
            base,
            root: format!("{}/{stem}", cfg.prefix()),
            db_name: db_name.to_string(),
        })
    }

    /// The object key of `obj`.
    pub(crate) fn key(&self, obj: &Obj) -> String {
        match obj {
            Obj::Latest => format!("{}/{}", self.root, self.db_name),
            Obj::Daily(day) => format!("{}/daily/{day}.sqlite", self.root),
            Obj::Blob(h) => format!(
                "{}/{}.blobs/{}/{h}",
                self.root,
                self.db_name,
                h.get(..2).unwrap_or("00")
            ),
        }
    }

    /// Signed headers for a request on `uri`.
    fn sign(&self, method: &str, uri: &str, payload: &str) -> Vec<(String, String)> {
        let now = crate::clock::amz_date(crate::clock::unix_ms(std::time::SystemTime::now()));
        let mut headers: Vec<(&str, &str)> = vec![("x-amz-content-sha256", payload), ("x-amz-date", &now)];
        if let Some(t) = &self.creds.token {
            headers.push(("x-amz-security-token", t));
        }
        let auth = authorization(&SignInput {
            method,
            host: &self.host,
            uri,
            query: &[],
            headers: &headers,
            payload_sha256: payload,
            amz_date: &now,
            region: self.cfg.region.trim(),
            service: "s3",
            key_id: &self.creds.key,
            secret: &self.creds.secret,
        });
        let mut out: Vec<(String, String)> = headers.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        out.push(("authorization".into(), auth));
        out
    }
}

impl Remote for S3 {
    fn put(&mut self, obj: &Obj, file: &Path) -> Result<()> {
        let key = self.key(obj);
        let uri = format!("{}/{}", self.base, enc(&key, true));
        let payload = http::sha256_file(file)?;
        let f = std::fs::File::open(file).map_err(|e| BackupError::Io(format!("{}: {e}", file.display())))?;
        let mut req = self.agent.put(format!("{}{uri}", self.origin));
        for (k, v) in self.sign("PUT", &uri, &payload) {
            req = req.header(k, v);
        }
        let what = format!("uploading {key}");
        let resp = req
            .header("content-type", "application/octet-stream")
            .send(f)
            .map_err(|e| http::net(&what, &e))?;
        http::ok(&what, resp).map(drop)
    }

    fn get(&mut self, obj: &Obj, dest: &Path) -> Result<u64> {
        let key = self.key(obj);
        let uri = format!("{}/{}", self.base, enc(&key, true));
        let mut req = self.agent.get(format!("{}{uri}", self.origin));
        for (k, v) in self.sign("GET", &uri, EMPTY_SHA256) {
            req = req.header(k, v);
        }
        let what = format!("downloading {key}");
        let resp = req.call().map_err(|e| http::net(&what, &e))?;
        http::download(&what, http::ok(&what, resp)?, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example of the AWS SigV4 documentation (IAM ListUsers, 2015-08-30).
    #[test]
    fn signs_the_aws_documentation_example() {
        let auth = authorization(&SignInput {
            method: "GET",
            host: "iam.amazonaws.com",
            uri: "/",
            query: &[("Version", "2010-05-08"), ("Action", "ListUsers")],
            headers: &[
                ("content-type", "application/x-www-form-urlencoded; charset=utf-8"),
                ("x-amz-date", "20150830T123600Z"),
            ],
            payload_sha256: EMPTY_SHA256,
            amz_date: "20150830T123600Z",
            region: "us-east-1",
            service: "iam",
            key_id: "AKIDEXAMPLE",
            secret: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        });
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    #[test]
    fn reads_profiles_and_builds_keys() {
        let text = "[default]\naws_access_key_id = A\naws_secret_access_key=B\n\n[work]\n# c\naws_access_key_id=C\naws_secret_access_key = D\naws_session_token = T\n";
        assert_eq!(
            profile_creds(text, "work"),
            Some(Creds {
                key: "C".into(),
                secret: "D".into(),
                token: Some("T".into())
            })
        );
        assert_eq!(profile_creds(text, "default").map(|c| c.key), Some("A".into()));
        assert_eq!(profile_creds(text, "none"), None);
        let cfg = S3Config {
            bucket: "my.bucket".into(),
            region: "eu-central-2".into(),
            access_key_id: Some("k".into()),
            secret_access_key: Some("s".into()),
            ..S3Config::default()
        };
        let s3 = S3::new(&cfg, "workspace.sqlite").unwrap();
        assert_eq!(s3.origin, "https://s3.eu-central-2.amazonaws.com");
        assert_eq!(s3.base, "/my.bucket");
        let h = "ab".repeat(32);
        assert_eq!(
            s3.key(&Obj::Blob(&h)),
            format!("vibenote/workspace/workspace.sqlite.blobs/ab/{h}")
        );
        assert_eq!(s3.key(&Obj::Daily("sun")), "vibenote/workspace/daily/sun.sqlite");
        let cfg = S3Config {
            bucket: "b".into(),
            endpoint: Some("http://127.0.0.1:9000/".into()),
            prefix: Some("/x/y/".into()),
            ..cfg
        };
        let s3 = S3::new(&cfg, "w.sqlite").unwrap();
        assert_eq!(
            (s3.origin.as_str(), s3.host.as_str(), s3.base.as_str()),
            ("http://127.0.0.1:9000", "127.0.0.1:9000", "/b")
        );
        assert_eq!(s3.key(&Obj::Latest), "x/y/w/w.sqlite");
    }
}
