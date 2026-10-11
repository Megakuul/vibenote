//! Phase 2 spike 4 — storage at scale + crash safety.
//!
//!   st bench <sqlite|redb> <dir>              workload benchmark → JSON
//!   st writer <sqlite|redb> <dir>             endless writer; prints "ACK <id>" after each durable commit
//!   st verify <sqlite|redb> <dir> <ack_max>   integrity + all ids ≤ ack_max present → exit 0/1
//!   st blobw <dir>                            endless blob writer (temp+fsync+rename+dirfsync), prints ACK <hash>
//!   st blobv <dir> <acks-file>                every acked blob exists and hashes correctly
//!
//! Stroke encoding: header (u32 n) + per sample zigzag-varint deltas of x,y (1/16 unit fixed point),
//! pressure (u16), tilt x/y (i8), dt (µs/100).

use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::time::Instant;

// ---------- data ----------
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}
struct Sample {
    x: f64,
    y: f64,
    p: f64,
    tx: f64,
    ty: f64,
    dt_us: u32,
}
fn stroke(i: usize, r: &mut Rng) -> (Vec<Sample>, [f64; 4]) {
    let per_page = 44 * 12;
    let (pg, k) = (i / per_page, i % per_page);
    let (line, word) = (k / 12, k % 12);
    let ox = (pg % 14) as f64 * 1600.0 + 60.0 + word as f64 * 110.0;
    let oy = (pg / 14) as f64 * 2200.0 + 80.0 + line as f64 * 44.0;
    let n = 40 + (r.f() * 80.0) as usize;
    let (w, loops, ph) = (60.0 + r.f() * 40.0, 3.0 + r.f() * 4.0, r.f() * 6.28);
    let mut bb = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let s: Vec<Sample> = (0..n)
        .map(|j| {
            let u = j as f64 / (n - 1) as f64;
            let a = loops * std::f64::consts::TAU * u;
            let x = ox + w * u - 5.0 * a.sin();
            let y = oy - 9.0 * (1.0 - (a + ph).cos());
            bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
            Sample { x, y, p: 0.35 + 0.4 * (u * 3.1 + ph).sin().abs(), tx: 15.0 + 5.0 * u, ty: -10.0, dt_us: 5000 }
        })
        .collect();
    (s, bb)
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn encode(s: &[Sample]) -> Vec<u8> {
    let mut o = Vec::with_capacity(s.len() * 8 + 8);
    o.extend_from_slice(&(s.len() as u32).to_le_bytes());
    let (mut px, mut py, mut pp) = (0i64, 0i64, 0i64);
    for q in s {
        let (x, y, p) = ((q.x * 16.0).round() as i64, (q.y * 16.0).round() as i64, (q.p * 65535.0) as i64);
        varint(&mut o, zz(x - px));
        varint(&mut o, zz(y - py));
        varint(&mut o, zz(p - pp));
        o.push(q.tx.round() as i8 as u8);
        o.push(q.ty.round() as i8 as u8);
        varint(&mut o, (q.dt_us / 100) as u64);
        (px, py, pp) = (x, y, p);
    }
    o
}
fn decode_len(b: &[u8]) -> Result<usize> {
    // Full decode to measure realistic load cost; returns sample count.
    if b.len() < 4 {
        bail!("short");
    }
    let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let mut i = 4;
    let rd = |i: &mut usize| -> Result<u64> {
        let mut v = 0u64;
        let mut sh = 0;
        loop {
            let c = *b.get(*i).ok_or_else(|| anyhow!("eof"))?;
            *i += 1;
            v |= ((c & 0x7f) as u64) << sh;
            if c < 0x80 {
                return Ok(v);
            }
            sh += 7;
            if sh > 63 {
                bail!("varint");
            }
        }
    };
    let mut acc = 0i64;
    for _ in 0..n {
        for _ in 0..3 {
            let v = rd(&mut i)?;
            acc = acc.wrapping_add(((v >> 1) as i64) ^ -((v & 1) as i64));
        }
        i += 2;
        rd(&mut i)?;
    }
    std::hint::black_box(acc);
    Ok(n)
}

// ---------- backends ----------
trait Store {
    fn put_batch(&mut self, note: u64, items: &[(u64, Vec<u8>, [f64; 4])]) -> Result<()>;
    fn load_note(&self, note: u64) -> Result<(usize, usize)>; // strokes, samples
    fn load_viewport(&self, note: u64, r: [f64; 4]) -> Result<(usize, usize)>;
    fn put_tree(&mut self, folders: usize, notes: usize) -> Result<()>;
    fn list_folder(&self, folder: u64) -> Result<usize>;
    fn max_id(&self, note: u64) -> Result<Option<u64>>;
    fn has_all(&self, note: u64, upto: u64) -> Result<bool>;
    fn integrity(&self) -> Result<String>;
}

mod sq {
    use super::*;
    use rusqlite::{params, Connection};
    pub struct Sq(pub Connection);
    pub fn open(dir: &str) -> Result<Sq> {
        let c = Connection::open(format!("{dir}/ws.sqlite"))?;
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.pragma_update(None, "synchronous", "FULL")?;
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS stroke(id INTEGER PRIMARY KEY, note INTEGER NOT NULL, data BLOB NOT NULL);
             CREATE INDEX IF NOT EXISTS stroke_note ON stroke(note);
             CREATE VIRTUAL TABLE IF NOT EXISTS stroke_rt USING rtree(id, minx, maxx, miny, maxy);
             CREATE TABLE IF NOT EXISTS folder(id INTEGER PRIMARY KEY, parent INTEGER, name TEXT, color INTEGER, sort REAL);
             CREATE TABLE IF NOT EXISTS note(id INTEGER PRIMARY KEY, folder INTEGER, name TEXT, color INTEGER, sort REAL);
             CREATE INDEX IF NOT EXISTS note_folder ON note(folder, sort);",
        )?;
        Ok(Sq(c))
    }
    impl Store for Sq {
        fn put_batch(&mut self, note: u64, items: &[(u64, Vec<u8>, [f64; 4])]) -> Result<()> {
            let tx = self.0.transaction()?;
            {
                let mut a = tx.prepare_cached("INSERT INTO stroke(id,note,data) VALUES(?,?,?)")?;
                let mut b = tx.prepare_cached("INSERT INTO stroke_rt VALUES(?,?,?,?,?)")?;
                for (id, d, bb) in items {
                    a.execute(params![*id as i64, note as i64, d])?;
                    b.execute(params![*id as i64, bb[0], bb[2], bb[1], bb[3]])?;
                }
            }
            tx.commit()?;
            Ok(())
        }
        fn load_note(&self, note: u64) -> Result<(usize, usize)> {
            let mut st = self.0.prepare_cached("SELECT data FROM stroke WHERE note=?")?;
            let mut rows = st.query([note as i64])?;
            let (mut n, mut s) = (0, 0);
            while let Some(r) = rows.next()? {
                let b: &[u8] = r.get_ref(0)?.as_blob()?;
                s += decode_len(b)?;
                n += 1;
            }
            Ok((n, s))
        }
        fn load_viewport(&self, _note: u64, r: [f64; 4]) -> Result<(usize, usize)> {
            let mut st = self.0.prepare_cached("SELECT s.data FROM stroke_rt t JOIN stroke s ON s.id=t.id WHERE t.maxx>=? AND t.minx<=? AND t.maxy>=? AND t.miny<=?")?;
            let mut rows = st.query(params![r[0], r[2], r[1], r[3]])?;
            let (mut n, mut s) = (0, 0);
            while let Some(row) = rows.next()? {
                s += decode_len(row.get_ref(0)?.as_blob()?)?;
                n += 1;
            }
            Ok((n, s))
        }
        fn put_tree(&mut self, folders: usize, notes: usize) -> Result<()> {
            let tx = self.0.transaction()?;
            {
                let mut f = tx.prepare("INSERT INTO folder VALUES(?,?,?,?,?)")?;
                for i in 0..folders {
                    let parent = if i < 20 { None } else { Some((i % 20) as i64) };
                    f.execute(params![i as i64, parent, format!("Folder {i}"), (i % 8) as i64, i as f64])?;
                }
                let mut n = tx.prepare("INSERT INTO note VALUES(?,?,?,?,?)")?;
                for i in 0..notes {
                    n.execute(params![i as i64 + 1_000_000, (i % folders) as i64, format!("Note {i} Vorlesung"), 0i64, i as f64])?;
                }
            }
            tx.commit()?;
            Ok(())
        }
        fn list_folder(&self, folder: u64) -> Result<usize> {
            let mut st = self.0.prepare_cached("SELECT id,name,color FROM note WHERE folder=? ORDER BY sort")?;
            Ok(st.query_map([folder as i64], |_| Ok(()))?.count())
        }
        fn max_id(&self, note: u64) -> Result<Option<u64>> {
            Ok(self.0.query_row("SELECT max(id) FROM stroke WHERE note=?", [note as i64], |r| r.get::<_, Option<i64>>(0))?.map(|v| v as u64))
        }
        fn has_all(&self, note: u64, upto: u64) -> Result<bool> {
            let c: i64 = self.0.query_row("SELECT count(*) FROM stroke WHERE note=? AND id<=?", params![note as i64, upto as i64], |r| r.get(0))?;
            let rt: i64 = self.0.query_row("SELECT count(*) FROM stroke_rt WHERE id<=?", params![upto as i64], |r| r.get(0))?;
            Ok(c as u64 == upto + 1 && rt as u64 == upto + 1)
        }
        fn integrity(&self) -> Result<String> {
            Ok(self.0.query_row("PRAGMA integrity_check", [], |r| r.get(0))?)
        }
    }
}

