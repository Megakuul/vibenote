# vibenote — Specification

Status: **Draft for Gate A** (2026-10-11). Items marked **[proposal]** are the agent's proposals
and need the owner's confirmation; everything else comes from the owner's vision (AGENTS.md §1)
or the owner's answers on 2026-10-11 (recorded in `plan/STATUS.md`).

vibenote is a fast, paper-like handwriting workspace for the Linux/Wayland desktop. It is a work
tool, not a document publisher. Locked stack decisions: `docs/adr/` (ADR 0001: direct Wayland
client, own tablet-v2 binding, wgpu renderer).

Conventions
- **No requirement depends on a physical pen** (owner 2026-10-12). Pen behaviour is implemented best effort and verified with simtab **[sim]**; real-pen issues are handled after implementation if the owner reports them.
- Requirement ids `R-<AREA>-NNN`. Each has acceptance criteria (AC) that must be testable by the
  headless harness (Phase 3) unless marked **(manual)** or **(hw)**.
- "Reference machine" = owner's machine (`docs/environment.md`): AMD Radeon 860M, 180 Hz panel,
  Hyprland. "Reference workload" is defined in R-PERF-000.
- Measurements carry **[sim]** or **[hw]** (AGENTS.md §2).

---

## 1. Platform (PLAT)

**R-PLAT-001** Linux + Wayland is the only supported and tested platform.
- AC: CI and all benchmarks run on Linux/Wayland only; no code path, abstraction or dependency
  exists solely for another platform (`cargo xtask layers` + review).

**R-PLAT-002** The app runs as a native Wayland client using the stack in ADR 0001.
- AC: Given a Wayland session without Xwayland, when the app starts, then it opens a window and
  accepts pen input (integration test under a headless compositor or the owner's session).

**R-PLAT-003** Desktop only; no sync, web or mobile. (owner 2026-10-18: one-way cloud backup,
§13a, is the one exception.) No network access at runtime unless the owner enabled the backup of
the open workspace; networking code lives only in the backup crate.
- AC: networking crates appear in the dependency tree only below `vibenote-backup`; with backup
  disabled (the default) the backup worker is not started.

---

## 2. Pen input & tools (PEN)

**R-PEN-001 Latency** Ink appears with minimal latency (budgets in R-PERF-001).
- AC: see R-PERF-001.

**R-PEN-002 Pressure** Stroke width follows pen pressure.
- AC: Given a replayed fixture with a pressure ramp 0→1→0, when rendered, then the stroke width
  at each sample equals `width_curve(pressure)` within ±0.25 px (golden + numeric check), and
  width is strictly monotonic with pressure on the ramp.

**R-PEN-003 Tilt** Tilt affects ink for tools that define a tilt response (pen: none by default
[proposal]; highlighter and pencil-like "brush" tools: width/shape follows tilt).
- AC: Given a tilt-sweep fixture with a tilt-responsive tool, rendered width varies with tilt per
  the tool's curve within ±0.25 px; with the default pen, tilt has no effect.

**R-PEN-004 Input fidelity** Every pen sample delivered by the compositor is kept: position,
pressure, tilt, timestamp. No sample is dropped by the app at event rates up to 1,000 Hz.
- AC: replay a 1 kHz fixture through the engine → stored stroke has exactly the fixture's samples
  (count and values). End-to-end via simtab: app sample count = compositor-delivered count.

**R-PEN-005 Pen tools** Tools: pen, highlighter (translucent, drawn below ink of the same layer
[proposal]), stroke eraser, area (pixel) eraser, lasso selection, shapes (R-SHAPE), ruler (R-RULER),
cover (R-PEN-014).
- AC: each tool is a registered command `tool.select.<name>`; switching is tested per tool.

**R-PEN-013 Colours** (owner 2026-10-15) Pen and highlighter each have their own colour, chosen
from a palette on the toolbar (8 pen colours, 6 highlighter colours) or a custom colour picker;
the choice persists with the workspace settings and applies to new strokes (selected strokes are
recoloured with `selection.recolor`).
- AC: settings round-trip; a stroke drawn after choosing a colour has it (engine test).

**R-PEN-014 Cover (flashcards)** (owner 2026-10-15) A cover tool (key M) draws wide, opaque strokes
that hide whatever is under them (ink, text, PDFs, images). Tapping a cover (pen, mouse, any
drawing tool) reveals what is underneath — the cover turns faint — and tapping it again hides it.
Reveals are not saved: every cover is closed again when the note is opened. Covers are strokes
otherwise (lasso, eraser, undo); they are never recognised as handwriting.
- AC: a cover stroke is stored with its tool; a tap on it toggles the reveal without adding ink;
  reopening the note closes all covers (engine tests).

**R-PEN-006 Eraser end** The pen's eraser end acts as the eraser tool (stroke eraser by default,
configurable to area eraser) without changing the selected tool.
- AC: eraser-flip fixture → strokes under the eraser path are removed (stroke mode) or cut (area
  mode); pen tool still selected afterwards.

