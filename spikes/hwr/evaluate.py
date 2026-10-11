"""Line-level HWR evaluation. Usage: python evaluate.py <model> <set> [n]
Prints one JSON line: CER/WER (case-sensitive and case-insensitive), latency per line, params.
Models never use trust_remote_code."""
import json, os, sys, time
import jiwer
from PIL import Image

ROOT = "/dev/shm/vibenote-hwr/eval"
THREADS = int(os.environ.get("THREADS", "4"))

def load_set(name, n):
    rows = [l.rstrip("\n").split("\t", 1) for l in open(f"{ROOT}/{name}/gt.tsv")]
    return [(f"{ROOT}/{name}/{i}.png", t) for i, t in rows[:n]]

class TrOCR:
    def __init__(self, repo):
        import torch
        from transformers import TrOCRProcessor, VisionEncoderDecoderModel
        torch.set_num_threads(THREADS)
        self.p = TrOCRProcessor.from_pretrained(repo)
        self.m = VisionEncoderDecoderModel.from_pretrained(repo).eval()
        self.params = sum(x.numel() for x in self.m.parameters())
    def __call__(self, img):
        import torch
        with torch.inference_mode():
            pv = self.p(images=img, return_tensors="pt").pixel_values
            ids = self.m.generate(pv, max_new_tokens=96, num_beams=1)
        return self.p.batch_decode(ids, skip_special_tokens=True)[0]

class GOT:
    def __init__(self, repo="stepfun-ai/GOT-OCR-2.0-hf"):
        import torch
        from transformers import AutoProcessor, AutoModelForImageTextToText
        torch.set_num_threads(THREADS)
        self.p = AutoProcessor.from_pretrained(repo)
        self.m = AutoModelForImageTextToText.from_pretrained(repo, dtype=torch.float32).eval()
        self.params = sum(x.numel() for x in self.m.parameters())
    def __call__(self, img):
        import torch
        with torch.inference_mode():
            inp = self.p(img, return_tensors="pt")
            out = self.m.generate(**inp, do_sample=False, tokenizer=self.p.tokenizer, stop_strings="<|im_end|>", max_new_tokens=96)
        return self.p.decode(out[0, inp["input_ids"].shape[1]:], skip_special_tokens=True)

class Rapid:
    """PaddleOCR PP-OCRv5 recognition via RapidOCR (ONNX Runtime). Recognition only (lines are pre-cropped)."""
    def __init__(self, lang, version="PPOCRV5", mtype="MOBILE"):
        from rapidocr import RapidOCR, LangRec, OCRVersion, ModelType
        self.e = RapidOCR(params={"Global.use_det": False, "Global.use_cls": False,
                                  "Rec.lang_type": getattr(LangRec, lang), "Rec.ocr_version": getattr(OCRVersion, version),
                                  "Rec.model_type": getattr(ModelType, mtype), "EngineConfig.onnxruntime.intra_op_num_threads": THREADS})
        self.params = None
    def __call__(self, img):
        import numpy as np
        r = self.e(np.array(img), use_det=False, use_cls=False, use_rec=True)
        return " ".join(r.txts) if r.txts else ""

MODELS = {
    "trocr-small": lambda: TrOCR("microsoft/trocr-small-handwritten"),
    "trocr-base": lambda: TrOCR("microsoft/trocr-base-handwritten"),
    "trocr-de-fhswf": lambda: TrOCR("fhswf/TrOCR_german_handwritten"),
    "got-ocr2": lambda: GOT(),
    "ppocrv5-latin": lambda: Rapid("LATIN"),
    "ppocrv5-en": lambda: Rapid("EN"),
    "ppocrv6-small": lambda: Rapid("CH", "PPOCRV6", "SMALL"),
    "ppocrv6-medium": lambda: Rapid("CH", "PPOCRV6", "MEDIUM"),
}

def main():
    model, set_, n = sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 400
    t0 = time.time(); m = MODELS[model](); load_s = time.time() - t0
    data = load_set(set_, n)
    hyp, ref, lat = [], [], []
    m(Image.open(data[0][0]).convert("RGB"))  # warm-up
    for path, gt in data:
        img = Image.open(path).convert("RGB")
        t = time.time(); h = m(img); lat.append((time.time() - t) * 1e3)
        hyp.append(h.strip()); ref.append(gt.strip())
    lat.sort()
    norm = lambda s: " ".join(s.lower().split())
    res = {"model": model, "set": set_, "n": len(data), "threads": THREADS, "params_M": round(m.params / 1e6, 1) if m.params else None,
           "cer": round(jiwer.cer(ref, hyp), 4), "wer": round(jiwer.wer(ref, hyp), 4),
           "cer_ci": round(jiwer.cer([norm(r) for r in ref], [norm(h) for h in hyp]), 4),
           "lat_ms": {"p50": round(lat[len(lat) // 2], 1), "p95": round(lat[int(len(lat) * .95)], 1)}, "load_s": round(load_s, 1),
           "examples": [[ref[i], hyp[i]] for i in range(3)]}
    print(json.dumps(res, ensure_ascii=False))

if __name__ == "__main__":
    main()
