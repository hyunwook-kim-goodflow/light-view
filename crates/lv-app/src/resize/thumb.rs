//! The filmstrip's photo tiles (spec §4, the approved mockup): a tiny copy of each
//! photo, at most `THUMB_EDGE` pixels on its long edge, made from the estimate's own
//! decode (no second decode), premultiplied BGRA ready for Direct2D.

use std::sync::atomic::{AtomicU64, Ordering};

/// The long edge of a tile's pixels: about 12 KB per photo.
pub const THUMB_EDGE: u32 = 64;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, PartialEq, Eq)]
pub struct Thumb {
    /// Unique per thumbnail (the renderer caches its bitmap by it).
    pub id: u64,
    pub size: (u32, u32),
    /// Premultiplied BGRA, `size.0 * size.1 * 4` bytes.
    pub bgra: Vec<u8>,
}

/// A box-filtered copy of straight RGBA `rgba` (`size`) whose long edge is at most
/// `edge`; `None` for an empty or mis-sized image.
pub fn thumbnail(rgba: &[u8], size: (u32, u32), edge: u32) -> Option<Thumb> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return None;
    }
    let long = size.0.max(size.1);
    let scale = f64::from(edge.min(long)) / f64::from(long);
    let tw = ((f64::from(size.0) * scale).round() as usize).max(1);
    let th = ((f64::from(size.1) * scale).round() as usize).max(1);
    // Sums per target pixel: every source pixel adds to the one it falls in.
    let mut sum = vec![[0u64; 5]; tw * th];
    for y in 0..h {
        let ty = y * th / h;
        let row = &rgba[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let tx = x * tw / w;
            let p = &row[x * 4..x * 4 + 4];
            let a = u64::from(p[3]);
            let s = &mut sum[ty * tw + tx];
            // Premultiplied while summing, so transparent pixels add no colour.
            s[0] += u64::from(p[2]) * a;
            s[1] += u64::from(p[1]) * a;
            s[2] += u64::from(p[0]) * a;
            s[3] += a;
            s[4] += 1;
        }
    }
    let mut bgra = Vec::with_capacity(tw * th * 4);
    for s in &sum {
        let n = s[4].max(1);
        for c in &s[..3] {
            bgra.push((c / 255 / n) as u8);
        }
        bgra.push((s[3] / n) as u8);
    }
    Some(Thumb {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        size: (tw as u32, th as u32),
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_are_tiny_and_keep_the_picture() {
        // 200×100: red on the left half, blue on the right half, opaque.
        let (w, h) = (200usize, 100usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for _ in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(if x < w / 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let t = thumbnail(&rgba, (200, 100), THUMB_EDGE).unwrap();
        assert_eq!(t.size, (64, 32));
        assert_eq!(t.bgra.len(), 64 * 32 * 4);
        // BGRA: the left is red, the right blue.
        assert_eq!(&t.bgra[..4], &[0, 0, 255, 255]);
        assert_eq!(&t.bgra[63 * 4..64 * 4], &[255, 0, 0, 255]);
        // A small photo is never enlarged; ids differ.
        let small = thumbnail(&rgba[..20 * 4], (10, 2), THUMB_EDGE).unwrap();
        assert_eq!(small.size, (10, 2));
        assert_ne!(small.id, t.id);
        // Transparent pixels are premultiplied to nothing.
        let clear = thumbnail(&[255, 255, 255, 0], (1, 1), THUMB_EDGE).unwrap();
        assert_eq!(clear.bgra, [0, 0, 0, 0]);
        assert!(thumbnail(&[], (0, 0), THUMB_EDGE).is_none());
        assert!(thumbnail(&[0; 7], (1, 2), THUMB_EDGE).is_none());
    }
}
