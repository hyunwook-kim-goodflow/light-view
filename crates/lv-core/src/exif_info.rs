//! Read-only EXIF extraction for the info card: a short summary and the full tag list.
//!
//! The input is untrusted. The TIFF block is capped at 4 MiB, at most 2,000 entries are
//! returned, and every value is cut to 512 characters with control characters replaced by
//! visible escapes, so a hostile file cannot break the card's layout or grow it without
//! bound.

/// The few facts the card shows without expanding.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExifSummary {
    /// `DateTimeOriginal`, else `DateTime`, as stored (`YYYY:MM:DD HH:MM:SS`).
    pub taken: Option<String>,
    /// Make and model, without repeating the make.
    pub camera: Option<String>,
    /// For example `1/250s · f/1.8 · ISO 64`; parts that are missing are left out.
    pub exposure: Option<String>,
    /// Latitude and longitude in decimal degrees (south and west are negative).
    pub gps: Option<(f64, f64)>,
}

/// One tag of the full list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExifEntry {
    /// `IFD0`, `Exif`, `GPS`, `Interop` or `IFD1`.
    pub group: String,
    pub name: String,
    pub value: String,
}

use std::fmt::Write as _;

use exif::{Exif, In, Tag, Value};

use crate::format::{sniff_format, ImageFormat, SNIFF_LEN};
use crate::heif_edit;
use crate::jpeg_markers::{Markers, APP1, SOS};

/// Most TIFF bytes looked at.
const MAX_TIFF: usize = 4 << 20;
/// Most entries returned.
const MAX_ENTRIES: usize = 2000;
/// Most characters in one value, including the ellipsis of a cut one.
const MAX_VALUE_CHARS: usize = 512;

/// The EXIF summary and tag list of an image file, or `None` without readable EXIF.
pub fn read_exif(bytes: &[u8]) -> Option<(ExifSummary, Vec<ExifEntry>)> {
    let head = &bytes[..bytes.len().min(SNIFF_LEN)];
    let tiff = match sniff_format(head)? {
        ImageFormat::Jpeg => jpeg_tiff(bytes),
        ImageFormat::Png => png_tiff(bytes),
        ImageFormat::WebP => webp_tiff(bytes),
        ImageFormat::Heif => heif_edit::exif_item_tiff(bytes),
        ImageFormat::Gif | ImageFormat::Bmp => None,
    }?;
    let tiff = &tiff[..tiff.len().min(MAX_TIFF)];
    let exif = parse(tiff)?;
    if exif.fields().len() == 0 {
        return None;
    }
    Some((summarize(&exif), entries(&exif)))
}

/// Parses a TIFF block, keeping what was read before an error.
fn parse(tiff: &[u8]) -> Option<Exif> {
    let mut reader = exif::Reader::new();
    reader.continue_on_error(true);
    match reader.read_raw(tiff.to_vec()) {
        Ok(exif) => Some(exif),
        Err(e) => e.distill_partial_result(|_| {}).ok(),
    }
}

const EXIF_PREFIX: &[u8] = b"Exif\0\0";

fn strip_prefix(block: &[u8]) -> &[u8] {
    block.strip_prefix(EXIF_PREFIX).unwrap_or(block)
}

fn jpeg_tiff(data: &[u8]) -> Option<&[u8]> {
    for marker in Markers::new(data) {
        let marker = marker.ok()?;
        if marker.code == SOS {
            break;
        }
        if marker.code == APP1 {
            if let Some(tiff) = marker.payload.strip_prefix(EXIF_PREFIX) {
                return Some(tiff);
            }
        }
    }
    None
}

fn png_tiff(data: &[u8]) -> Option<&[u8]> {
    let mut at: usize = 8;
    while let Some(header) = data.get(at..at.checked_add(8)?) {
        let len = usize::try_from(u32::from_be_bytes(header[..4].try_into().ok()?)).ok()?;
        let body = at + 8;
        let end = body.checked_add(len)?;
        if &header[4..] == b"eXIf" {
            return Some(strip_prefix(data.get(body..end)?));
        }
        // Past the data and its 4-byte CRC.
        at = end.checked_add(4)?;
    }
    None
}

