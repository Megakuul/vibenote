//! The line recogniser (ADR 0006 decisions 1–3): a pack's PP-OCR CTC model run by tract (pure
//! Rust) on 48 px-high line images; one optimised model serves every line width.

use crate::ctc::{self, Decoded};
use crate::pack::{MAX_MODEL, Pack};
use crate::raster::Gray;
use tract_onnx::prelude::*;

type Plan = std::sync::Arc<TypedRunnableModel>;

/// Narrowest model input (PP-OCR's 3×48×320 shape; narrower lines are padded).
const MIN_W: usize = 320;

pub struct Recognizer {
    plan: Plan,
    chars: Vec<String>,
    lexicon: ctc::Lexicon,
    decoder: Option<ctc::BeamParams>,
    /// Version tag stored with results.
    pub tag: String,
}

fn err(e: impl std::fmt::Display) -> String {
    format!("recogniser: {e}")
}

impl Recognizer {
    /// Loads and optimises a pack's model (about a second; done once, off the pen path).
    pub fn load(pack: &Pack) -> Result<Recognizer, String> {
        let path = pack
            .model_path()?
            .ok_or_else(|| format!("pack {} has no model", pack.manifest.lang))?;
        let len = std::fs::metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len();
        if len > MAX_MODEL {
            return Err(format!("{}: model too large", path.display()));
        }
        let chars = pack.chars()?;
        let lexicon = pack.lexicon()?;
        let decoder = pack.manifest.decoder.filter(ctc::BeamParams::valid);
        let model = tract_onnx::onnx()
            .with_ignore_output_shapes(true)
            .with_ignore_value_info(true)
            .model_for_path(&path)
            .map_err(err)?;
        let w = model.symbols.sym("W");
        let fact = InferenceFact::dt_shape(
            f32::datum_type(),
            tvec![1.to_dim(), 3.to_dim(), 48.to_dim(), w.to_dim()],
        );
        let plan = model
            .with_input_fact(0, fact)
            .map_err(err)?
            .into_optimized()
            .map_err(err)?
            .into_runnable()
            .map_err(err)?;
        Ok(Recognizer {
            plan,
            chars,
            lexicon,
            decoder,
            tag: pack.tag(),
        })
    }

    /// Reads one line image.
    pub fn recognize(&self, img: &Gray) -> Result<Decoded, String> {
        let (w, h) = (img.width, img.height);
        if h != 48 || w == 0 || img.px.len() != w.saturating_mul(h) {
            return Err(err("line image must be 48 px high"));
        }
        // PP-OCR input: 3 channels, (v − 0.5) / 0.5 with paper white = 1, at least 320 px wide
        // with zero (mid-grey) padding on the right, as in PaddleOCR/RapidOCR.
        let pw = w.max(MIN_W);
        let mut plane = vec![0.0f32; pw.saturating_mul(h)];
        for (y, row) in img.px.chunks_exact(w).enumerate() {
            for (x, v) in row.iter().enumerate() {
                if let Some(slot) = plane.get_mut(y.saturating_mul(pw).saturating_add(x)) {
                    *slot = (v - 0.5) / 0.5;
                }
            }
        }
        let mut data = Vec::with_capacity(plane.len().saturating_mul(3));
        for _ in 0..3 {
            data.extend_from_slice(&plane);
        }
        let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 3, h, pw), data)
            .map_err(err)?
            .into();
        let out = self.plan.run(tvec!(input.into())).map_err(err)?;
        let y = out
            .first()
            .ok_or_else(|| err("no output"))?
            .to_plain_array_view::<f32>()
            .map_err(err)?;
        let shape = y.shape().to_vec();
        let classes = shape.last().copied().unwrap_or(0);
        if classes != self.chars.len().saturating_add(2) {
            return Err(err(format!(
                "model has {classes} classes, the pack lists {} characters",
                self.chars.len()
            )));
        }
        let probs: Vec<f32> = y.iter().copied().collect();
        Ok(match &self.decoder {
            Some(p) => ctc::beam(&probs, classes, &self.chars, &self.lexicon, p),
            None => ctc::greedy(&probs, classes, &self.chars),
        })
    }
}
