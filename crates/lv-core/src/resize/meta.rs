//! Metadata for resized files: a fresh EXIF block holding only whitelisted tags, and
//! attaching it to encoded JPEG and WebP bytes.
//!
//! The block is written from scratch (little-endian TIFF), never copied: maker notes,
//! serial numbers, thumbnails, XMP and IPTC cannot survive because nothing outside the
//! whitelist is ever written. The source is untrusted; every value is bounded so the
//! block always fits a JPEG APP1 segment.

use exif::{Context, In, Tag, Value};

use super::OutputFormat;
use crate::jpeg_markers::{Markers, APP1, SOS};

const EXIF_ID: &[u8] = b"Exif\0\0";
/// The largest payload a JPEG segment can hold (its length field counts itself).
const MAX_SEGMENT_PAYLOAD: usize = 65533;
/// Values longer than this are dropped (no whitelisted tag needs more).
const MAX_VALUE_BYTES: usize = 1024;
/// The highest standard GPS tag (GPSHPositioningError); others are not written.
const MAX_GPS_TAG: u16 = 0x1F;

/// Kept in IFD0.
const TIFF_TAGS: [Tag; 3] = [Tag::Make, Tag::Model, Tag::DateTime];
/// Kept in the Exif IFD.
const EXIF_TAGS: [Tag; 12] = [
    Tag::DateTimeOriginal,
    Tag::DateTimeDigitized,
    Tag::OffsetTime,
    Tag::OffsetTimeOriginal,
    Tag::OffsetTimeDigitized,
    Tag::LensMake,
    Tag::LensModel,
    Tag::ExposureTime,
    Tag::FNumber,
    Tag::PhotographicSensitivity,
    Tag::FocalLength,
    Tag::FocalLengthIn35mmFilm,
];

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;

/// The WebP VP8X chunk: header and 10-byte payload.
const VP8X_CHUNK: u64 = 18;
const VP8X_ALPHA: u8 = 0x10;
const VP8X_EXIF: u8 = 0x08;

/// One IFD entry; its value bytes are already little-endian.
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

/// A new EXIF block (`Exif\0\0` and a little-endian TIFF) holding only the whitelisted
/// tags of `src_exif` (a raw EXIF block, the `Exif\0\0` header optional): Make, Model,
/// DateTime, DateTimeOriginal, DateTimeDigitized, the OffsetTime tags, LensMake,
/// LensModel, ExposureTime, FNumber, PhotographicSensitivity (ISOSpeedRatings),
/// FocalLength and FocalLengthIn35mmFilm, plus the standard GPS tags when `keep_gps`.
/// Orientation is always written as 1 (the pixels are already upright). `None` when the
/// source has nothing to keep or cannot be read.
pub fn whitelist_exif(src_exif: Option<&[u8]>, keep_gps: bool) -> Option<Vec<u8>> {
    let exif = crate::exif_info::parse_block(src_exif?)?;
    let (mut ifd0, mut exif_ifd, mut gps) = (Vec::new(), Vec::new(), Vec::new());
    for field in exif.fields() {
        if field.ifd_num != In::PRIMARY {
            continue;
        }
        let tag = field.tag;
        let list = if TIFF_TAGS.contains(&tag) {
            &mut ifd0
        } else if EXIF_TAGS.contains(&tag) {
            &mut exif_ifd
        } else if keep_gps && tag.context() == Context::Gps && tag.number() <= MAX_GPS_TAG {
            &mut gps
        } else {
            continue;
        };
        if let Some(entry) = entry(tag.number(), &field.value) {
            list.push(entry);
        }
    }
    if ifd0.is_empty() && exif_ifd.is_empty() && gps.is_empty() {
        return None;
    }
    ifd0.push(Entry {
        tag: TAG_ORIENTATION,
        kind: TYPE_SHORT,
        count: 1,
        data: 1u16.to_le_bytes().to_vec(),
    });
    Some(write_tiff(ifd0, exif_ifd, gps))
}

/// Little-endian bytes of each element.
fn le<T, const N: usize>(values: &[T], f: impl Fn(&T) -> [u8; N]) -> Vec<u8> {
    values.iter().flat_map(f).collect()
}

fn pair(a: [u8; 4], b: [u8; 4]) -> [u8; 8] {
    let mut out = [0; 8];
    out[..4].copy_from_slice(&a);
    out[4..].copy_from_slice(&b);
    out
}

