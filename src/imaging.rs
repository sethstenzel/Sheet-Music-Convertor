//! Image loading and preparation of page photos for Claude.

use crate::claude::prompts::PageImages;
use crate::claude::ImageInput;
use crate::model::project::SongPage;
use anyhow::{Context, Result};
use base64::Engine;
use image::{imageops, DynamicImage, GrayImage, ImageDecoder, ImageReader, RgbaImage};
use std::path::Path;

pub const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];

pub fn is_supported(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}

/// Load an image applying its EXIF orientation (phone photos).
pub fn load(path: &Path) -> Result<DynamicImage> {
    let reader = ImageReader::open(path).with_context(|| format!("opening {}", path.display()))?.with_guessed_format()?;
    let mut decoder = reader.into_decoder().with_context(|| format!("decoding {}", path.display()))?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).with_context(|| format!("decoding {}", path.display()))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

pub fn rotate(img: DynamicImage, degrees: u16) -> DynamicImage {
    match degrees % 360 {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    }
}

pub fn crop(img: DynamicImage, rect: Option<[f32; 4]>) -> DynamicImage {
    let Some([x0, y0, x1, y1]) = rect else { return img };
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (x0, x1) = (x0.min(x1).clamp(0.0, 1.0), x0.max(x1).clamp(0.0, 1.0));
    let (y0, y1) = (y0.min(y1).clamp(0.0, 1.0), y0.max(y1).clamp(0.0, 1.0));
    let cw = ((x1 - x0) * w) as u32;
    let ch = ((y1 - y0) * h) as u32;
    if cw < 32 || ch < 32 {
        return img;
    }
    img.crop_imm((x0 * w) as u32, (y0 * h) as u32, cw, ch)
}

/// Grayscale, remove uneven lighting / shadows, and stretch contrast.
pub fn enhance(img: &DynamicImage) -> DynamicImage {
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    // Estimate the paper background with a heavily blurred small copy.
    let small_w = (w / 8).max(16);
    let small_h = (h / 8).max(16);
    let small = imageops::resize(&gray, small_w, small_h, imageops::FilterType::Triangle);
    // Dilate (max filter) to remove the ink before blurring so text doesn't darken the background.
    let dilated = max_filter(&small, 3);
    let bg_small = imageops::blur(&dilated, (small_w.min(small_h) as f32 / 40.0).max(2.0));
    let bg = imageops::resize(&bg_small, w, h, imageops::FilterType::Triangle);

    let mut flat = GrayImage::new(w, h);
    for (x, y, p) in flat.enumerate_pixels_mut() {
        let v = gray.get_pixel(x, y)[0] as f32;
        let b = (bg.get_pixel(x, y)[0] as f32).max(16.0);
        p[0] = ((v / b) * 255.0).clamp(0.0, 255.0) as u8;
    }
    // Contrast stretch between the 1st and 99.5th percentile.
    let mut hist = [0u64; 256];
    for p in flat.pixels() {
        hist[p[0] as usize] += 1;
    }
    let total = (w as u64 * h as u64).max(1);
    let pct = |q: f64| -> u8 {
        let target = (total as f64 * q) as u64;
        let mut acc = 0;
        for (i, &c) in hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return i as u8;
            }
        }
        255
    };
    let lo = pct(0.01) as f32;
    let hi = (pct(0.995) as f32).max(lo + 32.0);
    for p in flat.pixels_mut() {
        let v = (p[0] as f32 - lo) / (hi - lo);
        // Slight gamma darkens thin staff lines.
        p[0] = (v.clamp(0.0, 1.0).powf(1.15) * 255.0) as u8;
    }
    DynamicImage::ImageLuma8(flat)
}

fn max_filter(img: &GrayImage, radius: u32) -> GrayImage {
    let (w, h) = img.dimensions();
    let mut out = GrayImage::new(w, h);
    let r = radius as i64;
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for dy in -r..=r {
                for dx in -r..=r {
                    let xx = (x as i64 + dx).clamp(0, w as i64 - 1) as u32;
                    let yy = (y as i64 + dy).clamp(0, h as i64 - 1) as u32;
                    m = m.max(img.get_pixel(xx, yy)[0]);
                }
            }
            out.put_pixel(x, y, image::Luma([m]));
        }
    }
    out
}

