//! Image downscaling for the PreToolUse `Read` hook (feature A).
//!
//! Claude's vision token cost scales with image **area** (~`w*h/750`
//! tokens). An oversized screenshot or photo costs far more than it needs
//! to — detail beyond ~1568 px on the long edge is not resolvable by the
//! model anyway, so those pixels are pure token waste. `crabcc media
//! downscale <path>` rewrites an oversized image to a bounded copy in
//! `~/.crabcc/media-cache/` and prints the path the `Read` tool should
//! open.
//!
//! **Lossless on failure / no-op when not worth it.** A non-image, a
//! decode error, an already-small image, or `CRABCC_NO_MEDIA=1` all print
//! the *original* path unchanged, so the agent never loses the read. The
//! only behavioural change is: an oversized png/jpg is read at bounded
//! resolution, and the saved vision tokens are recorded to the `crabcc
//! track` ledger (op `media`) so the dashboard can show the realized
//! reduction.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Longest-edge cap. ~1568 px is Anthropic's effective vision resolution;
/// beyond it the model gains no detail but pays linearly more tokens.
const DEFAULT_MAX_EDGE: u32 = 1568;

/// Anthropic vision-token estimate for a `w*h` image (tokens ≈ area/750).
pub fn vision_tokens(w: u32, h: u32) -> u64 {
    (w as u64 * h as u64) / 750
}

/// Only formats we can both decode and re-encode with the lean
/// `png`+`jpeg` feature set. Everything else passes through untouched.
fn is_downscalable(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg")
    )
}

fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = home.join(".crabcc").join("media-cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Cache file = `sha256(abs-path \0 mtime \0 max_edge).ext`. A changed
/// source (new mtime) misses and re-downscales; an unchanged one hits, so
/// re-reads of the same image are a single file stat.
fn cache_path(src: &Path, mtime_ns: u128, max_edge: u32, ext: &str) -> Option<PathBuf> {
    let key = crabcc_core::hash::sha256_hex(
        format!("{}\0{mtime_ns}\0{max_edge}", src.display()).as_bytes(),
    );
    Some(cache_dir()?.join(format!("{key}.{ext}")))
}

/// Returns the path the reader should open: a bounded copy when the source
/// is an oversized png/jpg, else `src` unchanged. Never errors out of the
/// read — any failure falls back to the original path.
pub fn downscale(src: &Path, max_edge: u32) -> PathBuf {
    if std::env::var_os("CRABCC_NO_MEDIA").is_some() || !is_downscalable(src) {
        return src.to_path_buf();
    }
    match try_downscale(src, max_edge) {
        Ok(Some(out)) => out,
        _ => src.to_path_buf(),
    }
}

fn try_downscale(src: &Path, max_edge: u32) -> Result<Option<PathBuf>> {
    // Canonicalise so the cache key is a stable absolute path regardless of
    // the cwd the Read hook ran from (matches `cache_path`'s "abs-path" doc).
    let canonical = std::fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let src = canonical.as_path();
    let meta = std::fs::metadata(src)?;
    let mtime_ns = meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    let Some(out) = cache_path(src, mtime_ns, max_edge, &ext) else {
        return Ok(None);
    };
    // Cache hit: this exact source was already downscaled.
    if out.exists() {
        return Ok(Some(out));
    }
    let img = image::open(src)?;
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max_edge {
        return Ok(None); // already within budget -> read the original
    }
    // `resize` preserves aspect ratio, fitting within max_edge x max_edge.
    let resized = img.resize(max_edge, max_edge, image::imageops::FilterType::Lanczos3);
    resized.save(&out)?;
    let (nw, nh) = (resized.width(), resized.height());
    let saved = vision_tokens(w, h).saturating_sub(vision_tokens(nw, nh));
    crabcc_core::track::record_saved(
        "media",
        &src.to_string_lossy(),
        1,
        "media",
        vision_tokens(nw, nh) as usize,
        saved as usize,
    );
    Ok(Some(out))
}

/// `crabcc media downscale <path>`: print the path the `Read` tool should
/// open (the bounded copy, or the original when no shrink applies).
pub fn run_downscale(path: &Path, max_edge: Option<u32>) -> Result<()> {
    let out = downscale(path, max_edge.unwrap_or(DEFAULT_MAX_EDGE));
    println!("{}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};
    use tempfile::tempdir;

    fn write_png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let p = dir.join(name);
        DynamicImage::ImageRgb8(RgbImage::new(w, h))
            .save(&p)
            .unwrap();
        p
    }

    #[test]
    fn vision_tokens_scale_with_area() {
        // Halving each edge quarters the area -> quarters the tokens.
        assert_eq!(vision_tokens(1500, 1000), 2000);
        assert_eq!(vision_tokens(750, 500), 500);
    }

    #[test]
    fn oversized_png_downscales_small_png_passes_through() {
        // Pin HOME so the cache dir is the tempdir (no real ~/.crabcc writes).
        let home = tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("CRABCC_NO_MEDIA");

        let src = home.path();
        let big = write_png(src, "big.png", 4000, 3000);
        let out = downscale(&big, 1568);
        assert_ne!(out, big, "oversized image should be rewritten to a copy");
        let resized = image::open(&out).unwrap();
        assert!(resized.width().max(resized.height()) <= 1568);

        // An already-small image is left alone (read the original).
        let small = write_png(src, "small.png", 800, 600);
        assert_eq!(downscale(&small, 1568), small);

        // A non-image extension is never touched.
        let txt = src.join("notes.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert_eq!(downscale(&txt, 1568), txt);

        // Disable switch -> always the original.
        std::env::set_var("CRABCC_NO_MEDIA", "1");
        assert_eq!(downscale(&big, 1568), big);
        std::env::remove_var("CRABCC_NO_MEDIA");

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