fn webp_tiff(data: &[u8]) -> Option<&[u8]> {
    let mut at: usize = 12;
    while let Some(header) = data.get(at..at.checked_add(8)?) {
        let len = usize::try_from(u32::from_le_bytes(header[4..].try_into().ok()?)).ok()?;
        let body = at + 8;
        let end = body.checked_add(len)?;
        if &header[..4] == b"EXIF" {
            return Some(strip_prefix(data.get(body..end)?));
        }
        // Chunks are padded to an even size.
        at = end.checked_add(len & 1)?;
    }
    None
}

// ---- Text -------------------------------------------------------------------------------

/// Characters that could break the card's layout or reorder its text.
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
        )
}

/// Collects text up to one character over the value limit, replacing unsafe characters by
/// `\u{XX}` escapes; refuses more once full, which stops the formatter feeding it.
#[derive(Default)]
struct Capped {
    text: String,
    chars: usize,
}

impl Capped {
    fn push(&mut self, c: char) -> std::fmt::Result {
        if self.chars > MAX_VALUE_CHARS {
            return Err(std::fmt::Error);
        }
        self.text.push(c);
        self.chars += 1;
        Ok(())
    }

    /// The text, cut to the limit with a trailing ellipsis when it was longer.
    fn finish(mut self) -> String {
        if self.chars > MAX_VALUE_CHARS {
            let cut = self
                .text
                .char_indices()
                .nth(MAX_VALUE_CHARS - 1)
                .map_or(self.text.len(), |(i, _)| i);
            self.text.truncate(cut);
            self.text.push('\u{2026}');
        }
        self.text
    }
}

impl std::fmt::Write for Capped {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for c in s.chars() {
            if is_unsafe(c) {
                for e in format!("\\u{{{:X}}}", u32::from(c)).chars() {
                    self.push(e)?;
                }
            } else {
                self.push(c)?;
            }
        }
        Ok(())
    }
}

/// `s` made safe to show and cut to the value limit.
fn clean(s: &str) -> String {
    let mut out = Capped::default();
    let _ = out.write_str(s);
    out.finish()
}

// ---- Entries ----------------------------------------------------------------------------

fn group_name(tag: Tag, ifd: In) -> String {
    match tag.context() {
        exif::Context::Exif => "Exif".into(),
        exif::Context::Gps => "GPS".into(),
        exif::Context::Interop => "Interop".into(),
        _ => format!("IFD{}", ifd.index()),
    }
}

fn entries(exif: &Exif) -> Vec<ExifEntry> {
    exif.fields()
        .take(MAX_ENTRIES)
        .map(|field| {
            let name = if field.tag.description().is_some() {
                field.tag.to_string()
            } else {
                format!("Tag 0x{:04X}", field.tag.number())
            };
            let mut value = Capped::default();
            let _ = write!(value, "{}", field.display_value().with_unit(exif));
            ExifEntry {
                group: group_name(field.tag, field.ifd_num),
                name: clean(&name),
                value: value.finish(),
            }
        })
        .collect()
}

// ---- Summary ----------------------------------------------------------------------------

fn summarize(exif: &Exif) -> ExifSummary {
    let taken = text(exif, Tag::DateTimeOriginal).or_else(|| text(exif, Tag::DateTime));
    ExifSummary {
        taken,
        camera: camera(exif),
        exposure: exposure(exif),
        gps: gps(exif),
    }
}

/// The first non-blank text of an ASCII tag in the primary IFD.
fn text(exif: &Exif, tag: Tag) -> Option<String> {
    let Value::Ascii(parts) = &exif.get_field(tag, In::PRIMARY)?.value else {
        return None;
    };
    parts.iter().find_map(|part| {
        let s = String::from_utf8_lossy(part);
        let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        (!s.is_empty()).then(|| clean(s))
    })
}

fn camera(exif: &Exif) -> Option<String> {
    let make = text(exif, Tag::Make);
    let model = text(exif, Tag::Model);
    let joined = match (make, model) {
        (Some(make), Some(model)) => {
            // Many models already start with the make ("Canon EOS R5").
            if model.to_lowercase().starts_with(&make.to_lowercase()) {
                model
            } else {
                format!("{make} {model}")
            }
        }
        (one, other) => one.or(other)?,
    };
    Some(clean(&joined))
}

