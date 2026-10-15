"""Training lines for fine-tuning the recogniser (M9.6, ADR 0006 step (b)). Permissive data only.

  en: GNHK **train** split (CC BY 4.0) — every handwritten, legible line of the train pages,
      cropped like `tools/evalgen/make_eval.py recognition_en` (the eval uses the test split).
  de: fhswf/german_handwriting (AFL-3.0), all parquet shards, **excluding** every line whose text
      or image equals one of the 400 lines sampled into $VIBENOTE_DATA/eval/de (the committed
      eval set = the first 150, the dev set = the rest). Note: fhswf has only 15 writers and no
      writer ids, so training lines share writers with the German eval lines.

Writes <out>/{en,de}/NNNNNN.png (greyscale, 48 px high) and <out>/{en,de}/labels.tsv.
Inputs are untrusted downloads: images are decoded with size limits, nothing is executed.

Usage: python -I prep_finetune.py <GNHK-dataset.zip> <fhswf parquet dir> <eval/de dir> <out>
"""
import glob, hashlib, io, json, os, re, sys, zipfile

from PIL import Image, ImageOps

Image.MAX_IMAGE_PIXELS = 120_000_000
H = 48
MAX_W = 3200


def save(img, d, k, rows, text):
    w = max(1, min(MAX_W, int(round(img.width * H / img.height))))
    img.resize((w, H), Image.LANCZOS).save(os.path.join(d, f"{k:06d}.png"), optimize=True)
    rows.append(f"{k:06d}\t{text}\n")


def gnhk_train(zpath, out):
    d = os.path.join(out, "en")
    os.makedirs(d, exist_ok=True)
    outer = zipfile.ZipFile(zpath)
    inner = zipfile.ZipFile(io.BytesIO(outer.read("train_data.zip")))
    names = sorted(inner.namelist())
    jsons = [n for n in names if n.endswith(".json")]
    rows, k = [], 0
    for jn in jsons:
        base = jn[:-5]
        jpg = next((n for n in (base + ".jpg", base + ".JPG", base + ".jpeg") if n in names), None)
        if jpg is None:
            continue
        words = json.loads(inner.read(jn))
        lines = {}
        for w in words:
            lines.setdefault(w["line_idx"], []).append(w)
        im = None
        for li in sorted(lines):
            ws = lines[li]
            if any(w["type"] != "H" or "%" in w["text"] or not w["text"].strip() for w in ws):
                continue
            if re.search(r"\d{5}", "".join(w["text"] for w in ws)):
                continue  # phone/account-like numbers: personal data
            xs = [w["polygon"][f"x{i}"] for w in ws for i in range(4)]
            ys = [w["polygon"][f"y{i}"] for w in ws for i in range(4)]
            x0, y0, x1, y1 = min(xs), min(ys), max(xs), max(ys)
            if im is None:
                im = ImageOps.exif_transpose(Image.open(io.BytesIO(inner.read(jpg)))).convert("L")
            pad = int(0.15 * (y1 - y0))
            c = (max(0, x0 - pad), max(0, y0 - pad), min(im.width, x1 + pad), min(im.height, y1 + pad))
            if c[2] - c[0] < 8 or c[3] - c[1] < 8:
                continue
            ws = sorted(ws, key=lambda w: min(w["polygon"][f"x{i}"] for i in range(4)))
            save(im.crop(c), d, k, rows, " ".join(w["text"] for w in ws))
            k += 1
    open(os.path.join(d, "labels.tsv"), "w", encoding="utf-8").writelines(rows)
    print("en train lines:", k, "from", len(jsons), "pages")


def fhswf(pdir, evaldir, out):
    import pyarrow.parquet as pq
    d = os.path.join(out, "de")
    os.makedirs(d, exist_ok=True)
    held = [l.rstrip("\n").split("\t", 1) for l in open(os.path.join(evaldir, "gt.tsv"), encoding="utf-8")]
    held_text = {t.strip() for _, t in held}
    held_px = set()
    for i, _ in held:
        p = os.path.join(evaldir, f"{i}.png")
        if os.path.exists(p):
            held_px.add(hashlib.sha256(Image.open(p).convert("L").tobytes()).hexdigest())
    rows, k, skipped = [], 0, 0
    for p in sorted(glob.glob(os.path.join(pdir, "*.parquet"))):
        for r in pq.read_table(p).to_pylist():
            text = (r.get("text") or "").strip()
            if not text:
                continue
            img = Image.open(io.BytesIO(r["image"]["bytes"])).convert("L")
            if text in held_text or hashlib.sha256(img.tobytes()).hexdigest() in held_px:
                skipped += 1
                continue
            save(img, d, k, rows, text)
            k += 1
    open(os.path.join(d, "labels.tsv"), "w", encoding="utf-8").writelines(rows)
    print("de train lines:", k, "excluded (eval/dev text or image):", skipped)


if __name__ == "__main__":
    z, pdir, evaldir, out = sys.argv[1:5]
    gnhk_train(z, out)
    fhswf(pdir, evaldir, out)
