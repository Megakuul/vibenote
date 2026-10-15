"""Deliberate faults for harness self-validation (AGENTS.md §2 Phase 3 item 12).
Each fault is (name, file, old, new); `old` must match exactly once. Applied to a scratch copy of
the repo, never to the working tree. Used by tools/harness/validate.sh. Keep patterns in sync
with the code (validate.sh refuses to run ci when a pattern does not match)."""
FAULTS = [
    # Pen pressure is lost: every sample gets pressure 1.0.
    ("drop-pressure", "crates/engine/src/commands/input.rs",
     "        pressure: e.p,\n", "        pressure: 1.0,\n"),
    # Undo applies only the first inverse op of a step.
    ("break-undo", "crates/engine/src/lib.rs",
     "        let step = h.undo.pop().ok_or(EngineError::NothingTo(\"undo\"))?;\n        match self.ws.apply_all(step.inverse.clone()) {",
     "        let step = h.undo.pop().ok_or(EngineError::NothingTo(\"undo\"))?;\n        match self.ws.apply_all(step.inverse.iter().take(1).cloned().collect()) {"),
    # Committed ink is silently shifted (stands in for "a block was modified without permission").
    ("modify-ink-silently", "crates/engine/src/commands/input.rs",
     "                samples: ls.samples,\n",
     "                samples: ls.samples.into_iter().map(|mut q| { q.y += 0.5; q }).collect(),\n"),
    # Save format corruption: timestamps off by one per sample.
    ("corrupt-save", "crates/storage/src/codec.rs",
     "        put_varint(&mut out, smp.t_us.wrapping_sub(pt));",
     "        put_varint(&mut out, smp.t_us.wrapping_sub(pt).wrapping_add(1));"),
    # Durability weakened: no fsync (only power loss would expose this).
    ("no-fsync", "crates/storage/src/lib.rs",
     'conn.pragma_update(None, "synchronous", "FULL")?;',
     'conn.pragma_update(None, "synchronous", "OFF")?;'),
    # Layering broken: UI crate below the UI layer.
    ("layer-violation", "crates/render/Cargo.toml",
     'thiserror = "2"\n', 'thiserror = "2"\negui = "0.36"\n'),
    # Panicking construct in product code.
    ("unwrap-in-product", "crates/model/src/lib.rs",
     "        self.folders.get(&id).ok_or(ModelError::FolderNotFound(id))",
     "        Ok(self.folders.get(&id).unwrap())"),
    # Budget loosened to make work "pass".
    ("loosen-budget", "tests/budgets.toml",
     "p50_ms = 10.0", "p50_ms = 50.0"),
]
