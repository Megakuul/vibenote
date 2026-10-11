//! PDF library benchmark. Usage:
//!   pbench pdfium|hayro bench <file.pdf>          → JSON timings
//!   pbench pdfium|hayro one <file.pdf>            → render page 1 + up to 5 pages (robustness probe)
//!   pbench pdfium|hayro png <file.pdf> <page> <out.png>
//! Env: PDFIUM_DIR (dir containing libpdfium.so)

use anyhow::{anyhow, Result};
use std::time::Instant;

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()))
        .unwrap_or(0.0)
        / 1024.0
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    if v.is_empty() { return f64::NAN; }
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn sample_pages(n: usize) -> Vec<usize> {
    let k = 30.min(n);
    (0..k).map(|i| i * n / k).collect()
}

fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()?.write_image_data(rgba)?;
    Ok(())
}

mod pd {
    use super::*;
    use pdfium_render::prelude::*;
    pub fn lib() -> Result<Pdfium> {
        let dir = std::env::var("PDFIUM_DIR").map_err(|_| anyhow!("PDFIUM_DIR"))?;
        Ok(Pdfium::new(Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir)).map_err(|e| anyhow!("{e:?}"))?))
    }
    pub fn bench(path: &str) -> Result<serde_json::Value> {
        let p = lib()?;
        let rss0 = rss_mb();
        let t = Instant::now();
        let doc = p.load_pdf_from_file(path, None).map_err(|e| anyhow!("{e:?}"))?;
        let n = doc.pages().len() as usize;
        let open_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let first = doc.pages().get(0).map_err(|e| anyhow!("{e:?}"))?;
        let _ = first.render_with_config(&PdfRenderConfig::new().set_target_width(1240)).map_err(|e| anyhow!("{e:?}"))?;
        let first_page_ms = t.elapsed().as_secs_f64() * 1e3;
        let (mut r1, mut r2, mut tx) = (vec![], vec![], vec![]);
        for i in sample_pages(n) {
            let pg = doc.pages().get(i as i32).map_err(|e| anyhow!("{e:?}"))?;
            let t = Instant::now();
            let b = pg.render_with_config(&PdfRenderConfig::new().set_target_width(1240)).map_err(|e| anyhow!("{e:?}"))?;
            std::hint::black_box(b.as_raw_bytes().len());
            r1.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            let b = pg.render_with_config(&PdfRenderConfig::new().set_target_width(2480)).map_err(|e| anyhow!("{e:?}"))?;
            std::hint::black_box(b.as_raw_bytes().len());
            r2.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            let s = pg.text().map_err(|e| anyhow!("{e:?}"))?.all();
            std::hint::black_box(s.len());
            tx.push(t.elapsed().as_secs_f64() * 1e3);
        }
        // search check: page 50 contains "Suchwort-50" (text doc only)
        let mut found = serde_json::Value::Null;
        if n >= 50 {
            let s = doc.pages().get(49).map_err(|e| anyhow!("{e:?}"))?.text().map_err(|e| anyhow!("{e:?}"))?.all();
            found = (s.contains("Suchwort-50") && s.contains("zebrafisch49")).into();
        }
        // full-document text extraction time (indexing cost)
        let t = Instant::now();
        let mut chars = 0usize;
        for pg in doc.pages().iter() {
            chars += pg.text().map(|t| t.all().len()).unwrap_or(0);
        }
        let all_text_ms = t.elapsed().as_secs_f64() * 1e3;
        Ok(serde_json::json!({"lib":"pdfium","pages":n,"open_ms":open_ms,"first_page_1240_ms":first_page_ms,
            "render_1240_ms":{"p50":pct(&mut r1,0.5),"p95":pct(&mut r1,0.95),"max":pct(&mut r1,1.0)},
            "render_2480_ms":{"p50":pct(&mut r2,0.5),"p95":pct(&mut r2,0.95),"max":pct(&mut r2,1.0)},
            "text_page_ms":{"p50":pct(&mut tx,0.5),"max":pct(&mut tx,1.0)},"all_text_ms":all_text_ms,"text_chars":chars,
            "search_page50_ok":found,"rss_delta_mb":rss_mb()-rss0}))
    }
    pub fn one(path: &str) -> Result<()> {
        let p = lib()?;
        let doc = p.load_pdf_from_file(path, None).map_err(|e| anyhow!("{e:?}"))?;
        let n = doc.pages().len();
        for i in 0..n.min(5) {
            let pg = doc.pages().get(i).map_err(|e| anyhow!("{e:?}"))?;
            let _ = pg.render_with_config(&PdfRenderConfig::new().set_target_width(600)).map_err(|e| anyhow!("{e:?}"))?;
            let _ = pg.text().map(|t| t.all());
        }
        Ok(())
    }
    pub fn png(path: &str, page: i32, out: &str) -> Result<()> {
        let p = lib()?;
        let doc = p.load_pdf_from_file(path, None).map_err(|e| anyhow!("{e:?}"))?;
        let pg = doc.pages().get(page).map_err(|e| anyhow!("{e:?}"))?;
        let b = pg.render_with_config(&PdfRenderConfig::new().set_target_width(1240)).map_err(|e| anyhow!("{e:?}"))?;
        let (w, h) = (b.width() as u32, b.height() as u32);
        write_png(out, w, h, &b.as_rgba_bytes())
    }
}

