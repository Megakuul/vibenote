//! The one audited `unsafe` in vibenote (ADR 0010): a wgpu surface for our Wayland window.
//!
//! `cargo xtask layers` fails if `unsafe` appears in any other file, or more than once here.

use smithay_client_toolkit::reexports::client::{Connection, Proxy};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::window::Window;
use std::ffi::c_void;
use std::ptr::NonNull;
use vibenote_render::wgpu;

/// A wgpu surface that keeps the Wayland objects it points to alive.
pub struct GpuSurface {
    surface: wgpu::Surface<'static>,
    // Field order matters (ADR 0010): Rust drops fields in declaration order, so `surface` is
    // destroyed before the window (wl_surface) and the connection (wl_display) it points to.
    _window: Window,
    _conn: Connection,
}

#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    #[error("null Wayland handle")]
    NullHandle,
    #[error("wgpu: {0}")]
    Create(String),
}

impl GpuSurface {
    #[allow(unsafe_code)]
    pub fn new(instance: &wgpu::Instance, conn: &Connection, window: &Window) -> Result<Self, SurfaceError> {
        use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
        let display = NonNull::new(conn.backend().display_ptr().cast::<c_void>()).ok_or(SurfaceError::NullHandle)?;
        let wl_surface =
            NonNull::new(window.wl_surface().id().as_ptr().cast::<c_void>()).ok_or(SurfaceError::NullHandle)?;
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display))),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(wl_surface)),
        };
        // SAFETY (ADR 0010): `display` is the live `wl_display` of `conn` and `wl_surface` the live
        // surface of `window`. The returned struct owns clones of both (`Connection` and `Window`
        // are reference-counted), declared after the wgpu surface, so they outlive it. The raw
        // pointers are never stored or handed out.
        let surface =
            unsafe { instance.create_surface_unsafe(target) }.map_err(|e| SurfaceError::Create(e.to_string()))?;
        Ok(GpuSurface {
            surface,
            _window: window.clone(),
            _conn: conn.clone(),
        })
    }

    pub fn surface(&self) -> &wgpu::Surface<'static> {
        &self.surface
    }
}
