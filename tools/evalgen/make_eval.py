"""Builds the committed, permissively licensed eval sets in tests/eval (dev tool, not shipped).

Sources (licences in ATTRIBUTION.md):
  - Quick, Draw! simplified ndjson (CC BY 4.0) — drawings
  - synthetic diagrams (own generator) — drawings
  - Hershey fonts via Hershey-Fonts (public domain + acknowledgement) — synthetic writing
  - fhswf/german_handwriting line images (AFL-3.0) — skeleton writing + German recognition lines
  - GNHK test split (GoodNotes, CC BY 4.0) — English recognition lines; unzipped test_data under
    $VIBENOTE_GNHK (default /dev/shm/vibenote-gnhk/ex/test)
Input data must be present under $VIBENOTE_DATA (default /dev/shm/vibenote-hwr; see
spikes/hwr/prep.py). Deterministic (fixed seeds).

Outputs:
  tests/eval/writing-drawing/blocks.jsonl   {"id","label":"writing"|"drawing","source","strokes":[[[x,y],...],...]}
  tests/eval/recognition/de/NNNN.png + gt.tsv   German lines (fhswf, AFL-3.0)
  tests/eval/recognition/en/NNNN.png + gt.tsv   English lines (GNHK, CC BY 4.0)
Usage: python -I make_eval.py [writing_drawing|recognition_de|recognition_en|blocks|packs ...] (default: all)
  tests/eval/blocks/blocks.jsonl + labels.json  synthetic page: ink fixture + block labels
  tests/eval/packs/xx/manifest.json             dummy language pack (R-HWR-007)
"""
import json, os, random, re, shutil, sys
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "../.."))
DATA = os.environ.get("VIBENOTE_DATA", "/dev/shm/vibenote-hwr")
OUT = os.path.join(ROOT, "tests/eval")
sys.path.insert(0, HERE)
import classify_ref as C  # reuses the spike's data readers (ROOT path patched below)

C.ROOT = DATA
random.seed(2026)

def poly(strokes):
    return [[[round(float(x), 2), round(float(y), 2)] for x, y in s] for s in strokes]

def writing_drawing():
    os.makedirs(os.path.join(OUT, "writing-drawing"), exist_ok=True)
    qd = C.quickdraw()
    rows = []
    for cat in sorted(qd):
        for k, s in enumerate(qd[cat][1200:1300]):  # held-out range of the spike
            rows.append({"id": f"qd-{cat}-{k}", "label": "drawing", "source": f"quickdraw:{cat}", "strokes": poly(s)})
    for k, s in enumerate(C.synth_drawings(4000)[3200:3600]):
        rows.append({"id": f"syn-{k}", "label": "drawing", "source": "synthetic", "strokes": poly(s)})
    for k, s in enumerate(C.hershey_lines(3000)[2500:2900]):
        rows.append({"id": f"hershey-{k}", "label": "writing", "source": "hershey", "strokes": poly(s)})
    for k, s in enumerate(C.fhswf_skeleton_lines(200)[160:200]):
        rows.append({"id": f"fhswf-{k}", "label": "writing", "source": "fhswf-skeleton", "strokes": poly(s)})
    with open(os.path.join(OUT, "writing-drawing/blocks.jsonl"), "w") as f:
        for r in rows:
            f.write(json.dumps(r, separators=(",", ":")) + "\n")
    print("writing-drawing:", sum(r["label"] == "drawing" for r in rows), "drawings,", sum(r["label"] == "writing" for r in rows), "writing")

def recognition_de(n=150):
    src = os.path.join(DATA, "eval/de")
    dst = os.path.join(OUT, "recognition/de")
    os.makedirs(dst, exist_ok=True)
    rows = [l.rstrip("\n").split("\t", 1) for l in open(os.path.join(src, "gt.tsv"))][:n]
    from PIL import Image
    with open(os.path.join(dst, "gt.tsv"), "w") as g:
        for i, t in rows:
            im = Image.open(os.path.join(src, f"{i}.png")).convert("L")
            h = 64
            im = im.resize((max(1, int(im.width * h / im.height)), h))
            im.save(os.path.join(dst, f"{i}.png"), optimize=True)
            g.write(f"{i}\t{t}\n")
    print("recognition/de:", len(rows), "lines")

GNHK = os.environ.get("VIBENOTE_GNHK", "/dev/shm/vibenote-gnhk/ex/test")