/// The entry for one value, or `None` for an empty, oversized or unknown-typed one.
fn entry(tag: u16, value: &Value) -> Option<Entry> {
    let (kind, count, data): (u16, usize, Vec<u8>) = match value {
        Value::Byte(v) => (1, v.len(), v.clone()),
        Value::Ascii(v) => {
            let data: Vec<u8> = v
                .iter()
                .flat_map(|s| s.iter().copied().chain([0]))
                .collect();
            (2, data.len(), data)
        }
        Value::Short(v) => (TYPE_SHORT, v.len(), le(v, |x| x.to_le_bytes())),
        Value::Long(v) => (TYPE_LONG, v.len(), le(v, |x| x.to_le_bytes())),
        Value::Rational(v) => (
            5,
            v.len(),
            le(v, |r| pair(r.num.to_le_bytes(), r.denom.to_le_bytes())),
        ),
        Value::SByte(v) => (6, v.len(), le(v, |x| x.to_le_bytes())),
        Value::Undefined(v, _) => (7, v.len(), v.clone()),
        Value::SShort(v) => (8, v.len(), le(v, |x| x.to_le_bytes())),
        Value::SLong(v) => (9, v.len(), le(v, |x| x.to_le_bytes())),
        Value::SRational(v) => (
            10,
            v.len(),
            le(v, |r| pair(r.num.to_le_bytes(), r.denom.to_le_bytes())),
        ),
        Value::Float(v) => (11, v.len(), le(v, |x| x.to_le_bytes())),
        Value::Double(v) => (12, v.len(), le(v, |x| x.to_le_bytes())),
        Value::Unknown(..) => return None,
    };
    if count == 0 || data.len() > MAX_VALUE_BYTES {
        return None;
    }
    Some(Entry {
        tag,
        kind,
        count: u32::try_from(count).ok()?,
        data,
    })
}

/// `Exif\0\0` and a little-endian TIFF: IFD0, then the Exif IFD and the GPS IFD when they
/// have entries, each IFD followed by its out-of-line values. Entries are sorted by tag;
/// a repeated tag keeps its first value.
fn write_tiff(mut ifd0: Vec<Entry>, mut exif_ifd: Vec<Entry>, mut gps: Vec<Entry>) -> Vec<u8> {
    for list in [&mut ifd0, &mut exif_ifd, &mut gps] {
        list.sort_by_key(|e| e.tag);
        list.dedup_by_key(|e| e.tag);
    }
    let pointer = |tag| Entry {
        tag,
        kind: TYPE_LONG,
        count: 1,
        data: vec![0; 4],
    };
    if !exif_ifd.is_empty() {
        ifd0.push(pointer(TAG_EXIF_IFD));
    }
    if !gps.is_empty() {
        ifd0.push(pointer(TAG_GPS_IFD));
    }
    ifd0.sort_by_key(|e| e.tag);
    // Pointer values are inline, so the IFD sizes do not depend on them. The whole block
    // stays far below 4 GiB (and a JPEG segment): at most 47 entries of 1 KiB.
    let exif_at = 8 + ifd_len(&ifd0);
    let gps_at = exif_at + ifd_len(&exif_ifd);
    for e in &mut ifd0 {
        match e.tag {
            TAG_EXIF_IFD => e.data = (exif_at as u32).to_le_bytes().to_vec(),
            TAG_GPS_IFD => e.data = (gps_at as u32).to_le_bytes().to_vec(),
            _ => {}
        }
    }
    let mut tiff = b"II*\0".to_vec();
    tiff.extend_from_slice(&8u32.to_le_bytes());
    for list in [&ifd0, &exif_ifd, &gps] {
        write_ifd(&mut tiff, list);
    }
    [EXIF_ID, &tiff].concat()
}

/// Bytes an IFD takes with its out-of-line values (each padded to even); 0 when empty.
fn ifd_len(entries: &[Entry]) -> usize {
    if entries.is_empty() {
        return 0;
    }
    let values: usize = entries
        .iter()
        .filter(|e| e.data.len() > 4)
        .map(|e| e.data.len() + (e.data.len() & 1))
        .sum();
    2 + 12 * entries.len() + 4 + values
}

