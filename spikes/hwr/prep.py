"""Prepare HWR eval sets in /dev/shm/vibenote-hwr/eval/<set>/{NNNN.png, gt.tsv}.

Sets (licences: see docs/adr/0006-handwriting-recognition.md):
  brush  - 400 BRUSH online samples (pen strokes) rendered to images  [eval-only, owner 2026-10-11]
  iam    - 400 IAM-line test lines (Teklia/IAM-line)                    [eval-only]
  de     - 400 fhswf/german_handwriting lines (AFL-3.0)                  [permissive]
BRUSH pickles are untrusted: loaded with a restricted unpickler that only allows numpy array
reconstruction (no arbitrary globals).
"""
import glob, io, os, pickle, random, sys, zipfile
import numpy as np
from PIL import Image, ImageDraw

ROOT = "/dev/shm/vibenote-hwr"
OUT = f"{ROOT}/eval"
N = 400
random.seed(42)

class SafeUnpickler(pickle.Unpickler):
    ALLOWED = {("numpy.core.multiarray", "_reconstruct"), ("numpy._core.multiarray", "_reconstruct"),
               ("numpy", "ndarray"), ("numpy", "dtype"), ("numpy.core.multiarray", "scalar"),
               ("numpy._core.multiarray", "scalar"), ("builtins", "list"), ("builtins", "str")}
    def find_class(self, module, name):
        if (module, name) in self.ALLOWED:
            return super().find_class(module, name)
        raise pickle.UnpicklingError(f"blocked global {module}.{name}")

def render_strokes(drawing, height=96, pad=12, width_px=3.0):
    """drawing: (N,3) x, y, end-of-stroke. Render like the app's ink, scaled to a fixed height."""
    d = np.asarray(drawing, dtype=np.float64)
    xy = d[:, :2]
    mn, mx = xy.min(0), xy.max(0)
    span_y = max(mx[1] - mn[1], 1e-6)
    s = (height - 2 * pad) / span_y
    w = int((mx[0] - mn[0]) * s) + 2 * pad
    img = Image.new("L", (max(w, 32), height), 255)
    dr = ImageDraw.Draw(img)
    pts = (xy - mn) * s + pad
    start = 0
    for i in range(len(d)):
        if d[i, 2] > 0.5 or i == len(d) - 1:
            seg = [tuple(p) for p in pts[start:i + 1]]
            if len(seg) > 1:
                dr.line(seg, fill=0, width=int(round(width_px)), joint="curve")
            elif seg:
                x, y = seg[0]; dr.ellipse([x - 1, y - 1, x + 1, y + 1], fill=0)
            start = i + 1
    return img.convert("RGB")

def write_set(name, items):
    d = f"{OUT}/{name}"
    os.makedirs(d, exist_ok=True)
    with open(f"{d}/gt.tsv", "w") as gt:
        for i, (img, text) in enumerate(items):
            img.save(f"{d}/{i:04d}.png")
            gt.write(f"{i:04d}\t{text.strip()}\n")
    print(name, len(items))

def brush():
    z = zipfile.ZipFile(f"{ROOT}/brush/brush.zip")
    plain = [n for n in z.namelist() if n.count("/") == 2 and not n.endswith("/") and "_" not in n.rsplit("/", 1)[1] and "DS_Store" not in n]
    items = []
    for n in random.sample(plain, N):
        sentence, drawing, _label = SafeUnpickler(io.BytesIO(z.read(n))).load()
        items.append((render_strokes(drawing), str(sentence)))
    write_set("brush", items)

def parquet_set(name, pattern):
    import pyarrow.parquet as pq
    rows = []
    for p in sorted(glob.glob(pattern)):
        rows += pq.read_table(p).to_pylist()
    rows = random.sample(rows, N)
    items = [(Image.open(io.BytesIO(r["image"]["bytes"])).convert("RGB"), r["text"]) for r in rows]
    write_set(name, items)

if __name__ == "__main__":
    brush()
    parquet_set("iam", f"{ROOT}/hf/hub/datasets--Teklia--IAM-line/snapshots/*/data/test.parquet")
    parquet_set("de", f"{ROOT}/hf/hub/datasets--fhswf--german_handwriting/snapshots/*/data/train-0000[12]-*.parquet")
