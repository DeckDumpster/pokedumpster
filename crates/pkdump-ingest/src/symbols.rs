//! Normalize upstream set symbol PNGs into a uniform-height local cache.
//!
//! pokemontcg.io's `symbol.png` for a set has whatever pixel dimensions
//! and transparent-padding the contributor uploaded — some sets are 25×15,
//! others are 38×38 with significant whitespace around the glyph. Rendering
//! those at a fixed CSS height makes the visible glyph size vary wildly.
//!
//! This phase fetches each upstream symbol, trims its alpha bounding box,
//! resizes the result to a uniform target height (`TARGET_HEIGHT`), and
//! writes a normalized PNG to `<data_dir>/symbols/<set_code>.png`. The set
//! row's `symbol_url` is rewritten to `/sym/<set_code>.png` (served by the
//! HTTP layer's `/sym` nest), and the original upstream URL is recorded in
//! `symbol_source_url` so a re-run skips work when nothing changed.
//!
//! Overrides (rows whose `symbol_url` does not start with `http`) are left
//! untouched — bridge SVGs like `/sets/mep-symbol.svg` win as before.

use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

use image::{ImageFormat, RgbaImage, imageops::FilterType};
use rusqlite::Connection;

use crate::error::{IngestError, Result};

/// Target height in pixels for every normalized symbol. 2× the 28px CSS
/// size used on the /browse tile keeps the rendered glyph crisp on retina.
pub const TARGET_HEIGHT: u32 = 64;

/// Outcome counts for a normalization pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct NormalizeStats {
    /// Sets fetched, trimmed, and written this run.
    pub processed: usize,
    /// Sets whose cached PNG was still current; only rewrote symbol_url.
    pub cached: usize,
    /// Sets skipped because symbol_url is an override (not an http(s) URL).
    pub overrides: usize,
    /// Sets we tried to fetch but couldn't (network, decode, empty alpha).
    pub failed: usize,
}

/// Run the symbol-normalization pipeline against every set row.
///
/// Idempotent: a set whose `symbol_source_url` matches its current
/// upstream `symbol_url` AND whose cached PNG file still exists only has
/// its `symbol_url` rewritten to the local path. Fetch / decode failures
/// log and are counted in `failed`, but don't abort the run.
pub fn normalize_all_symbols(conn: &mut Connection, data_dir: &Path) -> Result<NormalizeStats> {
    let symbols_dir = data_dir.join("symbols");
    std::fs::create_dir_all(&symbols_dir)?;

    let http = reqwest::blocking::Client::builder()
        .user_agent("pokedumpster/0.1 (+symbol-normalize)")
        .timeout(Duration::from_secs(30))
        .build()?;

    let rows: Vec<(String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare("SELECT set_code, symbol_url, symbol_source_url FROM sets")?;
        let r = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        r.collect::<rusqlite::Result<_>>()?
    };

    let mut stats = NormalizeStats::default();
    for (set_code, symbol_url, symbol_source_url) in rows {
        let Some(upstream) = symbol_url else {
            continue;
        };
        if !upstream.starts_with("http") {
            stats.overrides += 1;
            continue;
        }
        let local_path = symbols_dir.join(format!("{set_code}.png"));
        let local_url = format!("/sym/{set_code}.png");
        let cached = symbol_source_url.as_deref() == Some(upstream.as_str()) && local_path.exists();
        if cached {
            conn.execute(
                "UPDATE sets SET symbol_url = ?1 WHERE set_code = ?2",
                rusqlite::params![local_url, set_code],
            )?;
            stats.cached += 1;
            continue;
        }
        match fetch_trim_resize(&http, &upstream) {
            Ok(bytes) => {
                std::fs::write(&local_path, &bytes)?;
                conn.execute(
                    "UPDATE sets \
                        SET symbol_url = ?1, symbol_source_url = ?2 \
                      WHERE set_code = ?3",
                    rusqlite::params![local_url, upstream, set_code],
                )?;
                stats.processed += 1;
            }
            Err(e) => {
                eprintln!("  WARN: symbol normalize {set_code}: {e}");
                stats.failed += 1;
            }
        }
    }
    Ok(stats)
}

