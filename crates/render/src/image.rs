//! RGBA images, PNG encode/decode, tolerant comparison for golden tests.

use crate::RenderError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn encode_png(&self) -> Result<Vec<u8>, RenderError> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, self.width, self.height);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().map_err(|x| RenderError::Image(x.to_string()))?;
            w.write_image_data(&self.rgba)
                .map_err(|x| RenderError::Image(x.to_string()))?;
        }
        Ok(out)
    }

    /// Decodes an RGBA8 PNG (other formats are rejected). Bounded to 16k × 16k.
    pub fn decode_png(bytes: &[u8]) -> Result<Image, RenderError> {
        let mut d = png::Decoder::new(std::io::Cursor::new(bytes));
        d.set_transformations(png::Transformations::EXPAND);
        let mut r = d.read_info().map_err(|x| RenderError::Image(x.to_string()))?;
        let (width, height) = (r.info().width, r.info().height);
        if width > 16_384 || height > 16_384 {
            return Err(RenderError::Image("image too large".into()));
        }
        let size = r
            .output_buffer_size()
            .ok_or_else(|| RenderError::Image("unknown buffer size".into()))?;
        let mut buf = vec![0; size];
        let info = r.next_frame(&mut buf).map_err(|x| RenderError::Image(x.to_string()))?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return Err(RenderError::Image(format!(
                "expected RGBA8, got {:?}/{:?}",
                info.color_type, info.bit_depth
            )));
        }
        buf.truncate(info.buffer_size());
        Ok(Image {
            width,
            height,
            rgba: buf,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageDiff {
    /// Pixels whose max channel difference exceeds the per-channel tolerance.
    pub differing: u64,
    pub total: u64,
    pub max_channel_diff: u8,
    /// Visualisation: grey copy of `a` with differing pixels in red.
    pub diff: Image,
}

impl ImageDiff {
    pub fn fraction(&self) -> f64 {
        // Counts are ≤ 8192² < 2^32, so the u32 conversion is exact.
        let f = |v: u64| f64::from(u32::try_from(v).unwrap_or(u32::MAX));
        if self.total == 0 {
            0.0
        } else {
            f(self.differing) / f(self.total)
        }
    }
}

/// Compares two images. Different sizes are an error.
pub fn compare(a: &Image, b: &Image, channel_tolerance: u8) -> Result<ImageDiff, RenderError> {
    if a.width != b.width || a.height != b.height || a.rgba.len() != b.rgba.len() {
        return Err(RenderError::Image(format!(
            "size {}x{} vs {}x{}",
            a.width, a.height, b.width, b.height
        )));
    }
    let mut diff = Vec::with_capacity(a.rgba.len());
    let (mut n, mut maxd) = (0u64, 0u8);
    for (pa, pb) in a.rgba.as_chunks::<4>().0.iter().zip(b.rgba.as_chunks::<4>().0.iter()) {
        let d = pa.iter().zip(pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        maxd = maxd.max(d);
        if d > channel_tolerance {
            n = n.saturating_add(1);
            diff.extend_from_slice(&[255, 0, 0, 255]);
        } else {
            let g = pa.iter().take(3).map(|&c| u16::from(c)).sum::<u16>() / 3;
            let g = u8::try_from((g >> 1).saturating_add(127)).unwrap_or(255);
            diff.extend_from_slice(&[g, g, g, 255]);
        }
    }
    let total = u64::from(a.width).saturating_mul(u64::from(a.height));
    Ok(ImageDiff {
        differing: n,
        total,
        max_channel_diff: maxd,
        diff: Image {
            width: a.width,
            height: a.height,
            rgba: diff,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_and_compare() {
        let img = Image {
            width: 3,
            height: 2,
            rgba: (0..24).map(|i| i * 10).collect(),
        };
        let back = Image::decode_png(&img.encode_png().unwrap()).unwrap();
        assert_eq!(back, img);
        let mut other = img.clone();
        other.rgba[4] = other.rgba[4].wrapping_add(5);
        let d = compare(&img, &other, 2).unwrap();
        assert_eq!(d.differing, 1);
        assert_eq!(compare(&img, &other, 5).unwrap().differing, 0);
        assert!(
            compare(
                &img,
                &Image {
                    width: 1,
                    height: 1,
                    rgba: vec![0; 4]
                },
                0
            )
            .is_err()
        );
        assert!(Image::decode_png(b"not a png").is_err());
    }
}
