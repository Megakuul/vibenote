"""Prettify stage 1 (deterministic geometry) — Phase 2 spike 7.

Input: one writing line as pen strokes. Steps (all *partial* so the result stays the writer's
own hand; strength factors below):
  1. smoothing (Gaussian along arc length, σ = 0.04·x-height, endpoints kept)
  2. baseline: robust fit (Theil–Sen) through stroke-bottom points → rotate line to horizontal
  3. words: strokes grouped by horizontal gaps > 0.6·x-height
  4. per-word slant estimated from near-vertical segments → shear by SLANT_K × (word slant − line median)
     then line median slant reduced by SLANT_LINE_K
  5. per-word x-height scaled toward line median by SIZE_K; word baseline moved to line baseline by BASE_K
  6. word gaps moved toward the median gap by GAP_K
Checks: stroke count/order unchanged; max point displacement after removing the global rotation
≤ 0.35·x-height (R-HWR-004 bound); recognised text (PP-OCRv5) before/after.
Eval data: BRUSH (evaluation-only, owner decision) — no learning happens here.
"""
import io, json, math, os, random, sys, zipfile
import numpy as np
from PIL import Image, ImageDraw

SLANT_K, SLANT_LINE_K, SIZE_K, BASE_K, GAP_K = 0.7, 0.4, 0.5, 0.8, 0.5
STEPS = set(os.environ.get("STEPS", "smooth,rotate,slant,size,base,gap").split(","))
if "slant" not in STEPS: SLANT_K = SLANT_LINE_K = 0.0
if "size" not in STEPS: SIZE_K = 0.0
if "base" not in STEPS: BASE_K = 0.0
if "gap" not in STEPS: GAP_K = 0.0
ROOT = "/dev/shm/vibenote-hwr"

def smooth(s, sigma):
    if len(s) < 5:
        return s.copy()
    d = np.r_[0, np.cumsum(np.hypot(*np.diff(s, axis=0).T))]
    out = s.copy()
    for i in range(1, len(s) - 1):
        w = np.exp(-0.5 * ((d - d[i]) / max(sigma, 1e-6)) ** 2)
        out[i] = (w[:, None] * s).sum(0) / w.sum()
    return out

def theil_sen(x, y):
    if len(x) < 2:
        return 0.0, float(np.median(y)) if len(y) else 0.0
    idx = np.random.default_rng(0).choice(len(x), size=(min(400, len(x) * len(x)), 2))
    dx = x[idx[:, 0]] - x[idx[:, 1]]; dy = y[idx[:, 0]] - y[idx[:, 1]]
    m = np.median(dy[np.abs(dx) > 1e-6] / dx[np.abs(dx) > 1e-6]) if np.any(np.abs(dx) > 1e-6) else 0.0
    return float(m), float(np.median(y - m * x))

def xheight(strokes):
    ys = np.concatenate([s[:, 1] for s in strokes])
    lo, hi = np.percentile(ys, [20, 80])  # y grows downward
    return max(hi - lo, 1e-3)

def slant_of(strokes):
    """Median angle from vertical of near-vertical segments (radians, + = right-leaning)."""
    angs, ws = [], []
    for s in strokes:
        d = np.diff(s, axis=0)
        L = np.hypot(*d.T)
        a = np.arctan2(d[:, 0], -d[:, 1])  # 0 = straight up
        a = (a + np.pi / 2) % np.pi - np.pi / 2
        m = (np.abs(a) < math.radians(45)) & (L > 0)
        angs += list(a[m]); ws += list(L[m])
    if not angs:
        return 0.0
    o = np.argsort(angs); c = np.cumsum(np.array(ws)[o])
    return float(np.array(angs)[o][np.searchsorted(c, c[-1] / 2)])