/// Fetch one upstream PNG, crop to its non-transparent bounding box, and
/// resize to `TARGET_HEIGHT` preserving aspect ratio.
fn fetch_trim_resize(http: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    let bytes = http.get(url).send()?.error_for_status()?.bytes()?;
    let img = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .map_err(|e| IngestError::BadResponse(format!("decode {url}: {e}")))?
        .to_rgba8();
    let bbox = alpha_bbox(&img)
        .ok_or_else(|| IngestError::BadResponse(format!("{url}: fully transparent")))?;
    let cropped = image::imageops::crop_imm(&img, bbox.x, bbox.y, bbox.w, bbox.h).to_image();
    let target_w = ((cropped.width() as f32) * (TARGET_HEIGHT as f32) / (cropped.height() as f32))
        .round()
        .max(1.0) as u32;
    let resized = image::imageops::resize(&cropped, target_w, TARGET_HEIGHT, FilterType::Lanczos3);
    let mut out = Vec::with_capacity(bytes.len());
    resized
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| IngestError::BadResponse(format!("encode {url}: {e}")))?;
    Ok(out)
}

/// Tightest rectangle covering every pixel with non-zero alpha. `None` if
/// the image is entirely transparent.
fn alpha_bbox(img: &RgbaImage) -> Option<Bbox> {
    let (w, h) = (img.width(), img.height());
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y).0[3] != 0 {
                any = true;
                if x < min_x {
                    min_x = x;
                }
                if x > max_x {
                    max_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if y > max_y {
                    max_y = y;
                }
            }
        }
    }
    if !any {
        return None;
    }
    Some(Bbox {
        x: min_x,
        y: min_y,
        w: max_x - min_x + 1,
        h: max_y - min_y + 1,
    })
}

struct Bbox {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn fake_png(w: u32, h: u32, glyph: (u32, u32, u32, u32)) -> Vec<u8> {
        // Transparent canvas with an opaque white block at the given bbox.
        let mut img = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        for y in glyph.1..glyph.1 + glyph.3 {
            for x in glyph.0..glyph.0 + glyph.2 {
                img.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn alpha_bbox_finds_tight_crop_around_glyph() {
        // Canvas 40×40, opaque block at (5,8) size 20×12.
        let bytes = fake_png(40, 40, (5, 8, 20, 12));
        let img = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        let bbox = alpha_bbox(&img).unwrap();
        assert_eq!((bbox.x, bbox.y, bbox.w, bbox.h), (5, 8, 20, 12));
    }

    #[test]
    fn alpha_bbox_none_on_fully_transparent_input() {
        let img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 0]));
        assert!(alpha_bbox(&img).is_none());
    }

    #[test]
    fn fetch_trim_resize_normalizes_height_and_preserves_aspect() {
        // Build two source PNGs that have different canvas sizes and
        // different padding but the same intrinsic glyph shape (20×10).
        // After normalization they must come out at exactly TARGET_HEIGHT
        // and with matching widths.
        let a = fake_png(40, 40, (10, 15, 20, 10));
        let b = fake_png(22, 12, (1, 1, 20, 10));

        for src in [a, b] {
            // Decode directly (skip the http client) by inlining the
            // trim+resize half of fetch_trim_resize.
            let img = image::load_from_memory_with_format(&src, ImageFormat::Png)
                .unwrap()
                .to_rgba8();
            let bbox = alpha_bbox(&img).unwrap();
            let cropped =
                image::imageops::crop_imm(&img, bbox.x, bbox.y, bbox.w, bbox.h).to_image();
            let target_w = ((cropped.width() as f32) * (TARGET_HEIGHT as f32)
                / (cropped.height() as f32))
                .round() as u32;
            let resized =
                image::imageops::resize(&cropped, target_w, TARGET_HEIGHT, FilterType::Lanczos3);
            assert_eq!(resized.height(), TARGET_HEIGHT);
            // 20×10 aspect → 64-tall → 128 wide.
            assert_eq!(resized.width(), 128);
        }
    }
}