**R-PEN-007 Stroke eraser** Touching any part of a stroke removes the whole stroke.
- AC: Given strokes A, B where the eraser path intersects only A, then A is removed and B is
  unchanged; undo restores A exactly.

**R-PEN-008 Area eraser** Erases only the area under the eraser; strokes are split into remaining
pieces (vector, not pixels); pieces keep pressure/tilt samples.
- AC: Given a straight stroke and an eraser path crossing its middle, result is two strokes whose
  union equals the original minus the eraser footprint (geometric test, tolerance 0.5 px).

**R-PEN-009 Lasso** Lasso selects objects (strokes, images, PDFs, text, links, audio) whose
geometry is mostly inside the lasso [proposal: ≥ 50 % of stroke length / object area]; selection
can be moved, scaled, rotated, recoloured, copied, cut, deleted.
- AC: per operation, a command test; property test: transform then inverse transform restores
  geometry within 1e-6.

**R-PEN-010 Barrel buttons** [proposal] Button 1 = temporary lasso while held; button 2 = temporary
eraser while held. Configurable.
- AC: button fixture → mode active only while button held.

**R-PEN-011 Hover** While the pen hovers, a cursor shows the tool's footprint at the pen position.
- AC: hover fixture → state.inspect reports cursor position = last hover sample.

**R-PEN-012 Palm/mouse** Mouse can draw (fixed pressure 0.5) and pan; touch input is out of scope
for v1 [proposal].
- AC: mouse drag with pen tool selected → stroke with constant width_curve(0.5).

---

## 3. Canvas (CANVAS)

**R-CANVAS-001 Infinite canvas** Each note is an unbounded 2D canvas.
- AC: strokes at coordinates ±1e7 canvas units render and hit-test correctly (precision test: no
  visible jitter > 0.1 px at max zoom at those coordinates).