mod hy {
    use super::*;
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    use hayro::{render, PixmapSettings, RenderCache, RenderSettings};
    fn scale_for(page: &hayro::hayro_syntax::page::Page, w: f32) -> f32 {
        let (pw, _) = page.render_dimensions();
        w / pw
    }
    pub fn bench(path: &str) -> Result<serde_json::Value> {
        let rss0 = rss_mb();
        let t = Instant::now();
        let data = std::fs::read(path)?;
        let pdf = Pdf::new(data).map_err(|e| anyhow!("{e:?}"))?;
        let pages = pdf.pages();
        let n = pages.len();
        let open_ms = t.elapsed().as_secs_f64() * 1e3;
        let cache = RenderCache::new();
        let is = InterpreterSettings::default();
        let rs = RenderSettings::default();
        let rd = |i: usize, w: f32| {
            let pg = &pages[i];
            let s = scale_for(pg, w);
            let px = render(pg, &cache, &is, &rs, &PixmapSettings { x_scale: s, y_scale: s, bg_color: WHITE });
            std::hint::black_box(px.width());
        };
        let t = Instant::now();
        rd(0, 1240.0);
        let first_page_ms = t.elapsed().as_secs_f64() * 1e3;
        let (mut r1, mut r2) = (vec![], vec![]);
        for i in sample_pages(n) {
            let t = Instant::now();
            rd(i, 1240.0);
            r1.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            rd(i, 2480.0);
            r2.push(t.elapsed().as_secs_f64() * 1e3);
        }
        Ok(serde_json::json!({"lib":"hayro","pages":n,"open_ms":open_ms,"first_page_1240_ms":first_page_ms,
            "render_1240_ms":{"p50":pct(&mut r1,0.5),"p95":pct(&mut r1,0.95),"max":pct(&mut r1,1.0)},
            "render_2480_ms":{"p50":pct(&mut r2,0.5),"p95":pct(&mut r2,0.95),"max":pct(&mut r2,1.0)},
            "text":"not provided by hayro","rss_delta_mb":rss_mb()-rss0}))
    }
    pub fn one(path: &str) -> Result<()> {
        let data = std::fs::read(path)?;
        let pdf = Pdf::new(data).map_err(|e| anyhow!("{e:?}"))?;
        let pages = pdf.pages();
        let cache = RenderCache::new();
        for i in 0..pages.len().min(5) {
            let s = scale_for(&pages[i], 600.0);
            let _ = render(&pages[i], &cache, &InterpreterSettings::default(), &RenderSettings::default(), &PixmapSettings { x_scale: s, y_scale: s, bg_color: WHITE });
        }
        Ok(())
    }
    pub fn png(path: &str, page: usize, out: &str) -> Result<()> {
        let pdf = Pdf::new(std::fs::read(path)?).map_err(|e| anyhow!("{e:?}"))?;
        let pages = pdf.pages();
        let s = scale_for(&pages[page], 1240.0);
        let px = render(&pages[page], &RenderCache::new(), &InterpreterSettings::default(), &RenderSettings::default(), &PixmapSettings { x_scale: s, y_scale: s, bg_color: WHITE });
        let data: Vec<u8> = px.data_as_u8_slice().to_vec();
        write_png(out, px.width() as u32, px.height() as u32, &data)
    }
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (lib, mode, file) = (a.get(1).cloned().unwrap_or_default(), a.get(2).cloned().unwrap_or_default(), a.get(3).cloned().unwrap_or_default());
    match (lib.as_str(), mode.as_str()) {
        ("pdfium", "bench") => println!("{}", pd::bench(&file)?),
        ("hayro", "bench") => println!("{}", hy::bench(&file)?),
        ("pdfium", "one") => pd::one(&file)?,
        ("hayro", "one") => hy::one(&file)?,
        ("pdfium", "png") => pd::png(&file, a[4].parse()?, &a[5])?,
        ("hayro", "png") => hy::png(&file, a[4].parse()?, &a[5])?,
        _ => return Err(anyhow!("usage")),
    }
    Ok(())
}
