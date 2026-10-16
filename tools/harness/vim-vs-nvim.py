#!/usr/bin/env python3
"""Differential test of vibenote's vim mode (R-VIM-002) against Neovim.

Random key sequences run on random texts in headless `nvim --clean` (no swap, no shada, its
state in a temp dir) and in the engine (through `vibenote-cli script`); text and cursor are
compared. Prints the differing cases; EMIT=<file.json> writes the matching ones (the source of
the `r_vim_002_matches_neovim` table).

usage: tools/harness/vim-vs-nvim.py N SEED
env:   VIBENOTE_CLI (default $CARGO_TARGET_DIR/debug/vibenote-cli), SHOW (cases to print),
       TEXT_ONLY=1 (only text differences), NT (nvim timeout s), FEATURES=tab (type Tab too),
       NVIM_OPTS (default: the settings vim mode follows)."""
import json, random, subprocess, sys, os, tempfile
import atexit, shutil
HERE = tempfile.mkdtemp(prefix="vim-vs-nvim-")
atexit.register(shutil.rmtree, HERE, True)
os.environ.setdefault("XDG_STATE_HOME", HERE)
CLI = os.environ.get("VIBENOTE_CLI", os.path.join(os.environ.get("CARGO_TARGET_DIR", "target"), "debug", "vibenote-cli"))
N = int(sys.argv[1]); SEED = int(sys.argv[2])
NVIM_OPTS = os.environ.get("NVIM_OPTS", "set autoindent smarttab sw=4 sts=4 et")
FEATURES = set(os.environ.get("FEATURES", "basic").split(","))
rnd = random.Random(SEED)
WORDS = ["foo", "bar", "baz", "a", "hello", "x1", "qu_ux", "(a,b)", "f(x)", "!", "--", "x.y", "Zed", "AB"]
def text():
    lines = []
    for _ in range(rnd.randint(1, 6)):
        r = rnd.random()
        if r < 0.15: lines.append("")
        else:
            ind = " " * rnd.choice([0, 0, 0, 2, 4])
            lines.append(ind + " ".join(rnd.choice(WORDS) for _ in range(rnd.randint(1, 6))))
    return lines
CH = "abfoxz(.,_"
def count():
    return str(rnd.randint(2, 3)) if rnd.random() < 0.2 else ""
def motion():
    m = rnd.choice(["h","j","k","l","w","b","e","W","B","E","0","^","$","gg","G","f","t","F","T",";"])
    if m in "ftFT": m += rnd.choice(CH)
    return m
def ins_text():
    out = []
    for _ in range(rnd.randint(0, 5)):
        r = rnd.random()
        if r < 0.1: out.append("<cr>")
        elif r < 0.17: out.append("<bs>")
        elif r < 0.22 and "tab" in FEATURES: out.append("<tab>")
        else: out.append(rnd.choice("abcxyz ().-"))
    return out + ["<esc>"]
def command():
    """A list of feeds; each feed is a token list nvim runs at once (no failure inside)."""
    r = rnd.random()
    if r < 0.33:
        m = motion()
        return [[("" if m == "0" else count()) + m]]
    if r < 0.5:
        op = rnd.choice("dy><")
        t = rnd.random()
        if t < 0.25: tgt = op
        elif t < 0.4: tgt = rnd.choice(["iw","aw","ip","ap","iW","aW"])
        else: tgt = motion()
        return [[count() + op + ("" if tgt == "0" else count()) + tgt]]
    if r < 0.58:
        # changes that cannot fail, then typing
        tgt = rnd.choice(["c", "iw", "aw", "iW", "$", "0", "^", "w", "W", "e"])
        return [["c" + tgt] + ins_text()]
    if r < 0.72:
        c = rnd.choice(["x","X","~","J","p","P","u","<c-r>","r","s","S","C"])
        if c == "r": c += rnd.choice("Qz.")
        if c in ("s", "S", "C"): return [[c] + ins_text()]
        return [[(count() if c not in ("u","<c-r>") else "") + c]]
    if r < 0.88:
        return [[rnd.choice(["i","a","I","A","o","O"])] + ins_text()]
    v = rnd.choice(["v","V"])
    feeds = [[v]] + [[motion()] for _ in range(rnd.randint(1, 3))]
    end = rnd.choice(["d","c","y",">","<","x","~","J","<esc>"])
    feeds.append([end] + (ins_text() if end == "c" else []))
    return feeds
def case():
    t = text()
    units = [f for _ in range(rnd.randint(1, 5)) for f in command()]
    return t, units
cases = [case() for _ in range(N)][int(os.environ.get("START","0")):]
# --- nvim
def vimkeys(keys):
    m = {"<esc>": "<Esc>", "<cr>": "<CR>", "<bs>": "<BS>", "<c-r>": "<C-r>", "<tab>": "<Tab>"}
    return "".join(m.get(k, k) for k in keys)
