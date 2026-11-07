//! Image decoding. The original handled JPEG, PNG, GIF and BMP; this adds
//! WebP (lossy, lossless, alpha), JPEG XL, TIFF, TGA, ICO, PNM, QOI and,
//! with the `avif` feature, AVIF.

use anyhow::{Context, Result, anyhow};
use image::{DynamicImage, ImageFormat, ImageReader, RgbaImage};
use std::io::Cursor;

pub struct Decoded {
    pub image: RgbaImage,
    pub format: String,
}

fn is_jxl(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0x0A]) || b.starts_with(&[0, 0, 0, 0x0C, b'J', b'X', b'L', b' '])
}

fn limits() -> image::Limits {
    let mut l = image::Limits::default();
    l.max_alloc = Some(2 << 30);
    l
}

pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    if is_jxl(bytes) {
        let dec = jxl_oxide::integration::JxlDecoder::new(Cursor::new(bytes))
            .map_err(|e| anyhow!("JPEG XL: {e}"))?;
        let img = DynamicImage::from_decoder(dec).context("JPEG XL decode")?;
        return Ok(Decoded {
            image: img.into_rgba8(),
            format: "JPEG XL".into(),
        });
    }
    let mut r = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    r.limits(limits());
    let fmt = r
        .format()
        .ok_or_else(|| anyhow!("unrecognised image format"))?;
    let img = r.decode().with_context(|| format!("{fmt:?} decode"))?;
    Ok(Decoded {
        image: img.into_rgba8(),
        format: format_name(fmt).into(),
    })
}

/// Read only the header to get dimensions (used for the image-info overlay of
/// pages that haven't been decoded yet).
#[allow(dead_code)]
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if is_jxl(bytes) {
        let img = jxl_oxide::JxlImage::builder()
            .read(Cursor::new(bytes))
            .ok()?;
        return Some((img.width(), img.height()));
    }
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

pub fn format_name(f: ImageFormat) -> &'static str {
    match f {
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Png => "PNG",
        ImageFormat::Gif => "GIF",
        ImageFormat::Bmp => "BMP",
        ImageFormat::WebP => "WebP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Tga => "TGA",
        ImageFormat::Ico => "ICO",
        ImageFormat::Pnm => "PNM",
        ImageFormat::Qoi => "QOI",
        ImageFormat::Avif => "AVIF",
        _ => "image",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_webp_lossless_roundtrip() {
        let src = RgbaImage::from_fn(17, 9, |x, y| {
            image::Rgba([x as u8 * 10, y as u8 * 20, 7, 255])
        });
        let mut buf = Cursor::new(Vec::new());
        src.write_to(&mut buf, ImageFormat::WebP).unwrap();
        let d = decode(buf.get_ref()).unwrap();
        assert_eq!(d.format, "WebP");
        assert_eq!(d.image, src);
        assert_eq!(dimensions(buf.get_ref()), Some((17, 9)));
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"definitely not an image").is_err());
    }
}
