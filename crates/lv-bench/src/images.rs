//! The bench pictures: synthetic, deterministic, written into a fresh temp folder.
//!
//! Each case gets its own folder holding `COPIES` copies of one picture; the app opens
//! the middle one, as when a photo is opened from a folder (it then prefetches the
//! neighbours, which is part of what the memory figure measures).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};

/// Pictures per case folder; the app opens the middle one.
pub const COPIES: usize = 5;

/// What one case opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picture {
    /// A noisy photo-like JPEG, quality 90.
    Jpeg { width: u32, height: u32 },
    /// A PNG with gentle noise (fast compression).
    Png { width: u32, height: u32 },
}

impl Picture {
    fn extension(self) -> &'static str {
        match self {
            Picture::Jpeg { .. } => "jpg",
            Picture::Png { .. } => "png",
        }
    }
}

/// Writes `COPIES` copies of `picture` into `dir` (created) and returns the middle one.
pub fn prepare(dir: &Path, picture: Picture) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let first = dir.join(format!("p0.{}", picture.extension()));
    write(&first, picture)?;
    for i in 1..COPIES {
        std::fs::copy(&first, dir.join(format!("p{i}.{}", picture.extension())))?;
    }
    Ok(dir.join(format!("p{}.{}", COPIES / 2, picture.extension())))
}

/// Copies `file` (a picture the user supplied, e.g. a 12 MP HEIC) `COPIES` times into
/// `dir` and returns the middle copy.
pub fn prepare_copies(dir: &Path, file: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("heic")
        .to_string();
    for i in 0..COPIES {
        std::fs::copy(file, dir.join(format!("p{i}.{ext}")))?;
    }
    Ok(dir.join(format!("p{}.{ext}", COPIES / 2)))
}

fn write(path: &Path, picture: Picture) -> std::io::Result<()> {
    let out = BufWriter::new(File::create(path)?);
    let result =
        match picture {
            Picture::Jpeg { width, height } => {
                let rgb = pixels(width, height, 40);
                JpegEncoder::new_with_quality(out, 90).write_image(
                    &rgb,
                    width,
                    height,
                    ExtendedColorType::Rgb8,
                )
            }
            Picture::Png { width, height } => {
                let rgb = pixels(width, height, 6);
                PngEncoder::new_with_quality(out, CompressionType::Fast, FilterType::Sub)
                    .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
            }
        };
    result.map_err(std::io::Error::other)
}

/// RGB8 pixels: smooth colour gradients and waves (like a photo's large shapes) plus
/// per-pixel noise of `noise` levels (like sensor grain), the same on every run.
pub fn pixels(width: u32, height: u32, noise: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width as usize * height as usize * 3);
    let mut state: u32 = 0x9e37_79b9;
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    for y in 0..height {
        let fy = y as f32 / h;
        for x in 0..width {
            let fx = x as f32 / w;
            let wave = ((fx * 23.0).sin() * (fy * 17.0).cos() * 40.0) as i32;
            let base = [
                (fx * 200.0) as i32 + wave,
                (fy * 180.0) as i32 + 30 - wave / 2,
                ((1.0 - fx) * 120.0 + fy * 100.0) as i32 + wave / 3,
            ];
            for c in base {
                // xorshift32
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let n = if noise == 0 {
                    0
                } else {
                    (state % (2 * noise + 1)) as i32 - noise as i32
                };
                out.push((c + n).clamp(0, 255) as u8);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_are_deterministic_and_sized() {
        let a = pixels(17, 5, 40);
        assert_eq!(a.len(), 17 * 5 * 3);
        assert_eq!(a, pixels(17, 5, 40));
        assert_ne!(a, pixels(17, 5, 0));
    }

    #[test]
    fn prepare_writes_copies_and_returns_the_middle() {
        let dir = std::env::temp_dir().join(format!("lv-bench-test-{}", std::process::id()));
        let middle = prepare(
            &dir,
            Picture::Jpeg {
                width: 64,
                height: 48,
            },
        )
        .unwrap();
        assert_eq!(middle, dir.join("p2.jpg"));
        let count = std::fs::read_dir(&dir).unwrap().count();
        let decoded = image::open(&middle).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(count, COPIES);
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }
}