/// Appends an IFD (nothing when empty) at the end of `tiff`, with no next IFD.
fn write_ifd(tiff: &mut Vec<u8>, entries: &[Entry]) {
    if entries.is_empty() {
        return;
    }
    let mut value_at = tiff.len() + 2 + 12 * entries.len() + 4;
    let mut values = Vec::new();
    tiff.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for e in entries {
        tiff.extend_from_slice(&e.tag.to_le_bytes());
        tiff.extend_from_slice(&e.kind.to_le_bytes());
        tiff.extend_from_slice(&e.count.to_le_bytes());
        if e.data.len() <= 4 {
            let mut inline = [0u8; 4];
            inline[..e.data.len()].copy_from_slice(&e.data);
            tiff.extend_from_slice(&inline);
        } else {
            tiff.extend_from_slice(&(value_at as u32).to_le_bytes());
            values.extend_from_slice(&e.data);
            if e.data.len() & 1 == 1 {
                values.push(0);
            }
            value_at += e.data.len() + (e.data.len() & 1);
        }
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(&values);
}

/// `bytes` (an encoded file of `format`) with `exif` (a block from [`whitelist_exif`],
/// `Exif\0\0` header included) attached. JPEG: an APP1 segment right after SOI; EXIF
/// APP1 segments already there are dropped, JFIF and the rest kept. WebP: an `EXIF` chunk
/// holding the TIFF (without the header) before any `XMP ` chunk, replacing any existing
/// one, with the EXIF flag set in VP8X (a VP8X is created from the VP8/VP8L header for a
/// simple-format file). PNG: `bytes` unchanged. Errs on input it cannot parse.
pub fn attach_exif(bytes: &[u8], format: OutputFormat, exif: &[u8]) -> Result<Vec<u8>, String> {
    if format == OutputFormat::Png {
        return Ok(bytes.to_vec());
    }
    let tiff = exif
        .strip_prefix(EXIF_ID)
        .ok_or("EXIF block without the Exif header")?;
    match format {
        OutputFormat::Jpeg => attach_jpeg(bytes, exif),
        OutputFormat::WebP => attach_webp(bytes, tiff),
        OutputFormat::Png => unreachable!("handled above"),
    }
}

/// The bytes [`attach_exif`] adds for an EXIF block of `exif_len` bytes (header
/// included). Exact for JPEG (without an EXIF APP1 already), PNG (0) and simple-format
/// WebP; a WebP that already has a VP8X chunk gets no new one, so for it this is 18 bytes
/// over: an upper bound.
pub fn exif_overhead(format: OutputFormat, exif_len: usize) -> u64 {
    let len = exif_len as u64;
    match format {
        OutputFormat::Jpeg => 4 + len,
        OutputFormat::Png => 0,
        OutputFormat::WebP => {
            let tiff = len.saturating_sub(EXIF_ID.len() as u64);
            VP8X_CHUNK + 8 + tiff + (tiff & 1)
        }
    }
}

fn attach_jpeg(jpeg: &[u8], exif: &[u8]) -> Result<Vec<u8>, String> {
    if exif.len() > MAX_SEGMENT_PAYLOAD {
        return Err(format!("EXIF block of {} bytes", exif.len()));
    }
    // Existing EXIF segments before the first scan, each with any fill bytes before it.
    let mut drop = Vec::new();
    let mut prev = 2;
    for marker in Markers::new(jpeg) {
        let marker = marker.map_err(|_| "not a readable JPEG".to_string())?;
        if marker.code == SOS {
            break;
        }
        if marker.code == APP1 && marker.payload.starts_with(EXIF_ID) {
            drop.push(prev..marker.end);
        }
        prev = marker.end;
    }
    let mut out = Vec::with_capacity(jpeg.len() + exif.len() + 4);
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&[0xFF, APP1]);
    out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(exif);
    let mut at = 2;
    for range in drop {
        out.extend_from_slice(&jpeg[at..range.start]);
        at = range.end;
    }
    out.extend_from_slice(&jpeg[at..]);
    Ok(out)
}

