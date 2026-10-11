# Environment (owner's machine) — probed 2026-10-11

All Phase 0 measurements are taken on this machine. Raw evidence is in `spikes/input/evidence/`.

| Item | Value | Source |
|---|---|---|
| OS | NixOS 26.11 (build 26.11.20261008.e7439b6) | `/etc/os-release` |
| Kernel | Linux 7.2.9-zen1, PREEMPT_DYNAMIC | `uname -a` |
| Session | Wayland (`WAYLAND_DISPLAY=wayland-1`) | env |
| Compositor | Hyprland 0.56.2 (commit efb5099) | `hyprctl version` |
| GPU | AMD Radeon 860M (Krackan, 1002:1114), kernel driver `amdgpu` | `lspci -nnk` |
| GPU userspace | Mesa 26.2.4, Vulkan 1.4 via RADV | `vulkaninfo --summary` |
| CPU / RAM | 16 logical cores / 30 GiB | `nproc`, `free` |
| Display | eDP-1, BOE NE153QDM-NY3, 330×210 mm, **running 1920×1200 @ 180.002 Hz** (native panel mode 2560×1600 @ 180 Hz also available) | `hyprctl monitors` |
| Scaling | 1.0 (fractional scaling available via `wp_fractional_scale_manager_v1`) | `hyprctl monitors`, `wayland-info` |
| VRR / tearing | VRR off; tearing off (blocked by user settings) | `hyprctl monitors` |
| Framebuffer format | XRGB2101010 | `hyprctl monitors` |
| Hardware cursors | not in use (software cursor) | `hyprctl monitors` |
| Rust | rustc 1.98.1, cargo 1.98.0 | `--version` |
| libinput (tools via nix) | 1.31.3 | `nix shell nixpkgs#libinput` |

## Wayland protocols relevant to Phase 0
From `wayland-info` (full list: `spikes/input/evidence/wayland-info.txt`):
- `zwp_tablet_manager_v2` v1 — tablet input
- `wp_presentation` v2 — presentation feedback (latency measurement)
- `wp_fifo_manager_v1` v1, `wp_commit_timing_manager_v1` v1 — frame scheduling
- `wp_tearing_control_manager_v1` v1 — tearing hint (compositor setting currently blocks tearing)
- `wp_fractional_scale_manager_v1` v1, `wp_viewporter` v1 — fractional scaling
- `wp_linux_drm_syncobj_manager_v1` v1 — explicit sync
- `zwp_linux_dmabuf_v1` v5

## Input devices
- **No physical pen device is present.** `hyprctl devices` lists no tablets; built-in touchpad,
  keyboard only. All pen measurements in Phase 0 are **[sim]** (via simtab).
- `/dev/uinput`: `crw-rw----+ root root`, ACL `user:linus:rw-` (already granted by the system
  configuration; the owner is also in group `input`). See `docs/spikes/input-results.md` → simtab
  for the recommended udev rule on systems without this ACL.
