//! Cover image pipeline. Custom covers are copied into the app data dir
//! (never overwriting user originals) and downscaled to a 480px thumbnail.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::GenericImageView;

use crate::database::error::{AppError, AppResult};

pub const THUMB_MAX: u32 = 480;
/// Refuse absurdly large inputs to keep RAM bounded on weak machines.
pub const MAX_INPUT_BYTES: u64 = 40 * 1024 * 1024;

pub fn is_supported_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase()),
        Some(s) if s == "png" || s == "jpg" || s == "jpeg" || s == "webp"
    )
}

/// Validate + decode an image file, then write a JPEG thumbnail into `dest_dir`.
/// Returns the thumbnail path. The source file is never modified.
pub fn make_thumbnail(src: &Path, dest_dir: &Path, stem: &str) -> AppResult<PathBuf> {
    let meta = fs::metadata(src).map_err(AppError::Io)?;
    if !meta.is_file() {
        return Err(AppError::InvalidInput("cover source is not a file".into()));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(AppError::InvalidInput(format!(
            "image too large ({} MB); limit is {} MB",
            meta.len() / (1024 * 1024),
            MAX_INPUT_BYTES / (1024 * 1024)
        )));
    }
    if !is_supported_image(src) {
        return Err(AppError::InvalidInput(
            "unsupported image format (use PNG, JPG, JPEG or WebP)".into(),
        ));
    }
    let bytes = fs::read(src)?;
    let img = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| AppError::Image(e.to_string()))?
        .decode()
        .map_err(|e| AppError::Image(format!("cannot decode image: {e}")))?;

    // Reject corrupt/zero-size decoders output
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(AppError::Image("decoded image has zero size".into()));
    }

    let thumb = if w.max(h) <= THUMB_MAX {
        img
    } else {
        img.thumbnail(THUMB_MAX, THUMB_MAX)
    };
    let rgb = thumb.to_rgb8();

    fs::create_dir_all(dest_dir)?;
    let out = dest_dir.join(format!("{stem}.jpg"));
    rgb.save_with_quality(&out, image::ImageFormat::Jpeg, 82)
        .map_err(|e| AppError::Image(e.to_string()))?;
    Ok(out)
}

/// Generate the app's default cover (a simple gradient tile with initials).
/// Used when no custom cover exists and auto-thumbnailing is disabled.
pub fn default_cover(name: &str, dest_dir: &Path, id: i64) -> AppResult<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let out = dest_dir.join(format!("default_{id}.jpg"));
    if out.exists() {
        return Ok(out);
    }
    let (w, h) = (360u32, 504u32);
    let mut img = image::RgbImage::new(w, h);
    // deterministic hue from name
    let hash = name
        .chars()
        .fold(0u32, |acc, c| acc.wrapping_mul(31).wrapping_add(c as u32));
    let base: [u8; 3] = [
        40 + (hash % 60) as u8,
        44 + ((hash / 7) % 60) as u8,
        52 + ((hash / 13) % 80) as u8,
    ];
    for y in 0..h {
        for x in 0..w {
            let f = (x as f32 / w as f32 * 20.0) as u8;
            img.put_pixel(x, y, image::Rgb([base[0] + f / 2, base[1] + f / 3, base[2] + f]));
        }
    }
    img.save_with_quality(&out, image::ImageFormat::Jpeg, 80)
        .map_err(|e| AppError::Image(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_roundtrip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        // create a big PNG
        let src = dir.path().join("cover.png");
        image::RgbImage::from_pixel(1920, 1080, image::Rgb([10, 20, 30])).save(&src).unwrap();
        let out = make_thumbnail(&src, &dir.path().join("covers"), "anime1").unwrap();
        assert!(out.exists());
        let t = image::open(&out).unwrap();
        assert!(t.width() <= THUMB_MAX && t.height() <= THUMB_MAX);
    }

    #[test]
    fn rejects_corrupt_and_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.png");
        fs::write(&bad, b"not an image").unwrap();
        assert!(make_thumbnail(&bad, &dir.path().join("c"), "x").is_err());

        let txt = dir.path().join("a.txt");
        fs::write(&txt, b"hello").unwrap();
        assert!(make_thumbnail(&txt, &dir.path().join("c"), "y").is_err());
    }

    #[test]
    fn default_cover_created() {
        let dir = tempfile::tempdir().unwrap();
        let p = default_cover("Cowboy Bebop", dir.path(), 7).unwrap();
        assert!(p.exists());
    }
}
