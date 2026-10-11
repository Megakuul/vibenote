//! CPU/shm render path with partial damage ("partial redraw" technique).
//!
//! A master XRGB8888 image holds the canvas. New ink segments are rasterised (anti-aliased
//! capsules with interpolated radius) into the master; each frame copies only the rectangles
//! changed since that buffer was last shown and commits with `damage_buffer` of the new rect.

use common::ink::{self, Pt};
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::QueueHandle;

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}
impl Rect {
    fn union(self, o: Rect) -> Rect {
        Rect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    fn clip(self, w: i32, h: i32) -> Option<Rect> {
        let r = Rect { x0: self.x0.max(0), y0: self.y0.max(0), x1: self.x1.min(w), y1: self.y1.min(h) };
        (r.x1 > r.x0 && r.y1 > r.y0).then_some(r)
    }
}

pub struct Buf {
    pub buffer: wl_buffer::WlBuffer,
    pub busy: bool,
    /// Region of the master that this buffer has not received yet.
    stale: Option<Rect>,
    offset: usize,
}

pub struct Shm {
    pub w: i32,
    pub h: i32,
    master: Vec<u32>,
    map: *mut u32,
    map_len: usize,
    _fd: OwnedFd,
    pool: wl_shm_pool::WlShmPool,
    pub bufs: Vec<Buf>,
    frame_damage: Option<Rect>,
}

fn paper() -> u32 {
    let c = ink::PAPER;
    let b = |v: f32| (v * 255.0).round() as u32;
    0xff00_0000 | (b(c[0]) << 16) | (b(c[1]) << 8) | b(c[2])
}

impl Shm {
    pub fn new<D>(shm: &wl_shm::WlShm, w: i32, h: i32, qh: &QueueHandle<D>) -> anyhow::Result<Self>
    where
        D: wayland_client::Dispatch<wl_shm_pool::WlShmPool, ()> + wayland_client::Dispatch<wl_buffer::WlBuffer, usize> + 'static,
    {
        let n = (w * h) as usize;
        let len = n * 4 * 2;
        // SAFETY: plain syscalls; fd ownership transferred to OwnedFd.
        let fd = unsafe {
            let raw = libc::memfd_create(c"inkspike-shm".as_ptr(), libc::MFD_CLOEXEC);
            if raw < 0 {
                anyhow::bail!("memfd_create failed");
            }
            OwnedFd::from_raw_fd(raw)
        };
        use std::os::fd::AsRawFd;
        // SAFETY: valid fd.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as i64) } != 0 {
            anyhow::bail!("ftruncate failed");
        }
        // SAFETY: mapping a memfd we own, of the size we just set.
        let map = unsafe {
            libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), 0)
        };
        if map == libc::MAP_FAILED {
            anyhow::bail!("mmap failed");
        }
        let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
        let bufs = (0..2)
            .map(|i| Buf {
                buffer: pool.create_buffer((i * n * 4) as i32, w, h, w * 4, wl_shm::Format::Xrgb8888, qh, i),
                busy: false,
                stale: Some(Rect { x0: 0, y0: 0, x1: w, y1: h }),
                offset: i * n,
            })
            .collect();
        Ok(Shm { w, h, master: vec![paper(); n], map: map as *mut u32, map_len: len, _fd: fd, pool, bufs, frame_damage: Some(Rect { x0: 0, y0: 0, x1: w, y1: h }) })
    }

    fn damage(&mut self, r: Rect) {
        let Some(r) = r.clip(self.w, self.h) else { return };
        for b in &mut self.bufs {
            b.stale = Some(b.stale.map_or(r, |s| s.union(r)));
        }
        self.frame_damage = Some(self.frame_damage.map_or(r, |s| s.union(r)));
    }

    /// Rasterise segment a→b (physical px) with pressure width.
    pub fn segment(&mut self, a: Pt, b: Pt, scale: f32, color: [f32; 4]) {
        let (ra, rb) = (ink::width(a.p, scale) * 0.5, ink::width(b.p, scale) * 0.5);
        let rmax = ra.max(rb) + 1.0;
        let r = Rect {
            x0: (a.x.min(b.x) - rmax).floor() as i32,
            y0: (a.y.min(b.y) - rmax).floor() as i32,
            x1: (a.x.max(b.x) + rmax).ceil() as i32 + 1,
            y1: (a.y.max(b.y) + rmax).ceil() as i32 + 1,
        };
        let Some(c) = r.clip(self.w, self.h) else { return };
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let l2 = dx * dx + dy * dy;
        let ink = [color[0] * 255.0, color[1] * 255.0, color[2] * 255.0];
        for y in c.y0..c.y1 {
            for x in c.x0..c.x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = if l2 > 1e-6 { (((px - a.x) * dx + (py - a.y) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                let (qx, qy) = (a.x + dx * t - px, a.y + dy * t - py);
                let d = (qx * qx + qy * qy).sqrt();
                let rad = ra + (rb - ra) * t;
                let cov = (rad - d + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let i = (y * self.w + x) as usize;
                let o = self.master[i];
                let ch = |sh: u32, k: usize| {
                    let ov = ((o >> sh) & 0xff) as f32;
                    ((ov + (ink[k] - ov) * cov).round() as u32) << sh
                };
                self.master[i] = 0xff00_0000 | ch(16, 0) | ch(8, 1) | ch(0, 2);
            }
        }
        self.damage(r);
    }

    /// Copy stale regions into a free buffer, attach, damage, commit. Returns false if both busy.
    pub fn present(&mut self, surface: &wl_surface::WlSurface, before_commit: impl FnOnce()) -> bool {
        let Some(bi) = self.bufs.iter().position(|b| !b.busy) else { return false };
        let (w, h) = (self.w as usize, self.h as usize);
        if let Some(s) = self.bufs[bi].stale.take() {
            let off = self.bufs[bi].offset;
            for y in s.y0 as usize..s.y1 as usize {
                let row = y * w;
                // SAFETY: off + row + x < 2*w*h == map_len/4, bounds from clipped rect.
                let dst = unsafe { std::slice::from_raw_parts_mut(self.map.add(off + row + s.x0 as usize), (s.x1 - s.x0) as usize) };
                dst.copy_from_slice(&self.master[row + s.x0 as usize..row + s.x1 as usize]);
            }
            let _ = h;
        }
        let b = &mut self.bufs[bi];
        b.busy = true;
        surface.attach(Some(&b.buffer), 0, 0);
        if let Some(d) = self.frame_damage.take() {
            surface.damage_buffer(d.x0, d.y0, d.x1 - d.x0, d.y1 - d.y0);
        }
        before_commit();
        surface.commit();
        true
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        for b in &self.bufs {
            b.buffer.destroy();
        }
        self.pool.destroy();
        // SAFETY: unmapping our own mapping.
        unsafe { libc::munmap(self.map as *mut _, self.map_len) };
    }
}