fn attach_webp(webp: &[u8], tiff: &[u8]) -> Result<Vec<u8>, String> {
    let chunks = webp_chunks(webp).ok_or("not a readable WebP")?;
    let &(first, payload) = chunks.first().ok_or("WebP without chunks")?;
    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let rest = match &first {
        b"VP8X" => {
            let mut vp8x = payload.to_vec();
            *vp8x.first_mut().ok_or("empty VP8X chunk")? |= VP8X_EXIF;
            write_chunk(&mut out, b"VP8X", &vp8x);
            &chunks[1..]
        }
        b"VP8 " | b"VP8L" => {
            let (w, h, alpha) = canvas(&first, payload).ok_or("unreadable WebP header")?;
            let flags = VP8X_EXIF | if alpha { VP8X_ALPHA } else { 0 };
            let mut vp8x = vec![flags, 0, 0, 0];
            vp8x.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
            vp8x.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
            write_chunk(&mut out, b"VP8X", &vp8x);
            &chunks[..]
        }
        _ => return Err("WebP starts with an unknown chunk".into()),
    };
    let mut exif_written = false;
    for (fourcc, payload) in rest {
        if fourcc == b"EXIF" {
            continue;
        }
        if fourcc == b"XMP " && !exif_written {
            write_chunk(&mut out, b"EXIF", tiff);
            exif_written = true;
        }
        write_chunk(&mut out, fourcc, payload);
    }
    if !exif_written {
        write_chunk(&mut out, b"EXIF", tiff);
    }
    let riff = u32::try_from(out.len() - 8).map_err(|_| "WebP too large".to_string())?;
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    Ok(out)
}

/// The chunks of a WebP, or `None` if the RIFF structure does not hold together. Bytes
/// past the RIFF size are ignored; a missing pad byte after the last chunk is tolerated.
fn webp_chunks(webp: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    if webp.get(..4)? != b"RIFF" || webp.get(8..12)? != b"WEBP" {
        return None;
    }
    let size = usize::try_from(u32::from_le_bytes(webp[4..8].try_into().ok()?)).ok()?;
    let body = webp.get(..size.checked_add(8)?)?;
    let mut chunks = Vec::new();
    let mut at = 12;
    while at < body.len() {
        let header = body.get(at..at.checked_add(8)?)?;
        let fourcc: [u8; 4] = header[..4].try_into().ok()?;
        let len = usize::try_from(u32::from_le_bytes(header[4..].try_into().ok()?)).ok()?;
        let payload_end = (at + 8).checked_add(len)?;
        chunks.push((fourcc, body.get(at + 8..payload_end)?));
        at = payload_end + (len & 1);
    }
    Some(chunks)
}

/// Appends a RIFF chunk, padded to an even size. Payloads come from a parsed RIFF or are
/// bounded (VP8X, the EXIF block), so the length fits a `u32`.
fn write_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() & 1 == 1 {
        out.push(0);
    }
}

