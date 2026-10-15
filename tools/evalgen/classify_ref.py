"""Writing-vs-drawing baseline (Phase 2 spike 6).

Block = list of strokes (polylines). Features are order-invariant geometric statistics, so
writing derived from permissive *image* data (fhswf lines → skeleton polylines) can train it.

Training data (shippable, permissive):
  writing: fhswf/german_handwriting lines (AFL-3.0) → skeletonised polylines
           Hershey script/roman single-stroke fonts (public domain w/ acknowledgement) with jitter
  drawing: Quick, Draw! (CC BY 4.0), 24 categories; synthetic arrows/boxes/underlines/crossing-out/
           connectors/plots/tables
Evaluation:
  writing (real pen strokes): BRUSH 400 samples [evaluation-only, owner decision 2026-10-11]
  drawing: held-out Quick, Draw! drawings + held-out synthetic diagrams
Threshold: chosen on the validation split so that drawing false positives = 0, then reported on
the test split (drawing FP must stay 0 → hard gate).
"""
import glob, io, json, math, os, random, sys, zipfile
import numpy as np

ROOT = "/dev/shm/vibenote-hwr"
rng = random.Random(1)

# ---------------- features ----------------
def resample(p, step):
    p = np.asarray(p, float)
    if len(p) < 2:
        return p
    d = np.r_[0, np.cumsum(np.hypot(*np.diff(p, axis=0).T))]
    if d[-1] < 1e-9:
        return p[:1]
    step = max(step, d[-1] / 2000.0)  # cap at 2,000 points per stroke
    t = np.arange(0, d[-1], step)
    return np.c_[np.interp(t, d, p[:, 0]), np.interp(t, d, p[:, 1])]

def features(strokes):
    strokes = [np.asarray(s, float) for s in strokes if len(s) >= 1]
    allp = np.vstack(strokes)
    mn, mx = allp.min(0), allp.max(0)
    w, h = max(mx[0] - mn[0], 1e-6), max(mx[1] - mn[1], 1e-6)
    # robust scale: median stroke height (writing ≈ x-height); normalise everything by it
    sh = np.array([max(s[:, 1].max() - s[:, 1].min(), 1e-6) for s in strokes])
    sw = np.array([max(s[:, 0].max() - s[:, 0].min(), 1e-6) for s in strokes])
    ext = np.maximum(sh, sw)
    ext = ext[ext > 1e-3]
    # floor the unit at 1/100 of the block size so degenerate (dot) strokes cannot blow up resampling
    unit = max(float(np.median(ext)) if len(ext) else 0.0, max(w, h) / 100.0, 1e-3)
    lens, straight, closed, turns, dirs = [], [], [], [], np.zeros(8)
    for s in strokes:
        r = resample(s, unit / 8)
        if len(r) < 3:
            lens.append(0.0); straight.append(1.0); closed.append(0.0); turns.append(0.0); continue
        seg = np.diff(r, axis=0)
        L = np.hypot(*seg.T).sum()
        lens.append(L / unit)
        straight.append(np.hypot(*(r[-1] - r[0])) / max(L, 1e-9))
        closed.append(1.0 if np.hypot(*(r[-1] - r[0])) < 0.15 * max(L, 1e-9) and L > unit else 0.0)
        ang = np.arctan2(seg[:, 1], seg[:, 0])
        da = np.abs((np.diff(ang) + np.pi) % (2 * np.pi) - np.pi)
        turns.append(da.sum() / (2 * np.pi) / max(L / unit, 1e-6))  # full turns per unit length
        hist, _ = np.histogram((ang + np.pi) % np.pi, bins=8, range=(0, np.pi), weights=np.hypot(*seg.T))
        dirs += hist
    dirs = dirs / max(dirs.sum(), 1e-9)
    lens, straight, closed, turns = map(np.array, (lens, straight, closed, turns))
    # vertical ink profile: concentration in the densest band of height 1 unit (writing: x-height band)
    ys = np.vstack([resample(s, unit / 8) for s in strokes])[:, 1]
    hist, _ = np.histogram(ys, bins=max(int(h / unit * 4), 4))
    k = 4
    band = np.convolve(hist, np.ones(k), "valid").max() / max(hist.sum(), 1) if len(hist) >= k else 1.0
    cx = np.array([s[:, 0].mean() for s in strokes])
    return np.array([
        len(strokes), math.log(w / h), w / unit, h / unit,
        np.median(lens), lens.max(), np.mean(straight), (straight > 0.97).mean(), closed.mean(),
        np.median(turns), np.mean(turns), band, np.std(sh / unit), np.median(sh / unit),
        np.ptp(cx) / unit / max(len(strokes), 1), *dirs,
    ])

# ---------------- data sources ----------------
def quickdraw():
    out = {}
    for f in sorted(glob.glob(f"{ROOT}/quickdraw/*.ndjson")):
        cat = os.path.basename(f)[:-7]
        items = []
        for l in open(f):
            try:
                d = json.loads(l)
            except json.JSONDecodeError:
                continue
            items.append([np.c_[s[0], s[1]] for s in d["drawing"]])
        out[cat] = items
    return out

