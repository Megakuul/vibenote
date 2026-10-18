//! The HTTP agent and small helpers shared by the S3 and Drive clients.

use crate::{BackupError, Result};
use std::io::Read as _;
use std::time::Duration;
use ureq::http::Response;
use ureq::tls::{RootCerts, TlsConfig};

pub(crate) type Resp = Response<ureq::Body>;

/// Largest response body read into memory (JSON, XML, error texts).
const MAX_TEXT: u64 = 4 << 20;
/// Largest file downloaded by a restore (bytes).
pub(crate) const MAX_DOWNLOAD: u64 = 64 << 30;

/// A blocking agent: rustls with the system trust store; HTTP errors are returned as responses.
/// Connecting may take 30 s and the answer 120 s after a request was sent; bodies stream without
/// a time limit (large uploads on slow links).
pub(crate) fn agent() -> ureq::Agent {
    ureq::config::Config::builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .user_agent("vibenote-backup")
        .build()
        .new_agent()
}

pub(crate) fn net(what: &str, e: &ureq::Error) -> BackupError {
    BackupError::Remote(format!("{what}: {e}"))
}

/// The response body as text (bounded; empty if unreadable).
pub(crate) fn text(resp: &mut Resp) -> String {
    resp.body_mut()
        .with_config()
        .limit(MAX_TEXT)
        .read_to_string()
        .unwrap_or_default()
}

/// The response as JSON (bounded).
pub(crate) fn json(what: &str, resp: &mut Resp) -> Result<serde_json::Value> {
    serde_json::from_str(&text(resp)).map_err(|e| BackupError::Remote(format!("{what}: bad answer: {e}")))
}

/// `Ok(resp)` for 2xx; else an error with the status and the start of the body. 404 is
/// [`BackupError::NotFound`].
pub(crate) fn ok(what: &str, mut resp: Resp) -> Result<Resp> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(resp);
    }
    let body = reason(&text(&mut resp));
    if status == 404 {
        return Err(BackupError::NotFound(format!("{what}: HTTP 404 {body}")));
    }
    Err(BackupError::Remote(format!("{what}: HTTP {status} {body}")))
}

/// The readable part of an error body: S3's `<Code>`/`<Message>`, Google's `error` /
/// `error_description` / `error.message`, else its start.
fn reason(body: &str) -> String {
    let tag = |t: &str| {
        let open = format!("<{t}>");
        let start = body.find(&open)?.checked_add(open.len())?;
        let len = body.get(start..)?.find(&format!("</{t}>"))?;
        body.get(start..start.checked_add(len)?).map(str::to_string)
    };
    if let Some(code) = tag("Code") {
        return match tag("Message") {
            Some(m) => format!("{code}: {m}"),
            None => code,
        };
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let s = |p: &str| v.pointer(p).and_then(serde_json::Value::as_str).map(str::to_string);
        if let Some(m) = s("/error/message")
            .or_else(|| s("/error_description"))
            .or_else(|| s("/error"))
        {
            return m;
        }
    }
    body.trim().chars().take(300).collect()
}

/// Streams the response body into `dest` (created; must not be larger than [`MAX_DOWNLOAD`]).
pub(crate) fn download(what: &str, resp: Resp, dest: &std::path::Path) -> Result<u64> {
    let io = |e: std::io::Error| BackupError::Io(format!("{what}: {}: {e}", dest.display()));
    let mut f = std::fs::File::create(dest).map_err(io)?;
    let mut r = resp.into_body().into_reader().take(MAX_DOWNLOAD.saturating_add(1));
    let n = std::io::copy(&mut r, &mut f).map_err(io)?;
    if n > MAX_DOWNLOAD {
        return Err(BackupError::Remote(format!(
            "{what}: download larger than {MAX_DOWNLOAD} bytes"
        )));
    }
    f.sync_all().map_err(io)?;
    Ok(n)
}

/// Percent-encodes everything but RFC 3986 unreserved characters (and `/` if `keep_slash`),
/// as SigV4 and form bodies need.
pub(crate) fn enc(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (keep_slash && b == b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` and `+` (query strings, form bodies).
pub(crate) fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        let hex = b
            .get(i.saturating_add(1)..i.saturating_add(3))
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (c, hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i = i.saturating_add(3);
            }
            (b'+', _) => {
                out.push(b' ');
                i = i.saturating_add(1);
            }
            _ => {
                out.push(c);
                i = i.saturating_add(1);
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `k=v&…` with both sides encoded.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k, false), enc(v, false)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file, streamed.
pub(crate) fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::Digest as _;
    let mut f = std::fs::File::open(path).map_err(|e| BackupError::Io(format!("{}: {e}", path.display())))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| BackupError::Io(format!("{}: {e}", path.display())))?;
        let Some(chunk) = buf.get(..n) else { break };
        if n == 0 {
            break;
        }
        h.update(chunk);
    }
    Ok(hex(&h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_round_trips() {
        assert_eq!(enc("a b/c~d", false), "a%20b%2Fc~d");
        assert_eq!(enc("a b/c", true), "a%20b/c");
        assert_eq!(dec("a%20b%2Fc+d%zz"), "a b/c d%zz");
        assert_eq!(form(&[("a", "1 2"), ("b&", "=")]), "a=1%202&b%26=%3D");
    }

    #[test]
    fn error_bodies_are_shortened() {
        assert_eq!(
            reason("<?xml?><Error><Code>InvalidAccessKeyId</Code><Message>No such key.</Message></Error>"),
            "InvalidAccessKeyId: No such key."
        );
        assert_eq!(
            reason(r#"{"error":"invalid_client","error_description":"The OAuth client was not found."}"#),
            "The OAuth client was not found."
        );
        assert_eq!(
            reason(r#"{"error":{"code":403,"message":"Insufficient permissions"}}"#),
            "Insufficient permissions"
        );
        assert_eq!(reason("  plain  "), "plain");
    }
}
