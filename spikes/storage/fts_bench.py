# FTS5 search benchmark for R-PERF-005 (reference workspace: 10k notes). Uses Python's sqlite3
# (system SQLite) only as a quick probe of SQLite FTS5 behaviour; product uses bundled SQLite.
import sqlite3, random, time, os, sys
random.seed(3)
p = 'data/fts.sqlite'
if os.path.exists(p): os.remove(p)
c = sqlite3.connect(p)
print('sqlite', sqlite3.sqlite_version)
c.execute("CREATE VIRTUAL TABLE t USING fts5(note UNINDEXED, block UNINDEXED, body, tokenize='unicode61 remove_diacritics 2')")
vocab = [w.strip() for w in open('/usr/share/dict/words')] if os.path.exists('/usr/share/dict/words') else None
de = "Gegeben sei die Funktion Ableitung Integral Übung Größe Grenzwert Folge Reihe Matrix Vektor Eigenwert Lösung Beweis Satz Definition Beispiel Aufgabe Vorlesung Prüfung".split()
en = "given function derivative integral exercise limit sequence series matrix vector eigenvalue solution proof theorem definition example lecture exam".split()
syl = ["ka","ri","to","ne","mu","sa","lo","pe","di","va","zu","ho","ge","bi","fa","ru"]
words = de + en + ["".join(random.choice(syl) for _ in range(random.randint(2,4))) for _ in range(20000)]
t = time.time()
rows = []
for n in range(10000):
    for b in range(10):               # 10 recognised blocks per note
        rows.append((n, b, " ".join(random.choice(words) for _ in range(50))))
c.executemany("INSERT INTO t VALUES (?,?,?)", rows); c.commit()
print('indexed 100k blocks / 5.0M words in %.1f s, db %.0f MB' % (time.time()-t, os.path.getsize(p)/1e6))
qs = ['Grösse', 'eigenwert', 'Lösung Beweis', '"Gegeben sei"', 'kari*', 'theorem AND proof', 'Ubung']
for q in qs:
    ts=[]
    for _ in range(5):
        t=time.time(); r=c.execute("SELECT note, block, snippet(t,2,'[',']','…',8) FROM t WHERE t MATCH ? ORDER BY rank LIMIT 50",(q,)).fetchall(); ts.append((time.time()-t)*1e3)
    cnt = c.execute("SELECT count(*) FROM t WHERE t MATCH ?",(q,)).fetchone()[0]
    print(f'{q!r:20} top50 p50 {sorted(ts)[2]:6.1f} ms  max {max(ts):6.1f} ms  matches {cnt}')
