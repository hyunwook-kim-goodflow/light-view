use std::path::Path;

/// Image formats lv-core can recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    WebP,
    Bmp,
    Heif,
}

/// Number of leading bytes callers should read for `sniff_format`.
pub const SNIFF_LEN: usize = 32;

const HEIF_BRANDS: [&[u8; 4]; 8] = [
    b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1",
];

/// Detect the image format from the first bytes of a file.
pub fn sniff_format(head: &[u8]) -> Option<ImageFormat> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageFormat::Png);
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some(ImageFormat::WebP);
    }
    if head.starts_with(b"BM") && head.len() >= 6 {
        return Some(ImageFormat::Bmp);
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        // Brands are read only inside the ftyp box. Size 0 means the box runs to the end
        // of the file; a box under 16 bytes (or size 1, a 64-bit size) cannot hold the
        // major brand and minor version where they are expected.
        let box_end = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            0 => head.len(),
            1..=15 => return None,
            n => head.len().min(n as usize),
        };
        let major = &head[8..12];
        if !HEIF_BRANDS.iter().any(|b| b.as_slice() == major) {
            return None;
        }
        if major == b"mif1" || major == b"msf1" {
            // Compatible brands start at offset 16, 4 bytes each.
            let compat: Vec<&[u8]> = head
                .get(16..box_end)
                .unwrap_or(&[])
                .chunks_exact(4)
                .collect();
            let has = |brand: &[u8; 4]| compat.contains(&brand.as_slice());
            if has(b"avif") && !has(b"heic") && !has(b"heix") {
                return None;
            }
        }
        return Some(ImageFormat::Heif);
    }
    None
}

/// Supported file extensions, lowercase, without the dot.
pub const EXTENSIONS: [&str; 12] = [
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "webp", "bmp", "dib", "heic", "heif", "hif",
];

/// Case-insensitive check of the file extension against supported ones.
pub fn is_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.iter().any(|s| s.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_known_signatures() {
        assert_eq!(
            sniff_format(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            sniff_format(b"\x89PNG\r\n\x1a\n...."),
            Some(ImageFormat::Png)
        );
        assert_eq!(sniff_format(b"GIF89a"), Some(ImageFormat::Gif));
        assert_eq!(sniff_format(b"GIF87a"), Some(ImageFormat::Gif));
        assert_eq!(
            sniff_format(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(ImageFormat::WebP)
        );
        assert_eq!(sniff_format(b"BM\0\0\0\0"), Some(ImageFormat::Bmp));
        assert_eq!(
            sniff_format(b"\0\0\0\x18ftypheic\0\0\0\0"),
            Some(ImageFormat::Heif)
        );
        assert_eq!(
            sniff_format(b"\0\0\0\x18ftypmif1\0\0\0\0"),
            Some(ImageFormat::Heif)
        );
    }

    #[test]
    fn rejects_unknown_and_short() {
        assert_eq!(sniff_format(b""), None);
        assert_eq!(sniff_format(b"\xFF\xD8"), None);
        assert_eq!(sniff_format(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(sniff_format(b"\0\0\0\x18ftypavif\0\0\0\0"), None);
    }

    #[test]
    fn avif_compatible_mif1_is_rejected_unless_heic() {
        assert_eq!(sniff_format(b"\0\0\0\x1cftypmif1\0\0\0\0avifmif1"), None);
        assert_eq!(
            sniff_format(b"\0\0\0\x1cftypmif1\0\0\0\0avifheic"),
            Some(ImageFormat::Heif)
        );
        assert_eq!(sniff_format(b"\0\0\0\x1cftypmsf1\0\0\0\0avifmsf1"), None);
    }

    #[test]
    fn ftyp_brands_limited_to_box() {
        // A 20-byte ftyp box (one compatible brand); the next box happens to hold "avif".
        assert_eq!(
            sniff_format(b"\0\0\0\x14ftypmif1\0\0\0\0mif1\0\0\0\x08avif"),
            Some(ImageFormat::Heif)
        );
        // An AVIF-only box is not rescued by a "heic" after it.
        assert_eq!(
            sniff_format(b"\0\0\0\x18ftypmif1\0\0\0\0avifmif1heic"),
            None
        );
        // A box too short to hold its major brand and minor version is not HEIF.
        assert_eq!(sniff_format(b"\0\0\0\x0cftypheic\0\0\0\0"), None);
        // Size 0 means "to the end of the file": every brand read counts.
        assert_eq!(sniff_format(b"\0\0\0\0ftypmif1\0\0\0\0avifmif1"), None);
    }

    #[test]
    fn extension_filter_is_case_insensitive() {
        assert!(is_supported_extension(Path::new(r"C:\a\IMG_1.HEIC")));
        assert!(is_supported_extension(Path::new("x.Jpeg")));
        assert!(!is_supported_extension(Path::new("x.avif")));
        assert!(!is_supported_extension(Path::new("noext")));
    }
}
