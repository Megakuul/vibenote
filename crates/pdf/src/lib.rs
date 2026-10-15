//! PDF support (ADR 0004): the protocol to the sandboxed pdfium worker (`apps/vibenote-pdfd`)
//! and the client that runs it. pdfium never runs in the app process.

pub mod client;
pub mod proto;

pub use client::{Bitmap, PdfClient, PdfError, find_worker};
pub use proto::PageSize;