def recognition_en(per_image=2, n=300):
    """English handwritten lines cropped from GNHK test pages (camera photos, word polygons with line_idx).
    A line is used only if every word is handwritten ("H") and legible (no %NA%/%SC%/%math% tokens),
    it has >= 2 words, and its box does not overlap another line's words. At most `per_image` lines per
    page (writer diversity), pages in sorted order."""
    from PIL import Image, ImageOps
    dst = os.path.join(OUT, "recognition/en")
    os.makedirs(dst, exist_ok=True)
    def box(ws):
        xs = [w["polygon"][f"x{i}"] for w in ws for i in range(4)]
        ys = [w["polygon"][f"y{i}"] for w in ws for i in range(4)]
        return min(xs), min(ys), max(xs), max(ys)
    def overlap(a, b):
        w = min(a[2], b[2]) - max(a[0], b[0]); h = min(a[3], b[3]) - max(a[1], b[1])
        return max(0, w) * max(0, h)
    out = []
    for jf in sorted(f for f in os.listdir(GNHK) if f.endswith(".json")):
        words = json.load(open(os.path.join(GNHK, jf)))
        lines = {}
        for w in words:
            lines.setdefault(w["line_idx"], []).append(w)
        took = 0
        for li in sorted(lines):
            ws = lines[li]
            if took >= per_image or len(ws) < 2:
                continue
            if any(w["type"] != "H" or "%" in w["text"] or not w["text"].strip() for w in ws):
                continue
            if re.search(r"\d{5}", "".join(w["text"] for w in ws)):
                continue  # phone/account-like numbers: personal data, not useful for search

            b = box(ws)
            others = [box([w]) for w in words if w["line_idx"] != li]
            if any(overlap(b, o) > 0.1 * (o[2] - o[0]) * (o[3] - o[1]) for o in others):
                continue
            ws = sorted(ws, key=lambda w: min(w["polygon"][f"x{i}"] for i in range(4)))
            out.append((jf[:-5], b, " ".join(w["text"] for w in ws)))
            took += 1
    for f in os.listdir(dst):
        os.remove(os.path.join(dst, f))
    k = 0
    with open(os.path.join(dst, "gt.tsv"), "w") as g:
        for img, (x0, y0, x1, y1), text in out:
            if k >= n:
                break
            # GNHK polygons are in display orientation: apply the EXIF rotation first.
            im = ImageOps.exif_transpose(Image.open(os.path.join(GNHK, img + ".jpg"))).convert("L")
            pad = int(0.15 * (y1 - y0))
            c = (max(0, x0 - pad), max(0, y0 - pad), min(im.width, x1 + pad), min(im.height, y1 + pad))
            if c[2] - c[0] < 8 or c[3] - c[1] < 8:
                continue  # annotation outside the image
            im = im.crop(c)
            h = 64
            im = im.resize((max(1, int(im.width * h / im.height)), h), Image.LANCZOS)
            i = f"{k:04d}"
            im.save(os.path.join(dst, f"{i}.png"), optimize=True)
            g.write(f"{i}\t{text}\n")
            k += 1
    print("recognition/en:", k, "lines")

def blocks():
    """Synthetic page: writing lines and drawings at known places, with idle gaps, as an ink
    fixture (surface 1600x1200) + labels (stroke index ranges per block)."""
    dst = os.path.join(OUT, "blocks")
    os.makedirs(dst, exist_ok=True)
    hw = C.hershey_lines(40)
    dr = C.synth_drawings(40)
    events, labels, t, sidx = [], [], 0, 0
    header = {"format": "vibenote-ink-fixture", "version": 1, "source": "evalgen-blocks", "note": "synthetic page for block segmentation", "surface": {"width": 1600, "height": 1200, "scale": 1}}
    y = 60.0
    for b in range(12):
        is_w = b % 3 != 2
        strokes = hw[b] if is_w else dr[b]
        P = np.vstack(strokes); mn, mx = P.min(0), P.max(0)
        scale = (40.0 / max(mx[1] - mn[1], 1e-6)) if is_w else (160.0 / max(mx[1] - mn[1], mx[0] - mn[0], 1e-6))
        x0 = 80.0 if b % 2 == 0 else 820.0
        first = sidx
        for s in strokes:
            pts = [((p[0] - mn[0]) * scale + x0, (p[1] - mn[1]) * scale + y) for p in s]
            if len(pts) < 2:
                continue
            events.append({"t_us": t, "kind": "proximity_in", "tool": "pen", "x": pts[0][0], "y": pts[0][1], "p": 0, "tx": 0, "ty": 0, "d": 0.5})
            for j, (px, py) in enumerate(pts):
                events.append({"t_us": t, "kind": "down" if j == 0 else "motion", "tool": "pen", "x": round(px, 3), "y": round(py, 3), "p": 0.5, "tx": 0, "ty": 0, "d": 0})
                t += 5000
            events.append({"t_us": t, "kind": "up", "tool": "pen", "x": round(pts[-1][0], 3), "y": round(pts[-1][1], 3), "p": 0, "tx": 0, "ty": 0, "d": 0})
            events.append({"t_us": t, "kind": "proximity_out", "tool": "pen", "x": round(pts[-1][0], 3), "y": round(pts[-1][1], 3), "p": 0, "tx": 0, "ty": 0, "d": 1})
            t += 120_000  # intra-block pause 120 ms
            sidx += 1
        labels.append({"label": "writing" if is_w else "drawing", "strokes": [first, sidx - 1]})
        t += 2_500_000  # idle gap between blocks (> 1.5 s threshold)
        if b % 2 == 1:
            y += 200.0
    with open(os.path.join(dst, "blocks.jsonl"), "w") as f:
        f.write(json.dumps(header) + "\n")
        for e in events:
            f.write(json.dumps(e, separators=(",", ":")) + "\n")
    json.dump({"idle_ms": 1500, "blocks": labels}, open(os.path.join(dst, "labels.json"), "w"), indent=1)
    print("blocks:", len(labels), "blocks,", sidx, "strokes")

def packs():
    d = os.path.join(OUT, "packs/xx")
    os.makedirs(d, exist_ok=True)
    json.dump({"format": "vibenote-language-pack", "version": 1, "lang": "xx", "name": "Test pack (no model)", "model": None}, open(os.path.join(d, "manifest.json"), "w"), indent=1)

if __name__ == "__main__":
    steps = sys.argv[1:] or ["writing_drawing", "recognition_de", "recognition_en", "blocks", "packs"]
    for st in steps:
        globals()[st]()
