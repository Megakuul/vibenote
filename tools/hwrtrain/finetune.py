"""Fine-tune the pack's PP-OCR recogniser on permissive lines (M9.6, ADR 0006 step (b)).

The ONNX model is converted to PyTorch (onnx2torch; the three SAME_UPPER kernel-2/stride-1 nodes
are first rewritten as explicit pads [0,0,1,1], which is equivalent), trained with CTC loss on
lines from `prep_finetune.py`, selected on dev lines (never the eval sets in tests/eval), and
exported back to ONNX with a dynamic width.

Preprocessing equals the product (`crates/hwr/src/recognizer.rs`, `crates/engine/src/hwr.rs`):
greyscale in [0, 1], 48 px high, (v − 0.5) / 0.5, three equal channels, right-padded with 0 to
at least 320 px.

Usage:
  python -I finetune.py patch <rec.onnx> <out.onnx>
  python -I finetune.py train <patched.onnx> <dict.txt> <lines dir> <dev dir> <work dir> [hours]
  python -I finetune.py export <work dir>/best.pt <out.onnx>
  python -I finetune.py devsets <eval/de dir> <GNHK test dir> <dev dir>
"""
import json, math, os, random, re, sys, time

import numpy as np
from PIL import Image, ImageFilter, ImageOps

H, MIN_W, MAX_W = 48, 320, 1600
SEED = 7