def prettify(strokes):
    S = [np.asarray(s, float).copy() for s in strokes]
    xh = xheight(S)
    if "smooth" in STEPS:
        S = [smooth(s, 0.04 * xh) for s in S]
    # baseline from stroke-bottom points (local maxima of y)
    bx, by = [], []
    for s in S:
        i = np.argmax(s[:, 1]); bx.append(s[i, 0]); by.append(s[i, 1])
    m, c = theil_sen(np.array(bx), np.array(by))
    ang = math.atan(m)
    if abs(ang) < math.radians(20) and "rotate" in STEPS:
        R = np.array([[math.cos(-ang), -math.sin(-ang)], [math.sin(-ang), math.cos(-ang)]])
        cx = np.mean(np.vstack(S), 0)
        S = [(s - cx) @ R.T + cx for s in S]
    rotated = [s.copy() for s in S]  # reference for displacement check (rotation is global)
    # words by horizontal gaps
    order = sorted(range(len(S)), key=lambda i: S[i][:, 0].min())
    words, cur, right = [], [order[0]], S[order[0]][:, 0].max()
    for i in order[1:]:
        if S[i][:, 0].min() - right > 0.6 * xh:
            words.append(cur); cur = [i]
        else:
            cur.append(i)
        right = max(right, S[i][:, 0].max())
    words.append(cur)
    def ext(w):
        P = np.vstack([S[i] for i in w]); return P[:, 0].min(), P[:, 0].max(), np.ptp(P[:, 1]), np.ptp(P[:, 0])
    k = 0
    while len(words) > 1 and k < len(words):
        x0, x1, hgt, wid = ext(words[k])
        if hgt < 0.45 * xh and wid < 0.6 * xh:  # mark, not a word: attach to the closer neighbour
            if k == 0:
                j = 1
            elif k == len(words) - 1:
                j = k - 1
            else:
                j = k - 1 if x0 - ext(words[k - 1])[1] <= ext(words[k + 1])[0] - x1 else k + 1
            words[j] = sorted(words[j] + words[k], key=lambda i: S[i][:, 0].min())
            words.pop(k)
            k = 0
            continue
        k += 1
    line_base = np.median([max(s[:, 1].max() for s in [S[i] for i in w]) for w in words])
    wslant = [slant_of([S[i] for i in w]) for w in words]
    med_slant = float(np.median(wslant))
    wxh = [xheight([S[i] for i in w]) for w in words]
    med_xh = float(np.median(wxh))
    for w, sl, h in zip(words, wslant, wxh):
        pts = [S[i] for i in w]
        base = np.percentile(np.concatenate([p[:, 1] for p in pts]), 85)
        cxw = np.mean(np.concatenate([p[:, 0] for p in pts]))
        target_sl = sl + SLANT_K * (med_slant - sl) - SLANT_LINE_K * med_slant
        target_sl = min(max(target_sl, sl - math.radians(8)), sl + math.radians(8))
        sh = math.tan(sl) - math.tan(target_sl)
        sc = 1 + SIZE_K * (med_xh / h - 1)
        sc = min(max(sc, 0.85), 1.18)
        dy = float(np.clip(BASE_K * (line_base - base), -0.3 * xh, 0.3 * xh))
        for i in w:
            s = S[i]
            x = s[:, 0] + sh * (s[:, 1] - base)          # shear about the word baseline
            x = cxw + (x - cxw) * sc
            y = base + (s[:, 1] - base) * sc + dy
            S[i] = np.c_[x, y]
    # spacing
    spans = [(min(S[i][:, 0].min() for i in w), max(S[i][:, 0].max() for i in w)) for w in words]
    gaps = [spans[k + 1][0] - spans[k][1] for k in range(len(spans) - 1)]
    if gaps:
        mg = float(np.median(gaps)); shift = 0.0
        for k in range(1, len(words)):
            shift += float(np.clip(GAP_K * (mg - gaps[k - 1]), -0.25 * xh, 0.25 * xh))
            for i in words[k]:
                S[i][:, 0] += shift
    shape_disp, trans = 0.0, 0.0
    for w in words:
        A = np.vstack([S[i] for i in w]); B = np.vstack([rotated[i] for i in w])
        t = (A - B).mean(0)
        trans = max(trans, float(np.hypot(*t)))
        shape_disp = max(shape_disp, float(np.max(np.hypot(*(A - B - t).T))))
    disp = shape_disp / xh
    return S, {"xheight": xh, "baseline_angle_deg": math.degrees(ang), "words": len(words), "max_disp_xh": disp, "max_word_translation_xh": trans / xh,
               "median_slant_deg": math.degrees(med_slant)}

