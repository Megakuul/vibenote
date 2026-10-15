//! Wire protocol between the app and the PDF worker (ADR 0004): frames on a Unix stream
//! socket. A frame is a little-endian `u32` length, a JSON header of that length, then
//! `payload` raw bytes if the header announces them. Rendered bitmaps travel in a memfd whose
//! descriptor is attached (SCM_RIGHTS) to the frame's first bytes.
//!
//! Every size is bounded on receipt: the worker handles hostile PDFs and the app must not
//! trust a misbehaving worker either.

use serde::{Deserialize, Serialize};
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

/// Largest JSON header (page text can be long).
pub const MAX_HEADER: usize = 32 << 20;
/// Largest PDF accepted (bytes).
pub const MAX_PDF_BYTES: u64 = 1 << 30;
/// Error message for a document the worker does not have (never opened, or evicted to stay
/// within its document limit); the client reopens it and retries.
pub const NOT_OPEN: &str = "document not open";
/// Largest rendered bitmap side and area (pixels).
pub const MAX_SIDE: u32 = 16_384;
pub const MAX_PIXELS: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Opens a document from `payload` bytes under `key` (the blob hash).
    Open {
        key: String,
        payload: u64,
    },
    /// Renders page `page` (0-based) `width` pixels wide; the reply carries a memfd.
    Render {
        key: String,
        page: u32,
        width: u32,
    },
    /// The page's text layer.
    Text {
        key: String,
        page: u32,
    },
    Close {
        key: String,
    },
    /// Test hook (only honoured when the worker runs with `VIBENOTE_PDFD_TEST=1`): reports
    /// which forbidden operations the sandbox blocks.
    Probe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageSize {
    /// PDF points.
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "r", rename_all = "snake_case")]
pub enum Response {
    Opened {
        pages: Vec<PageSize>,
    },
    /// RGBA8, premultiplied, rows of `width * 4` bytes, in the attached memfd.
    Rendered {
        width: u32,
        height: u32,
    },
    Text {
        text: String,
    },
    Closed,
    Probe {
        network_blocked: bool,
        exec_blocked: bool,
        write_blocked: bool,
        /// Signals to other processes (the parent was tried, with signal 0).
        #[serde(default)]
        kill_blocked: bool,
        /// openat2 (flags in a struct the filter cannot read) is unavailable.
        #[serde(default)]
        openat2_blocked: bool,
    },
    /// A clean failure (corrupt PDF, password, bad page …).
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Bad(String),
}

/// Writes a frame (header, optional payload, optional descriptor).
pub fn send<T: Serialize>(
    sock: &UnixStream,
    header: &T,
    payload: Option<&[u8]>,
    fd: Option<&OwnedFd>,
) -> Result<(), ProtoError> {
    use std::os::fd::AsFd;
    let json = serde_json::to_vec(header).map_err(|e| ProtoError::Bad(e.to_string()))?;
    if json.len() > MAX_HEADER {
        return Err(ProtoError::Bad("header too large".into()));
    }
    let len = u32::try_from(json.len()).map_err(|_| ProtoError::Bad("header too large".into()))?;
    let prefix = len.to_le_bytes();
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
    let fds;
    if let Some(fd) = fd {
        fds = [fd.as_fd()];
        if !control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)) {
            return Err(ProtoError::Bad("no room for the descriptor".into()));
        }
    }
    // The descriptor rides on the first bytes; the rest is plain stream writes.
    let sent = rustix::net::sendmsg(
        sock,
        &[IoSlice::new(&prefix)],
        &mut control,
        rustix::net::SendFlags::NOSIGNAL,
    )
    .map_err(std::io::Error::from)?;
    let mut w = sock;
    w.write_all(prefix.get(sent..).unwrap_or_default())?;
    w.write_all(&json)?;
    if let Some(p) = payload {
        w.write_all(p)?;
    }
    w.flush()?;
    Ok(())
}

/// Reads a frame header; returns it with the descriptor attached to it, if any.
pub fn recv<T: for<'de> Deserialize<'de>>(sock: &UnixStream) -> Result<(T, Option<OwnedFd>), ProtoError> {
    let mut prefix = [0u8; 4];
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(2))];
    let mut control = rustix::net::RecvAncillaryBuffer::new(&mut space);
    let got = {
        let mut iov = [IoSliceMut::new(&mut prefix)];
        rustix::net::recvmsg(sock, &mut iov, &mut control, rustix::net::RecvFlags::CMSG_CLOEXEC)
            .map_err(std::io::Error::from)?
            .bytes
    };
    if got == 0 {
        return Err(ProtoError::Io(std::io::ErrorKind::UnexpectedEof.into()));
    }
    let mut fd = None;
    for m in control.drain() {
        if let rustix::net::RecvAncillaryMessage::ScmRights(fds) = m {
            for f in fds {
                // Keep the first; extra descriptors are closed when dropped.
                if fd.is_none() {
                    fd = Some(f);
                }
            }
        }
    }
    let mut r = sock;
    r.read_exact(prefix.get_mut(got..).unwrap_or_default())?;
    let len = usize::try_from(u32::from_le_bytes(prefix)).unwrap_or(usize::MAX);
    if len > MAX_HEADER {
        return Err(ProtoError::Bad(format!("header of {len} bytes")));
    }
    let mut json = vec![0u8; len];
    r.read_exact(&mut json)?;
    let h = serde_json::from_slice(&json).map_err(|e| ProtoError::Bad(e.to_string()))?;
    Ok((h, fd))
}

/// Reads exactly `len` payload bytes (bounded by the caller).
pub fn recv_payload(sock: &UnixStream, len: u64) -> Result<Vec<u8>, ProtoError> {
    if len > MAX_PDF_BYTES {
        return Err(ProtoError::Bad(format!("payload of {len} bytes")));
    }
    let n = usize::try_from(len).map_err(|_| ProtoError::Bad("payload too large".into()))?;
    let mut buf = vec![0u8; n];
    let mut r = sock;
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Checks bitmap dimensions from an untrusted reply; returns the byte length.
pub fn bitmap_len(width: u32, height: u32) -> Result<usize, ProtoError> {
    let px = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE || px > MAX_PIXELS {
        return Err(ProtoError::Bad(format!("bitmap {width}×{height}")));
    }
    usize::try_from(px.saturating_mul(4)).map_err(|_| ProtoError::Bad("bitmap too large".into()))
}