**R-CANVAS-002 Pan & zoom** Pan (two-finger touchpad, middle-mouse drag, right-mouse drag (owner 2026-10-18: "like swiping
in touch"), space+drag, pen barrel
[proposal]) and zoom (pinch, Ctrl+scroll) from 1 % to 6,400 % [proposal].
- AC: zoom range enforced; zoom is anchored at the cursor/pinch centre (point under cursor stays
  within 0.5 px).

**R-CANVAS-003 Object types** Canvas objects: stroke, PDF, image (PNG, JPEG, WebP, SVG
[proposal]), text box, link, audio clip, shape.
- AC: each type can be created, moved, scaled, deleted, undone; each has a save/load round-trip test.

**R-CANVAS-004 Draw on anything** The user can draw on top of and next to any object; ink drawn
over an object stays above it and is not attached to it [proposal: optionally "pin ink to object"
so it moves with it — default on for PDFs].
- AC: Given a PDF object, when a stroke is drawn over it, then the stroke renders above the PDF and
  hit-tests to the stroke first.

**R-CANVAS-005 Z-order** Objects have a z-order; bring forward/back commands.
- AC: command tests; render golden shows order.

**R-CANVAS-006 Background** Note background: blank, lined, grid, dots (spacing configurable);
background is infinite and not an object.
- AC: goldens per background type.

---

## 4. PDFs (PDF)

**R-PDF-001 PDF as object** Importing a PDF creates one canvas object; pages are laid out as a
**vertical column** with a fixed gap (owner decision). The object moves and scales as a unit.
- AC: importing a 3-page PDF → one object; page k's top = page k-1's bottom + gap.

**R-PDF-002 Split out** A page can be split out into its own object ("detach page") and rejoined.
- AC: detach page 2 of 3 → two objects (pages 1,3 column; page 2) ; undo restores.

**R-PDF-003 Large PDFs** PDFs with up to 2,000 pages [proposal] open without loading all pages:
pages render lazily at the needed resolution.
- AC: see R-PERF-006; memory stays within budget while scrolling a 2,000-page PDF.

**R-PDF-004 Text search** Text inside PDFs (text layer) is searchable [proposal].
- AC: search for a word on page 50 of a fixture PDF returns that page's region.

**R-PDF-005 No export** There is no PDF export (owner). Original PDF bytes are kept unmodified.
- AC: stored PDF blob hash = imported file hash.

---

## 5. Text boxes (TEXT)

**R-TEXT-001** Text boxes with basic rich text (owner decision): bold, italic, underline,
strikethrough, three heading sizes, bullet and numbered lists, checkboxes, text colour, font size.
- AC: each formatting command has a test; save/load preserves formatting exactly.

**R-TEXT-002** Text input supports IME (text-input-v3) and German/English keyboard layouts.
- AC: typing "ÄÖÜäöüß" via xkb (ch/de layouts) yields the same text (integration test).

**R-TEXT-003** Text boxes can be drawn over and next to; typed text is searchable (R-SEARCH).

**R-TEXT-004 Auto-size** (owner 2026-10-15) A text box's frame (outline, selection box, hit
area) always fits its laid-out text: its height is all lines at their real sizes (headings, font
size), its width the longest line up to the box's wrap width. Typing more or making the font
larger grows the frame at once; deleting shrinks it. New boxes wrap at 600 screen px at the zoom
they were created at; the width bar sets the wrap width.
- AC: a new box grows in width while one line is typed and wraps beyond the wrap width; setting a
  larger size or a heading grows the height; the frame equals the renderer's layout extent (±1 px).

**R-TEXT-005 Clicks in text** (owner 2026-10-15, revised the same day: "it should be literally
the normal field behaviour") Text boxes behave like ordinary text fields. With the text tool (and
a mouse click with the pen or highlighter): a click on a box types into it with the caret exactly
where it was clicked; a drag selects from the press to the pointer; a double click (two presses
within 400 ms and 8 px) selects the word, a triple click the line; Shift+click extends the
selection. A click outside ends editing. The box being edited is moved by dragging its frame
(the band just outside the text) and resized by its width bar; the lasso moves and scales boxes
as before. With vim mode on, a click puts the Normal-mode cursor on the clicked character, a drag
or double click selects in Visual mode. Shortcuts for every tool are shown on the toolbar buttons.
- AC: engine tests of click, drag, double and triple click, and frame drag.

**R-TEXT-006 Text colour** (owner 2026-10-15) The text format bar has a colour palette button
(the pen colours, plus a custom picker) that colours the selection, or the whole box when nothing
is selected (like the other formats).
- AC: `text.format` colour over a range is stored and rendered (existing tests); UI test of the
  palette.

