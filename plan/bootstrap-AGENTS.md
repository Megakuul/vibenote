# BOOTSTRAP — instructions for the agent building this project

> **You are reading a bootstrapper, not the final AGENTS.md.**
> Your job is to turn the product vision below into (1) a binding spec, (2) a validated stack,
> (3) a strong test harness, and (4) an execution plan — in that order, with human approval gates
> between phases. At the end of Phase 4 you rewrite this file into the permanent AGENTS.md
> (keep the guardrails, drop the bootstrap phases you have completed).
>
> Name: **vibenote** (owner chose it over the working name "Inkwell" on 2026-10-11; any remaining
> "Inkwell"/"inkwell-" below means vibenote). The owner is the human you work for.

---

## 0. Start every session here

1. Read `plan/STATUS.md`. If it does not exist, you are at the very beginning: create `plan/` and
   `plan/STATUS.md` (current phase, current task, open questions, blockers, last gate passed).
2. Do the next task of the current phase. Do not skip phases. Do not start a phase whose gate
   has not been passed (gates are recorded in `plan/STATUS.md` with date and owner's words).
3. Update `plan/STATUS.md` at the end of every work arc.
4. Ask the owner only for decisions that are genuinely theirs: product behaviour, licensing,
   stack approval at gates, fixtures only they can produce (pen recordings, labels), spending.
   Batch questions; never ask more than ~8 at once.

---

## 1. Product vision (captured from the owner — this is the source for SPEC.md)

A **fast, paper-like handwriting workspace for Linux/Wayland desktop**. A work tool, not a
document publisher.

### Platform
- **Linux + Wayland is first-class and the only tested target.** Other platforms may happen to
  work but must not cost design effort, abstractions, or performance.
- **Desktop only.** Optimise for responsiveness. No sync, no web, no mobile planning.

### Canvas
- **Infinite canvas** per page/note.
- Anything can be placed **anywhere** on the canvas: handwriting, PDFs, images, text boxes,
  links, audio. Users must be able to draw/write **on top of** and **next to** any object.
- PDFs are **objects** on the canvas (movable, scalable, multi-page PDFs laid out sensibly —
  exact behaviour to be specified), **not** page backgrounds.
- **No PDF export requirement.** Never sacrifice practical behaviour for exportability.

### Organisation
- **Folders, recursively nestable** to arbitrary depth, for large hierarchies.
- Folders (and notes) can be **named, coloured**, reordered, moved — optimised for practical,
  fast organisation of a lot of material.
- Search across recognised handwriting and typed text (see §1 Handwriting).

### Pen & tools
- Pen, highlighter, **stroke eraser and pixel/area eraser**, lasso selection, shapes, ruler.
- **Pressure and tilt** must affect ink.
- **Latency is a top priority**: it must feel like paper; the user must never feel lag.

### Handwriting intelligence (offline)
- **Both**: (a) prettify handwriting so it **stays handwriting** (still recognisably the owner's
  style, just more legible), and (b) recognise it as searchable text in the background.
- Triggered **automatically when a writing block is finished**: after the pen is lifted and not
  placed again within the same block for a threshold duration. "Block" = spatially and
  temporally coherent group of strokes. The threshold is configurable; the feature can be
  **disabled**.
- **Must detect whether strokes are writing at all.** Drawings, diagrams, arrows, sketches must
  **never** be modified. When uncertain → do nothing. A false positive on a drawing is the
  worst failure of this feature.
- Original strokes are always kept; prettifying is undoable and per-block revertible.
- **Fully offline.** Languages: **German and English** now; architecture must allow adding
  languages later (pluggable language/model packs).

### Data
- Storage format is free to choose. Priority: works well at large scale (many notes, huge
  canvases, big PDFs, audio) without workarounds. Must be crash-safe (no data loss on crash or
  power loss).

### Explicitly out of scope (unless the owner changes it)
- PDF/print export, sync/cloud, collaboration, mobile/web, Microsoft OneNote format compatibility,
  look-alike of any existing product.

---

## 2. Phases and gates

### Hardware situation: simulator-first development

The owner currently has **no pen device**. Development runs against a **virtual tablet** (the
"simulator"). **Real-pen testing is not required** (owner, 2026-10-12): no phase, gate,
milestone, task or test may depend on a physical pen, owner recordings or calibration.
Everything pen-related is implemented **best effort** on the simulator. If the owner later finds
problems with a real pen, the owner reports or fixes them after implementation, through the
normal bug / change-proposal process.

- **Virtual tablet ("simtab").** A tool that creates a virtual tablet through Linux `uinput`
  (pen tool, eraser tool, absolute X/Y, pressure, tilt X/Y, distance, barrel buttons) and drives
  it from stroke files (recorded or generated). Events then travel the **real path**:
  kernel → libinput → compositor → Wayland tablet protocol → app. The app must not be able to
  tell it from a physical tablet. Verify on the owner's machine that libinput and the compositor
  classify it as a tablet; record the evidence. Document the `/dev/uinput` permission setup
  (udev rule preferred over running as root).
- **Generated strokes.** simtab ships a stroke generator: lines, curves, scribbles, fast/slow
  strokes, pressure ramps, tilt sweeps, eraser flips, hover/proximity, high event rates. Plus
  replay of stroke files from public datasets (see §2 Phase 2, handwriting) where licenses allow.
- **What the simulator may decide:** which variant delivers which axes, event rate and timestamp
  handling of the software path, coordinate mapping and scaling, **software latency** (event
  received → frame presented), rendering performance. These measurements are valid for Gate 0.
- **Not covered by the simulator (optional, owner's choice, after implementation):** the device's
  own latency, jitter and pressure response, pen-display vs. screenless-tablet mapping, and the
  owner's feel test. `docs/hardware-verification.md` lists them as an **optional** checklist the
  owner may run whenever they like. Nothing waits for it. If a real-pen finding contradicts a
  locked decision, it goes through the change-proposal process (§3), not a silent switch.
- Every measurement in every results doc is labelled **[sim]** or **[hw]**.

### Phase 0 — Input & latency foundation spike (BOXED; exit: Gate 0)

This is the **first and only task** until Gate 0 is passed. Pen input and latency are the
project's riskiest requirement, and every later decision depends on them.

**Box rules (strict):**
- Work **only** in `spikes/input/` (including `spikes/input/simtab/`) and
  `docs/adr/0001-input-and-render-path.md` plus `docs/spikes/input-results.md`,
  `docs/environment.md` and `docs/hardware-verification.md`. Do not create the product
  workspace, do not write SPEC.md, do not start other spikes.
- Spike code is throwaway. It may later be used as reference, never copied into the product
  without review. (simtab is the exception: it graduates into the Phase 3 harness after review.)
- Language: Rust. If measurements show Rust cannot meet the requirement, report it; do not
  switch languages on your own.

**Tasks:**
1. **Environment probe** → `docs/environment.md`: compositor and version, GPU and driver,
   display(s) and refresh rate, scaling. Record that no physical pen device is present yet.
2. **Build simtab** (see "Hardware situation") and prove libinput + compositor treat it as a
   tablet (`libinput list-devices`, `libinput debug-events`).
3. **Three variants**, each a minimal app: one window, receives pen input, draws strokes whose
   width follows pressure, logs every raw event with timestamps, nothing else.
   - **(a)** GTK4 (Rust bindings), using the toolkit's tablet input.
   - **(b)** a winit-based stack, using whatever pen support it offers. If pressure/tilt are
     unsupported, record that with the source (issue, docs, code) and stop that variant.
   - **(c)** direct Wayland client (Rust Wayland libraries) binding the tablet-v2 protocol
     itself, with own GPU rendering (e.g. wgpu).
4. **Measure each viable variant with simtab** on the owner's machine (all results **[sim]**):
   - axes delivered: pressure (with range), tilt, eraser end, barrel buttons, proximity, distance;
   - event rate and timestamp handling (compare to `libinput debug-events` for the same input);
   - software latency: event timestamp → presented frame (Wayland presentation feedback);
     define the method in the results doc;
   - effect of latency techniques that are available: drawing immediately on event, frame
     scheduling choices, partial redraw, ink prediction;
   - correct coordinate mapping with window moved, scaled (fractional scaling), and on the
     owner's monitor setup.
5. **Write `docs/hardware-verification.md`:** the checklist for when a real device exists
   (device latency, jitter, pressure curve, pen display vs. tablet mapping, owner feel test,
   re-run of the [sim] measurements as [hw]).
6. **Raw evdev reading is NOT built.** Document it in the ADR as a rejected/fallback option
   (reasons: no window position on Wayland, double input unless the device is grabbed,
   permissions, loss of libinput calibration/mapping). It may only be prototyped if (c) shows a
   measured, specific problem that evdev would solve, and only after the owner approves.
7. **ADR 0001** with a comparison table, the recommendation, and the measured evidence. Every
   capability claim must cite a measurement or a source; nothing from memory. State clearly
   which conclusions rest on [sim] data.
8. **Commit** everything as `P0: input and latency spike` and **stop**. Update STATUS.md to
   "waiting for Gate 0" and summarise the recommendation for the owner.

**Gate 0:** the owner reads ADR 0001, tries the variants with simtab (and the mouse for a rough
feel), and approves one. After approval the ADR status becomes **Accepted (locked)** — see §3
"Locked decisions". The hardware checklist is optional and not part of any gate.

### Phase 1 — SPEC (exit: Gate A)
Produce `SPEC.md`:
- Every requirement from §1 becomes numbered requirements (`R-CANVAS-001`, …) with
  **testable acceptance criteria** (Given/When/Then or measurable thresholds).
- Fill gaps by asking the owner (batched). Examples of things to clarify: multi-page PDF layout
  on canvas, audio recording vs. import only, text box formatting depth, link types, undo scope,
  search UI, keyboard shortcuts, what "shapes" includes, ruler behaviour, default block-idle
  threshold.
- Propose **performance budgets** as concrete numbers, clearly marked as proposals, for the owner
  to confirm. At minimum: input-to-ink latency, frame time while inking/panning/zooming at the
  display's refresh rate, canvas scale targets (e.g. strokes per canvas, PDF pages per canvas),
  cold start, note open time, search latency, max memory for a reference workload.
- Propose handwriting quality targets: writing-vs-drawing classifier precision (drawings
  modified must be ~0 on the eval set), recognition accuracy target per language, prettify
  "style preserved" criteria (owner judgement via review gallery + automated bounds).
- Write `docs/out-of-scope.md`.

**Gate A:** owner approves SPEC.md (including budgets). Record approval in STATUS.md.

### Phase 2 — Remaining stack spikes (exit: Gate B)
All spikes **build on the input/render path locked in ADR 0001**; none may re-open it.

Build **throwaway spikes** in `spikes/` and write one ADR per decision in `docs/adr/`.
Every claim must be backed by a measurement or a cited source; never assert library capabilities
from memory.

Required spikes:
1. **UI layer on top of the locked path:** how panels, folder tree, dialogs and text boxes are
   built without compromising the input/render path (e.g. own UI on the GPU renderer vs. a
   toolkit around the canvas — only options compatible with ADR 0001).
2. **Rendering at scale:** 100k+ strokes with pressure-varying width, pan/zoom at refresh rate,
   tiling/caching strategy, GPU path.
3. **PDF rendering:** candidate libraries; speed on large documents; **license** (flag any
   GPL/AGPL dependency — needs owner approval).
4. **Storage:** prototype the chosen format at scale (e.g. SQLite + content-addressed blob store,
   or alternatives); crash safety test (kill -9 during writes).
5. **Offline handwriting recognition (de/en):** survey and benchmark candidate models/libraries
   (accuracy, latency, size, license, runtime). Online (stroke-based) recognition preferred.
6. **Writing-vs-drawing classification:** survey approaches; prototype a baseline.
7. **Prettifying:** prototype stage 1 = deterministic geometry (baseline straightening, slant and
   size normalisation, smoothing, spacing). Stage 2 (learned, style-preserving re-synthesis) is
   research: survey only, no commitment.

**Handwriting work proceeds without the owner's ink (owner data is optional, never required):**
- Build recognition, classification and stage-1 prettify **now**, using public datasets,
  existing open models and synthetic data. Do not wait for owner recordings.
- **Licenses of data and model weights are checked before use** and recorded in `ATTRIBUTION.md`.
  Many handwriting datasets require registration or allow research/non-commercial use only
  (e.g. IAM On-Line must be checked); weights trained on such data inherit that question. Anything
  not clearly permissive needs owner approval. Code licenses (e.g. MIT) do not cover weights or
  data.
- Note that offline (image-based) datasets and models exist for German too; stroke data can be
  rendered to images if an image-based recogniser turns out stronger. Decide by benchmark.
- **Synthetic writing-vs-drawing data:** generate drawings (arrows, boxes, circles, diagrams,
  sketches, crossings-out, underlines) and mix with public handwriting so the classifier can be
  trained and tested before any owner data exists.
- **Conservative defaults (best effort, no calibration needed):** the classifier threshold is set
  so that drawing false positives on the public/synthetic eval set are **zero**, even at the cost
  of missing real writing. Prettify is fully implemented. Its mode is a user setting
  (off / suggest / automatic) that defaults to **suggest** as a safety default, not as a wait for
  calibration. Recognition-for-search may run on everything.
- **Owner tuning (optional, later, owner-initiated):** if the owner wants, they can record
  fixtures, label writing/drawing and review the prettify gallery. Thresholds may then be
  re-tuned and the owner set added as a protected test set. Nothing is blocked on this.
  Listed in `docs/hardware-verification.md` (optional).
8. **Audio:** recording/playback on Linux (PipeWire) if audio recording is in SPEC.

Patent caution: handwriting beautification has existing patents. Note this in the prettify ADR
and tell the owner; do not give legal conclusions.

**Gate B:** owner approves the stack ADRs based on measured results.

### Phase 3 — Harness (exit: Gate C)
Build the real workspace skeleton and **the full test harness before any feature code**.
Features may be stubs that return "not implemented" errors; tests for them start red.

Required harness components:
1. **Workspace & layering.** Cargo workspace with layered crates (proposal, adapt in ADR):
   `geom` → `model` (document, folders, objects, strokes) → `storage` / `ink` (stroke processing)
   / `hwr` (recognition, classification, prettify) → `render` → `engine` (session, undo,
   command registry) → `control` (JSON control channel + CLI) → `ui` (thin) → `apps/`.
   A `cargo xtask layers` check enforces that nothing below `ui` depends on UI/windowing crates.
2. **Everything is a command.** Every user-visible action is a registered command with id,
   params schema, `enabled`, `run`, and tests. UI, shortcuts, CLI and control channel all
   dispatch by id. Programmatic calls never open dialogs.
3. **Headless engine + control channel.** Drive the app without UI: JSON lines over a local
   socket (`engine.execute`, `ui.screenshot`, `state.inspect`, …) and a CLI
   (`inkwell-cli run --cmd …`, `inspect`, `render-png`). Agents verify work via `state.inspect`
   first, screenshots second.
4. **Ink recording & replay.** A recorder tool that captures raw pen events (x, y, pressure,
   tilt, timestamps, device info) to a versioned fixture format. Replaying a fixture through the
   engine must be **deterministic**, both directly into the engine (fast, for CI) and through
   **simtab** into the running app (end-to-end through the real Wayland path). Initial fixtures
   come from the simtab generator and license-cleared public data. The generator must cover the
   cases a real owner would produce: German text, English text, mixed, fast/slow, small/large,
   diagrams, arrows, sketches, text next to drawings, text over a PDF, crossings-out,
   underlines. Owner recordings are **optional** and may be added later by the owner.
5. **Labelled eval sets:** initially public + synthetic (writing vs. drawing per block; ground-truth
   text). An owner set may be added later if the owner chooses (optional); it is then protected
   like all tests.
6. **Golden images.** Headless render to PNG, compare with tolerance against approved goldens in
   `tests/golden/`. Approval workflow: `cargo xtask golden --review` produces an HTML page showing
   old/new/diff; only the owner approves new goldens.
7. **Property & fuzz tests.** Save→load is identity; any command sequence followed by full undo
   restores the exact state; every command with hostile params never panics; storage survives
   kill -9 (crash test).
8. **Performance benchmarks with budgets** from SPEC (latency path, render at scale, open times,
   search). CI fails on budget regressions beyond a tolerance.
9. **Handwriting quality suite.** Classifier precision/recall on the eval set (drawing false
   positives gated hard), recognition accuracy per language, prettify geometric bounds. Plus
   `cargo xtask prettify-gallery` → HTML before/after gallery for owner review.
10. **Parity catalog.** `catalog` lists every SPEC requirement id with its command ids and tests;
    `cargo xtask parity` writes `docs/parity.md`; a test enforces a pass-count floor that may only
    rise.
11. **One gate command:** `cargo xtask ci` = fmt, clippy `-D warnings`, tests, layers, asset/license
    check, goldens, quality suite, benchmarks (fast subset; full set via `cargo xtask bench`).
12. **Harness self-validation.** Prove the harness catches bugs: introduce deliberate faults
    (e.g. drop pressure, break undo, modify a drawing block, corrupt a save) on a scratch branch
    and show each one makes `cargo xtask ci` fail. Record results in `docs/harness-validation.md`.

**Gate C:** owner reviews the tests, fixtures, eval labels, budgets in code, and harness
validation report, and approves. After Gate C, **tests are protected** (see §3). No gate requires a
physical pen. Hardware items in `docs/hardware-verification.md` are optional.

### Phase 4 — Plan (exit: Gate D)
- Write `plan/execution-plan.md`: milestones → tasks; every task names the failing tests it must
  turn green and the requirement ids it covers. Order by dependency and risk; inking latency and
  storage come early, UI polish late.
- Write `plan/architecture.md` from the ADRs.
- Write `ROADMAP.md` (committed) with milestones and honest status.
- Rewrite this file into the permanent `AGENTS.md`: keep §0 (adapted), §3, §4, §5, §6; replace
  §2 with the implementation loop below; keep §1 as a short summary pointing at SPEC.md.
- Create the subagent definitions from §5.

**Gate D:** owner approves the plan. Then implementation begins.

### Implementation loop (after Gate D)
1. Pick the next task from `plan/execution-plan.md` (or the next red test in your area).
2. Implement until its tests are green and `cargo xtask ci` passes.
3. For UI/visual work: render via the headless harness, look at the PNG, compare with SPEC.
4. Commit with the task id (`M2.3: pressure-width curve`). Update STATUS.md and ROADMAP.md.
5. Repeat. Stop and ask only for owner decisions.

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
- Tests, goldens, fixtures and eval sets live in clearly separated directories; `xtask ci` reports
  any diff in them so the owner sees it.

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

Create subagent definitions in `.claude/agents/` (frontmatter `model:` per role). Proposal:
- **architect** (most capable model): ADRs, cross-crate design, hard algorithms (ink rendering,
  classifier, prettify), reviewing risky diffs.
- **implementer** (capable model): feature tasks against existing tests.
- **explorer / test-runner / log-triage** (cheaper, faster model): searching code, running
  `xtask ci`, summarising failures, reading traces, updating docs.
- **reviewer** (capable model, fresh context): reviews a finished milestone against SPEC without
  having written it.

Log in `plan/usage.md` which role handled which task, so the owner can tune cost vs. quality.

Parallel agents: each owns specific crates for a task; separate `CARGO_TARGET_DIR` per agent;
never edit another agent's crates; merge only when `xtask ci` is green; clean up target dirs.

---

## 6. Running and looking at the app

All commands run from the repo root inside `nix-shell` (root `shell.nix`). On the owner's machine
the agent builds with `CARGO_TARGET_DIR=/dev/shm/vibenote-target` (disk nearly full; RAM-backed,
lost on reboot, so rebuild then).

**Headless engine (no UI yet; the GUI app arrives with its milestone):**
- List commands (id, requirement ids, implemented, params schema): `cargo run -p vibenote-cli -- commands`
- Run commands in sequence on a fresh in-memory session:
  `cargo run -p vibenote-cli -- run workspace.note.create '{"name":"n"}' note.open '{"id":1}' ink.stroke.add '{"samples":[{"x":1,"y":2,"p":0.5}]}'`
- Persistent workspace: add `--workspace <path.sqlite>` before the subcommand (any subcommand).
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
- Replay through the real Wayland path: `cargo run -p simtab -- play <stroke-file|--pattern …> --window <app-class>`
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
| `cargo xtask bench` | slow checks: kill -9 ×1000, performance budgets in release mode; `bench latency` needs the app |
| `cargo xtask prettify-gallery` | before/after prettify gallery (needs `hwr.prettify_block`) |
| `cargo xtask protect [--write]` | protected test data hashes (`tests/PROTECTED.sha256`); `--write` only with owner approval |
| `tools/harness/validate.sh` | harness self-validation with deliberate faults (scratch copy) |

**Test layout:** `cargo test` = implemented behaviour (must pass). `tests/acceptance` = one or
more tests per SPEC requirement (`r_<area>_<nnn>_…`), may be red; tracked in `docs/parity.md`.
Protected after Gate C: `tests/golden`, `tests/fixtures`, `tests/eval`, `tests/budgets.toml`,
`tests/parity_floor.txt`.
