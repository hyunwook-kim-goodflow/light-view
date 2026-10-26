//! Whether a JPEG file is one image and nothing else (ruling R7).
//!
//! A lossless pixel rotation (TurboJPEG) rebuilds the file from the DCT coefficients and the
//! saved markers and stops at EOI. Anything after EOI is lost: MPF secondary images, Ultra
//! HDR / iOS gain maps, motion-photo video trailers. An MPF APP2 segment would be copied with
//! offsets that point at nothing. Such files must not be pixel-rotated; they keep their bytes
//! and only get a new EXIF orientation.
//!
//! The check walks the markers to the first scan, steps over entropy-coded data (stuffed
//! `0xFF00` bytes, `RSTn` markers and fill bytes) and the markers between scans, and finds
//! the real EOI. It never panics and runs in time linear in the input.

/// Why a JPEG must not be rotated pixel by pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSingleImage {
    /// Bytes follow the EOI marker (a secondary image, gain map, video, padding...).
    TrailingData,
    /// An MPF (`APP2 "MPF\0"`) segment: the file indexes images stored after its EOI.
    MultiPicture,
    /// The structure could not be followed to an EOI (truncated or malformed), so whatever
    /// the rest holds cannot be ruled out.
    Unreadable,
}

/// `Ok` when `jpeg` is SOI, segments and scans, and an EOI that ends the data exactly, with
/// no MPF segment.
pub fn check_single_image(jpeg: &[u8]) -> Result<(), NotSingleImage> {
    let end = end_of_image(jpeg)?;
    if end == jpeg.len() {
        Ok(())
    } else {
        Err(NotSingleImage::TrailingData)
    }
}

/// The offset just past the EOI that ends the first image.
fn end_of_image(data: &[u8]) -> Result<usize, NotSingleImage> {
    use NotSingleImage::{MultiPicture, Unreadable};
    if data.get(..2) != Some(&[0xFF, 0xD8][..]) {
        return Err(Unreadable);
    }
    let mut i = 2;
    loop {
        if *data.get(i).ok_or(Unreadable)? != 0xFF {
            return Err(Unreadable);
        }
        // Fill bytes before a marker code.
        while *data.get(i + 1).ok_or(Unreadable)? == 0xFF {
            i += 1;
        }
        let code = data[i + 1];
        i += 2;
        match code {
            0xD9 => return Ok(i),
            // Standalone markers: TEM, RST0..7.
            0x01 | 0xD0..=0xD7 => continue,
            // A stuffed zero or a second SOI is not a marker here.
            0x00 | 0xD8 => return Err(Unreadable),
            _ => {}
        }
        let len_bytes = data.get(i..i + 2).ok_or(Unreadable)?;
        let len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
        if len < 2 {
            return Err(Unreadable);
        }
        let payload = data.get(i + 2..i + len).ok_or(Unreadable)?;
        if code == 0xE2 && payload.starts_with(b"MPF\0") {
            return Err(MultiPicture);
        }
        i += len;
        if code == 0xDA {
            i = skip_entropy_data(data, i).ok_or(Unreadable)?;
        }
    }
}

