# ADR 0010 — One audited `unsafe` block: wgpu surface from the Wayland window

- **Status:** Accepted (locked) — owner approved 2026-10-13 ("Approve")
- **Context milestone:** M1.2 (frame loop), required by ADR 0001 (direct Wayland client + wgpu)

## Problem

ADR 0001 (locked) renders the canvas with wgpu into our own `wl_surface`. wgpu can only create a
presentation surface from raw window handles (`wl_display*`, `wl_surface*`), and that API is
`unsafe` because wgpu cannot check that those objects outlive the GPU surface:

- `wgpu::Instance::create_surface_unsafe(SurfaceTargetUnsafe::RawHandle { .. })` — `unsafe fn`.
- The safe `Instance::create_surface` needs a type implementing `raw_window_handle`'s
  `HasWindowHandle`/`HasDisplayHandle`, whose constructors (`WindowHandle::borrow_raw`,
  `DisplayHandle::borrow_raw`) are themselves `unsafe`.
- smithay-client-toolkit 0.21.1 (the window-plumbing crate named in ADR 0001) does not implement
  these traits (checked in its source: only its examples use raw handles, with `unsafe`).
- Toolkits that hide this (winit) were rejected for input by ADR 0001; adding one only for the
  surface would mean a second Wayland connection and a parallel stack.

The workspace denies `unsafe_code`. The Phase 0 and Phase 2 spikes used exactly this call.

## Options

| Option | Cost | Risk |
|---|---|---|
| **A. One `#[allow(unsafe_code)]` function in `vibenote-ui`** (`gpu_surface.rs`), ~15 lines, with a SAFETY comment and an ownership type that keeps the `wl_display` connection and `wl_surface` alive until the wgpu `Surface` is dropped (field order + `Drop` test) | none | low: a single, reviewed invariant (lifetime of two protocol objects) |
| B. Depend on a third-party crate that wraps the same call | dependency + licence review | same `unsafe`, just not visible to us/review |
| C. Software rendering into wl_shm (no GPU surface) | violates ADR 0001/0003 performance basis | high: frame budget at 100k strokes not reachable |

## Decision

**A.** `unsafe` stays denied everywhere else. The allowance is exactly one function in
`crates/ui/src/gpu_surface.rs`; `cargo xtask layers` (or a ci grep) fails if `unsafe` or
`allow(unsafe_code)` appears in any other file. Invariant documented at the call site:

> The `wl_display` (owned `Connection`) and the `wl_surface` proxy are stored in the same
> struct as the `wgpu::Surface`, declared **after** it, so Rust drops the surface first; the
> struct is not `Clone` and never hands out the raw pointers.

Other places that might seem to need `unsafe` are solved safely:
- PDF bitmaps via memfd (ADR 0004): read the memfd with `read`/`pread` into a buffer (one copy,
  measured later) instead of `mmap`.
- pdfium FFI lives in `vibenote-pdfd` through the `pdfium-render` crate's safe API; any `unsafe`
  needed there would come back as its own ADR.

## Consequences

- M1.2 can proceed; the reviewer role checks this file in every milestone review.
- If the owner rejects A, M1.2 is blocked (ADR 0001 has no safe route); M1.1 event handling,
  M2 and later headless milestones continue.
