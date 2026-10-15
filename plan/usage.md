# Model usage log

Which subagent role (`.claude/agents/`) handled which task, so the owner can tune cost vs.
quality (AGENTS.md §5). One row per task or review; add a row when a task finishes.

| Date | Task | Role (model) | Outcome | Notes |
|---|---|---|---|---|
| 2026-10-11 – 2026-10-13 | Bootstrap phases 0–4 | main session (Opus 5.5), no subagents | gates 0, A, B, C, D passed | — |
| 2026-10-13 | M1.1–M1.4 | main session (Opus 5.5) | done; 4 requirements green | — |
| 2026-10-13 | M1 review | reviewer (Opus 5.5; run as a general-purpose agent with the reviewer definition, since `.claude/agents/` loads only in a new session) | 13 findings (3 high, 3 medium, 7 low); dispositions in STATUS | ~134k tokens, 27 tool calls, 4.5 min |
| 2026-10-13 | M2.1 storage thread + M1 review fixes | main session (Opus 5.5) | done; R-PERF-009 green | — |
| 2026-10-13 | M2.2 persistent undo | main session (Opus 5.5) | done; R-UNDO-001 green | — |
| 2026-10-13 | M2.3 lazy open | main session (Opus 5.5) | done; R-PERF-004 green | — |
| 2026-10-13 | M2.4 settings | main session (Opus 5.5) | done; R-HWR-006 green | — |
| 2026-10-13 | M2 review | reviewer (Opus 5.5; general-purpose agent with the reviewer definition) | 12 findings (1 high, 3 medium, 8 low); 10 fixed/mitigated, 2 recorded | ~134k tokens, 34 tool calls, 6.4 min |
| 2026-10-13 | M3 design | architect (Opus 5.5; general-purpose agent with the architect definition) | design note + ADR 0011 evidence (owner approved) | ~215k tokens, 42 tool calls, 19.5 min |
| 2026-10-13 | M5.1 trash | main session (Opus 5.5) | done; R-ORG-004 green | — |
| 2026-10-13 | M3.1 GPU-resident chunks | main session (Opus 5.5) | done; render tests + goldens green; R-PERF-002 not counted yet | — |
| 2026-10-13 | M3.1b change feed | main session (Opus 5.5) | done | — |
| 2026-10-13 | M3.2 LOD | main session (Opus 5.5) | done; R-PERF-002 green | — |
| 2026-10-13 | M3 review | reviewer (Opus 5.5; general-purpose agent with the reviewer definition) | 13 findings (1 high, 4 medium, 8 low); 11 fixed, 2 recorded | ~142k tokens, 28 tool calls, 5.8 min |
| 2026-10-13 | M4.7 view, M4.1 tools | main session (Opus 5.5) | done; R-CANVAS-002, R-PEN-003, R-PEN-005 green | — |
| 2026-10-13 | M4.2–M4.4 erasers, M3.3 tiles | main session (Opus 5.5) | done; R-PEN-006/007/008 green; R-PERF-002 passing but not in the floor | — |
| 2026-10-13 | M4.5 lasso/selection, M4.4 barrel buttons | main session (Opus 5.5) | done; R-PEN-009, R-PEN-010 green | — |
| 2026-10-13 | M4.6 mouse | main session (Opus 5.5) | done; R-PEN-012 green | — |
| 2026-10-13 | M4.8 z-order, backgrounds | main session (Opus 5.5) | done; R-CANVAS-005, R-CANVAS-006 green; 5 goldens proposed | — |
| 2026-10-13 | M4.9 clipboard, selection overlay | main session (Opus 5.5) | done; R-UNDO-002 green | — |
| 2026-10-13 | M5.2 quick-open (engine) | main session (Opus 5.5) | done; R-ORG-003, R-PERF-008 green | — |
| 2026-10-13 | M4 review | reviewer (Opus 5.5; `.claude/agents/reviewer.md`) | 13 findings (2 high, 6 medium, 5 low); 10 fixed, 3 recorded | ~137k tokens, 52 tool calls, 12.7 min |
| 2026-10-13 | M5.3 keymap, M4 review fixes | main session (Opus 5.5) | done; R-CMD-002 green | — |
| 2026-10-14 | M6.1–M6.5 PDFs, M7 objects | main session (Opus 5.5) | done; 16 requirements green | — |
| 2026-10-14 | M8 search, M6.6 PDF text | main session (Opus 5.5) | done; 6 requirements green | — |
| 2026-10-14 | M9.1–M9.4, M9.7 handwriting | main session (Opus 5.5) | done; R-HWR-001/002/004..008 green | — |
| 2026-10-14 | M9.5 beam search + lexicon | main session (Opus 5.5) | done; CER en 15.2 % / de 18.0 % (R-HWR-003 still red) | — |
| 2026-10-14 | M6–M9 review | reviewer (Opus 5.5; `.claude/agents/reviewer.md`) | 14 findings (3 high, 7 medium, 4 low); 13 fixed, 1 for the owner (protected tests) | ~193k tokens, 73 tool calls, 9.2 min (a first attempt stopped on an API rate limit before doing any work) |
| 2026-10-14 | M6–M9 review fixes, M9.6 fine-tuning setup | main session (Opus 5.5) | done / training running | — |
| 2026-10-14 | M11 owner feedback: cursors, selection drag, shape tool + snap, text tool, prettify suggestions, icons/tree styling | main session (Opus 5.5) | done; live-checked with simtab [sim] | — |
