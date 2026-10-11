# ADR 0005 — Storage

- **Status:** Accepted (locked) — Gate B (partial), 2026-10-11, owner approved.
- **Date:** 2026-10-11
- **Evidence:** `spikes/storage/` (`st`: bench / crash writer / verifier; `crash.sh`;
  `fts_bench.py`), results in `spikes/storage/bench-results.jsonl`. Disk: ext4 on dm-crypt
  (`/dev/mapper/cryptroot`) on the owner's machine.

## Requirements
R-DATA-001 (no data loss on crash; ink durable ≤ 1 s after pen-up), R-DATA-002 (scale),
R-DATA-003 (versioned format, save→load identity), R-PERF-004 (first viewport ≤ 150 ms),
R-PERF-005 (search ≤ 100 ms), R-PERF-009 (save never blocks inking), R-UNDO-001 (persistent
undo).

## Candidates
| | Licence | Notes |
|---|---|---|
| **SQLite 3.53.2** (bundled via rusqlite 0.40.2 / libsqlite3-sys 0.38.2) | public domain; rusqlite MIT | WAL, `synchronous=FULL`; bundled build enables RTREE, FTS5, JSON1 (`libsqlite3-sys build.rs`) |
| **redb 4.3.0** | MIT OR Apache-2.0 | pure-Rust embedded ACID key-value store; no secondary indexes or full-text search |
| Blob store (both) | — | content-addressed files `blobs/<h[0..2]>/<blake3>`: write temp → `fsync` → `rename` → `fsync(dir)`; blake3 1.8.7 (CC0/Apache) |

## Workload
The reference note (ADR 0003 generator) has 100,000 strokes and 7,956,597 samples. Stroke
encoding: zig-zag varint deltas of x/y (1/16 unit), pressure (u16), tilt (i8 ×2) and dt
(100 µs). That is **7.22 bytes/sample**, 57.5 MB for the note.

## Results
| | SQLite (WAL, FULL) | redb |
|---|---|---|
| Durable commit per pen-up (1 stroke + spatial index), p50 / p95 / p99 / max | 8.3 / 25.9 / 37.4 / 68.3 ms | 4.6 / 22.3 / 34.5 / 50.0 ms |
| Bulk insert 98k strokes (batches of 500) | 7.8 s | 4.3 s |
| DB size for the note | **82 MB** | 127 MB |
| Open | 0.5 ms | 6.5 ms |
| Load + decode first viewport (1920×1200 at zoom 1) | 0.87 ms (390 strokes, R-tree) | 1.44 ms (704 strokes, 2,048-unit chunk keys) |
| Load + decode **whole note** (8M samples) | 94.7 ms | 84.4 ms |
| Insert 100k notes / 2,000 folders | 401 ms | 235 ms |
| List one folder (50 notes, sorted) p50 / max | 0.019 / 0.33 ms | 0.006 / 0.04 ms |

**Crash safety (kill -9):** a writer commits 1–3 strokes per transaction in a loop and prints
`ACK <id>` only after `commit()` returns. A driver kills it with SIGKILL after a random
20–400 ms, reopens the database and checks two things: SQLite `PRAGMA integrity_check = ok`
(redb: open succeeds), and every acknowledged id is present in both the stroke table and the
spatial index, with every stored stroke decoding.
- SQLite: **200/200 iterations passed**
- redb: **200/200 passed**
- Blob store (1 KB–2 MB blobs, re-hashed after each kill): **100/100 passed**

**Full-text search (FTS5,** probed with system SQLite 3.53.3, `unicode61 remove_diacritics 2`):
- 100,000 recognised blocks / 5.0 M words (10k notes × 10 blocks) indexed in 1.9 s, 67 MB.
- Top-50 ranked queries with snippets: 0.1–0.3 ms for words and phrases, 11.7 ms for a
  prefix query matching 18,489 blocks.
- "Ubung" finds "Übung". "Grösse" does **not** find "Größe" (ß ≠ ss).

## Decision
1. **One SQLite database per workspace** (WAL, `synchronous=FULL`, bundled SQLite) for folders,
   notes, canvas objects, strokes (encoded blobs as above), the per-note persistent undo log,
   the spatial index (R-tree per object) and the search index (FTS5).
2. **Large binary assets** (PDFs, images, audio) go in the **content-addressed blob store**
   next to the database. Rows reference blobs by blake3 hash; the blob is written durably
   *before* the transaction that references it. Garbage collection by reference scan
   (never while a referencing transaction may be pending).
3. **Writes happen on a storage thread**, never on the input/render path (R-PERF-009). Pen-up →
   enqueue → group-commit at most every 250 ms. Measured durable commit p99 is 37 ms, well
   inside the 1 s durability target.
4. **Note open:** the viewport query through the R-tree first (0.9 ms here), then stream the
   rest in the background. The whole 100k-stroke note decodes in ~95 ms.
5. **Search normalisation** happens in our code before FTS5, at index and query time:
   casefold, ß→ss, ä/ö/ü → both "a/o/u" and "ae/oe/ue" forms (R-SEARCH-003). The tokenizer
   alone does not do this, as measured.
6. **Schema versioning:** `PRAGMA user_version` plus forward migrations in transactions,
   tested from every released version (Phase 3 harness).

Why SQLite over redb: crash safety and performance are both sufficient for both. SQLite
additionally gives R-tree and FTS5 (redb has neither, so we would build our own spatial and
full-text indexes), a smaller file (82 vs 127 MB), and an inspection tool (`sqlite3` CLI) that
helps the agent-driven workflow. redb's faster commits do not matter, because commits are off
the pen path.

## Not tested / open
- **Power loss** (as opposed to process crash) was not simulated. SQLite documents that WAL with
  `synchronous=FULL` is durable across power loss provided the OS/disk honour fsync
  (sqlite.org "Atomic Commit", "WAL" docs). Testing it needs a fault-injecting block device
  (e.g. dm-flakey/dm-log-writes) and is planned for the Phase 3 crash harness.
- Concurrent access by two app instances: prevent with a workspace lock file.
- Disk-full handling: the owner's disk had 4.4 GB free during the spike. Writes must fail
  cleanly and surface an error; to be tested in Phase 3.