fn rational(exif: &Exif, tag: Tag) -> Option<(u32, u32)> {
    match &exif.get_field(tag, In::PRIMARY)?.value {
        Value::Rational(v) => v.first().map(|r| (r.num, r.denom)),
        _ => None,
    }
}

/// `1.8` for 1.8 and `8` for 8.0.
fn plain_number(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

fn exposure_time(num: u32, denom: u32) -> Option<String> {
    if num == 0 || denom == 0 {
        return None;
    }
    if num < denom {
        let n = if num == 1 {
            u64::from(denom)
        } else {
            (f64::from(denom) / f64::from(num)).round().max(1.0) as u64
        };
        return Some(format!("1/{n}s"));
    }
    Some(format!(
        "{}s",
        plain_number(f64::from(num) / f64::from(denom), 1)
    ))
}

fn exposure(exif: &Exif) -> Option<String> {
    let mut parts = Vec::new();
    if let Some((num, denom)) = rational(exif, Tag::ExposureTime) {
        parts.extend(exposure_time(num, denom));
    }
    if let Some((num, denom)) = rational(exif, Tag::FNumber) {
        let f = f64::from(num) / f64::from(denom);
        if denom != 0 && f > 0.0 && f.is_finite() {
            parts.push(format!("f/{}", plain_number(f, 1)));
        }
    }
    let iso = exif
        .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .filter(|&iso| iso > 0);
    if let Some(iso) = iso {
        parts.push(format!("ISO {iso}"));
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// Decimal degrees from a degrees/minutes/seconds triple and its N/S or E/W reference.
fn coordinate(exif: &Exif, value: Tag, reference: Tag, positive: u8, negative: u8) -> Option<f64> {
    let Value::Rational(dms) = &exif.get_field(value, In::PRIMARY)?.value else {
        return None;
    };
    if dms.is_empty() || dms.len() > 3 {
        return None;
    }
    let degrees: f64 = dms
        .iter()
        .zip([1.0, 60.0, 3600.0])
        .map(|(r, unit)| r.to_f64() / unit)
        .sum();
    let Value::Ascii(parts) = &exif.get_field(reference, In::PRIMARY)?.value else {
        return None;
    };
    let sign = match parts.first()?.first()?.to_ascii_uppercase() {
        c if c == positive => 1.0,
        c if c == negative => -1.0,
        _ => return None,
    };
    degrees.is_finite().then_some(sign * degrees)
}

fn gps(exif: &Exif) -> Option<(f64, f64)> {
    let lat = coordinate(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, b'N', b'S')?;
    let lon = coordinate(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, b'E', b'W')?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;
    use exif::experimental::Writer;
    use exif::{Context, Field, In, Tag, Value};

    fn ascii(tag: Tag, s: &str) -> Field {
        Field {
            tag,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![s.as_bytes().to_vec()]),
        }
    }

    fn rationals(tag: Tag, v: &[(u32, u32)]) -> Field {
        Field {
            tag,
            ifd_num: In::PRIMARY,
            value: Value::Rational(v.iter().map(|&r| r.into()).collect()),
        }
    }

    fn short(tag: Tag, v: u16) -> Field {
        Field {
            tag,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![v]),
        }
    }

    fn tiff(fields: &[Field]) -> Vec<u8> {
        let mut w = Writer::new();
        for f in fields {
            w.push_field(f);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        w.write(&mut out, false).unwrap();
        out.into_inner()
    }

    fn app1_jpeg(tiff: &[u8]) -> Vec<u8> {
        let base = test_fixtures::jpeg_16x8_with_orientation(1);
        // Drop the fixture's own APP1 (right after SOI) and put ours there.
        let len = usize::from(u16::from_be_bytes([base[4], base[5]]));
        let mut out = base[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(tiff);
        out.extend_from_slice(&base[4 + len..]);
        out
    }

    fn png_with(tiff: &[u8]) -> Vec<u8> {
        let base = test_fixtures::png_2x1();
        let mut out = base[..33].to_vec(); // signature + IHDR
        out.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
        out.extend_from_slice(b"eXIf");
        out.extend_from_slice(tiff);
        out.extend_from_slice(&[0; 4]); // CRC is not checked
        out.extend_from_slice(&base[33..]);
        out
    }

    fn webp_with(payload: &[u8]) -> Vec<u8> {
        let mut chunks = b"EXIF".to_vec();
        chunks.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        chunks.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            chunks.push(0);
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&chunks);
        out
    }

    fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(typ);
        b.extend_from_slice(payload);
        b
    }

    fn full(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0; 4];
        p.extend_from_slice(payload);
        bx(typ, &p)
    }

    /// A HEIF file whose only item is an EXIF item holding `tiff` behind the
    /// `exif_tiff_header_offset` field (with `gap` bytes between them).
    fn heif_with(tiff: &[u8], gap: &[u8]) -> Vec<u8> {
        let mut item = (gap.len() as u32).to_be_bytes().to_vec();
        item.extend_from_slice(gap);
        item.extend_from_slice(tiff);
        let build = |at: u32| {
            let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
            let mut hdlr = vec![0; 4];
            hdlr.extend_from_slice(b"pict");
            hdlr.extend_from_slice(&[0; 13]);
            let pitm = full(b"pitm", &1u16.to_be_bytes());
            let mut infe = vec![2, 0, 0, 0];
            infe.extend_from_slice(&1u16.to_be_bytes()); // item id
            infe.extend_from_slice(&[0, 0]); // protection index
            infe.extend_from_slice(b"Exif\0");
            let mut iinf = 1u16.to_be_bytes().to_vec();
            iinf.extend(bx(b"infe", &infe));
            // iloc version 0: 4-byte offset and length, no base offset, one extent.
            let mut iloc = vec![0x44, 0x00];
            iloc.extend_from_slice(&1u16.to_be_bytes()); // item count
            iloc.extend_from_slice(&1u16.to_be_bytes()); // item id
            iloc.extend_from_slice(&0u16.to_be_bytes()); // data reference
            iloc.extend_from_slice(&1u16.to_be_bytes()); // extent count
            iloc.extend_from_slice(&at.to_be_bytes());
            iloc.extend_from_slice(&(item.len() as u32).to_be_bytes());
            let mut iprp = bx(b"ipco", &[]);
            iprp.extend(full(b"ipma", &0u32.to_be_bytes()));
            let mut meta = vec![0; 4];
            meta.extend(full(b"hdlr", &hdlr));
            meta.extend(pitm);
            meta.extend(full(b"iloc", &iloc));
            meta.extend(full(b"iinf", &iinf));
            meta.extend(bx(b"iprp", &iprp));
            let mut file = ftyp;
            file.extend(bx(b"meta", &meta));
            file.extend(bx(b"mdat", &item));
            file
        };
        let at = build(0).len() - item.len();
        build(at as u32)
    }

    fn rich_fields() -> Vec<Field> {
        vec![
            ascii(Tag::Make, "Canon"),
            ascii(Tag::Model, "Canon EOS R5"),
            ascii(Tag::DateTime, "2024:01:02 03:04:05"),
            ascii(Tag::DateTimeOriginal, "2023:12:25 10:20:30"),
            rationals(Tag::ExposureTime, &[(1, 250)]),
            rationals(Tag::FNumber, &[(18, 10)]),
            short(Tag::PhotographicSensitivity, 64),
        ]
    }

    #[test]
    fn jpeg_summary_fields() {
        let (summary, entries) = read_exif(&app1_jpeg(&tiff(&rich_fields()))).unwrap();
        assert_eq!(summary.taken.as_deref(), Some("2023:12:25 10:20:30"));
        assert_eq!(summary.camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(
            summary.exposure.as_deref(),
            Some("1/250s \u{b7} f/1.8 \u{b7} ISO 64")
        );
        assert_eq!(summary.gps, None);
        let make = entries.iter().find(|e| e.name == "Make").unwrap();
        assert_eq!(
            (make.group.as_str(), make.value.as_str()),
            ("IFD0", "\"Canon\"")
        );
        assert!(entries
            .iter()
            .any(|e| e.group == "Exif" && e.name == "ExposureTime"));
    }

    #[test]
    fn summary_falls_back_and_omits_missing_parts() {
        let fields = [
            ascii(Tag::Make, "FUJIFILM"),
            ascii(Tag::Model, "X-T4"),
            ascii(Tag::DateTime, "2024:01:02 03:04:05"),
            rationals(Tag::ExposureTime, &[(5, 2)]),
        ];
        let (summary, _) = read_exif(&app1_jpeg(&tiff(&fields))).unwrap();
        assert_eq!(summary.taken.as_deref(), Some("2024:01:02 03:04:05"));
        assert_eq!(summary.camera.as_deref(), Some("FUJIFILM X-T4"));
        assert_eq!(summary.exposure.as_deref(), Some("2.5s"));
        let (summary, _) = read_exif(&app1_jpeg(&tiff(&[short(Tag::Orientation, 6)]))).unwrap();
        assert_eq!(summary, ExifSummary::default());
    }

    #[test]
    fn heif_exif_item_found() {
        let t = tiff(&rich_fields());
        let camera = |heif: Vec<u8>| read_exif(&heif).unwrap().0.camera;
        assert_eq!(camera(heif_with(&t, &[])).as_deref(), Some("Canon EOS R5"));
        // A gap after the header offset, and the JPEG-style prefix some writers keep.
        assert_eq!(
            camera(heif_with(&t, b"xyz")).as_deref(),
            Some("Canon EOS R5")
        );
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend_from_slice(&t);
        assert_eq!(
            camera(heif_with(&prefixed, &[])).as_deref(),
            Some("Canon EOS R5")
        );
    }

    #[test]
    fn png_and_webp_exif() {
        let t = tiff(&rich_fields());
        let (summary, _) = read_exif(&png_with(&t)).unwrap();
        assert_eq!(summary.camera.as_deref(), Some("Canon EOS R5"));
        let (summary, _) = read_exif(&webp_with(&t)).unwrap();
        assert_eq!(summary.camera.as_deref(), Some("Canon EOS R5"));
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend_from_slice(&t);
        let (summary, _) = read_exif(&webp_with(&prefixed)).unwrap();
        assert_eq!(summary.camera.as_deref(), Some("Canon EOS R5"));
    }

    #[test]
    fn no_exif_is_none() {
        assert!(read_exif(&test_fixtures::png_2x1()).is_none());
        assert!(read_exif(&test_fixtures::gif_two_frames(40)).is_none());
        assert!(read_exif(b"").is_none());
        assert!(read_exif(&[0xFF, 0xD8, 0xFF, 0xD9]).is_none());
        // An EXIF block that is not TIFF.
        assert!(read_exif(&png_with(b"not a tiff block")).is_none());
    }

    type Dms = [(u32, u32); 3];

    fn gps(lat: (&str, Dms), lon: (&str, Dms)) -> Vec<u8> {
        app1_jpeg(&tiff(&[
            ascii(Tag::GPSLatitudeRef, lat.0),
            rationals(Tag::GPSLatitude, &lat.1),
            ascii(Tag::GPSLongitudeRef, lon.0),
            rationals(Tag::GPSLongitude, &lon.1),
        ]))
    }

    #[test]
    fn gps_to_decimal_degrees() {
        let lat: Dms = [(37, 1), (30, 1), (0, 1)];
        let lon: Dms = [(127, 1), (0, 1), (36, 1)];
        let near = |got: Option<(f64, f64)>, want: (f64, f64)| {
            let (a, b) = got.expect("gps");
            assert!(
                (a - want.0).abs() < 1e-9 && (b - want.1).abs() < 1e-9,
                "{a} {b}"
            );
        };
        let gps_of = |bytes: Vec<u8>| read_exif(&bytes).and_then(|(s, _)| s.gps);
        near(gps_of(gps(("N", lat), ("E", lon))), (37.5, 127.01));
        near(gps_of(gps(("S", lat), ("E", lon))), (-37.5, 127.01));
        near(gps_of(gps(("N", lat), ("W", lon))), (37.5, -127.01));
        near(gps_of(gps(("S", lat), ("W", lon))), (-37.5, -127.01));
        // Out of range, a zero denominator and a bad reference are rejected.
        let high: Dms = [(91, 1), (0, 1), (0, 1)];
        let wide: Dms = [(181, 1), (0, 1), (0, 1)];
        let zero: Dms = [(37, 0), (0, 1), (0, 1)];
        assert_eq!(gps_of(gps(("N", high), ("E", lon))), None);
        assert_eq!(gps_of(gps(("N", lat), ("E", wide))), None);
        assert_eq!(gps_of(gps(("N", zero), ("E", lon))), None);
        assert_eq!(gps_of(gps(("X", lat), ("E", lon))), None);
        // The GPS tags are in the full list too, under their own group.
        let (_, entries) = read_exif(&gps(("N", lat), ("E", lon))).unwrap();
        assert!(entries
            .iter()
            .any(|e| e.group == "GPS" && e.name == "GPSLatitude"));
    }

    #[test]
    fn large_exif_is_bounded() {
        let mut fields = vec![ascii(Tag::ImageDescription, &"x".repeat(100_000))];
        fields.push(ascii(Tag::Artist, "a\u{1}b\u{1b}[31mc\nd\u{202e}e"));
        for n in 0..2500u16 {
            fields.push(short(Tag(Context::Tiff, 0x1000 + n), n));
        }
        let (_, entries) = read_exif(&png_with(&tiff(&fields))).unwrap();
        assert_eq!(entries.len(), 2000);
        for e in &entries {
            assert!(e.value.chars().count() <= 512, "{}", e.name);
            assert!(
                !e.value.chars().any(|c| c.is_control()),
                "{}: {:?}",
                e.name,
                e.value
            );
        }
        let long = entries
            .iter()
            .find(|e| e.name == "ImageDescription")
            .unwrap();
        assert_eq!(long.value.chars().count(), 512);
        assert!(long.value.ends_with('\u{2026}'));
        let artist = entries.iter().find(|e| e.name == "Artist").unwrap();
        // kamadak already shows the raw bytes as `\x01`; nothing raw reaches the card.
        assert!(artist.value.contains("\\x01"), "{:?}", artist.value);
        assert!(!artist.value.contains('\u{202e}'));
        // Unnamed tags still get a name.
        assert!(entries.iter().any(|e| e.name == "Tag 0x1000"));
    }

    #[test]
    fn clean_escapes_layout_breakers() {
        assert_eq!(clean("a\u{1}b\nc\u{202e}d"), "a\\u{1}b\\u{A}c\\u{202E}d");
        let (summary, _) = read_exif(&app1_jpeg(&tiff(&[ascii(Tag::Model, "Cam\u{7}x")]))).unwrap();
        assert_eq!(summary.camera.as_deref(), Some("Cam\\u{7}x"));
        let long = clean(&"\u{1}".repeat(600));
        assert_eq!(long.chars().count(), 512);
        assert!(long.ends_with('\u{2026}'));
        assert_eq!(clean(&"y".repeat(512)), "y".repeat(512));
    }

    #[test]
    fn oversized_block_does_not_panic() {
        assert!(read_exif(&png_with(&vec![0u8; 5 << 20])).is_none());
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let valid = app1_jpeg(&tiff(&rich_fields()));
        let tiff_head = b"MM\0*\0\0\0\x08";
        for i in 0..10_000u32 {
            let len = (next() % 300) as usize;
            let junk: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let bytes = match i % 6 {
                0 => junk,
                1 => {
                    let mut b = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x01, 0x00];
                    b.extend_from_slice(b"Exif\0\0");
                    b.extend_from_slice(tiff_head);
                    b.extend(junk);
                    b
                }
                2 => png_with(&[tiff_head.as_slice(), &junk].concat()),
                3 => webp_with(&[tiff_head.as_slice(), &junk].concat()),
                4 => {
                    let mut b = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
                    b.extend(junk);
                    b
                }
                _ => {
                    let mut b = valid.clone();
                    for _ in 0..1 + next() % 4 {
                        let at = (next() % 300) as usize;
                        b[at] = next() as u8;
                    }
                    b
                }
            };
            let _ = read_exif(&bytes);
        }
    }
}