/// From the start of a scan's entropy-coded data, the offset of the next real marker (its
/// `0xFF`), or `None` if the data ends first.
fn skip_entropy_data(data: &[u8], mut i: usize) -> Option<usize> {
    loop {
        i += data.get(i..)?.iter().position(|&b| b == 0xFF)?;
        match *data.get(i + 1)? {
            // Stuffed 0xFF data byte, or a restart marker inside the scan.
            0x00 | 0xD0..=0xD7 => i += 2,
            // A fill byte: the marker starts at the next 0xFF.
            0xFF => i += 1,
            _ => return Some(i),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(code: u8, payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, code];
        s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        s.extend_from_slice(payload);
        s
    }

    /// A structurally complete JPEG (not decodable; only the layout matters here): SOI,
    /// APP0, SOF0, SOS, entropy data with stuffing, a restart marker and fill bytes, EOI.
    fn jpeg_with(extra: &[Vec<u8>]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        j.extend(segment(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        for e in extra {
            j.extend_from_slice(e);
        }
        j.extend(segment(0xC0, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]));
        j.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
        j.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56, 0xFF, 0xFF, 0xD9]);
        j
    }

    #[test]
    fn plain_jpeg_is_one_image() {
        assert_eq!(check_single_image(&jpeg_with(&[])), Ok(()));
        // Other APP2 segments (ICC) are fine.
        let icc = segment(0xE2, b"ICC_PROFILE\0\x01\x01abc");
        assert_eq!(check_single_image(&jpeg_with(&[icc])), Ok(()));
    }

    #[test]
    fn progressive_scans_with_tables_between_them() {
        let mut j = jpeg_with(&[]);
        j.truncate(j.len() - 2); // drop EOI
        j.extend(segment(0xC4, &[0x10; 17]));
        j.extend(segment(0xDA, &[1, 1, 0, 1, 63, 0]));
        j.extend_from_slice(&[0xAB, 0xFF, 0x00, 0xFF, 0xD9]);
        assert_eq!(check_single_image(&j), Ok(()));
    }

    #[test]
    fn data_after_eoi_is_refused() {
        let mut j = jpeg_with(&[]);
        j.push(0);
        assert_eq!(check_single_image(&j), Err(NotSingleImage::TrailingData));
        // A whole second JPEG after the first (MPF without its index, motion photo).
        let mut two = jpeg_with(&[]);
        two.extend(jpeg_with(&[]));
        assert_eq!(check_single_image(&two), Err(NotSingleImage::TrailingData));
        // An EOI-looking pair inside a segment payload does not end the image early.
        let tricky = segment(0xFE, &[0xFF, 0xD9]);
        assert_eq!(check_single_image(&jpeg_with(&[tricky])), Ok(()));
    }

    #[test]
    fn mpf_segment_is_refused() {
        let mpf = segment(0xE2, b"MPF\0MM\0\x2a\0\0\0\x08");
        assert_eq!(
            check_single_image(&jpeg_with(&[mpf])),
            Err(NotSingleImage::MultiPicture)
        );
    }

    #[test]
    fn hostile_and_truncated_inputs_are_unreadable_without_panicking() {
        let full = jpeg_with(&[]);
        for cut in 0..full.len() {
            assert_ne!(check_single_image(&full[..cut]), Ok(()), "cut {cut}");
        }
        for bad in [
            &b""[..],
            &b"not a jpeg"[..],
            &[0xFF][..],
            &[0xFF, 0xD8][..],
            &[0xFF, 0xD8, 0xFF][..],
            &[0xFF, 0xD8, 0xFF, 0xFF, 0xFF][..],
            &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01][..],
            &[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0, 0][..],
            &[0xFF, 0xD8, 0xFF, 0x00][..],
            &[0xFF, 0xD8, 0xFF, 0xD8, 0xFF, 0xD9][..],
            &[0xFF, 0xD8, 0x00, 0xFF, 0xD9][..],
            &[0xFF, 0xD8, 0xFF, 0xDA, 0, 2, 0xFF, 0x00, 0xFF][..],
            &[0xFF, 0xD8, 0xFF, 0xDA, 0, 2, 0xFF, 0xD0, 0xFF, 0xFF][..],
        ] {
            assert_eq!(
                check_single_image(bad),
                Err(NotSingleImage::Unreadable),
                "{bad:?}"
            );
        }
        // Arbitrary bytes after SOI: any answer, but no panic.
        let mut seed = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut j = vec![0xFF, 0xD8];
            for _ in 0..64 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                j.push(if seed >> 30 == 0 {
                    0xFF
                } else {
                    (seed >> 16) as u8
                });
            }
            let _ = check_single_image(&j);
        }
    }
}
