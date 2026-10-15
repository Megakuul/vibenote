# Harness self-validation (AGENTS.md §2 Phase 3 item 12)

- **Date:** 2026-10-12
- **Method:** `tools/harness/validate.sh` copies the repo to a scratch dir in RAM (never touching the
  working tree), applies one deliberate fault from `tools/harness/faults.py`, and runs
  `cargo xtask ci`. A `baseline` run (no fault) must pass first. Raw output:
  `docs/harness-validation.raw.txt` (last runs).

## Final result

| Fault | What it simulates | ci | Caught by |
|---|---|---|---|
| baseline | — | ✅ passes | — |
| drop-pressure | every pen sample gets pressure 1.0 | ❌ fails | `tests` (fixture replay: exact pressures), `parity` (R-PEN-002 left the floor) |
| break-undo | undo applies only the first op of a step | ❌ fails | `tests` (multi-op undo test) |
| modify-ink-silently | committed ink shifted by 0.5 units | ❌ fails | `tests` (exact replay, goldens) |
| corrupt-save | stroke codec writes wrong timestamps | ❌ fails | `tests` (storage property tests, reopen identity) |
| no-fsync | SQLite `synchronous=OFF` | ❌ fails | `tests` (durability configuration test) |
| layer-violation | `egui` added to `vibenote-render` | ❌ fails | `layers`, `licences` (egui fonts' OFL/Ubuntu licence), `parity` |
| unwrap-in-product | `.unwrap()` in `vibenote-model` | ❌ fails | `clippy`, `tests` |
| loosen-budget | latency budget p50 10 → 50 ms | ❌ fails | `protected` (tests/budgets.toml differs from the recorded manifest) |

## What the validation found (and what was fixed)

The first runs showed real gaps. The harness was improved before this report:

1. **break-undo was not caught.** Nearly every command produces a one-op undo step, and the
   property test only deleted one object at a time, so "undo only the first op" was invisible.
   - Added a multi-object delete/undo/redo test.
   - The random-command property test now deletes 1–3 objects per step.
2. **no-fsync was not caught.** kill -9 cannot detect a missing fsync; the OS page cache
   survives a process crash, so only power loss would expose it. Added a test asserting
   WAL + `synchronous=FULL` (`Store::durability`). A real power-loss test is still open (below).
3. **R-PEN-002's acceptance test was too weak.** A constant pressure of 1.0 still satisfied
   "pressure rises". It now requires the stored pressures to equal the delivered (quantised)
   ones and to span the ramp.
4. **Changed protected files were only reported.** Loosening a budget still produced a green
   ci. ci now **fails** when goldens, fixtures, eval sets, budgets or the parity floor differ
   from `tests/PROTECTED.sha256`. The owner accepts a change with
   `cargo xtask protect --write`.

Bugs in the validation tooling itself (fixed):
- Fault patterns went stale after rustfmt. The runner ran ci on an unmodified copy and reported
  that as a result. It now refuses to run ci when a pattern does not match exactly once
  (PATCH-FAILED).
- `rsync -a` preserved old mtimes, so cargo reused binaries built from the previous fault.
  Results were contaminated (e.g. loosen-budget showed the drop-pressure failure). It now
  copies with fresh mtimes (`--no-times`).
- The RAM disk (`/dev/shm`, 16 GB) filled up twice. Builds failed with "No space left on
  device", which looked like test failures. Builds now run without incremental artefacts and
  the fault build dir is removed after each validation.

## Limits (not covered by the harness yet)

- **Power loss:** no fault-injecting block device test (dm-flakey / dm-log-writes). The
  durability configuration is asserted, and kill -9 is tested 25× in ci and 1,000× in
  `cargo xtask bench`.
- **"Modify a drawing block" (handwriting safety):** the writing-vs-drawing gate (R-HWR-002,
  0 drawing false positives) is an acceptance test that is red until the classifier exists. A
  fault in classification cannot be validated before that. Generic ink modification is
  caught (row "modify-ink-silently").
- **End-to-end latency (R-PEN-001 / R-PERF-001)** and other app-level budgets need the GUI app.
  They are red acceptance tests until the app milestone.
- Performance budgets are enforced through the parity floor: once a budget test passes, a
  regression fails ci. A 10 % tolerance band (`tests/budgets.toml [regression]`) is not yet
  applied, since no budget test measures the app path yet.
