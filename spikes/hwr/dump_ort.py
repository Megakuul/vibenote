# Dump a real preprocessed line tensor + onnxruntime output for the tract comparison.
import numpy as np, onnxruntime as ort, time, sys
from PIL import Image
m = sys.argv[1]
img = Image.open("/dev/shm/vibenote-hwr/eval/brush/0000.png").convert("RGB")
h = 48; w = int(img.width * h / img.height)
x = (np.asarray(img.resize((w, h)), np.float32) / 255.0 - 0.5) / 0.5
x = x.transpose(2, 0, 1)[None].copy()
so = ort.SessionOptions(); so.intra_op_num_threads = 4
s = ort.InferenceSession(m, so, providers=["CPUExecutionProvider"])
y = s.run(None, {s.get_inputs()[0].name: x})[0]
ts = []
for _ in range(30):
    t = time.time(); s.run(None, {s.get_inputs()[0].name: x}); ts.append((time.time() - t) * 1e3)
x.tofile("/dev/shm/vibenote-hwr/x.f32"); y.astype(np.float32).tofile("/dev/shm/vibenote-hwr/y_ort.f32")
print("input", x.shape, "output", y.shape, "ort ms p50 %.2f" % sorted(ts)[15])