/// Resize so the longest side is at most `max_dim`.
pub fn fit(img: &DynamicImage, max_dim: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max_dim {
        return img.clone();
    }
    img.resize(max_dim, max_dim, imageops::FilterType::Lanczos3)
}

pub fn encode_jpeg(img: &DynamicImage) -> Result<ImageInput> {
    let mut buf = Vec::new();
    let rgb_or_luma = match img {
        DynamicImage::ImageLuma8(_) => img.clone(),
        _ => DynamicImage::ImageRgb8(img.to_rgb8()),
    };
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90);
    enc.encode_image(&rgb_or_luma).context("encoding JPEG")?;
    Ok(ImageInput {
        media_type: "image/jpeg".into(),
        data_b64: base64::engine::general_purpose::STANDARD.encode(&buf),
        width: img.width(),
        height: img.height(),
    })
}

/// The page after rotation, crop and (optionally) enhancement, at full resolution.
pub fn processed_page(images_dir: &Path, page: &SongPage) -> Result<DynamicImage> {
    let img = load(&images_dir.join(&page.image))?;
    let img = rotate(img, page.rotation);
    let img = crop(img, page.crop);
    Ok(if page.enhance { enhance(&img) } else { img })
}

/// Prepare one page for Claude: the full page and, optionally, zoomed tiles.
///
/// Tiles form a grid of two columns and as many rows as needed so each tile is
/// shown close to the photo's native resolution — small noteheads, dots and
/// accidentals in inner voices are much easier to read that way.
pub fn prepare_page(images_dir: &Path, page: &SongPage, max_dim: u32, tiles: bool) -> Result<PageImages> {
    let img = processed_page(images_dir, page)?;
    let full = encode_jpeg(&fit(&img, max_dim))?;
    let mut out = Vec::new();
    if tiles {
        let (w, h) = (img.width(), img.height());
        let cols: u32 = if w >= 1200 { 2 } else { 1 };
        let tile_w = w / cols;
        // Rows roughly one system tall (about a third of the width).
        let rows = ((h as f32 / (w as f32 * 0.36)).ceil() as u32).clamp(1, 8);
        let tile_h = h / rows;
        let ox = tile_w / 10;
        let oy = tile_h / 6;
        for r in 0..rows {
            for c in 0..cols {
                let x0 = (c * tile_w).saturating_sub(ox);
                let x1 = ((c + 1) * tile_w + ox).min(w);
                let y0 = (r * tile_h).saturating_sub(oy);
                let y1 = ((r + 1) * tile_h + oy).min(h);
                let part = img.crop_imm(x0, y0, x1 - x0, y1 - y0);
                let side = match (cols, c) {
                    (1, _) => "full width",
                    (_, 0) => "left half",
                    _ => "right half",
                };
                out.push((format!("row {} of {rows}, {side}", r + 1), encode_jpeg(&fit(&part, max_dim))?));
            }
        }
    }
    Ok(PageImages { full, tiles: out })
}

/// A small RGBA preview for the UI.
pub fn thumbnail(path: &Path, rotation: u16, max_dim: u32) -> Result<RgbaImage> {
    let img = rotate(load(path)?, rotation);
    Ok(img.thumbnail(max_dim, max_dim).to_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enhance_keeps_size() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(120, 80, |x, _| image::Rgb([(x * 2) as u8, 200, 200])));
        let e = enhance(&img);
        assert_eq!((e.width(), e.height()), (120, 80));
    }

    #[test]
    fn tiles_cover_page() {
        let dir = std::env::temp_dir().join(format!("smc-tiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::from_pixel(1400, 2100, image::Rgb([250, 250, 240])).save(dir.join("p.png")).unwrap();
        let page = SongPage { image: "p.png".into(), enhance: false, ..Default::default() };
        let imgs = prepare_page(&dir, &page, 1000, true).unwrap();
        assert!(imgs.full.width.max(imgs.full.height) <= 1000);
        // 2 columns × ceil(2100 / (1400 × 0.36)) = 5 rows.
        assert_eq!(imgs.tiles.len(), 10);
        assert!(imgs.tiles[0].0.contains("left half"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crop_normalized() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::new(200, 100));
        let c = crop(img, Some([0.25, 0.0, 0.75, 1.0]));
        assert_eq!((c.width(), c.height()), (100, 100));
    }
}