def render(strokes, height=96, pad=12, wpx=3):
    allp = np.vstack(strokes); mn, mx = allp.min(0), allp.max(0)
    s = (height - 2 * pad) / max(mx[1] - mn[1], 1e-6)
    img = Image.new("RGB", (int((mx[0] - mn[0]) * s) + 2 * pad, height), "white")
    d = ImageDraw.Draw(img)
    for st in strokes:
        p = [tuple(q) for q in (st - mn) * s + pad]
        if len(p) > 1:
            d.line(p, fill="black", width=wpx, joint="curve")
    return img

def baseline_spread(strokes):
    """Std of word baselines relative to x-height (lower = straighter)."""
    S = strokes; xh = xheight(S)
    bottoms = np.array([s[:, 1].max() for s in S]); xs = np.array([s[:, 0].mean() for s in S])
    m, c = theil_sen(xs, bottoms)
    return float(np.std(bottoms - (m * xs + c)) / xh)

def main(n=150):
    sys.path.insert(0, os.path.dirname(__file__))
    from prep import SafeUnpickler
    from evaluate import Rapid
    import jiwer
    rec = Rapid("EN")
    z = zipfile.ZipFile(f"{ROOT}/brush/brush.zip")
    plain = [x for x in z.namelist() if x.count("/") == 2 and not x.endswith("/") and "_" not in x.rsplit("/", 1)[1] and "DS_Store" not in x]
    r = random.Random(11)
    refs, hb, ha, stats, gallery = [], [], [], [], []
    os.makedirs(f"{ROOT}/gallery", exist_ok=True)
    for k, name in enumerate(r.sample(plain, n)):
        text, d, _ = SafeUnpickler(io.BytesIO(z.read(name))).load()
        d = np.asarray(d, float)
        strokes, st = [], 0
        for i in range(len(d)):
            if d[i, 2] > 0.5 or i == len(d) - 1:
                if i + 1 - st >= 1:
                    strokes.append(d[st:i + 1, :2].copy())
                st = i + 1
        if len(strokes) < 2:
            continue
        after, info = prettify(strokes)
        assert len(after) == len(strokes) and all(len(a) == len(b) for a, b in zip(after, strokes))
        ib, ia = render(strokes), render(after)
        refs.append(str(text).strip()); hb.append(rec(ib).strip()); ha.append(rec(ia).strip())
        info.update({"spread_before": baseline_spread(strokes), "spread_after": baseline_spread(after)})
        stats.append(info)
        if k < 24:
            ib.save(f"{ROOT}/gallery/{k:02d}a.png"); ia.save(f"{ROOT}/gallery/{k:02d}b.png"); gallery.append((k, refs[-1], hb[-1], ha[-1]))
    disp = np.array([s["max_disp_xh"] for s in stats])
    tr = np.array([s["max_word_translation_xh"] for s in stats])
    sb, sa = np.array([s["spread_before"] for s in stats]), np.array([s["spread_after"] for s in stats])
    res = {"lines": len(stats), "cer_before": round(jiwer.cer(refs, hb), 4), "cer_after": round(jiwer.cer(refs, ha), 4),
           "wer_before": round(jiwer.wer(refs, hb), 4), "wer_after": round(jiwer.wer(refs, ha), 4),
           "lines_text_unchanged": int(sum(a == b for a, b in zip(hb, ha))),
           "max_disp_xh": {"p50": round(float(np.median(disp)), 3), "p95": round(float(np.percentile(disp, 95)), 3), "max": round(float(disp.max()), 3),
                           "lines_over_0.35": int((disp > 0.35).sum())},
           "max_word_translation_xh": {"p50": round(float(np.median(tr)), 3), "max": round(float(tr.max()), 3)},
           "baseline_spread_xh": {"before_p50": round(float(np.median(sb)), 3), "after_p50": round(float(np.median(sa)), 3)}}
    print(json.dumps(res, indent=1))
    with open(f"{ROOT}/gallery/index.html", "w") as f:
        f.write("<!doctype html><meta charset=utf-8><title>Prettify gallery</title><style>body{font:14px sans-serif;background:#fafaf7}td{padding:6px;border-bottom:1px solid #ddd}img{max-width:560px}</style><table><tr><th>#</th><th>before</th><th>after</th><th>text / OCR before → after</th></tr>")
        for k, t, b, a in gallery:
            f.write(f"<tr><td>{k}</td><td><img src={k:02d}a.png></td><td><img src={k:02d}b.png></td><td>{t}<br><small>{b}<br>→ {a}</small></td></tr>")

if __name__ == "__main__":
    main()
