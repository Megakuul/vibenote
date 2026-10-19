# AGENTS.md — how to work on vibenote

vibenote is built by agents for one owner (the human you work for). This file is binding.
The bootstrap (Phases 0–4, Gates 0/A/B/C/D) is complete once Gate D is recorded; its history lives in
`plan/STATUS.md`; the original bootstrap instructions are archived in `plan/bootstrap-AGENTS.md`.

---

## 0. Start every session here

1. Read `plan/STATUS.md` (current milestone/task, blockers, pending owner actions, log).
   **Implementation starts only after Gate D is recorded there** with date and the owner's words.
2. Take the next task from `plan/execution-plan.md` (or the next red test in your area).
3. Update `plan/STATUS.md` at the end of every work arc; update `ROADMAP.md` when a milestone
   changes status; log subagent use in `plan/usage.md`.
4. Ask the owner only for decisions that are genuinely theirs: product behaviour, licensing,
   approval of goldens/fixtures/eval data and protected-file changes, change proposals against
   locked ADRs, spending, anything needing root. Batch questions; never more than ~8 at once.
5. Owner preferences: **do not create git commits** (the owner's signing key needs a
   passphrase); leave changes uncommitted and list them in your report.

---

## 1. Product (summary — the binding source is `SPEC.md`)

A **fast, paper-like handwriting workspace for the Linux/Wayland desktop**: infinite canvas per
note with ink, PDFs (vertical page column), images, rich-text boxes, links, audio (recorded and
imported, synced to ink), shapes and a ruler; recursive coloured folders; offline handwriting
recognition (de/en, pluggable language packs) for search; prettify that keeps the owner's
handwriting and never touches drawings; crash-safe storage at large scale. Latency is the top
priority. Out of scope: `docs/out-of-scope.md` (export, sync, collaboration, mobile/web, other
platforms).

Architecture: `plan/architecture.md`. Decisions: `docs/adr/` (0001–0010 Accepted (locked)).

### Simulator-first (owner, 2026-10-12)
There is no physical pen. **No task, test or milestone may depend on a physical pen, owner
recordings or calibration.** Pen behaviour is implemented best effort and verified with
**simtab** (`tools/simtab`, a uinput virtual tablet driving the real kernel → libinput →
compositor → Wayland path). Real-pen findings come later from the owner through the normal
bug/change-proposal process (`docs/hardware-verification.md` is an optional checklist). Every
measurement in results docs is labelled **[sim]** or **[hw]**.

---

## 2. Implementation loop

1. Pick the next task from `plan/execution-plan.md`. Respect its **Needs** column and the
   crates it lists.
2. If the task has open design questions, use the **architect** role first (§5).
3. Implement until the task's tests are green and `cargo xtask ci` passes; then
   `cargo xtask parity --raise` (the floor only grows).
4. For UI/visual work: render via the headless harness (`render-png`, `ui.screenshot`), look at
   the PNG, compare with SPEC. Propose new goldens; the owner approves them.
5. Milestone done → **reviewer** role (fresh context) against SPEC; fix findings.
6. Update STATUS.md, ROADMAP.md, usage.md. Report changed files (no commits, §0.5).
7. Repeat. Stop and ask only for owner decisions.

Never report a task done unless `cargo xtask ci` passed on the final tree.

---

## 3. Guardrails (non-negotiable)

### Locked decisions
- An ADR marked **Accepted (locked)** is binding: language, input path, render path, and any
  other locked stack choice. Build on it; never switch, replace, or add a parallel alternative
  on your own — not even "temporarily" or "to try something".
- If you hit a real limit of a locked decision, stop and write a change proposal in
  `docs/adr/` (status: Proposed) with the measured problem, the options, and their cost, then
  ask the owner. Continue other unblocked work meanwhile.

### Tests are protected
- After Gate C you **must not** weaken, delete, skip, loosen tolerances of, or re-bless tests,
  goldens, budgets or eval labels to make work pass. If you believe a test is wrong, stop, explain
  with evidence in STATUS.md, and ask the owner.
- Tests, goldens, fixtures and eval sets live in clearly separated directories
  (`tests/golden`, `tests/fixtures`, `tests/eval`, `tests/budgets.toml`, `tests/parity_floor.txt`,
  hashed in `tests/PROTECTED.sha256`); `xtask ci` **fails** on any diff in them. Only the owner
  runs `cargo xtask protect --write` and `cargo xtask golden --approve`.