def run_nvim(chunk):
    lua = ["local out = {}", f"vim.cmd([[{NVIM_OPTS}]])"]
    for t, units in chunk:
        lua.append("local prev = vim.api.nvim_get_current_buf() vim.cmd('enew!') pcall(vim.cmd, 'bwipeout! ' .. prev) vim.bo.undolevels=-1")
        lua.append(f"vim.api.nvim_buf_set_lines(0, 0, -1, false, vim.json.decode({json.dumps(json.dumps(t))}))")
        lua.append("vim.bo.undolevels=1000 vim.api.nvim_win_set_cursor(0, {1, 0})")
        for u in units:
            lua.append(f"vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes({json.dumps(vimkeys(u))}, true, false, true), 'xt!', false)")
        lua.append("vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>', true, false, true), 'xt', false)")
        lua.append("local c = vim.api.nvim_win_get_cursor(0) table.insert(out, vim.json.encode({lines = vim.api.nvim_buf_get_lines(0, 0, -1, false), row = c[1], col = c[2]}))")
    lua.append(f"vim.fn.writefile(out, '{HERE}/nvim.out') vim.cmd('qa!')")
    open(f"{HERE}/run.lua", "w").write("\n".join(lua))
    subprocess.run(["nvim", "-n", "-i", "NONE", "--clean", "--headless", "-c", f"luafile {HERE}/run.lua"], check=True, timeout=int(os.environ.get("NT","600")), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return [json.loads(l) for l in open(f"{HERE}/nvim.out")]
def safe(chunk):
    try:
        return run_nvim(chunk)
    except subprocess.TimeoutExpired:
        if len(chunk) == 1:
            print("nvim hangs on", chunk[0][1]); return [None]
        h = len(chunk) // 2
        return safe(chunk[:h]) + safe(chunk[h:])
nv = []
for c0 in range(0, len(cases), 100):
    nv += safe(cases[c0:c0 + 100])

# --- vibenote
req = []; rid = [0]
def ex(cmd, args):
    rid[0] += 1; req.append(json.dumps({"id": rid[0], "method": "engine.execute", "params": {"cmd": cmd, "args": args}})); return rid[0]
ex("settings.set", {"vim": True})
marks = []
oid = 0
tms = 0
for i, (t, units) in enumerate(cases):
    keys = [k for u in units for k in u]
    ex("workspace.note.create", {"name": f"n{i}"}); oid += 1; note = oid
    ex("note.open", {"id": note})
    ex("text.create", {"at": [0, 0], "width": 4000, "content": "\n".join(t)}); oid += 1; box = oid
    ex("text.open", {"id": box, "offset": 0})
    last = None
    for k in keys:
        toks = []
        if k.startswith("<"): toks = [k]
        else: toks = list(k)
        for tk in toks:
            tms += 50
            p = {"<esc>": {"key": "escape"}, "<cr>": {"key": "enter"}, "<bs>": {"key": "backspace"}, "<tab>": {"key": "tab"}, "<c-r>": {"text": "r", "ctrl": True}}.get(tk, {"text": tk})
            p = dict(p); p["t_ms"] = tms
            last = ex("text.input", p)
    tms += 2000
    g = ex("text.get", {"id": box})
    st = rid[0] + 1; req.append(json.dumps({"id": st, "method": "state.inspect", "params": {"pointer": "/text_edit"}})); rid[0] = st
    marks.append((g, st))
    # leave editing so the next note starts clean
    ex("text.input", {"key": "escape"}); ex("text.input", {"key": "escape"})
p = subprocess.run([CLI, "script", "-"], input="\n".join(req) + "\n", capture_output=True, text=True, timeout=600)
res = {}
for l in p.stdout.splitlines():
    try: d = json.loads(l); res[d["id"]] = d
    except Exception: pass
bad = 0
cursor_only = [0]
good = []
for i, ((t, units), n, (g, st)) in enumerate(zip(cases, nv, marks)):
    keys = [" ".join(u) for u in units]
    if n is None: continue
    gc = res.get(g, {}).get("result", {}) or {}
    content = gc.get("content")
    ed = res.get(st, {}).get("result")
    want = "\n".join(n["lines"])
    lines = want.split("\n")
    woff = sum(len(l) + 1 for l in lines[: n["row"] - 1]) + n["col"]
    got_off = ed["caret"] if isinstance(ed, dict) else None
    flat_keys = "".join(k if k in ("<esc>","<cr>","<bs>","<c-r>","<tab>") else k.replace("<", "<lt>") for u in units for k in u)
    flat = "".join(k for u in units for k in u if not k.startswith("<") or k == "<lt>")
    fill = [i for i, ch in enumerate(flat) if ch in "ydcxsXDCS"]
    put = [i for i, ch in enumerate(flat) if ch in "pP"]
    if put and (not fill or put[0] < fill[0]): continue
    if ed is None: continue  # Esc in Normal mode ends our editing (vim just beeps)
    if content == want and got_off != woff: cursor_only[0] += 1
    ok = content == want and got_off == woff
    if ok and os.environ.get("EMIT"):
        good.append({"text": t, "keys": flat_keys, "want": want, "cursor": woff})
    if os.environ.get("TEXT_ONLY") and content == want: continue
    if not ok:
        bad += 1
        if bad <= int(os.environ.get("SHOW", "15")):
            print(f"--- case {i}: keys {' | '.join(keys)}")
            print(f"text   {t!r}")
            print(f"nvim   {want!r} cursor {woff}")
            print(f"ours   {content!r} cursor {got_off}  {('' if isinstance(ed, dict) else ed)}")
if os.environ.get("EMIT"): json.dump(good, open(os.environ["EMIT"], "w"))
print(f"{bad}/{N} differ ({cursor_only[0]} only in the cursor)")