def patch(src, dst):
    import onnx
    m = onnx.load(src)
    n_fixed = 0
    for n in m.graph.node:
        a = {x.name: x for x in n.attribute}
        if "auto_pad" in a and a["auto_pad"].s == b"SAME_UPPER":
            k = list(a["kernel_shape"].ints)
            s = list(a["strides"].ints) if "strides" in a else [1] * len(k)
            if s != [1] * len(k):
                raise SystemExit(f"{n.name}: SAME_UPPER with stride {s} not supported")
            pads = [(ki - 1) // 2 for ki in k] + [(ki - 1) - (ki - 1) // 2 for ki in k]
            n.attribute.remove(a["auto_pad"])
            n.attribute.append(onnx.helper.make_attribute("pads", pads))
            n_fixed += 1
    onnx.save(m, dst)
    print("patched", n_fixed, "nodes")


def to_input(img):
    """PIL greyscale line → float32 [48, w] normalised (the product's preprocessing)."""
    g = np.asarray(img.convert("L"), dtype=np.float32) / 255.0
    w = max(1, min(MAX_W, int(math.ceil(g.shape[1] * H / g.shape[0]))))
    r = np.asarray(Image.fromarray(g, mode="F").resize((w, H), Image.BILINEAR), dtype=np.float32)
    return (r - 0.5) / 0.5


def batch_tensor(planes):
    import torch
    w = max(MIN_W, max(p.shape[1] for p in planes))
    x = np.zeros((len(planes), 3, H, w), np.float32)
    for i, p in enumerate(planes):
        x[i, :, :, : p.shape[1]] = p[None]
    return torch.from_numpy(x)


def augment(img, rng):
    """Mild, label-preserving changes: scale, slant, blur, contrast, margins."""
    img = img.convert("L")
    if rng.random() < 0.5:
        sh = rng.uniform(-0.25, 0.25)
        w, h = img.size
        img = img.transform((w + int(abs(sh) * h), h), Image.AFFINE,
                            (1, sh, -sh * h if sh > 0 else 0, 0, 1, 0), fillcolor=255)
    if rng.random() < 0.5:
        f = rng.uniform(0.8, 1.15)
        img = img.resize((max(1, int(img.width * f)), img.height), Image.BILINEAR)
    if rng.random() < 0.3:
        img = img.filter(ImageFilter.GaussianBlur(rng.uniform(0.3, 1.0)))
    if rng.random() < 0.5:
        a = np.asarray(img, np.float32)
        lo, hi = rng.uniform(0, 60), rng.uniform(190, 255)
        img = Image.fromarray(np.clip(lo + a * (hi - lo) / 255.0, 0, 255).astype(np.uint8))
    if rng.random() < 0.5:
        p = [int(rng.uniform(0, 6)) for _ in range(4)]
        img = ImageOps.expand(img, tuple(p), fill=255)
    return img


def lev(a, b):
    p = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        c = [i]
        for j, y in enumerate(b, 1):
            c.append(min(p[j] + 1, c[j - 1] + 1, p[j - 1] + (x != y)))
        p = c
    return p[-1]


def greedy(probs, chars):
    out, prev = [], 0
    space = len(chars) + 1
    for k in probs.argmax(-1):
        k = int(k)
        if k != 0 and k != prev:
            out.append(" " if k == space else (chars[k - 1] if k - 1 < len(chars) else ""))
        prev = k
    return " ".join("".join(out).split())


def load_lines(d):
    rows = [l.rstrip("\n").split("\t", 1) for l in open(os.path.join(d, "labels.tsv"), encoding="utf-8")]
    return [(os.path.join(d, f"{i}.png"), t) for i, t in rows if len(t) > 0]


def dev_cer(net, items, chars):
    """Greedy CER, one line at a time exactly as the product runs it (the model attends over the
    whole line, so padding a line to a batch's width would change its output)."""
    import torch
    net.eval()
    e = n = 0
    with torch.no_grad():
        for plane, gt in items:
            y = net(batch_tensor([plane]))
            y = y[0] if isinstance(y, (list, tuple)) else y
            e += lev(greedy(y[0].numpy(), chars), gt)
            n += len(gt)
    return e / max(1, n)


def train(onnx_path, dict_path, lines_dir, dev_dir, work, hours=6.0):
    import torch
    from onnx2torch import convert
    import onnx
    torch.manual_seed(SEED)
    torch.set_num_threads(int(os.environ.get("FT_THREADS", "12")))
    rng = random.Random(SEED)
    chars = [l.rstrip("\n") for l in open(dict_path, encoding="utf-8")]
    index = {c: i + 1 for i, c in enumerate(chars)}
    space = len(chars) + 1
    net = convert(onnx.load(onnx_path))
    # Output steps per input width (for CTC input lengths).
    with torch.no_grad():
        net.eval()
        t320 = net(torch.zeros(1, 3, H, 320)).shape[1]
        t640 = net(torch.zeros(1, 3, H, 640)).shape[1]
    stride = 320 / t320
    assert abs(640 / t640 - stride) < 1e-6, (t320, t640)

    def encode(t):
        ids = []
        for c in t:
            if c == " ":
                ids.append(space)
            elif c in index:
                ids.append(index[c])
            else:
                return None
        return ids

    train_items = []
    for lang in ("en", "de"):
        for p, t in load_lines(os.path.join(lines_dir, lang)):
            ids = encode(t)
            if ids:
                train_items.append((p, ids, lang))
    print("train lines:", len(train_items), "(skipped lines with characters outside the dict)", flush=True)
    dev = {}
    for lang in ("en", "de"):
        dev[lang] = [(to_input(Image.open(p)), t) for p, t in load_lines(os.path.join(dev_dir, lang))]
    os.makedirs(work, exist_ok=True)
    log = open(os.path.join(work, "log.jsonl"), "a")
    base = {k: dev_cer(net, v, chars) for k, v in dev.items()}
    print("dev CER before:", base, flush=True)
    log.write(json.dumps({"step": 0, "dev": base}) + "\n"); log.flush()
    best = sum(base.values())
    torch.save(net, os.path.join(work, "best.pt"))
    opt = torch.optim.AdamW(net.parameters(), lr=float(os.environ.get("FT_LR", "1e-4")), weight_decay=1e-4)
    ctc = torch.nn.CTCLoss(blank=0, zero_infinity=True)
    bs = int(os.environ.get("FT_BATCH", "16"))
    eval_every = int(os.environ.get("FT_EVAL_EVERY", "400"))
    deadline = time.time() + hours * 3600
    step = 0
    while time.time() < deadline:
        rng.shuffle(train_items)
        # Batches of similar widths (less padding).
        chunks = [train_items[i:i + bs * 20] for i in range(0, len(train_items), bs * 20)]
        for ch in chunks:
            planes = [(to_input(augment(Image.open(p), rng)), ids) for p, ids, _ in ch]
            planes.sort(key=lambda x: x[0].shape[1])
            batches = [planes[i:i + bs] for i in range(0, len(planes), bs)]
            rng.shuffle(batches)
            for b in batches:
                net.train()
                for m in net.modules():
                    if isinstance(m, torch.nn.BatchNorm2d):
                        m.eval()  # frozen statistics (small batches)
                x = batch_tensor([p for p, _ in b])
                y = net(x)
                y = y[0] if isinstance(y, (list, tuple)) else y
                logp = torch.log(y.clamp_min(1e-8)).permute(1, 0, 2)  # T, N, C
                T = logp.shape[0]
                in_len = torch.tensor([min(T, max(1, math.ceil(max(MIN_W, p.shape[1]) / stride))) for p, _ in b])
                tg = torch.tensor([i for _, ids in b for i in ids])
                tg_len = torch.tensor([len(ids) for _, ids in b])
                loss = ctc(logp, tg, in_len, tg_len)
                opt.zero_grad()
                loss.backward()
                torch.nn.utils.clip_grad_norm_(net.parameters(), 5.0)
                opt.step()
                step += 1
                if step % 50 == 0:
                    print(f"step {step} loss {loss.item():.3f}", flush=True)
                if step % eval_every == 0:
                    cer = {k: dev_cer(net, v, chars) for k, v in dev.items()}
                    tot = sum(cer.values())
                    print(f"step {step} dev CER {cer}", flush=True)
                    log.write(json.dumps({"step": step, "dev": cer, "loss": loss.item()}) + "\n"); log.flush()
                    if tot < best:
                        best = tot
                        torch.save(net, os.path.join(work, "best.pt"))
                        print("  new best", flush=True)
                if time.time() >= deadline:
                    break
            if time.time() >= deadline:
                break
    print("done; best dev CER sum", best, flush=True)


def export(pt, out):
    import torch
    net = torch.load(pt, weights_only=False)
    net.eval()
    x = torch.zeros(1, 3, H, 640)
    torch.onnx.export(net, (x,), out, input_names=["x"], output_names=["y"],
                      dynamic_axes={"x": {0: "N", 3: "W"}, "y": {0: "N", 1: "T"}}, opset_version=17, dynamo=False)
    import onnxruntime as ort
    s = ort.InferenceSession(out, providers=["CPUExecutionProvider"])
    for w in (320, 777, 1600):
        xi = np.random.default_rng(w).uniform(-1, 1, (1, 3, H, w)).astype(np.float32)
        with torch.no_grad():
            ref = net(torch.from_numpy(xi))
        ref = (ref[0] if isinstance(ref, (list, tuple)) else ref).numpy()
        got = s.run(None, {"x": xi})[0]
        print("width", w, "max abs diff", float(np.abs(ref - got).max()))


def devsets(de_eval_dir, gnhk_test, out):
    """Dev lines: fhswf lines 150.. of the sampled 400 (the eval uses the first 150) and GNHK
    test lines beyond the first two qualifying ones per page (the eval takes at most two)."""
    d = os.path.join(out, "de"); os.makedirs(d, exist_ok=True)
    rows = [l.rstrip("\n").split("\t", 1) for l in open(os.path.join(de_eval_dir, "gt.tsv"), encoding="utf-8")][150:]
    lab = []
    for i, t in rows:
        p = os.path.join(de_eval_dir, f"{i}.png")
        if os.path.exists(p):
            Image.open(p).convert("L").save(os.path.join(d, f"{i}.png")); lab.append(f"{i}\t{t}\n")
    open(os.path.join(d, "labels.tsv"), "w", encoding="utf-8").writelines(lab)
    d = os.path.join(out, "en"); os.makedirs(d, exist_ok=True)
    lab, k = [], 0
    for jf in sorted(f for f in os.listdir(gnhk_test) if f.endswith(".json")):
        words = json.load(open(os.path.join(gnhk_test, jf)))
        lines = {}
        for w in words:
            lines.setdefault(w["line_idx"], []).append(w)
        ok = []
        for li in sorted(lines):
            ws = lines[li]
            if len(ws) < 2 or any(w["type"] != "H" or "%" in w["text"] or not w["text"].strip() for w in ws):
                continue
            if re.search(r"\d{5}", "".join(w["text"] for w in ws)):
                continue
            ok.append(ws)
        im = None
        for ws in ok[2:6]:
            xs = [w["polygon"][f"x{i}"] for w in ws for i in range(4)]
            ys = [w["polygon"][f"y{i}"] for w in ws for i in range(4)]
            x0, y0, x1, y1 = min(xs), min(ys), max(xs), max(ys)
            if im is None:
                im = ImageOps.exif_transpose(Image.open(os.path.join(gnhk_test, jf[:-5] + ".jpg"))).convert("L")
            pad = int(0.15 * (y1 - y0))
            c = (max(0, x0 - pad), max(0, y0 - pad), min(im.width, x1 + pad), min(im.height, y1 + pad))
            if c[2] - c[0] < 8 or c[3] - c[1] < 8:
                continue
            cr = im.crop(c)
            cr = cr.resize((max(1, int(cr.width * 64 / cr.height)), 64), Image.LANCZOS)
            ws = sorted(ws, key=lambda w: min(w["polygon"][f"x{i}"] for i in range(4)))
            cr.save(os.path.join(d, f"{k:04d}.png")); lab.append(f"{k:04d}\t{' '.join(w['text'] for w in ws)}\n"); k += 1
    open(os.path.join(d, "labels.tsv"), "w", encoding="utf-8").writelines(lab)
    print("dev lines: de", len(rows), "en", k)


if __name__ == "__main__":
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "patch":
        patch(*args)
    elif cmd == "train":
        train(*args[:5], hours=float(args[5]) if len(args) > 5 else 6.0)
    elif cmd == "export":
        export(*args)
    elif cmd == "devsets":
        devsets(*args)
    else:
        raise SystemExit(__doc__)
