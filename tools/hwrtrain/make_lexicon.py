"""Word lexicon for the de/en language pack (M9.5, ADR 0006 step (a)).

Sources (permissive, see ATTRIBUTION.md):
  - Tatoeba sentences, German and English per-language exports (CC BY 2.0 FR):
    https://downloads.tatoeba.org/exports/per_language/{deu,eng}/{deu,eng}_sentences.tsv.bz2
  - SCOWL word lists up to size 70 (Kevin Atkinson; permissive notice), e.g. nixpkgs `scowl`
    (`$(nix-build '<nixpkgs>' -A scowl --no-out-link)/share/scowl`).

Score per word: Tatoeba words (seen at least twice) by frequency rank r within their language,
-(1 + log10 r); words only in SCOWL, -(1 + level / 10); a word in several lists keeps its best
score. The decoder adds the score of every completed word to the CTC log-probability (weights
in the pack manifest, tuned on dev lines that are not in tests/eval).

Usage: python -I make_lexicon.py <deu_sentences.tsv.bz2> <eng_sentences.tsv.bz2> <scowl dir> <out.tsv>
"""
import bz2, collections, math, os, re, sys

WORD = re.compile(r"[^\W\d_]+(?:['’-][^\W\d_]+)*")
MAX_LEN = 40


def tatoeba(path):
    c = collections.Counter()
    for line in bz2.open(path, "rt", encoding="utf-8", errors="replace"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) >= 3:
            c.update(WORD.findall(parts[2]))
    words = [(w, n) for w, n in c.items() if n >= 2 and len(w) <= MAX_LEN]
    words.sort(key=lambda wn: (-wn[1], wn[0]))
    return {w: -(1.0 + math.log10(r)) for r, (w, _) in enumerate(words, 1)}


def scowl(d):
    best = {}
    for f in sorted(os.listdir(d)):
        cat, _, lvl = f.rpartition(".")
        if not lvl.isdigit() or int(lvl) > 70:
            continue
        if not cat.startswith(("english", "american", "british")):
            continue
        if not cat.endswith(("words", "upper", "contractions", "proper-names", "abbreviations")):
            continue
        for w in open(os.path.join(d, f), encoding="latin-1"):
            w = w.strip()
            if w and len(w) <= MAX_LEN:
                best[w] = min(best.get(w, 99), int(lvl))
    return {w: -(1.0 + l / 10.0) for w, l in best.items()}


def main():
    deu, eng, sc, out = sys.argv[1:5]
    best = {}
    for part in (tatoeba(deu), tatoeba(eng), scowl(sc)):
        for w, s in part.items():
            best[w] = max(best.get(w, -99.0), s)
    with open(out, "w", encoding="utf-8") as o:
        for w in sorted(best):
            o.write(f"{w}\t{best[w]:.3f}\n")
    print(out, len(best), "words")


if __name__ == "__main__":
    main()