def synth_drawings(n):
    """Arrows, boxes, underlines, crossing-out, connectors, plots, tables, circles-around."""
    res = []
    for i in range(n):
        k = i % 8; j = lambda a=2.0: rng.uniform(-a, a)
        if k == 0:  # arrow
            L = rng.uniform(40, 400); a = rng.uniform(0, 2 * math.pi)
            e = np.array([L * math.cos(a), L * math.sin(a)])
            sh = [np.array([[0, 0], e + [j(), j()]])]
            for s in (+1, -1):
                b = a + math.pi + s * rng.uniform(0.35, 0.6); hl = rng.uniform(8, 25)
                sh.append(np.array([e, e + [hl * math.cos(b), hl * math.sin(b)]]))
            res.append(sh)
        elif k == 1:  # box (one or four strokes)
            w, h = rng.uniform(40, 300), rng.uniform(30, 200)
            c = np.array([[0, 0], [w, 0], [w, h], [0, h], [0, 0]]) + np.random.default_rng(i).normal(0, 1.5, (5, 2))
            res.append([c] if rng.random() < .5 else [c[q:q + 2] for q in range(4)])
        elif k == 2:  # underline (single long stroke)
            L = rng.uniform(60, 500)
            x = np.linspace(0, L, 20); res.append([np.c_[x, np.random.default_rng(i).normal(0, 1, 20) + x * rng.uniform(-.03, .03)]])
        elif k == 3:  # crossing-out zigzag / scribble over a word area
            L = rng.uniform(60, 250); n_ = rng.randint(4, 12)
            x = np.linspace(0, L, n_ * 2); y = np.tile([0, rng.uniform(10, 30)], n_)
            res.append([np.c_[x, y]])
        elif k == 4:  # connector curve
            t = np.linspace(0, 1, 30); L = rng.uniform(80, 400)
            res.append([np.c_[t * L, np.sin(t * math.pi) * rng.uniform(20, 120)]])
        elif k == 5:  # plot: axes + curve
            W = rng.uniform(150, 400); H = rng.uniform(100, 300)
            t = np.linspace(0, 1, 40)
            res.append([np.array([[0, 0], [0, H]]), np.array([[0, H], [W, H]]), np.c_[t * W, H - (np.sin(t * rng.uniform(2, 9)) * .4 + .5) * H]])
        elif k == 6:  # table grid
            r, c = rng.randint(2, 5), rng.randint(2, 5); cw, rh = rng.uniform(40, 90), rng.uniform(20, 40)
            res.append([np.array([[0, y * rh], [c * cw, y * rh]]) for y in range(r + 1)] + [np.array([[x * cw, 0], [x * cw, r * rh]]) for x in range(c + 1)])
        else:  # ellipse around something
            t = np.linspace(0, 2 * math.pi * rng.uniform(1.0, 1.15), 50); a, b = rng.uniform(40, 200), rng.uniform(20, 80)
            res.append([np.c_[a * np.cos(t), b * np.sin(t)]])
    return res

HERSHEY = None
def hershey_lines(n):
    """Synthetic single-stroke writing from Hershey fonts with jitter/slant (needs the HersheyFonts pkg)."""
    from HersheyFonts import HersheyFonts
    words = open(f"{ROOT}/eval/de/gt.tsv").read().split() + "the quick brown fox jumps over lazy dog lecture notes given function".split()
    fonts = ["scripts", "scriptc", "cursive", "futural", "rowmans", "timesr"]
    res = []
    for i in range(n):
        hf = HersheyFonts(); hf.load_default_font(rng.choice(fonts)); hf.normalize_rendering(rng.uniform(20, 60))
        text = " ".join(rng.choice(words) for _ in range(rng.randint(1, 6)))
        slant = rng.uniform(-0.35, 0.1)
        strokes = []
        for seg in hf.strokes_for_text(text):
            p = np.array(seg, float)
            p[:, 0] += slant * p[:, 1]
            p += np.random.default_rng(i).normal(0, 0.4, p.shape)
            strokes.append(p)
        if strokes:
            res.append(strokes)
    return res

def fhswf_skeleton_lines(n):
    """fhswf line images → skeleton polylines (traced from endpoints)."""
    from PIL import Image
    from skimage.morphology import skeletonize
    from skimage.filters import threshold_otsu
    res = []
    files = sorted(glob.glob(f"{ROOT}/eval/de/*.png"))[200:200 + n]  # 200..: not used by recognition eval
    for f in files:
        g = np.asarray(Image.open(f).convert("L"), float)
        sk = skeletonize(g < threshold_otsu(g) - 10)
        ys, xs = np.nonzero(sk)
        pts = set(zip(ys.tolist(), xs.tolist()))
        nb = lambda p: [(p[0] + dy, p[1] + dx) for dy in (-1, 0, 1) for dx in (-1, 0, 1) if (dy or dx) and (p[0] + dy, p[1] + dx) in pts]
        strokes = []
        while pts:
            start = min(pts, key=lambda p: (len(nb(p)) != 1, p[1]))
            path = [start]; pts.discard(start); cur = start
            while True:
                nxt = nb(cur)
                if not nxt:
                    break
                cur = nxt[0]; pts.discard(cur); path.append(cur)
            if len(path) >= 4:
                strokes.append(np.array([(x, -y) for y, x in path], float))
        if len(strokes) >= 2:
            res.append(strokes)
    return res

