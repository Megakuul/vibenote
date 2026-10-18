# ADR 0012 — One-way cloud backup (S3, Google Drive)

- **Status:** Accepted — scope by the owner 2026-10-18 ("Implement sync / backup to the cloud.
  Should be capable of doing S3 and Google Drive. If I enable it it should just backup the
  file."); the technical choices below are the agent's and open to owner change.
- **Requirements:** R-BACKUP-001..004, revised R-PLAT-003 (SPEC §13a, §1)

## Context

Until now the product had no network access at all (R-PLAT-003, `docs/out-of-scope.md`). The
owner asked for a cloud backup of the workspace to S3 and Google Drive that "just backs up the
file". A workspace is one SQLite database (WAL) plus a content-addressed blob directory
(ADR 0005). Writes come from the storage thread at any time.

## Decisions

1. **One-way backup, no sync.** Nothing is ever downloaded into an open workspace; restore is an
   explicit CLI command into a new directory. No merge, no accounts, no conflict handling.
2. **New crate `vibenote-backup`** (layer: storage, for the snapshot; engine and CLI depend on it). It is the only crate
   with networking dependencies. HTTP client: **ureq 3** (blocking, MIT/Apache) with **rustls**
   (ring provider) and **rustls-platform-verifier** (system trust store; avoids bundling
   Mozilla roots under CDLA). Signing: `hmac` + `sha2` (RustCrypto, MIT/Apache); `base64`.
   No async runtime: one background thread does everything.
3. **Snapshot = `VACUUM INTO`** on the backup thread's own read connection (WAL readers never
   block the writer or the UI's reader), then `PRAGMA quick_check` on the copy. A failing check
   aborts the run without uploading. The temporary file lives next to the database and is
   always removed.
4. **Layout** under `<prefix>/<workspace file stem>/`: `workspace.sqlite` (latest),
   `daily/<mon..sun>.sqlite` (overwritten by the first backup of each weekday: 7 days of
   history without list/delete calls), blobs at `<db file name>.blobs/<h2>/<hash>` — on S3 this
   mirrors the local layout, so `aws s3 sync` is a valid restore too. On Drive the same names live
   in an app-created folder `vibenote-backup/<stem>/` with subfolders `daily` and `blobs` (blobs
   flat, named by hash). Blobs are immutable (content addressed), so each is uploaded once per
   target; the set of uploaded hashes is kept in local state. Remote blobs are never deleted.
5. **When:** the engine checks once a minute (and at start) whether backup is enabled, the
   workspace files (database, WAL) were modified after the modification time recorded when the
   last successful backup began (independent of wall-clock jumps), and that backup is at least
   `interval_min` (default 10) old; a failed run is retried one interval later. "Back up now"
   skips the interval. Only one run at a time.
6. **Configuration outside the workspace:** `$XDG_CONFIG_HOME/vibenote/backup.json` (0600,
   written atomically), keyed by the workspace's canonical path. Secrets (S3 secret key, Drive
   refresh token) are never part of the workspace, its backup, or `backup.status`. The engine
   reads this file only when the app or CLI passes its path explicitly (`Engine::use_backup_config`),
   so tests and scratch engines can never start a backup; `$VIBENOTE_BACKUP_CONFIG` points the
   app at another file (state next to it) for scratch runs. Local state (last success, uploaded
   blobs, Drive file ids) lives in `$XDG_STATE_HOME/vibenote/backup-state.json`.
7. **S3:** Signature V4, path-style URLs on custom endpoints (MinIO, R2, …) and virtual-hosted
   style on AWS; single `PUT` per object (S3 limit 5 GB, above R-DATA-002's 1 GB PDF); payload
   SHA-256 computed while streaming the file twice. Credentials: access key pair in the config,
   or a named profile in `~/.aws/credentials` (incl. `aws_session_token`); region from the
   config.
8. **Google Drive:** OAuth 2.0 for installed apps with PKCE and a loopback redirect on
   `127.0.0.1:<random port>`; scope `drive.file` (only files the app created; no Google
   verification needed for personal use). The owner supplies a client id/secret of type
   "Desktop app" from their own Google Cloud project (Google does not allow borrowing another
   app's client). Uploads use the resumable protocol with one `PUT` of the whole file; existing
   files are updated in place (file ids cached, looked up by name on a miss).
9. **Never on the latency path:** the worker owns its connection and HTTP agent; the engine only
   exchanges messages with it in `Engine::backup_tick` (non-blocking). Timeouts: 30 s to connect
   and 120 s for the answer after a request was sent; bodies stream without a time limit (large
   files on slow uplinks).

## Verification

- SigV4 reproduces the worked example of the AWS documentation (unit test).
- Acceptance tests run the whole flow against fake S3 and Drive servers on loopback (payload
  hashes and PKCE checked by the fakes), including restore.
- [sim] live: the real app backed up a scratch workspace to a local fake S3 at start; the CLI
  restored it. Real endpoints with deliberately fake credentials (download only): AWS answered
  `InvalidAccessKeyId`, Google `invalid_client` — TLS with the system trust store and the
  request formats work. No real account was used.

## Consequences

- R-PLAT-003 is revised: networking only in `vibenote-backup`, and only after the owner enables
  it for that workspace.
- Restore needs the same config (the CLI reads it); a target can also be restored with
  `aws s3 sync` (S3) or by downloading the Drive folder and moving blobs into `<h2>/` folders.
- Not done (possible later on request): client-side encryption, compression, pruning of blobs
  no longer referenced, other providers.
