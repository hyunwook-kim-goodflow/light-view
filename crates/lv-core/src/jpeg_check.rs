//! JPEG structure check: one frame header, at least one scan, an EOI (bytes after it are
//! allowed here; [`crate::jpeg_extent`] is the stricter check for pixel rotation), every
//! segment length inside the data. Gives the image size and the iMCU size a lossless
//! transform works in. Used on files before a rotation and on the worker's output.

use crate::jpeg_markers::{is_frame_header, Markers, EOI, SOS};

/// What [`inspect`] reads from a JPEG's frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegInfo {
    pub width: u16,
    pub height: u16,
    /// The iMCU size in pixels: 8 times the largest sampling factor, or 8x8 for a
    /// single-component image (libjpeg's `transupp.c`).
    pub mcu_w: u8,
    pub mcu_h: u8,
}

/// Checks the structure of `jpeg` and reads its size and iMCU size: it starts with SOI,
/// every segment length stays inside the data, there is exactly one frame header and it
/// comes before the first scan, there is a scan, and an EOI ends the image.
pub fn inspect(jpeg: &[u8]) -> Result<JpegInfo, String> {
    let mut info = None;
    let mut scans = false;
    for marker in Markers::new(jpeg) {
        let marker = marker.map_err(|_| "malformed JPEG marker structure".to_string())?;
        match marker.code {
            code if is_frame_header(code) => {
                if info.is_some() {
                    return Err("more than one JPEG frame header".into());
                }
                info = Some(frame_info(marker.payload)?);
            }
            SOS if info.is_none() => return Err("JPEG scan before the frame header".into()),
            SOS => scans = true,
            EOI if !scans => return Err("JPEG has no scan".into()),
            EOI => return info.ok_or_else(|| "JPEG has no frame header".into()),
            _ => {}
        }
    }
    Err("JPEG ends without EOI".into())
}

/// Reads a frame header's payload: precision, height, width, component count, then
/// (id, sampling factors, table) per component.
fn frame_info(sof: &[u8]) -> Result<JpegInfo, String> {
    let bad = || "malformed JPEG frame header".to_string();
    let header = sof.get(..6).ok_or_else(bad)?;
    let height = u16::from_be_bytes([header[1], header[2]]);
    let width = u16::from_be_bytes([header[3], header[4]]);
    let count = usize::from(header[5]);
    if width == 0 || height == 0 || count == 0 || sof.len() != 6 + 3 * count {
        return Err(bad());
    }
    let (mut max_h, mut max_v) = (1, 1);
    for comp in sof[6..].chunks_exact(3) {
        let (h, v) = (comp[1] >> 4, comp[1] & 0x0F);
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
            return Err(bad());
        }
        max_h = max_h.max(h);
        max_v = max_v.max(v);
    }
    // A single-component image is coded in 8x8 blocks whatever its factors say.
    if count == 1 {
        (max_h, max_v) = (1, 1);
    }
    Ok(JpegInfo {
        width,
        height,
        mcu_w: 8 * max_h,
        mcu_h: 8 * max_v,
    })
}