def brush_eval():
    import pickle
    sys.path.insert(0, os.path.dirname(__file__))
    from prep import SafeUnpickler
    z = zipfile.ZipFile(f"{ROOT}/brush/brush.zip")
    plain = [n for n in z.namelist() if n.count("/") == 2 and not n.endswith("/") and "_" not in n.rsplit("/", 1)[1] and "DS_Store" not in n]
    r2 = random.Random(7)
    out = []
    for n in r2.sample(plain, 1000):
        _s, d, _l = SafeUnpickler(io.BytesIO(z.read(n))).load()
        d = np.asarray(d, float)
        strokes, start = [], 0
        for i in range(len(d)):
            if d[i, 2] > 0.5 or i == len(d) - 1:
                strokes.append(d[start:i + 1, :2] * [1, -1]); start = i + 1
        out.append(strokes)
    return out

def main():
    from sklearn.ensemble import HistGradientBoostingClassifier
    qd = quickdraw()
    cats = sorted(qd)
    # split drawings by index: 0-899 train, 900-1199 val, 1200-1499 test
    split = lambda xs: (xs[:900], xs[900:1200], xs[1200:1500])
    D = {"train": [], "val": [], "test": []}
    for c in cats:
        a, b, t = split(qd[c])
        D["train"] += a; D["val"] += b; D["test"] += t
    sd = synth_drawings(4000)
    D["train"] += sd[:2400]; D["val"] += sd[2400:3200]; D["test"] += sd[3200:]
    hw = hershey_lines(3000)
    fw = fhswf_skeleton_lines(200)
    Wtr = hw[:2000] + fw[:120]; Wval = hw[2000:2500] + fw[120:160]; Wte = hw[2500:] + fw[160:]
    br = brush_eval()
    F = lambda xs: np.array([features(x) for x in xs])
    Xtr = np.vstack([F(Wtr), F(D["train"])]); ytr = np.r_[np.ones(len(Wtr)), np.zeros(len(D["train"]))]
    clf = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, max_leaf_nodes=31, random_state=0).fit(Xtr, ytr)
    pv_w, pv_d = clf.predict_proba(F(Wval))[:, 1], clf.predict_proba(F(D["val"]))[:, 1]
    thr = float(pv_d.max()) + 1e-6   # zero drawing FP on validation
    pt_w, pt_d, pb = clf.predict_proba(F(Wte))[:, 1], clf.predict_proba(F(D["test"]))[:, 1], clf.predict_proba(F(br))[:, 1]
    # per-category FP check on test at thr and at 0.5
    percat = {}
    off = 0
    for c in cats:
        n = 300; p = pt_d[off:off + n]; off += n
        percat[c] = {"fp_at_thr": int((p >= thr).sum()), "fp_at_0.5": int((p >= .5).sum())}
    synth_p = pt_d[off:]
    res = {
        "train": {"writing": len(Wtr), "drawing": len(D["train"])},
        "threshold_zero_fp_on_val": round(thr, 4),
        "test_drawing_fp_at_thr": int((pt_d >= thr).sum()), "test_drawings": len(pt_d),
        "test_synth_fp_at_thr": int((synth_p >= thr).sum()),
        "writing_recall_at_thr": {"synthetic+fhswf_test": round(float((pt_w >= thr).mean()), 3), "BRUSH_real_pen": round(float((pb >= thr).mean()), 3)},
        "at_0.5": {"drawing_fp": int((pt_d >= .5).sum()), "BRUSH_recall": round(float((pb >= .5).mean()), 3)},
        "per_category_test_fp": {c: v for c, v in percat.items() if v["fp_at_0.5"]},
    }
    curve = []
    for t in [0.5, 0.9, thr, 0.97, 0.98, 0.99, 0.995, 0.999]:
        curve.append({"thr": round(float(t), 4), "test_drawing_fp": int((pt_d >= t).sum()), "val_drawing_fp": int((pv_d >= t).sum()),
                      "BRUSH_recall": round(float((pb >= t).mean()), 3), "synth_fhswf_recall": round(float((pt_w >= t).mean()), 3)})
    res["curve"] = curve
    import joblib, time
    t0 = time.time(); _ = [features(x) for x in br[:200]]; fe_ms = (time.time() - t0) / 200 * 1e3
    t0 = time.time(); clf.predict_proba(F(br[:200])); pr_ms = (time.time() - t0) / 200 * 1e3
    res["cost_ms_per_block"] = {"features_py": round(fe_ms, 2), "features+predict_py": round(pr_ms, 2)}
    print(json.dumps(res, indent=1))

if __name__ == "__main__":
    main()