**R-TEXT-007 Code blocks** (owner 2026-10-15) In a text box, lines between a fence line of three
backticks with an optional language name (```` ```rust ````) and a closing fence are a code block:
shown in the monospace font on a tinted background, with syntax highlighting (keywords, strings,
comments, numbers, types, function names) for rust, python, javascript/typescript, c/c++, go,
java, bash/sh, lua, sql, json, toml, yaml, nix, html/xml and css; other or missing languages get
generic highlighting. Display only: the stored text is the plain text with its fences. Tab
indents (4 spaces), Shift+Tab dedents, in any text box.
- AC: highlighting spans for a sample per language (unit tests); fenced lines render mono.

---

## 5a. Vim mode and finder (VIM) — owner 2026-10-15

**R-VIM-001 Vim mode** A global setting (Settings and a toolbar toggle, default off; persisted).
While it is on, every text box is shown in a monospace font (JetBrains Mono), left-aligned —
display only, the stored text and formatting do not change — and editing an existing box starts
in Normal mode (a new, empty box starts in Insert mode). The mode (NORMAL / INSERT / VISUAL / VISUAL LINE) is shown while editing; Normal mode shows
a block cursor. Like an editor, a box wraps at no fewer than 100 columns, its frame spans that
width with padding around the text, and selections are drawn as whole rows (Visual Line rows
span the box; rows a selection continues past reach its right edge; an empty line is one cell).
- AC: setting round-trips; with vim on, text items are laid out mono (render test), wrap at
  ≥ 100 columns and have a padded frame; editing starts in Normal mode.

**R-VIM-002 Motions and operators** In Normal mode the keys are vim's (counts allowed):
`h j k l`, `w b e W B E`, `0 ^ $`, `gg G`, `f F t T` + char, `;`; `i a I A o O` enter Insert;
`x X s S r` + char, `~`, `J`, `D C`, `dd cc yy`, `d c y` + motion or text object (`iw aw ip`),
`p P`, `u` undo, Ctrl+R redo; `v` / `V` Visual (motions extend; `d c y x > ~ J` act on it).
Unnamed register only. One Insert session is one undo step (a change and the typing after it
too); every Normal command is one step; `u` never undoes past the start of editing the box.
Behaviour follows Neovim's defaults with `autoindent smarttab shiftwidth=4 softtabstop=4
expandtab` (owner 2026-10-15: "ensure this works properly like normal vim"): failed motions
abort operators, 'nostartofline' (linewise commands keep the column), `$` keeps to line ends,
`cw` special cases, Enter/`o`/`O`/`cc` keep the indent (an untouched auto indent is removed on
Esc), Tab inserts spaces to the next stop, Backspace over spaces goes back to the previous stop,
Shift+Tab dedents.
- AC: a table of key sequences with expected text and cursor (engine tests), and a table of
  random sequences whose results were produced by Neovim (differential test).

**R-VIM-003 jk** In Insert mode, `j` followed by `k` within 300 ms leaves Insert like Esc (the `j`
is removed); a `j` alone stays text. Esc in Insert → Normal; Esc in Normal stops editing.
- AC: "abjk" typed fast → text "ab", Normal mode; "j", pause, "k" → "jk" stays.

**R-VIM-004 Leader** With vim mode on, Space is the leader key in Normal mode and on the canvas
(when no text is being typed): Space, then `f`, then `w` (each within 1 s of the previous key,
pressed one after the other) opens the text finder; Space `f` `f` opens the note finder. Holding
Space and dragging still pans. Any other key cancels the sequence. In Insert mode (owner
2026-10-15: "should ALWAYS work even if I'm in a field typing") the same keys typed quickly
(each within 400 ms, like `jk`) also open the finder and the typed " f…" is taken back; typed
slowly they stay text.
- AC: engine test: the sequence opens the finder; a slow sequence (> 1 s) does not; in Insert
  mode a fast " fw" opens the finder and leaves the text unchanged, a slow one is text.

**R-VIM-006 Typing into a selected box** (owner 2026-10-15) With vim mode on, when a single text
box is selected (lasso, or the box under a click), typing a key starts editing it in Normal mode
and the key acts as a vim command (`i`, `A`, `o`, `dd`, …); Esc deselects.
- AC: engine/UI test: selected box + `A` `x` → "…x" in Insert mode.

**R-VIM-005 Finder** A telescope-like overlay in the centre: a prompt (focused), a ranked result
list and a preview of the selected result. Typing filters with fuzzy matching (characters in
order; word starts and runs rank higher). Up/Down, Ctrl+N/Ctrl+P, Ctrl+J/Ctrl+K move; Enter opens;
Esc closes. Never needs the mouse.
- Text finder (Space f w): lines of typed text (text boxes) and recognised handwriting in all
  notes; a result shows the line, the note and its folder path. Enter opens the note centred on
  the block; a text box is put into editing (Normal mode) with the cursor at the match.
- Note finder (Space f f, also Ctrl+P): notes by name and folder path, recent first when empty.
  Enter opens the note.
- AC: `finder.query` ranks fuzzy matches (tests); `finder.open` opens the note, centres the block
  and starts editing a text box at the match; the overlay is keyboard-only (UI test).

---

## 6. Links (LINK)

**R-LINK-001** Link objects to: another note or folder; a position (+zoom) inside a note; a web
URL (owner decision). Local file links are out of scope.
- AC: activating a note link opens that note; position link opens it at the stored viewport
  (±1 px, zoom exact); URL link calls the desktop opener (`xdg-open`) — tested with a stub.

**R-LINK-002** Links to deleted targets show as broken and do nothing harmful.
- AC: delete target → link reports `broken`, activation returns an error, no panic.

**R-LINK-003** Moving/renaming a target note or folder keeps links working (links use stable ids).

---

## 7. Audio (AUDIO)

**R-AUDIO-001** Record audio via PipeWire into a note, and import audio files (owner decision)
[proposal: Opus/Ogg storage; import WAV, FLAC, MP3, Ogg, Opus].
- AC: record 10 s from a test source → clip of 10 s ±50 ms stored; imported files play.

**R-AUDIO-002 Sync** Ink written while recording is time-linked to the recording; selecting a
stroke (or tapping it in "audio mode") plays from the moment it was written.
- AC: Given a recording started at t0 and a stroke written at t0+5 s, when the stroke is tapped in
  audio mode, then playback starts at 5 s ±100 ms (minus a configurable lead-in, default 2 s
  [proposal]).

**R-AUDIO-003** Recording never blocks inking (latency budget R-PERF-001 holds while recording).

**R-AUDIO-004 Players** (owner 2026-10-15) Audio objects (recordings and imported or dropped
music/MP3 files) are players on the canvas: a tap (pen or mouse, any drawing tool) on the play
button plays and a tap while playing pauses; a tap on the progress bar plays from there. While
playing the player shows the position and progress. The toolbar's record button shows the
elapsed time while recording; a recording or playback error is shown to the user, not only logged.
- AC: engine tests: a tap on a player starts playback (status), a second tap stops it, a tap on
  the bar seeks; UI test: an error from `audio.record.start` is shown.

---

## 8. Shapes & ruler (SHAPE, RULER)

**R-SHAPE-001** Shape tool: line, arrow, rectangle, ellipse, polygon (owner decision: tool +
snapping).
- AC: command tests; shapes are vector objects with stroke width/colour.

**R-SHAPE-002 Snap from ink** Drawing a rough shape and holding the pen still for 500 ms
[proposal] converts it to the clean shape (line, arrow, rectangle, ellipse, triangle). Undo
returns to the original ink. A line is the stroke's best-fit line (hooks at its ends and a
slight bow allowed), levelled exactly within 6° of horizontal or vertical; a nearly round
ellipse becomes a circle; corner overshoot does not grow a rectangle.
- AC: fixture set of rough shapes → correct class for ≥ 95 % [proposal]; strokes that are not
  held are never converted.

**R-RULER-001** (owner 2026-10-15: "a virtual ruler that also allows doing straight lines") A ruler
can be shown/hidden (toolbar button, key R). It is drawn as a translucent band with tick marks
and its angle; dragging its body (pen, mouse) moves it, dragging the round handle at its end or
the wheel over it rotates it (Shift: 15° steps; Alt+Left / Alt+Right turn by 15°). Pen
and mouse strokes starting within 12 px of its drawing edge are laid exactly along the edge —
already while drawing. (owner 2026-10-17: "on top of the ruler the pen should block and not write
below") The ruler blocks ink like a real one: any other pen or mouse stroke that runs into it
stops at the edge it came from — samples on the body or behind it are laid on that edge, already
while drawing; going around an end of the ruler reaches the other side.
- AC: fixture: stroke drawn near ruler edge → resulting stroke is collinear with the edge
  (max deviation 0.5 px).
- AC: a stroke from above crossing the ruler has no sample on or below the body; one passing
  beside its end is unchanged.

---

## 9. Organisation (ORG)

**R-ORG-001** Workspace contains folders, nestable to arbitrary depth (owner).
- AC: create depth 1,000 hierarchy; operations work; no recursion limits hit (iterative code).

**R-ORG-002** Folders and notes can be named, coloured, reordered (manual order), moved
(drag and drop + commands).
- AC: command tests; order persisted.

**R-ORG-003** Fast organisation of a lot of material [proposal]: multi-select move, keyboard
navigation in the tree, quick-open (fuzzy by name, Ctrl+P), recent notes.
- AC: quick-open over 100k notes returns within R-PERF-008.

**R-ORG-004** Deleting goes to a trash with restore; permanent delete only from trash [proposal].

---

## 10. Handwriting intelligence (HWR)

**R-HWR-001 Blocks** A writing block is a spatially and temporally coherent group of strokes. A
block is "finished" when the pen has not touched down within the block's region for the idle
threshold (default **1.5 s**, owner decision; configurable 0.5–10 s).
- AC: fixture with pauses → block segmentation matches labels (≥ 95 % [proposal]).

**R-HWR-002 Writing vs drawing** Each finished block is classified writing / drawing / uncertain.
Drawings, diagrams, arrows, sketches are **never** modified. Uncertain → nothing happens.
- AC: on the eval set, **drawing false positives = 0** (hard gate). Writing recall ≥ 60 % at
  that threshold on the public/synthetic set [proposal]; tuned on owner data later.

**R-HWR-003 Recognition** Writing blocks are recognised offline (German, English) to searchable
text in the background.
- AC: character error rate on the public eval sets ≤ 10 % (English), ≤ 12 % (German)
  [proposal, revisited after Phase 2 benchmark].

**R-HWR-004 Prettify** Writing blocks can be prettified so the result stays the owner's
handwriting, just more legible (baseline, slant, size, spacing, smoothing).
- AC (automated bounds): each glyph group's displacement ≤ 0.35 × x-height, no stroke reordered,
  stroke count unchanged, recognised text of prettified ink equals that of the original.
- AC (manual): owner approves the prettify gallery.

**R-HWR-005 Safety & reversibility** Originals are always kept; prettify is undoable and
revertible per block at any later time.
- AC: prettify → revert → strokes byte-identical to original.

**R-HWR-006 Control** Prettify mode is a setting: off / suggest / automatic, default **suggest**
(safety default; no calibration step is required, owner 2026-10-12). Recognition for search may
run on everything.
- AC: each mode is a command; switching modes never modifies existing ink.

**R-HWR-007 Languages** Language/model packs are pluggable; adding a language needs no code change
outside the pack.
- AC: test pack "xx" loaded from a directory is listed and used.

**R-HWR-008 Background work** Recognition/prettify never block inking; work is cancellable and
restarts when a block changes.

---

## 11. Search (SEARCH)

**R-SEARCH-001** Search across recognised handwriting, typed text, PDF text, note and folder
names.
- AC: fixture workspace → each source type found.

**R-SEARCH-002 UI** [proposal] Global search (Ctrl+Shift+F) lists results grouped by note with
snippet/thumbnail; activating a result opens the note and highlights the match region. In-note
find (Ctrl+F) highlights matches on the canvas.

**R-SEARCH-003** Search is accent- and case-insensitive by default; German umlaut equivalence
(ä=ae…) [proposal].

---

## 12. Undo & editing (UNDO)

**R-UNDO-001** Per-note undo/redo history, persisted across restarts (owner decision), bounded to
the last 1,000 steps per note [proposal].
- AC: property test: any command sequence followed by full undo restores the exact state; restart
  then undo still works.

**R-UNDO-002** Clipboard: copy/paste objects within and between notes; paste images and text from
other apps; copy selection as PNG to other apps [proposal].

---

## 13. Data & storage (DATA)

**R-DATA-001 Crash safety** No data loss on crash or power loss for any operation acknowledged to
the user; ink is durable within 1 s of pen-up [proposal].
- AC: kill -9 at random points during a fixture replay (1,000 iterations) → store opens, is
  consistent, and contains all strokes completed ≥ 1 s before the kill.

**R-DATA-002 Scale** Works without workarounds for: 100k notes, 1M strokes per canvas, PDFs up to
2,000 pages / 1 GB, audio up to 10 h per note [proposal].
- AC: R-PERF budgets hold on the reference workload.

**R-DATA-003 Format** Storage format chosen in Phase 2 (ADR); round-trip save→load is identity
(property test). The format is versioned with forward migration.

**R-DATA-004** Hostile files never crash the app (fuzzed PDF/image/audio/workspace inputs).

---

## 13a. Cloud backup (BACKUP) — owner 2026-10-18

Owner: "Implement sync / backup to the cloud. Should be capable of doing S3 and Google Drive. If I
enable it it should just backup the file." One-way backup only: nothing is ever downloaded into
the open workspace, there are no accounts and no merge. Design: ADR 0012.

**R-BACKUP-001 Targets and enabling** Backup is off by default and enabled per workspace in
Settings → Backup, to one target:
- **S3**: AWS or any S3-compatible service (endpoint URL); bucket, region, key prefix; an access
  key pair, or a profile from `~/.aws/credentials`.
- **Google Drive**: "Connect" signs in through the browser (OAuth for desktop apps, scope
  `drive.file`: the app sees only files it created) with the owner's own OAuth client id.

Configuration and secrets are stored outside the workspace (`$XDG_CONFIG_HOME/vibenote/backup.json`,
mode 0600, keyed by the workspace path), so they are never uploaded and other workspaces (tests,
scratch copies) are never backed up by accident.
- AC: settings round-trip through `backup.configure` / `backup.status`; secrets are never shown in
  `backup.status`; the file has mode 0600; another workspace path sees backup disabled.

**R-BACKUP-002 What and when** A backup is a consistent snapshot of the workspace database
(`VACUUM INTO`, verified with `quick_check` before upload; a damaged snapshot is never uploaded)
plus every blob (PDFs, images, audio) not uploaded to that target before. It runs on a background
thread with its own database connection, never on the input or render path, when the workspace
changed since the last successful backup and that backup is at least the interval (default 10 min)
old — so also right after start when there were changes — and at once on "Back up now". Remote
layout under `<prefix>/<workspace name>/`: `workspace.sqlite` (latest), `daily/<weekday>.sqlite`
(the first backup of each day; 7 days of history) and the blobs as `<db file>.blobs/<h2>/<hash>`
(mirrors the local layout).
- AC: against a fake S3 server: the uploaded database opens as the same workspace; a second run
  without changes uploads nothing; after a change only the database is uploaded again (blobs
  are not); a corrupt snapshot fails with an error and uploads nothing.

**R-BACKUP-003 Status and errors** Settings show the target, the last successful backup time,
whether one is running, and the last error; a failed backup is shown as a notice and retried at
the next interval. Network errors never block, slow or crash the app.
- AC: a target that refuses the upload → status has the error and no success time; the next
  scheduled attempt runs again.

**R-BACKUP-004 Restore** `vibenote-cli backup restore <dir> [--daily <weekday>]` downloads the
latest (or that day's) database and its blobs from the configured target into `<dir>`, which then
opens as the backed-up workspace.
- AC: round trip through a fake S3 server and a fake Google Drive server: the restored workspace
  has the same notes and objects, and its PDFs/images open.

---

## 14. Commands & control (CMD)

**R-CMD-001** Every user-visible action is a registered command (id, params schema, enabled, run);
UI, shortcuts, CLI and control channel dispatch by id. Programmatic calls never open dialogs.

**R-CMD-002 Shortcuts** [proposal] P pen, H highlighter, E eraser (again: toggle stroke/area),
L lasso, S shapes, R ruler, T text, Ctrl+Z / Ctrl+Shift+Z undo/redo, Ctrl+C/X/V, Del delete,
Ctrl+P quick-open (the note finder, R-VIM-005), Ctrl+F find, Ctrl+Shift+F global search, Ctrl+0 zoom to fit, Ctrl+1 100 %,
Space+drag pan. All rebindable.
- AC: keymap test maps every shortcut to an existing command id.

---

## 15. Performance budgets (PERF) — all **[proposal]**, owner to confirm

**R-PERF-000 Reference workload:** workspace with 10k notes in 500 folders (depth ≤ 8); one note
with 100k strokes (avg 80 samples), 3 PDFs (total 600 pages), 50 images, 200 text boxes, 1 h audio.

| Id | Budget | Measured basis |
|---|---|---|
| R-PERF-001 | Input-to-ink software latency (simtab emit → presented frame) p50 ≤ 10 ms, p95 ≤ 13 ms on the reference machine **[sim]**, also while the reference note is open and while recognition runs. Real-pen [hw] measurement is optional and owner-initiated (no requirement depends on it). | Phase 0: 9.18 / 11.64 ms (empty canvas) |
| R-PERF-002 | Frame time while inking, panning and zooming the reference note: p99 ≤ 5.5 ms (180 Hz), no more than 1 missed frame per 1,000 | display refresh 5.555 ms |
| R-PERF-003 | Cold start to first interactive frame ≤ 400 ms (warm disk cache) | — |
| R-PERF-004 | Open the reference note: first viewport fully drawn ≤ 150 ms; full note interactive ≤ 500 ms | — |
| R-PERF-005 | Search over the reference workspace: first results ≤ 100 ms, complete ≤ 300 ms | — |
| R-PERF-006 | PDF page visible at screen resolution ≤ 100 ms after scrolling into view (cached: ≤ 1 frame); 2,000-page PDF import ≤ 2 s to first page | — |
| R-PERF-007 | Memory: reference note open ≤ 1.5 GB RSS; idle with no note open ≤ 200 MB | — |
| R-PERF-008 | Quick-open over 100k notes ≤ 50 ms per keystroke | — |
| R-PERF-009 | Pen-up to stroke durable ≤ 1 s; save never blocks inking | — |
| R-PERF-010 | Block finished → recognised text searchable ≤ 3 s (background) | — |

CI fails on regressions beyond tolerance (10 % [proposal]) against these budgets.

---

## 16. Handwriting quality targets — **[proposal]**

| Metric | Target (public + synthetic eval set; an owner set is optional and may be added later) |
|---|---|
| Drawing blocks modified (false positives) | **0** — hard gate, any > 0 fails CI |
| Writing recall at that threshold | ≥ 60 % (best effort; optional owner tuning may raise it later) |
| Recognition CER (en / de) | ≤ 10 % / ≤ 12 % |
| Recognition WER for search usefulness (en / de) | ≤ 25 % / ≤ 30 % |
| Prettify bounds | glyph displacement ≤ 0.35 × x-height; stroke count/order unchanged; recognised text unchanged |
| Prettify "style preserved" | owner approval of gallery (manual), ≥ 90 % of samples rated "still mine" |

---

## 17. Out of scope
See `docs/out-of-scope.md`.