/// Whether a lossless rotation by `quarters` clockwise quarter turns keeps every pixel
/// (libjpeg's `TJXOPT_PERFECT`): a clockwise quarter turn needs whole iMCU rows, a
/// counter-clockwise one whole iMCU columns, a half turn both. Turns count modulo 4.
pub fn is_mcu_aligned(info: &JpegInfo, quarters: u8) -> bool {
    let rows = info.height.is_multiple_of(u16::from(info.mcu_h));
    let columns = info.width.is_multiple_of(u16::from(info.mcu_w));
    match quarters % 4 {
        0 => true,
        1 => rows,
        2 => rows && columns,
        _ => columns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jpeg_markers::tests::segment;

    /// A structurally complete JPEG (only the layout matters): SOI, APP0, a SOF0 with the
    /// given size and (id, sampling) components, SOS, entropy data, EOI.
    fn jpeg(width: u16, height: u16, comps: &[u8]) -> Vec<u8> {
        let mut sof = vec![8];
        sof.extend_from_slice(&height.to_be_bytes());
        sof.extend_from_slice(&width.to_be_bytes());
        sof.push(comps.len() as u8);
        for (i, &sampling) in comps.iter().enumerate() {
            sof.extend_from_slice(&[i as u8 + 1, sampling, 0]);
        }
        let mut j = vec![0xFF, 0xD8];
        j.extend(segment(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        j.extend(segment(0xC0, &sof));
        j.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
        j.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56, 0xFF, 0xD9]);
        j
    }

    fn info(width: u16, height: u16, mcu_w: u8, mcu_h: u8) -> JpegInfo {
        JpegInfo {
            width,
            height,
            mcu_w,
            mcu_h,
        }
    }

    fn sample(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/jpeg")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn inspect_reads_dims_and_mcu() {
        // Real files: GDI+ writes 4:2:0.
        assert_eq!(
            inspect(&sample("gradient-64x32.jpg")),
            Ok(info(64, 32, 16, 16))
        );
        assert_eq!(
            inspect(&sample("gradient-65x33.jpg")),
            Ok(info(65, 33, 16, 16))
        );
        // 4:2:0, 4:4:4, 4:2:2, 4:4:0, grayscale (any factors: one component is 8x8).
        assert_eq!(
            inspect(&jpeg(300, 200, &[0x22, 0x11, 0x11])),
            Ok(info(300, 200, 16, 16))
        );
        assert_eq!(
            inspect(&jpeg(300, 200, &[0x11, 0x11, 0x11])),
            Ok(info(300, 200, 8, 8))
        );
        assert_eq!(
            inspect(&jpeg(300, 200, &[0x21, 0x11, 0x11])),
            Ok(info(300, 200, 16, 8))
        );
        assert_eq!(
            inspect(&jpeg(300, 200, &[0x12, 0x11, 0x11])),
            Ok(info(300, 200, 8, 16))
        );
        assert_eq!(inspect(&jpeg(7, 9, &[0x22])), Ok(info(7, 9, 8, 8)));
        // 4:1:1 and CMYK with 2x2 luma.
        assert_eq!(
            inspect(&jpeg(64, 64, &[0x41, 0x11, 0x11])),
            Ok(info(64, 64, 32, 8))
        );
        assert_eq!(
            inspect(&jpeg(64, 64, &[0x22, 0x11, 0x11, 0x22])),
            Ok(info(64, 64, 16, 16))
        );
        // Data after EOI is allowed here.
        let mut trailing = jpeg(16, 16, &[0x11]);
        trailing.extend(jpeg(8, 8, &[0x11]));
        assert_eq!(inspect(&trailing), Ok(info(16, 16, 8, 8)));
        // Progressive frame header, scans with tables between them.
        let mut progressive = jpeg(40, 30, &[0x22, 0x11, 0x11]);
        progressive[2 + 18 + 1] = 0xC2;
        progressive.truncate(progressive.len() - 2);
        progressive.extend(segment(0xC4, &[0x10; 17]));
        progressive.extend(segment(0xDA, &[1, 1, 0, 1, 63, 0]));
        progressive.extend_from_slice(&[0xAB, 0xFF, 0xD9]);
        assert_eq!(inspect(&progressive), Ok(info(40, 30, 16, 16)));
    }

    #[test]
    fn inspect_rejects_truncated_and_bad_lengths() {
        let full = jpeg(32, 16, &[0x22, 0x11, 0x11]);
        for cut in 0..full.len() {
            assert!(inspect(&full[..cut]).is_err(), "cut {cut}");
        }
        let real = sample("gradient-64x32.jpg");
        for cut in [0, 2, 20, real.len() / 2, real.len() - 1] {
            assert!(inspect(&real[..cut]).is_err(), "cut {cut}");
        }
        let with_frame = |sof: &[u8]| {
            let mut j = vec![0xFF, 0xD8];
            j.extend(segment(0xC0, sof));
            j.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
            j.extend_from_slice(&[0xFF, 0xD9]);
            j
        };
        for bad in [
            // A segment length running past the data, and one under 2.
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0, 0, 0xFF, 0xD9],
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01, 0xFF, 0xD9],
            // Not a JPEG, SOI only, no frame, no scan.
            b"GIF89a".to_vec(),
            vec![0xFF, 0xD8],
            vec![0xFF, 0xD8, 0xFF, 0xDA, 0, 2, 0xFF, 0xD9],
            with_frame(&[8, 0, 8, 0, 8, 1, 1, 0x11, 0])[..15].to_vec(),
            // Zero width or height, no components, a length that disagrees with the
            // component count, a sampling factor of 0 or 5.
            with_frame(&[8, 0, 0, 0, 8, 1, 1, 0x11, 0]),
            with_frame(&[8, 0, 8, 0, 0, 1, 1, 0x11, 0]),
            with_frame(&[8, 0, 8, 0, 8, 0]),
            with_frame(&[8, 0, 8, 0, 8, 2, 1, 0x11, 0]),
            with_frame(&[8, 0, 8, 0, 8, 1, 1, 0x11, 0, 0]),
            with_frame(&[8, 0, 8, 0, 8, 1, 1, 0x01, 0]),
            with_frame(&[8, 0, 8, 0, 8, 1, 1, 0x15, 0]),
        ] {
            assert!(inspect(&bad).is_err(), "{bad:?}");
        }
        // Two frame headers.
        let mut two = jpeg(8, 8, &[0x11]);
        let sof = segment(0xC1, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]);
        two.splice(2..2, sof);
        assert!(inspect(&two).is_err());
    }

    #[test]
    fn mcu_alignment_follows_transupp() {
        // 4:2:2: a clockwise quarter turn needs whole MCU rows (height), a
        // counter-clockwise one whole columns (width), half a turn both.
        let i = |w, h| info(w, h, 16, 8);
        assert!(is_mcu_aligned(&i(17, 8), 1));
        assert!(!is_mcu_aligned(&i(16, 9), 1));
        assert!(is_mcu_aligned(&i(16, 9), 3));
        assert!(!is_mcu_aligned(&i(17, 8), 3));
        assert!(is_mcu_aligned(&i(32, 16), 2));
        assert!(!is_mcu_aligned(&i(32, 17), 2));
        assert!(!is_mcu_aligned(&i(31, 16), 2));
        // No turn moves nothing; turns count modulo 4.
        assert!(is_mcu_aligned(&i(17, 9), 0));
        assert!(is_mcu_aligned(&i(17, 9), 4));
        assert!(is_mcu_aligned(&i(17, 8), 5));
        assert!(!is_mcu_aligned(&i(17, 8), 7));
    }
}
