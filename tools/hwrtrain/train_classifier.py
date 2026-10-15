"""Train the writing-vs-drawing classifier (gate A, ADR 0007) and export it for the Rust runtime.

Data (permissive only, as in the Phase 2 spike): Quick, Draw! (CC BY 4.0) indices 0-899 train /
900-1199 validation; synthetic diagrams 0-2399 / 2400-3199; Hershey synthetic writing 0-1999 /
2000-2499; fhswf skeleton lines 0-119 / 120-159. The protected eval set uses Quick, Draw!
1200-1299, synthetic 3200-3599, Hershey 2500-2899 and fhswf 160-199, generated in the same order
by tools/evalgen/make_eval.py, so the ranges never overlap.

Features are computed by the Rust code the app runs (`vibenote-cli hwr-features`), so training and
runtime see the same values. The threshold gives zero drawing false positives on validation.

Usage (from the repo root):
  nix-shell spikes/hwr/shell.nix --run "python -I tools/hwrtrain/train_classifier.py <vibenote-cli>"
Writes assets/hwr/classifier.json.
"""
import json, os, subprocess, sys, tempfile
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "../.."))
sys.path.insert(0, os.path.join(ROOT, "tools/evalgen"))
import classify_ref as C  # noqa: E402  (the spike's data readers)

C.ROOT = os.environ.get("VIBENOTE_DATA", "/dev/shm/vibenote-hwr")


def rust_features(cli, blocks):
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False, dir="/dev/shm") as f:
        for b in blocks:
            f.write(json.dumps({"strokes": [[[float(x), float(y)] for x, y in s] for s in b]}) + "\n")
        path = f.name
    try:
        out = subprocess.run([cli, "hwr-features", path], check=True, capture_output=True, text=True).stdout
    finally:
        os.unlink(path)
    rows = [json.loads(l) for l in out.splitlines()]
    assert len(rows) == len(blocks)
    return rows


def export(clf, threshold, about):
    trees = []
    for it in clf._predictors:
        t = it[0]
        nodes = []
        for n in t.nodes:
            if n["is_leaf"]:
                nodes.append({"leaf": True, "v": float(n["value"])})
            else:
                nodes.append({"leaf": False, "f": int(n["feature_idx"]), "t": float(n["num_threshold"]),
                              "l": int(n["left"]), "r": int(n["right"]), "nl": bool(n["missing_go_to_left"])})
        trees.append(nodes)
    return {"features": int(clf.n_features_in_), "baseline": float(np.ravel(clf._baseline_prediction)[0]),
            "trees": trees, "threshold": float(threshold), "about": about}


def predict_exported(m, X):
    out = []
    for x in X:
        raw = m["baseline"]
        for t in m["trees"]:
            i = 0
            while not t[i]["leaf"]:
                n = t[i]
                i = n["l"] if x[n["f"]] <= n["t"] else n["r"]
            raw += t[i]["v"]
        out.append(1 / (1 + np.exp(-raw)))
    return np.array(out)


def main():
    from sklearn.ensemble import HistGradientBoostingClassifier
    cli = sys.argv[1]
    qd = C.quickdraw()
    cats = sorted(qd)
    d_tr, d_va = [], []
    for c in cats:
        d_tr += qd[c][:900]
        d_va += qd[c][900:1200]
    sd = C.synth_drawings(4000)          # same call order as make_eval.py (shared RNG)
    hw = C.hershey_lines(3000)
    fw = C.fhswf_skeleton_lines(200)
    d_tr += sd[:2400]
    d_va += sd[2400:3200]
    w_tr = hw[:2000] + fw[:120]
    w_va = hw[2000:2500] + fw[120:160]
    F = lambda xs: np.array([r for r in rust_features(cli, xs)], dtype=float)
    keep = lambda xs: [x for x, r in zip(xs, rust_features(cli, xs)) if r is not None]
    w_tr, w_va, d_tr, d_va = keep(w_tr), keep(w_va), keep(d_tr), keep(d_va)
    X = np.vstack([F(w_tr), F(d_tr)])
    y = np.r_[np.ones(len(w_tr)), np.zeros(len(d_tr))]
    clf = HistGradientBoostingClassifier(max_iter=300, learning_rate=0.08, max_leaf_nodes=31, random_state=0).fit(X, y)
    pv_w, pv_d = clf.predict_proba(F(w_va))[:, 1], clf.predict_proba(F(d_va))[:, 1]
    thr = min(float(pv_d.max()) + 1e-6, 1.0)
    about = {
        "trainer": "tools/hwrtrain/train_classifier.py", "sklearn": __import__("sklearn").__version__,
        "train": {"writing": len(w_tr), "drawing": len(d_tr)}, "validation": {"writing": len(w_va), "drawing": len(d_va)},
        "validation_writing_recall": round(float((pv_w >= thr).mean()), 4),
        "data": "Quick, Draw! (CC BY 4.0), synthetic diagrams, Hershey fonts (public domain), fhswf/german_handwriting skeletons (AFL-3.0)",
    }
    m = export(clf, thr, about)
    # The exported trees must reproduce scikit-learn's probabilities.
    Xc = F(w_va[:200] + d_va[:200])
    diff = np.abs(predict_exported(m, Xc) - clf.predict_proba(Xc)[:, 1]).max()
    assert diff < 1e-9, diff
    out = os.path.join(ROOT, "assets/hwr/classifier.json")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(m, f, separators=(",", ":"))
    print(json.dumps(about, indent=1), "threshold", round(thr, 6), "export max |Δp|", diff)


if __name__ == "__main__":
    main()