/// Canvas width, height and whether alpha is used, from a VP8 or VP8L chunk.
fn canvas(fourcc: &[u8; 4], payload: &[u8]) -> Option<(u32, u32, bool)> {
    let (w, h, alpha) = if fourcc == b"VP8L" {
        if *payload.first()? != 0x2F {
            return None;
        }
        let bits = u32::from_le_bytes(payload.get(1..5)?.try_into().ok()?);
        let w = (bits & 0x3FFF) + 1;
        let h = ((bits >> 14) & 0x3FFF) + 1;
        (w, h, (bits >> 28) & 1 == 1)
    } else {
        let header = payload.get(..10)?;
        if header[3..6] != [0x9D, 0x01, 0x2A] {
            return None;
        }
        let dim = |i: usize| u32::from(u16::from_le_bytes([header[i], header[i + 1]]) & 0x3FFF);
        (dim(6), dim(8), false)
    };
    (w > 0 && h > 0).then_some((w, h, alpha))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use exif::experimental::Writer;
    use exif::{Exif, Field, In, Rational, Tag, Value};
    use image::{ExtendedColorType, ImageEncoder};

    use super::*;
    use crate::resize::encode;
    use crate::test_fixtures::rgba_noise;

    fn ascii(s: &str) -> Value {
        Value::Ascii(vec![s.as_bytes().to_vec()])
    }

    fn field(tag: Tag, value: Value) -> Field {
        Field {
            tag,
            ifd_num: In::PRIMARY,
            value,
        }
    }

    fn rational(num: u32, denom: u32) -> Rational {
        Rational { num, denom }
    }

    /// A big-endian EXIF block (with the `Exif\0\0` header) holding kept and dropped tags,
    /// GPS and a thumbnail.
    fn camera_exif(orientation: u16) -> Vec<u8> {
        let thumb = encode(&rgba_noise(8, 8, 1), (8, 8), OutputFormat::Jpeg, 80).unwrap();
        let fields = [
            field(Tag::Make, ascii("Apple")),
            field(Tag::Model, ascii("iPhone 15 Pro")),
            field(Tag::Orientation, Value::Short(vec![orientation])),
            field(Tag::Artist, ascii("Someone")),
            field(Tag::DateTimeOriginal, ascii("2026:10:28 09:30:00")),
            field(Tag::OffsetTimeOriginal, ascii("+09:00")),
            field(Tag::BodySerialNumber, ascii("SN-12345")),
            field(
                Tag::MakerNote,
                Value::Undefined(b"Apple iOS\0secret".to_vec(), 0),
            ),
            field(Tag::ExposureTime, Value::Rational(vec![rational(1, 250)])),
            field(Tag::PhotographicSensitivity, Value::Short(vec![64])),
            field(Tag::GPSLatitudeRef, ascii("N")),
            field(
                Tag::GPSLatitude,
                Value::Rational(vec![rational(37, 1), rational(33, 1), rational(1234, 100)]),
            ),
        ];
        let mut writer = Writer::new();
        for f in &fields {
            writer.push_field(f);
        }
        writer.set_jpeg(&thumb, In::THUMBNAIL);
        let mut buf = Cursor::new(Vec::new());
        writer.write(&mut buf, false).unwrap();
        let mut out = b"Exif\0\0".to_vec();
        out.extend(buf.into_inner());
        out
    }

    fn read(block: &[u8]) -> Exif {
        assert!(block.starts_with(b"Exif\0\0"), "missing Exif header");
        exif::Reader::new().read_raw(block[6..].to_vec()).unwrap()
    }

    fn shown(exif: &Exif, tag: Tag) -> Option<String> {
        exif.get_field(tag, In::PRIMARY)
            .map(|f| f.display_value().to_string())
    }

    #[test]
    fn gps_is_removed() {
        let src = camera_exif(1);
        let out = whitelist_exif(Some(&src), false).expect("kept tags");
        let (src, out) = (read(&src), read(&out));
        for gone in [
            Tag::GPSLatitude,
            Tag::GPSLatitudeRef,
            Tag::Artist,
            Tag::BodySerialNumber,
            Tag::MakerNote,
        ] {
            assert!(shown(&src, gone).is_some(), "{gone}");
            assert_eq!(shown(&out, gone), None, "{gone}");
        }
        assert!(src.fields().any(|f| f.ifd_num == In::THUMBNAIL));
        assert!(
            out.fields().all(|f| f.ifd_num == In::PRIMARY),
            "thumbnail kept"
        );
        for kept in [
            Tag::Make,
            Tag::Model,
            Tag::DateTimeOriginal,
            Tag::OffsetTimeOriginal,
            Tag::ExposureTime,
            Tag::PhotographicSensitivity,
        ] {
            assert!(shown(&src, kept).is_some(), "{kept}");
            assert_eq!(shown(&out, kept), shown(&src, kept), "{kept}");
        }
    }

    #[test]
    fn gps_kept_when_asked() {
        let src = camera_exif(1);
        let out = whitelist_exif(Some(&src), true).expect("kept tags");
        let (src, out) = (read(&src), read(&out));
        for tag in [Tag::GPSLatitude, Tag::GPSLatitudeRef] {
            assert!(shown(&src, tag).is_some(), "{tag}");
            assert_eq!(shown(&out, tag), shown(&src, tag), "{tag}");
        }
        assert_eq!(shown(&out, Tag::Artist), None);
    }

    #[test]
    fn orientation_tag_is_reset() {
        let out = whitelist_exif(Some(&camera_exif(6)), false).expect("kept tags");
        let orientation = read(&out)
            .get_field(Tag::Orientation, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0));
        assert_eq!(orientation, Some(1));
    }

    #[test]
    fn no_exif_gives_none() {
        assert_eq!(whitelist_exif(None, true), None);
        assert_eq!(whitelist_exif(Some(b"Exif\0\0garbage"), true), None);
        // Only tags that are dropped (Orientation is rewritten, not kept).
        let fields = [
            field(Tag::Orientation, Value::Short(vec![6])),
            field(Tag::Artist, ascii("Someone")),
        ];
        let mut writer = Writer::new();
        for f in &fields {
            writer.push_field(f);
        }
        let mut buf = Cursor::new(Vec::new());
        writer.write(&mut buf, true).unwrap();
        assert_eq!(whitelist_exif(Some(&buf.into_inner()), true), None);
    }

    fn jpeg() -> Vec<u8> {
        encode(&rgba_noise(40, 30, 2), (40, 30), OutputFormat::Jpeg, 80).unwrap()
    }

    /// A simple-format lossless WebP (a lone VP8L chunk).
    fn webp() -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .write_image(&rgba_noise(40, 30, 2), 40, 30, ExtendedColorType::Rgba8)
            .unwrap();
        assert_eq!(&out[12..16], b"VP8L");
        out
    }

    #[test]
    fn attached_size_matches_overhead() {
        let exif = whitelist_exif(Some(&camera_exif(1)), true).unwrap();
        for (format, plain) in [(OutputFormat::Jpeg, jpeg()), (OutputFormat::WebP, webp())] {
            let attached = attach_exif(&plain, format, &exif).unwrap();
            assert_eq!(
                (attached.len() - plain.len()) as u64,
                exif_overhead(format, exif.len()),
                "{format:?}"
            );
        }
        // Odd-length TIFF data gets a WebP pad byte.
        let odd = [&exif[..], b"\0"].concat();
        let attached = attach_exif(&webp(), OutputFormat::WebP, &odd).unwrap();
        assert_eq!(
            (attached.len() - webp().len()) as u64,
            exif_overhead(OutputFormat::WebP, odd.len())
        );
        let png = crate::test_fixtures::png_solid(2, 2, [1, 2, 3, 255]);
        assert_eq!(attach_exif(&png, OutputFormat::Png, &exif).unwrap(), png);
        assert_eq!(exif_overhead(OutputFormat::Png, exif.len()), 0);
    }

    #[test]
    fn attached_exif_reads_back() {
        let exif = whitelist_exif(Some(&camera_exif(6)), false).unwrap();
        for (format, plain) in [(OutputFormat::Jpeg, jpeg()), (OutputFormat::WebP, webp())] {
            let attached = attach_exif(&plain, format, &exif).unwrap();
            let read = exif::Reader::new()
                .read_from_container(&mut Cursor::new(&attached))
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!(
                shown(&read, Tag::DateTimeOriginal).as_deref(),
                Some("2026-10-28 09:30:00"),
                "{format:?}"
            );
            let img = image::load_from_memory(&attached).unwrap();
            assert_eq!((img.width(), img.height()), (40, 30), "{format:?}");
        }
    }

    #[test]
    fn webp_extended_gets_one_exif_and_the_flag() {
        let exif = whitelist_exif(Some(&camera_exif(1)), false).unwrap();
        let once = attach_exif(&webp(), OutputFormat::WebP, &exif).unwrap();
        assert_eq!(&once[12..16], b"VP8X");
        assert_eq!(once[20] & 0x08, 0x08, "EXIF flag");
        // Canvas 40x30, stored minus one.
        assert_eq!(&once[24..30], &[39, 0, 0, 29, 0, 0]);
        // Attaching again replaces the chunk: the file does not grow.
        let twice = attach_exif(&once, OutputFormat::WebP, &exif).unwrap();
        assert_eq!(twice, once);
        assert!(exif_overhead(OutputFormat::WebP, exif.len()) >= 18);
    }

    #[test]
    fn lossy_webp_canvas_comes_from_the_vp8_header() {
        // A VP8 chunk header only: frame tag, start code, 300x200.
        let mut vp8 = vec![0x10, 0x02, 0x00, 0x9D, 0x01, 0x2A];
        vp8.extend_from_slice(&300u16.to_le_bytes());
        vp8.extend_from_slice(&200u16.to_le_bytes());
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&(4 + 8 + vp8.len() as u32).to_le_bytes());
        webp.extend_from_slice(b"WEBPVP8 ");
        webp.extend_from_slice(&(vp8.len() as u32).to_le_bytes());
        webp.extend_from_slice(&vp8);
        let exif = whitelist_exif(Some(&camera_exif(1)), false).unwrap();
        let out = attach_exif(&webp, OutputFormat::WebP, &exif).unwrap();
        assert_eq!(&out[12..16], b"VP8X");
        assert_eq!(out[20], 0x08, "EXIF flag only, no alpha");
        assert_eq!(&out[24..30], &[0x2B, 0x01, 0, 0xC7, 0, 0]);
        let riff = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff + 8, out.len());
        assert_eq!(
            (out.len() - webp.len()) as u64,
            exif_overhead(OutputFormat::WebP, exif.len())
        );
    }

    #[test]
    fn bad_input_is_an_error() {
        let exif = whitelist_exif(Some(&camera_exif(1)), false).unwrap();
        assert!(attach_exif(b"not a jpeg", OutputFormat::Jpeg, &exif).is_err());
        assert!(attach_exif(b"RIFF\x04\0\0\0WEBP", OutputFormat::WebP, &exif).is_err());
        assert!(attach_exif(&jpeg(), OutputFormat::Jpeg, &exif[6..]).is_err());
    }
}
