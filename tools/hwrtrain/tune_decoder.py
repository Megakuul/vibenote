"""Tune the beam decoder's lexicon weights (M9.5, ADR 0006 step (a)) on dev lines only.

Dev lines (never the eval sets in tests/eval): `finetune.py devsets` writes
  de: fhswf lines 150–399 of the 400 sampled by spikes/hwr/prep.py (the eval has the first 150),
  en: GNHK test-split lines beyond the first two qualifying ones per page (the eval takes ≤ 2).

  python -I tune_decoder.py dump <rec.onnx> <dev dir> <out dir>       # CTC probabilities per line
  python -I tune_decoder.py grid <dict.txt> <lexicon.tsv> <out dir> <lang> alpha beta oov [beam]

`grid` prints greedy and beam CER for one weight setting; the decoder is the same algorithm as
`crates/hwr/src/ctc.rs::beam` (prefix beam search; each completed token with letters adds
alpha·score + beta, unlisted words score oov; tokens without letters add nothing).

Recorded sweep (2026-10-14, pack lexicon, beam 16; dev CER greedy → beam):
  alpha 0.8, beta 5, oov −6:   de 18.61 → 17.82 %, en 23.44 → 20.72 %
  alpha 0.8, beta 5, oov −6.5: de 18.61 → 17.71 %, en 23.44 → 20.66 %
  alpha 0.8, beta 5, oov −7:   de 18.61 → 17.53 %, en 23.44 → 20.60 %   ← chosen (pack manifest)
  alpha 1.0, beta 6, oov −7:   de 18.61 → 17.51 %, en 23.44 → 20.75 %
  alpha 1.0, beta 6, oov −6.5: de 18.61 → 17.59 %, en 23.44 → 20.70 %
Then measured once on eval (Rust, `r_hwr_003`): en 17.34 → 15.20 %, de 18.36 → 17.98 %.
"""
import collections, json, math, os, re, sys

import numpy as np


def dump(model, dev, out):
    import onnxruntime as ort
    from PIL import Image
    sess = ort.InferenceSession(model, providers=["CPUExecutionProvider"])
    name = sess.get_inputs()[0].name
    for lang in ("de", "en"):
        rows = [l.rstrip("\n").split("\t", 1) for l in open(os.path.join(dev, lang, "labels.tsv"), encoding="utf-8")]
        probs = {}
        for k, (i, _) in enumerate(rows):
            g = np.asarray(Image.open(os.path.join(dev, lang, f"{i}.png")).convert("L"), dtype=np.float32) / 255.0
            w = max(1, min(3200, int(np.ceil(g.shape[1] * 48 / g.shape[0]))))
            r = np.asarray(Image.fromarray(g, mode="F").resize((w, 48), Image.BILINEAR))
            x = np.zeros((48, max(320, w)), np.float32)
            x[:, :w] = (r - 0.5) / 0.5
            probs[f"p{k}"] = sess.run(None, {name: np.repeat(x[None, None], 3, axis=1)})[0][0]
        np.savez_compressed(os.path.join(out, f"dev-{lang}.npz"), **probs)
        json.dump([t for _, t in rows], open(os.path.join(out, f"dev-{lang}.json"), "w"), ensure_ascii=False)


def lev(a, b):
    p = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        c = [i]
        for j, y in enumerate(b, 1):
            c.append(min(p[j] + 1, c[j - 1] + 1, p[j - 1] + (x != y)))
        p = c
    return p[-1]


def lse(a, b):
    if a == -math.inf:
        return b
    if b == -math.inf:
        return a
    m = max(a, b)
    return m + math.log(math.exp(a - m) + math.exp(b - m))


def grid(dict_path, lex_path, out, lang, alpha, beta, oov, beam=16):
    chars = [l.rstrip("\n") for l in open(dict_path, encoding="utf-8")]
    space = len(chars) + 1
    lex = {}
    for l in open(lex_path, encoding="utf-8"):
        w, v = l.rstrip("\n").split("\t")
        lex[w] = float(v)
    core_re = re.compile(r"^[^\w]*(.*?)[^\w]*$", re.S)

    def wscore(w):
        core = core_re.match(w).group(1)
        if not core or not any(c.isalpha() for c in core):
            return None
        for k in (core, core.lower(), core[:1].upper() + core[1:].lower()):
            if k in lex:
                return lex[k]
        return oov

    def ch(k):
        return " " if k == space else chars[k - 1]

    def decode(P):
        L = np.log(np.maximum(P, 1e-30))
        beams = {"": (0.0, -math.inf, 0.0)}
        for t in range(L.shape[0]):
            row = L[t]
            cand = [k for k in np.argsort(-row)[:32] if k != 0 and row[k] > math.log(1e-3)]
            nb = collections.defaultdict(lambda: [-math.inf, -math.inf, 0.0])
            for text, (pb, pnb, lm) in beams.items():
                tot = lse(pb, pnb)
                e = nb[text]
                e[2] = lm
                e[0] = lse(e[0], tot + row[0])
                last = text[-1:] if text else None
                for k in cand:
                    c = ch(k)
                    if c == last:
                        e[1] = lse(e[1], pnb + row[k])
                    n = text + c
                    nlm = lm
                    if c == " " and text and not text.endswith(" "):
                        sc = wscore(text.rsplit(" ", 1)[-1])
                        nlm = lm if sc is None else lm + alpha * sc + beta
                    f = nb[n]
                    f[2] = nlm
                    f[1] = lse(f[1], (pb if c == last else tot) + row[k])
            beams = {k: tuple(v) for k, v in sorted(nb.items(), key=lambda kv: (-(lse(kv[1][0], kv[1][1]) + kv[1][2]), kv[0]))[:beam]}

        def final(kv):
            text, (pb, pnb, lm) = kv
            w = text.rsplit(" ", 1)[-1] if text and not text.endswith(" ") else ""
            sc = wscore(w) if w else None
            return lse(pb, pnb) + lm + (0 if sc is None else alpha * sc + beta)
        return " ".join(max(beams.items(), key=final)[0].split())

    def greedy(P):
        out, prev = [], 0
        for k in P.argmax(1):
            if k != 0 and k != prev:
                out.append(ch(k))
            prev = k
        return " ".join("".join(out).split())

    Z = np.load(os.path.join(out, f"dev-{lang}.npz"))
    gts = json.load(open(os.path.join(out, f"dev-{lang}.json"), encoding="utf-8"))
    eg = eb = n = 0
    for k, gt in enumerate(gts):
        P = Z[f"p{k}"]
        eg += lev(greedy(P), gt)
        eb += lev(decode(P), gt)
        n += len(gt)
    print(f"dev-{lang} alpha={alpha} beta={beta} oov={oov} beam={beam}: greedy {100*eg/n:.2f} %  beam {100*eb/n:.2f} %")


if __name__ == "__main__":
    cmd, a = sys.argv[1], sys.argv[2:]
    if cmd == "dump":
        dump(*a)
    elif cmd == "grid":
        grid(a[0], a[1], a[2], a[3], *map(float, a[4:7]), *(int(x) for x in a[7:8]))
    else:
        raise SystemExit(__doc__)
