use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const THUMB_EDGE: u32 = 360;
const MAX_SOURCE_BYTES: u64 = 48 * 1024 * 1024;

/// Decode a local image, shrink the long edge to `THUMB_EDGE`, encode JPEG.
/// Returns `None` when the file is unsuitable (too big / undecodable).
pub fn make_jpeg_thumb(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?;
    let resized = img.thumbnail(THUMB_EDGE, THUMB_EDGE);
    let rgb = resized.to_rgb8();
    let mut out = Cursor::new(Vec::with_capacity(48 * 1024));
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82);
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(out.into_inner())
}

/// Disk cache keyed by path + mtime + size so edits invalidate.
pub fn cached_jpeg_thumb(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    let cache_path = cache_file_for(path, &meta)?;
    if let Ok(bytes) = std::fs::read(&cache_path) {
        if !bytes.is_empty() {
            return Some(bytes);
        }
    }
    let bytes = make_jpeg_thumb(path)?;
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&cache_path, &bytes);
    Some(bytes)
}

pub fn thumb_content_type() -> &'static str {
    "image/jpeg"
}

fn cache_file_for(path: &Path, meta: &std::fs::Metadata) -> Option<PathBuf> {
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!(
        "{:x}-{}-{}-{THUMB_EDGE}.jpg",
        fnv1a(path.to_string_lossy().as_bytes()),
        modified,
        meta.len()
    );
    Some(std::env::temp_dir().join("popstall-thumbs").join(key))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn thumbs_a_png() {
        let dir = std::env::temp_dir().join(format!("popstall-thumb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tiny.png");
        let img = RgbImage::from_pixel(64, 48, Rgb([200, 40, 40]));
        img.save(&path).unwrap();
        let bytes = make_jpeg_thumb(&path).expect("thumb");
        assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]), "not jpeg");
        let cached = cached_jpeg_thumb(&path).expect("cached");
        assert_eq!(bytes, cached);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