### Never crash, never lose data
- No `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`, `unimplemented!` in non-test code;
  no `unsafe` unless approved in an ADR with justification. Enforce with crate-level
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, …)]`.
- Errors are `Result` with crate error types. Unfinished features return errors, never panic.
- All input-derived values (files, PDFs, images, audio, pen events, control-channel params) are
  hostile: bounds-checked access, checked arithmetic, capped allocations, bounded recursion.
- Writes are atomic/transactional; every crash fix lands with a regression test.

### Handwriting safety
- Never modify strokes that are not confidently classified as writing. Uncertain → unchanged.
- Original strokes are always preserved; every prettify is undoable.

### Dependencies, assets, licenses
- Every dependency's license is recorded (`cargo deny` or equivalent in `xtask ci`).
  GPL/AGPL/LGPL dependencies or model weights with restrictive licenses require owner approval.
- Every bundled asset (icons, fonts, models, sample files) has a row in `ATTRIBUTION.md`
  (author, source, license). No assets copied from proprietary products.

### Honesty
- Never claim a library, API or protocol supports something without verifying it (docs, source,
  or a test). Never report a task done unless `cargo xtask ci` passed.

---

## 4. Engineering principles

- **Latency path is sacred.** Pen event → ink on screen must not wait on storage, recognition,
  layout or any lock held by background work. Recognition/prettify run on background workers
  with cancellation.
- **Determinism.** Engine behaviour depends only on inputs (fixtures, commands, seeds), so every
  bug is reproducible from a recording.
- **Scale by design:** spatial index for canvas objects, tiled/cached rendering, lazy loading of
  PDF pages/images/audio, incremental saves.
- **UI is thin** and reads engine state; colours/spacing come from theme tokens.
- **Small commits, always building.**

---

## 5. Model delegation & parallel work

Subagent definitions live in `.claude/agents/`:

| Role | Model | Use for |
|---|---|---|
| `architect` | opus | ADRs / change proposals, cross-crate design, hard algorithms (ink rendering, LOD/tiles, classifier, prettify, decoding), risky-diff review |
| `implementer` | sonnet | one execution-plan task against its failing tests, in its listed crates |
| `explorer` | haiku | read-only code/doc/log search and summaries |
| `test-runner` | haiku | running `xtask ci`/parity/bench/tests and summarising failures |
| `reviewer` | opus | fresh-context milestone review against SPEC and guardrails |

Log in `plan/usage.md` which role handled which task, so the owner can tune cost vs. quality.

Parallel agents: each owns specific crates for a task (tracks in `plan/execution-plan.md`
§Parallelism); separate `CARGO_TARGET_DIR` per agent; never edit another agent's crates; merge
only when `xtask ci` is green; clean up target dirs (`/dev/shm` is 16 GB and fills up).

---

## 6. Running and looking at the app

All commands run from the repo root inside `nix-shell` (root `shell.nix`). On the owner's machine
the agent builds with `CARGO_TARGET_DIR=/dev/shm/vibenote-target` (disk nearly full; RAM-backed,
lost on reboot, so rebuild then).

**The app (M1: Wayland window that inks; no chrome yet):** inside nix-shell (the Vulkan loader is
dlopened from it), `cargo run -p vibenote -- [--workspace <path.sqlite>] [--note <id>]` (default
`$XDG_DATA_HOME/vibenote/workspace.sqlite`, first note). Keys come from the engine keymap
(`keymap.get`; SPEC R-CMD-002: W/A/E/Q pen/highlighter/eraser/lasso (1–7 tools in toolbar order), Ctrl+Z / Ctrl+Shift+Z,
Ctrl+C/X/V, Delete, Escape, Ctrl+0 fit, Ctrl+1 100 %, Ctrl+= / Ctrl+- zoom, Ctrl+Q quit). Wheel,
middle-drag or Space+drag pan; Ctrl+wheel or pinch zoom at the cursor; left mouse draws;
barrel buttons per `settings.barrel1/2`.
`VIBENOTE_LATENCY_LOG=<file>` writes latency telemetry. Live pen test through the real path:
`tools/harness/live-pen.sh <out-dir> [simtab play args]` (dedicated workspace, lock guard,
screenshot via grim; `LIVE_WORKSPACE_FROM=<db>` starts from a copy of a workspace,
`LIVE_APP_ARGS` adds app arguments). The owner allows live tests anytime while the session is unlocked.

**Headless engine:**
- List commands (id, requirement ids, implemented, params schema): `cargo run -p vibenote-cli -- commands`
- Run commands in sequence on a fresh in-memory session:
  `cargo run -p vibenote-cli -- run workspace.note.create '{"name":"n"}' note.open '{"id":1}' ink.stroke.add '{"samples":[{"x":1,"y":2,"p":0.5}]}'`
- Persistent workspace: add `--workspace <path.sqlite>` before the subcommand (any subcommand).
  The CLI loads every canvas at start (tools see the whole workspace); the app and
  `Engine::open` load lazily: folders/notes at open, a note's viewport on `note.open`, the rest
  in the background (`/loading` in `state.inspect`; `Engine::load_all`/`wait_loaded` in tests).
- Inspect state (JSON, optional JSON pointer), e.g. after a script:
  `cargo run -p vibenote-cli -- inspect /ws/notes --script requests.jsonl`
- Control channel (JSON lines, see `crates/control/src/lib.rs`): over stdio with
  `vibenote-cli script [file|-]`, or a Unix socket with `vibenote-cli serve --socket /tmp/vn.sock`
  (then e.g. `socat - UNIX-CONNECT:/tmp/vn.sock`). Methods: `ping`, `engine.commands`,
  `engine.execute {cmd,args}`, `state.inspect {pointer}`, `control.methods`.
  **Verify work via `state.inspect` first, screenshots second.**

**Fixtures and pictures:**
- Generate an ink fixture: `cargo run -p simtab -- fixture --pattern loops --strokes 4 --surface 1200x800 -o f.jsonl`
  (patterns: lines, curves, scribble, pressure-ramp, tilt-sweep, eraser-flip, hover, loops, buttons).
- Replay a fixture into the engine: `cargo run -p vibenote-cli -- replay tests/fixtures/ink/loops-4.jsonl`
- Headless screenshot (PNG): `cargo run -p vibenote-cli -- render-png tests/fixtures/ink/loops-4.jsonl out.png [--size 1200x800] [--origin X,Y] [--zoom Z]`
  (`VIBENOTE_ADAPTER=cpu` renders with llvmpipe instead of the GPU).
- Replay through the real Wayland path: `cargo run -p simtab -- play <stroke-file|--pattern …> --window <app-class> --window-pid <pid>`
  (needs `/dev/uinput` access, see `tools/simtab/README.md`; refuses windows not fully on screen;
  never run while the session is locked).

**xtask:**
| Task | What |
|---|---|
| `cargo xtask ci` | fmt, clippy -D warnings, layers, tests (incl. goldens + 25× kill -9), licences (cargo deny), parity (acceptance suite + floor), protected-file report |
| `cargo xtask layers` | crate layering + no UI crates below `ui` |
| `cargo xtask golden [--review]` | golden images; `--review` writes `<target>/golden-new/review.html` |
| `cargo xtask golden --approve <name>` | **owner only after Gate C** |
| `cargo xtask parity [--raise]` | runs `tests/acceptance`, writes `docs/parity.md`, enforces `tests/parity_floor.txt` (only grows) |
| `cargo xtask bench` | slow checks: kill -9 ×1000, performance budgets in release mode |
| `cargo xtask bench app-smoke\|latency\|startup` | live app benches on the owner's session → `target/e2e-*.json` (read by acceptance tests); `latency --analyze <dir>` re-analyses a recorded run, `latency --reference` writes with the 100k-stroke note open (`target/e2e-latency-reference.json`); `startup` also measures the reference workspace (built by `cargo xtask parity`) |
| `cargo xtask prettify-gallery` | before/after prettify gallery (needs `hwr.prettify_block`) |
| `cargo xtask protect [--write]` | protected test data hashes (`tests/PROTECTED.sha256`); `--write` only with owner approval |
| `tools/harness/validate.sh` | harness self-validation with deliberate faults (scratch copy) |

**Test layout:** `cargo test` = implemented behaviour (must pass). `tests/acceptance` = one or
more tests per SPEC requirement (`r_<area>_<nnn>_…`), may be red; tracked in `docs/parity.md`.
Protected after Gate C: `tests/golden`, `tests/fixtures`, `tests/eval`, `tests/budgets.toml`,
`tests/parity_floor.txt`.

**Safety rules for simtab and the owner's session (learned in the bootstrap):**
- Never inject pen input while the session is locked (`tools/simtab` and the bench scripts check
  for hyprlock; simtab also aborts mid-playback and lifts the pen if the session locks or the target
  window moves or is hidden); never target windows not fully on the monitor (simtab refuses).
- Never use `pkill -f` with a pattern that can match your own shell; kill by PID.
- No microphone recording without the owner's consent; audio tests use a null sink or generated
  sources.
- The owner's disk is nearly full: build in `/dev/shm`, keep large data (HWR eval data in
  `/dev/shm/vibenote-hwr`, GNHK in `/dev/shm/vibenote-gnhk`) out of the repo, watch free space.