mod rd {
    use super::*;
    use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
    const STROKE: TableDefinition<(u64, u64), &[u8]> = TableDefinition::new("stroke");
    const CHUNK: TableDefinition<(u64, i32, i32, u64), ()> = TableDefinition::new("chunk"); // note, cx, cy, id
    const NOTE: TableDefinition<(u64, u64), &str> = TableDefinition::new("note"); // folder, id -> name
    pub struct Rd(pub Database);
    pub fn open(dir: &str) -> Result<Rd> {
        let db = Database::create(format!("{dir}/ws.redb"))?;
        let w = db.begin_write()?;
        w.open_table(STROKE)?;
        w.open_table(CHUNK)?;
        w.open_table(NOTE)?;
        w.commit()?;
        Ok(Rd(db))
    }
    fn chunks(bb: [f64; 4]) -> impl Iterator<Item = (i32, i32)> {
        let c = |v: f64| (v / 2048.0).floor() as i32;
        let (x0, y0, x1, y1) = (c(bb[0]), c(bb[1]), c(bb[2]), c(bb[3]));
        (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
    }
    impl Store for Rd {
        fn put_batch(&mut self, note: u64, items: &[(u64, Vec<u8>, [f64; 4])]) -> Result<()> {
            let w = self.0.begin_write()?;
            {
                let mut s = w.open_table(STROKE)?;
                let mut c = w.open_table(CHUNK)?;
                for (id, d, bb) in items {
                    s.insert((note, *id), d.as_slice())?;
                    for (cx, cy) in chunks(*bb) {
                        c.insert((note, cx, cy, *id), ())?;
                    }
                }
            }
            w.commit()?;
            Ok(())
        }
        fn load_note(&self, note: u64) -> Result<(usize, usize)> {
            let r = self.0.begin_read()?;
            let t = r.open_table(STROKE)?;
            let (mut n, mut s) = (0, 0);
            for e in t.range((note, 0)..(note + 1, 0))? {
                let (_, v) = e?;
                s += decode_len(v.value())?;
                n += 1;
            }
            Ok((n, s))
        }
        fn load_viewport(&self, note: u64, rr: [f64; 4]) -> Result<(usize, usize)> {
            let r = self.0.begin_read()?;
            let c = r.open_table(CHUNK)?;
            let t = r.open_table(STROKE)?;
            let mut ids = std::collections::BTreeSet::new();
            let cl = |v: f64| (v / 2048.0).floor() as i32;
            for cx in cl(rr[0])..=cl(rr[2]) {
                for e in c.range((note, cx, cl(rr[1]), 0)..=(note, cx, cl(rr[3]), u64::MAX))? {
                    ids.insert(e?.0.value().3);
                }
            }
            let (mut n, mut s) = (0, 0);
            for id in ids {
                if let Some(v) = t.get((note, id))? {
                    s += decode_len(v.value())?;
                    n += 1;
                }
            }
            Ok((n, s))
        }
        fn put_tree(&mut self, folders: usize, notes: usize) -> Result<()> {
            let w = self.0.begin_write()?;
            {
                let mut t = w.open_table(NOTE)?;
                for i in 0..notes {
                    t.insert(((i % folders) as u64, i as u64), format!("Note {i} Vorlesung").as_str())?;
                }
            }
            w.commit()?;
            Ok(())
        }
        fn list_folder(&self, folder: u64) -> Result<usize> {
            let r = self.0.begin_read()?;
            Ok(r.open_table(NOTE)?.range((folder, 0)..(folder + 1, 0))?.count())
        }
        fn max_id(&self, note: u64) -> Result<Option<u64>> {
            let r = self.0.begin_read()?;
            let t = r.open_table(STROKE)?;
            Ok(t.range((note, 0)..(note + 1, 0))?.next_back().transpose()?.map(|(k, _)| k.value().1))
        }
        fn has_all(&self, note: u64, upto: u64) -> Result<bool> {
            let r = self.0.begin_read()?;
            let t = r.open_table(STROKE)?;
            Ok(t.range((note, 0)..=(note, upto))?.count() as u64 == upto + 1)
        }
        fn integrity(&self) -> Result<String> {
            Ok("redb: opened (redb verifies checksums / repairs on open)".into())
        }
    }
}

fn open(kind: &str, dir: &str) -> Result<Box<dyn Store>> {
    std::fs::create_dir_all(dir)?;
    Ok(match kind {
        "sqlite" => Box::new(sq::open(dir)?),
        "redb" => Box::new(rd::open(dir)?),
        _ => bail!("kind"),
    })
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn dir_size(d: &str) -> u64 {
    std::fs::read_dir(d).map(|r| r.filter_map(|e| e.ok()?.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

fn bench(kind: &str, dir: &str) -> Result<serde_json::Value> {
    let _ = std::fs::remove_dir_all(dir);
    let mut s = open(kind, dir)?;
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    // 1) live writing: commit after every stroke (pen-up), 2,000 strokes → latency per commit
    let mut commit = vec![];
    let mut raw_bytes = 0usize;
    let mut samples = 0usize;
    for i in 0..2000 {
        let (st, bb) = stroke(i, &mut r);
        samples += st.len();
        let e = encode(&st);
        raw_bytes += e.len();
        let t = Instant::now();
        s.put_batch(1, &[(i as u64, e, bb)])?;
        commit.push(t.elapsed().as_secs_f64() * 1e3);
    }
    // 2) bulk: remaining 98,000 strokes in batches of 500 (import / generator)
    let t = Instant::now();
    let mut batch = vec![];
    for i in 2000..100_000 {
        let (st, bb) = stroke(i, &mut r);
        samples += st.len();
        let e = encode(&st);
        raw_bytes += e.len();
        batch.push((i as u64, e, bb));
        if batch.len() == 500 {
            s.put_batch(1, &batch)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        s.put_batch(1, &batch)?;
    }
    let bulk_s = t.elapsed().as_secs_f64();
    drop(s);
    // 3) cold-ish reopen + loads (page cache warm: same process run)
    let t = Instant::now();
    let s = open(kind, dir)?;
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let vp = s.load_viewport(1, [0.0, 0.0, 1920.0, 1200.0])?;
    let vp_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let all = s.load_note(1)?;
    let all_ms = t.elapsed().as_secs_f64() * 1e3;
    drop(s);
    let mut s = open(kind, dir)?;
    // 4) workspace tree: 100k notes in 2,000 folders
    let t = Instant::now();
    s.put_tree(2000, 100_000)?;
    let tree_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut lf = vec![];
    for f in 0..200u64 {
        let t = Instant::now();
        let n = s.list_folder(f * 7 % 2000)?;
        std::hint::black_box(n);
        lf.push(t.elapsed().as_secs_f64() * 1e3);
    }
    Ok(serde_json::json!({"kind":kind,"strokes":100000,"samples":samples,"encoded_bytes":raw_bytes,"bytes_per_sample":raw_bytes as f64/samples as f64,
        "commit_per_stroke_ms":{"p50":pct(&mut commit,0.5),"p95":pct(&mut commit,0.95),"p99":pct(&mut commit,0.99),"max":pct(&mut commit,1.0)},
        "bulk_98k_s":bulk_s,"db_size_mb":dir_size(dir) as f64/1e6,"open_ms":open_ms,
        "viewport_load":{"ms":vp_ms,"strokes":vp.0,"samples":vp.1},"full_note_load":{"ms":all_ms,"strokes":all.0,"samples":all.1},
        "tree_insert_100k_ms":tree_ms,"list_folder_ms":{"p50":pct(&mut lf,0.5),"max":pct(&mut lf,1.0)}}))
}

fn writer(kind: &str, dir: &str) -> Result<()> {
    let mut s = open(kind, dir)?;
    let mut next = s.max_id(1)?.map(|v| v + 1).unwrap_or(0);
    let mut r = Rng(next.wrapping_add(7));
    let out = std::io::stdout();
    loop {
        let (st, bb) = stroke(next as usize, &mut r);
        let n = 1 + (next % 3); // 1–3 strokes per commit
        let items: Vec<_> = (0..n).map(|k| (next + k, encode(&st), bb)).collect();
        s.put_batch(1, &items)?;
        next += n;
        let mut o = out.lock();
        writeln!(o, "ACK {}", next - 1)?;
        o.flush()?;
    }
}

fn verify(kind: &str, dir: &str, ack: i64) -> Result<()> {
    let s = open(kind, dir)?;
    let integ = s.integrity()?;
    if kind == "sqlite" && integ != "ok" {
        bail!("integrity: {integ}");
    }
    if ack >= 0 && !s.has_all(1, ack as u64)? {
        bail!("missing acked strokes ≤ {ack}");
    }
    // every stored stroke decodes
    s.load_note(1)?;
    Ok(())
}

fn fsync_dir(d: &str) -> Result<()> {
    let f = std::fs::File::open(d)?;
    f.sync_all()?;
    Ok(())
}

fn blobw(dir: &str) -> Result<()> {
    std::fs::create_dir_all(format!("{dir}/blobs/tmp"))?;
    let mut r = Rng(std::process::id() as u64 + 1);
    let out = std::io::stdout();
    loop {
        let n = 1000 + (r.f() * 2_000_000.0) as usize;
        let data: Vec<u8> = (0..n).map(|i| (i as u64 ^ r.0) as u8).collect();
        let _ = r.f();
        let h = blake3::hash(&data).to_hex().to_string();
        let tmp = format!("{dir}/blobs/tmp/{h}.{}", std::process::id());
        let dst_dir = format!("{dir}/blobs/{}", &h[..2]);
        std::fs::create_dir_all(&dst_dir)?;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, format!("{dst_dir}/{h}"))?;
        fsync_dir(&dst_dir)?;
        let mut o = out.lock();
        writeln!(o, "ACK {h}")?;
        o.flush()?;
    }
}

fn blobv(dir: &str, acks: &str) -> Result<()> {
    for l in std::fs::read_to_string(acks)?.lines() {
        let Some(h) = l.strip_prefix("ACK ") else { continue };
        let p = format!("{dir}/blobs/{}/{h}", &h[..2]);
        let d = std::fs::read(&p).map_err(|e| anyhow!("{p}: {e}"))?;
        if blake3::hash(&d).to_hex().as_str() != h {
            bail!("hash mismatch {h}");
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let g = |i: usize| a.get(i).cloned().unwrap_or_default();
    match g(1).as_str() {
        "bench" => println!("{}", bench(&g(2), &g(3))?),
        "writer" => writer(&g(2), &g(3))?,
        "verify" => verify(&g(2), &g(3), g(4).parse()?)?,
        "blobw" => blobw(&g(2))?,
        "blobv" => blobv(&g(2), &g(3))?,
        _ => bail!("usage"),
    }
    Ok(())
}
