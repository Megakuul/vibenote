//! Run PP-OCR rec ONNX in tract (pure Rust) and compare with onnxruntime's output.
use tract_onnx::prelude::*;
fn read(p: &str) -> anyhow::Result<Vec<f32>> {
    Ok(std::fs::read(p)?.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (model, w) = (&a[1], a[2].parse::<usize>()?);
    let x = read("/dev/shm/vibenote-hwr/x.f32")?;
    let y_ort = read("/dev/shm/vibenote-hwr/y_ort.f32")?;
    let t = std::time::Instant::now();
    let m = tract_onnx::onnx().with_ignore_output_shapes(true).with_ignore_value_info(true)
        .model_for_path(model)?
        .with_input_fact(0, f32::fact([1, 3, 48, w]).into())?
        .into_optimized()?
        .into_runnable()?;
    let load = t.elapsed().as_secs_f64() * 1e3;
    let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 3, 48, w), x)?.into();
    let out = m.run(tvec!(input.clone().into()))?;
    let y = out[0].to_plain_array_view::<f32>()?;
    let ys: Vec<f32> = y.iter().copied().collect();
    let diff = ys.iter().zip(&y_ort).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    let mut ts = vec![];
    for _ in 0..30 {
        let t = std::time::Instant::now();
        let _ = m.run(tvec!(input.clone().into()))?;
        ts.push(t.elapsed().as_secs_f64() * 1e3);
    }
    ts.sort_by(|a, b| a.total_cmp(b));
    println!("tract: output shape {:?}, max |tract-ort| = {diff:e}, load+optimise {load:.0} ms, run p50 {:.2} ms", y.shape(), ts[15]);
    Ok(())
}
