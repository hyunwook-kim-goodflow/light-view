//! Editing the EXIF orientation of a JPEG without re-encoding anything.
//!
//! Reading goes through kamadak-exif ([`crate::decode::exif_orientation`]). Writing is
//! bounded byte patching of the TIFF block in the EXIF APP1: every byte is kept except the
//! ones an edit changes. A value is patched in place; a missing Orientation tag is added by
//! appending a copy of IFD0 (plus the tag) at the end of the TIFF block and pointing the
//! TIFF header at it, so no existing data moves (maker notes use absolute offsets). Every
//! offset is checked against the data; hostile input gives an error, never a panic.

use crate::jpeg_markers::{Markers, APP0, APP1, EOI, SOS};

const EXIF_ID: &[u8] = b"Exif\0\0";
/// The largest payload a JPEG segment can hold (its length field counts itself).
const MAX_SEGMENT_PAYLOAD: usize = 65533;

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_PIXEL_X: u16 = 0xA002;
const TAG_PIXEL_Y: u16 = 0xA003;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_IFD: u16 = 13;

/// The APP1 inserted into a JPEG without EXIF: `Exif\0\0`, a little-endian TIFF header,
/// IFD0 with only Orientation (its value's low byte at [`MINIMAL_VALUE_AT`]) and no next
/// IFD.
#[rustfmt::skip]
const MINIMAL_APP1: [u8; 36] = [
    0xFF, 0xE1, 0x00, 0x22,
    b'E', b'x', b'i', b'f', 0, 0,
    b'I', b'I', 0x2A, 0, 8, 0, 0, 0,
    1, 0,
    0x12, 0x01, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0,
    0, 0, 0, 0,
];
const MINIMAL_VALUE_AT: usize = 28;

/// The EXIF orientation of a JPEG as the viewer reads it (1..=8, 1 if absent or invalid).
pub fn read_orientation(jpeg: &[u8]) -> u16 {
    crate::decode::exif_orientation(jpeg)
}

/// `jpeg` with EXIF orientation `value` (1..=8). Without EXIF a minimal APP1 is inserted
/// after SOI (after a leading APP0, which JFIF wants first); with EXIF the Orientation in
/// IFD0 is patched in place, or added as the module documentation describes. Errs when
/// the APP1 would outgrow a JPEG segment, and when the viewer ([`read_orientation`]) would
/// not read `value` back from the result (EXIF the patcher can edit but kamadak-exif
/// cannot parse), so a save never silently fails to stick.
///
/// This is the tag-only save. After a pixel rotation use [`after_pixel_rotation`] instead.
pub fn set_orientation(jpeg: &[u8], value: u16) -> Result<Vec<u8>, String> {
    check_orientation(value)?;
    let out = patch_orientation(jpeg, value)?;
    reads_back(out, value)
}

fn check_orientation(value: u16) -> Result<(), String> {
    if (1..=8).contains(&value) {
        Ok(())
    } else {
        Err(format!("EXIF orientation {value} is not 1..=8"))
    }
}

/// The post-condition of every edit: the viewer reads `value` from `out`.
fn reads_back(out: Vec<u8>, value: u16) -> Result<Vec<u8>, String> {
    let read = read_orientation(&out);
    if read == value {
        Ok(out)
    } else {
        Err(format!(
            "EXIF orientation {value} was written but reads back as {read}"
        ))
    }
}

/// [`set_orientation`] without the read-back check; `value` is already in 1..=8.
fn patch_orientation(jpeg: &[u8], value: u16) -> Result<Vec<u8>, String> {
    let Some(app1) = find_exif(jpeg)? else {
        if value == 1 {
            return Ok(jpeg.to_vec());
        }
        let at = insertion_point(jpeg)?;
        let mut segment = MINIMAL_APP1;
        segment[MINIMAL_VALUE_AT] = value as u8;
        let mut out = Vec::with_capacity(jpeg.len() + segment.len());
        out.extend_from_slice(&jpeg[..at]);
        out.extend_from_slice(&segment);
        out.extend_from_slice(&jpeg[at..]);
        return Ok(out);
    };
    let tiff = Tiff::new(&jpeg[app1.tiff_start..app1.end])?;
    let ifd0 = tiff.ifd(tiff.u32(4)?)?;
    let mut out = jpeg.to_vec();
    let mut patch = Patch {
        out: &mut out,
        base: app1.tiff_start,
        be: tiff.be,
    };
    let mut found = false;
    for entry in ifd0.entries().filter(|e| e.tag == TAG_ORIENTATION) {
        patch.short(&entry, value)?;
        found = true;
    }
    if found || value == 1 {
        return Ok(out);
    }

    // Append a copy of IFD0 with the tag, word-aligned, at the end of the TIFF block.
    let pad = tiff.data.len() % 2;
    let new_ifd0 = u32::try_from(tiff.data.len() + pad).map_err(|_| "TIFF block too large")?;
    let count = u16::try_from(ifd0.count + 1).map_err(|_| "IFD0 is full")?;
    let mut new_entry = Vec::with_capacity(12);
    new_entry.extend_from_slice(&tiff.w16(TAG_ORIENTATION));
    new_entry.extend_from_slice(&tiff.w16(TYPE_SHORT));
    new_entry.extend_from_slice(&tiff.w32(1));
    new_entry.extend_from_slice(&tiff.w16(value));
    new_entry.extend_from_slice(&[0, 0]);
    let mut ifd = vec![0u8; pad];
    ifd.extend_from_slice(&tiff.w16(count));
    let mut pending = Some(new_entry);
    for entry in ifd0.entries() {
        if entry.tag > TAG_ORIENTATION {
            if let Some(new) = pending.take() {
                ifd.extend_from_slice(&new);
            }
        }
        ifd.extend_from_slice(entry.raw);
    }
    if let Some(new) = pending {
        ifd.extend_from_slice(&new);
    }
    ifd.extend_from_slice(ifd0.next_raw);
    let payload_len = app1.end - app1.payload_start + ifd.len();
    if payload_len > MAX_SEGMENT_PAYLOAD {
        return Err("EXIF APP1 would exceed the JPEG segment size limit".into());
    }
    patch.u32_at(4, new_ifd0);
    out[app1.payload_start - 2..app1.payload_start]
        .copy_from_slice(&((payload_len + 2) as u16).to_be_bytes());
    out.splice(app1.end..app1.end, ifd);
    Ok(out)
}

/// Fixes the EXIF of a JPEG whose pixels were just rotated: Orientation becomes
/// `orientation` (1..=8), the Exif IFD's PixelX/YDimension become `new_w`/`new_h` (when
/// present, in their own type) and the thumbnail is dropped by setting IFD0's next-IFD
/// offset to 0 (its bytes stay). Orientation is patched in place, or added as
/// [`set_orientation`] adds it when the tag (or the whole EXIF) is missing and
/// `orientation` is not 1. Errs, like [`set_orientation`], when the viewer would not read
/// `orientation` back.
///
/// The rotation save: `(o2, p) = compose(stored, user_quarters)`, turn the pixels `p`
/// quarter turns clockwise, then `after_pixel_rotation(rotated, new_w, new_h, o2)`. For a
/// mirrored orientation `o2` is 2, so passing 1 here would lose the mirror.
pub fn after_pixel_rotation(
    jpeg: &[u8],
    new_w: u32,
    new_h: u32,
    orientation: u16,
) -> Result<Vec<u8>, String> {
    check_orientation(orientation)?;
    let Some(app1) = find_exif(jpeg)? else {
        let out = patch_orientation(jpeg, orientation)?;
        return reads_back(out, orientation);
    };
    let tiff = Tiff::new(&jpeg[app1.tiff_start..app1.end])?;
    let ifd0 = tiff.ifd(tiff.u32(4)?)?;
    let mut out = jpeg.to_vec();
    let mut patch = Patch {
        out: &mut out,
        base: app1.tiff_start,
        be: tiff.be,
    };
    let mut found = false;
    for entry in ifd0.entries() {
        match entry.tag {
            TAG_ORIENTATION => {
                patch.short(&entry, orientation)?;
                found = true;
            }
            TAG_EXIF_IFD => {
                if !matches!(entry.typ, TYPE_LONG | TYPE_IFD) || entry.count != 1 {
                    return Err("malformed Exif IFD pointer".into());
                }
                let exif_ifd = tiff.ifd(tiff.u32(entry.value_at)?)?;
                for dim in exif_ifd.entries() {
                    match dim.tag {
                        TAG_PIXEL_X => patch.dimension(&dim, new_w)?,
                        TAG_PIXEL_Y => patch.dimension(&dim, new_h)?,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    patch.u32_at(ifd0.next_at, 0);
    if !found && orientation != 1 {
        // The appended IFD0 copies the next-IFD link just cleared.
        out = patch_orientation(&out, orientation)?;
    }
    reads_back(out, orientation)
}

/// An EXIF orientation as a mirror flag and clockwise quarter turns: the viewer shows the
/// stored pixels mirrored left-right (if the flag is set) and then turned that many times
/// clockwise. Values outside 1..=8 count as 1.
fn split(orientation: u16) -> (bool, u8) {
    match orientation {
        2 => (true, 0),
        3 => (false, 2),
        4 => (true, 2),
        5 => (true, 3),
        6 => (false, 1),
        7 => (true, 1),
        8 => (false, 3),
        _ => (false, 0),
    }
}

/// The inverse of [`split`].
fn join(mirror: bool, quarters: u8) -> u16 {
    const PLAIN: [u16; 4] = [1, 6, 3, 8];
    const MIRRORED: [u16; 4] = [2, 7, 4, 5];
    let table = if mirror { MIRRORED } else { PLAIN };
    table[usize::from(quarters % 4)]
}

/// Splits a stored orientation plus a user rotation into `(o', p)`: turning the stored
/// pixels `p` quarter turns clockwise and then showing them with orientation `o'` gives
/// exactly what was on screen (the stored pixels shown with `orientation`) turned
/// `user_quarters` times clockwise. The save turns the pixels by `p` and then writes the
/// new orientation with [`after_pixel_rotation`] (never a fixed 1: it is 2 for mirrored
/// inputs).
///
/// Write `orientation` as "mirror left-right if `m`, then turn `r` quarters clockwise".
/// The target is mirror-if-`m` followed by `r + q` turns. Only the mirror stays in the
/// tag; the rotation goes into the pixels:
/// - not mirrored: `o' = 1`, `p = (r + q) mod 4`;
/// - mirrored (2, 4, 5, 7): `o' = 2` (mirror only). Mirroring after `p` turns equals
///   `-p` turns after mirroring, so `p = -(r + q) mod 4`.
///
/// | o \ q | 0     | 1     | 2     | 3     |
/// |-------|-------|-------|-------|-------|
/// | 1     | (1,0) | (1,1) | (1,2) | (1,3) |
/// | 2     | (2,0) | (2,3) | (2,2) | (2,1) |
/// | 3     | (1,2) | (1,3) | (1,0) | (1,1) |
/// | 4     | (2,2) | (2,1) | (2,0) | (2,3) |
/// | 5     | (2,1) | (2,0) | (2,3) | (2,2) |
/// | 6     | (1,1) | (1,2) | (1,3) | (1,0) |
/// | 7     | (2,3) | (2,2) | (2,1) | (2,0) |
/// | 8     | (1,3) | (1,0) | (1,1) | (1,2) |
///
/// Orientations outside 1..=8 count as 1, as in [`crate::decode::exif_orientation`];
/// `user_quarters` counts modulo 4.
pub fn compose(orientation: u16, user_quarters: u8) -> (u16, u8) {
    let (mirror, r) = split(orientation);
    let turns = (r + user_quarters % 4) % 4;
    if mirror {
        (2, (4 - turns) % 4)
    } else {
        (1, turns)
    }
}

/// The orientation that shows the stored pixels as `orientation` plus `user_quarters`
/// clockwise quarter turns, without touching the pixels (the save for a JPEG that cannot
/// be turned losslessly).
pub fn compose_tag_only(orientation: u16, user_quarters: u8) -> u16 {
    let (mirror, r) = split(orientation);
    join(mirror, r + user_quarters % 4)
}

/// Where the EXIF APP1 is in a JPEG.
struct App1 {
    /// Offset of the payload (just past the length field).
    payload_start: usize,
    /// Offset of the TIFF block (past `Exif\0\0`).
    tiff_start: usize,
    /// Offset just past the segment.
    end: usize,
}

/// The first APP1 holding EXIF before the first scan, the one kamadak-exif reads.
fn find_exif(jpeg: &[u8]) -> Result<Option<App1>, String> {
    for marker in Markers::new(jpeg) {
        let marker = marker.map_err(|_| "malformed JPEG marker structure")?;
        match marker.code {
            SOS | EOI => break,
            APP1 if marker.payload.starts_with(EXIF_ID) => {
                let payload_start = marker.end - marker.payload.len();
                return Ok(Some(App1 {
                    payload_start,
                    tiff_start: payload_start + EXIF_ID.len(),
                    end: marker.end,
                }));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Where a new APP1 goes: after a leading APP0, else right after SOI.
fn insertion_point(jpeg: &[u8]) -> Result<usize, String> {
    match Markers::new(jpeg).next() {
        Some(Ok(m)) if m.code == APP0 => Ok(m.end),
        Some(Ok(_)) => Ok(2),
        _ => Err("malformed JPEG marker structure".into()),
    }
}

/// A TIFF block with bounds-checked reads; offsets are relative to its start.
struct Tiff<'a> {
    data: &'a [u8],
    be: bool,
}

struct Ifd<'a> {
    count: usize,
    /// The `count` 12-byte entries.
    raw: &'a [u8],
    /// Offset of the first entry.
    at: usize,
    /// Offset of the next-IFD field, and its 4 bytes.
    next_at: usize,
    next_raw: &'a [u8],
    be: bool,
}

struct Entry<'a> {
    tag: u16,
    typ: u16,
    count: u32,
    /// Offset of the 4-byte value field.
    value_at: usize,
    raw: &'a [u8],
}

fn oob() -> String {
    "EXIF offset out of bounds".into()
}

fn rd16(be: bool, b: [u8; 2]) -> u16 {
    if be {
        u16::from_be_bytes(b)
    } else {
        u16::from_le_bytes(b)
    }
}

fn rd32(be: bool, b: [u8; 4]) -> u32 {
    if be {
        u32::from_be_bytes(b)
    } else {
        u32::from_le_bytes(b)
    }
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Result<Tiff<'a>, String> {
        let be = match data.get(..4) {
            Some(b"II\x2A\0") => false,
            Some(b"MM\0\x2A") => true,
            _ => return Err("malformed TIFF header in EXIF".into()),
        };
        Ok(Tiff { data, be })
    }

    fn u32(&self, at: usize) -> Result<usize, String> {
        let b = at
            .checked_add(4)
            .and_then(|end| self.data.get(at..end))
            .ok_or_else(oob)?;
        let v = rd32(self.be, [b[0], b[1], b[2], b[3]]);
        usize::try_from(v).map_err(|_| oob())
    }

    fn w16(&self, v: u16) -> [u8; 2] {
        if self.be {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    }

    fn w32(&self, v: u32) -> [u8; 4] {
        if self.be {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    }

    fn ifd(&self, at: usize) -> Result<Ifd<'a>, String> {
        let count_end = at.checked_add(2).ok_or_else(oob)?;
        let c = self.data.get(at..count_end).ok_or_else(oob)?;
        let count = usize::from(rd16(self.be, [c[0], c[1]]));
        let next_at = count_end + 12 * count;
        let raw = self.data.get(count_end..next_at).ok_or_else(oob)?;
        let next_raw = self.data.get(next_at..next_at + 4).ok_or_else(oob)?;
        Ok(Ifd {
            count,
            raw,
            at: count_end,
            next_at,
            next_raw,
            be: self.be,
        })
    }
}

impl<'a> Ifd<'a> {
    fn entries(&self) -> impl Iterator<Item = Entry<'a>> + use<'a> {
        let (be, at, raw) = (self.be, self.at, self.raw);
        raw.chunks_exact(12).enumerate().map(move |(i, raw)| Entry {
            tag: rd16(be, [raw[0], raw[1]]),
            typ: rd16(be, [raw[2], raw[3]]),
            count: rd32(be, [raw[4], raw[5], raw[6], raw[7]]),
            value_at: at + 12 * i + 8,
            raw,
        })
    }
}

/// Writes values into a copy of the JPEG at TIFF offsets. Every offset it is given lies
/// in an IFD that [`Tiff::ifd`] checked to be inside the block.
struct Patch<'o> {
    out: &'o mut Vec<u8>,
    /// Offset of the TIFF block in `out`.
    base: usize,
    be: bool,
}

impl Patch<'_> {
    fn put(&mut self, at: usize, bytes: &[u8]) {
        let at = self.base + at;
        self.out[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn u32_at(&mut self, at: usize, v: u32) {
        let b = if self.be {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.put(at, &b);
    }

    /// Sets an inline value that must be one SHORT.
    fn short(&mut self, entry: &Entry, v: u16) -> Result<(), String> {
        if entry.typ != TYPE_SHORT || entry.count != 1 {
            return Err(format!("EXIF tag {:#06x} is not one SHORT", entry.tag));
        }
        let b = if self.be {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.put(entry.value_at, &b);
        Ok(())
    }

    /// Sets a pixel dimension in the entry's own type (one SHORT or one LONG).
    fn dimension(&mut self, entry: &Entry, v: u32) -> Result<(), String> {
        match (entry.typ, entry.count) {
            (TYPE_SHORT, 1) => {
                let v = u16::try_from(v).map_err(|_| format!("{v} does not fit a SHORT"))?;
                self.short(entry, v)
            }
            (TYPE_LONG, 1) => {
                self.u32_at(entry.value_at, v);
                Ok(())
            }
            _ => Err(format!(
                "EXIF tag {:#06x} is not one SHORT or LONG",
                entry.tag
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::exif_orientation;
    use crate::jpeg_markers::tests::segment;
    use exif::{In, Tag, Value};
    use image::metadata::Orientation;
    use image::{imageops, DynamicImage, Rgba, RgbaImage};

    const SHORT: u16 = 3;
    const LONG: u16 = 4;

    /// A TIFF block and where its interesting fields are, as offsets into it.
    struct Tiff {
        bytes: Vec<u8>,
        orientation_at: Option<usize>,
        pixel_x_at: Option<usize>,
        pixel_y_at: Option<usize>,
        ifd0_next_at: usize,
        exif_ptr_at: usize,
    }

    struct Builder {
        be: bool,
        out: Vec<u8>,
    }

    impl Builder {
        fn u16(&mut self, v: u16) {
            let b = if self.be {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            };
            self.out.extend_from_slice(&b);
        }
        fn u32(&mut self, v: u32) {
            let b = if self.be {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            };
            self.out.extend_from_slice(&b);
        }
        /// An entry; returns the offset of its value field.
        fn entry(&mut self, tag: u16, typ: u16, count: u32, value: u32) -> usize {
            self.u16(tag);
            self.u16(typ);
            self.u32(count);
            let at = self.out.len();
            if typ == SHORT && count == 1 {
                self.u16(value as u16);
                self.u16(0);
            } else {
                self.u32(value);
            }
            at
        }
    }

    /// IFD0 (Make, optional Orientation, Exif pointer), its Make string, the Exif IFD
    /// (MakerNote, optional PixelX/YDimension of type `dims.0`), the maker note, and with
    /// `thumbnail` an IFD1 and its 5-byte JPEG (so the block has an odd length).
    fn tiff(
        be: bool,
        orientation: Option<u16>,
        dims: Option<(u16, u32, u32)>,
        thumbnail: bool,
    ) -> Tiff {
        let mut b = Builder {
            be,
            out: Vec::new(),
        };
        b.out
            .extend_from_slice(if be { b"MM\0\x2a" } else { b"II\x2a\0" });
        b.u32(8);
        let n0 = 2 + u32::from(orientation.is_some());
        let make_off = 8 + 2 + 12 * n0 + 4;
        let exif_off = make_off + 6;
        let n_exif = 1 + 2 * u32::from(dims.is_some());
        let note_off = exif_off + 2 + 12 * n_exif + 4;
        let ifd1_off = note_off + 8;
        b.u16(n0 as u16);
        b.entry(0x010F, 2, 6, make_off);
        let orientation_at = orientation.map(|o| b.entry(0x0112, SHORT, 1, u32::from(o)));
        let exif_ptr_at = b.entry(0x8769, LONG, 1, exif_off);
        let ifd0_next_at = b.out.len();
        b.u32(if thumbnail { ifd1_off } else { 0 });
        b.out.extend_from_slice(b"Canon\0");
        b.u16(n_exif as u16);
        b.entry(0x927C, 7, 8, note_off);
        let (mut pixel_x_at, mut pixel_y_at) = (None, None);
        if let Some((typ, w, h)) = dims {
            pixel_x_at = Some(b.entry(0xA002, typ, 1, w));
            pixel_y_at = Some(b.entry(0xA003, typ, 1, h));
        }
        b.u32(0);
        b.out.extend_from_slice(b"MAKERNOT");
        if thumbnail {
            b.u16(3);
            b.entry(0x0103, SHORT, 1, 6);
            b.entry(0x0201, LONG, 1, ifd1_off + 2 + 3 * 12 + 4);
            b.entry(0x0202, LONG, 1, 5);
            b.u32(0);
            b.out.extend_from_slice(&[0xFF, 0xD8, 0, 0xFF, 0xD9]);
        }
        Tiff {
            bytes: b.out,
            orientation_at,
            pixel_x_at,
            pixel_y_at,
            ifd0_next_at,
            exif_ptr_at,
        }
    }

    fn sample(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/jpeg")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// Offset of the TIFF block in a JPEG made by [`with_exif`].
    const TIFF_AT: usize = 2 + 4 + 6;

    /// `base` (a real JPEG starting SOI APP0) with an EXIF APP1 holding `tiff` right
    /// after SOI.
    fn with_exif(base: &[u8], tiff: &[u8]) -> Vec<u8> {
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(tiff);
        let mut j = base[..2].to_vec();
        j.extend(segment(0xE1, &payload));
        j.extend_from_slice(&base[2..]);
        j
    }

    fn differing(a: &[u8], b: &[u8]) -> Vec<usize> {
        assert_eq!(a.len(), b.len());
        (0..a.len()).filter(|&i| a[i] != b[i]).collect()
    }

    fn exif_of(jpeg: &[u8]) -> exif::Exif {
        exif::Reader::new()
            .read_from_container(&mut std::io::Cursor::new(jpeg))
            .expect("readable EXIF")
    }

    #[test]
    fn set_orientation_inserts_minimal_app1_when_missing() {
        let base = sample("gradient-64x32.jpg");
        assert_eq!(&base[2..4], [0xFF, 0xE0], "the sample starts SOI APP0");
        let app0_end = 4 + usize::from(u16::from_be_bytes([base[4], base[5]]));
        let minimal: &[u8] = b"\xFF\xE1\x00\x22Exif\0\0II\x2a\0\x08\0\0\0\
            \x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        // Kept after JFIF's APP0, which must stay first.
        let out = set_orientation(&base, 6).unwrap();
        assert_eq!(out[..app0_end], base[..app0_end]);
        assert_eq!(&out[app0_end..app0_end + minimal.len()], minimal);
        assert_eq!(out[app0_end + minimal.len()..], base[app0_end..]);
        assert_eq!(read_orientation(&out), 6);
        assert_eq!(exif_orientation(&out), 6);
        assert!(crate::jpeg_check::inspect(&out).is_ok());
        // Without APP0 it goes right after SOI.
        let mut bare = base[..2].to_vec();
        bare.extend_from_slice(&base[app0_end..]);
        let out = set_orientation(&bare, 6).unwrap();
        assert_eq!(&out[2..2 + minimal.len()], minimal);
        assert_eq!(out[2 + minimal.len()..], bare[2..]);
        // Orientation 1 is what no EXIF means: nothing to add.
        assert_eq!(set_orientation(&base, 1).unwrap(), base);
        // Not an orientation, not a JPEG.
        assert!(set_orientation(&base, 0).is_err());
        assert!(set_orientation(&base, 9).is_err());
        assert!(set_orientation(b"GIF89a", 6).is_err());
    }

    #[test]
    fn set_orientation_in_place_le_and_be() {
        let base = sample("gradient-64x32.jpg");
        for be in [false, true] {
            // The tag exists: only its value changes.
            let t = tiff(be, Some(1), Some((LONG, 64, 32)), true);
            let j = with_exif(&base, &t.bytes);
            assert_eq!(read_orientation(&j), 1);
            let out = set_orientation(&j, 8).unwrap();
            let at = TIFF_AT + t.orientation_at.unwrap() + usize::from(be);
            assert_eq!(differing(&j, &out), [at], "be {be}");
            assert_eq!(read_orientation(&out), 8);
            assert_eq!(set_orientation(&out, 1).unwrap(), j);

            // The tag is missing: a new IFD0 is appended at the end of the TIFF block and
            // nothing before it moves.
            let t = tiff(be, None, Some((LONG, 64, 32)), true);
            let j = with_exif(&base, &t.bytes);
            let out = set_orientation(&j, 3).unwrap();
            assert_eq!(read_orientation(&out), 3);
            let tiff_end = TIFF_AT + t.bytes.len();
            let grown = out.len() - j.len();
            // One pad byte (odd block), then count, three entries, next offset.
            assert_eq!(grown, 1 + 2 + 3 * 12 + 4);
            assert_eq!(out[tiff_end + grown..], j[tiff_end..]);
            // Only the APP1 length and the IFD0 offset in the TIFF header changed before.
            let mut changed = differing(&j[..tiff_end], &out[..tiff_end]);
            changed.retain(|&i| !(4..6).contains(&i) && !(TIFF_AT + 4..TIFF_AT + 8).contains(&i));
            assert!(changed.is_empty(), "{changed:?}");
            let read = |at: usize| {
                let b = [out[at], out[at + 1]];
                if be {
                    u16::from_be_bytes(b)
                } else {
                    u16::from_le_bytes(b)
                }
            };
            let ifd = tiff_end + 1;
            assert_eq!(read(ifd), 3);
            let tags: Vec<u16> = (0..3).map(|k| read(ifd + 2 + 12 * k)).collect();
            assert_eq!(tags, [0x010F, 0x0112, 0x8769], "sorted");
            // Everything else still reads: the old entries' offsets, the thumbnail link.
            let exif = exif_of(&out);
            let make = exif.get_field(Tag::Make, In::PRIMARY).unwrap();
            assert_eq!(make.display_value().to_string(), "\"Canon\"");
            let note = exif.get_field(Tag::MakerNote, In::PRIMARY).unwrap();
            assert!(matches!(&note.value, Value::Undefined(v, _) if v == b"MAKERNOT"));
            assert!(exif.get_field(Tag::Compression, In::THUMBNAIL).is_some());
        }
    }

    #[test]
    fn set_orientation_refuses_an_app1_over_the_segment_limit() {
        let base = sample("gradient-64x32.jpg");
        // A TIFF block that fills the APP1 to its 65533-byte payload limit.
        let mut t = tiff(false, None, None, false);
        t.bytes.resize(65533 - 6, 0);
        let j = with_exif(&base, &t.bytes);
        assert!(set_orientation(&j, 6).is_err());
        // With the tag present, the in-place edit still works.
        let mut t = tiff(false, Some(1), None, false);
        t.bytes.resize(65533 - 6, 0);
        let j = with_exif(&base, &t.bytes);
        assert_eq!(read_orientation(&set_orientation(&j, 6).unwrap()), 6);
    }

    #[test]
    fn after_pixel_rotation_updates_dims_and_drops_thumbnail() {
        let base = sample("gradient-64x32.jpg");
        for be in [false, true] {
            for typ in [SHORT, LONG] {
                let t = tiff(be, Some(6), Some((typ, 64, 32)), true);
                let j = with_exif(&base, &t.bytes);
                assert!(exif_of(&j)
                    .get_field(Tag::Compression, In::THUMBNAIL)
                    .is_some());
                let out = after_pixel_rotation(&j, 32, 64, 1).unwrap();
                assert_eq!(read_orientation(&out), 1);
                let exif = exif_of(&out);
                let dim = |tag| format!("{:?}", exif.get_field(tag, In::PRIMARY).unwrap().value);
                let expect = |v: u32| {
                    let v = if typ == SHORT {
                        Value::Short(vec![v as u16])
                    } else {
                        Value::Long(vec![v])
                    };
                    format!("{v:?}")
                };
                assert_eq!(dim(Tag::PixelXDimension), expect(32), "be {be} type {typ}");
                assert_eq!(dim(Tag::PixelYDimension), expect(64));
                assert!(exif.get_field(Tag::Compression, In::THUMBNAIL).is_none());
                assert!(exif.get_field(Tag::MakerNote, In::PRIMARY).is_some());
                // Only those value fields and IFD0's next offset changed.
                let fields = [
                    (t.orientation_at.unwrap(), 2),
                    (t.pixel_x_at.unwrap(), 4),
                    (t.pixel_y_at.unwrap(), 4),
                    (t.ifd0_next_at, 4),
                ];
                for i in differing(&j, &out) {
                    let rel = i - TIFF_AT;
                    assert!(
                        fields.iter().any(|&(at, n)| (at..at + n).contains(&rel)),
                        "{i}"
                    );
                }
            }
        }
        // No dimension tags: they stay absent.
        let t = tiff(true, Some(8), None, false);
        let out = after_pixel_rotation(&with_exif(&base, &t.bytes), 32, 64, 1).unwrap();
        assert_eq!(read_orientation(&out), 1);
        assert!(exif_of(&out)
            .get_field(Tag::PixelXDimension, In::PRIMARY)
            .is_none());
        // A SHORT field cannot hold the new size.
        let t = tiff(false, Some(6), Some((SHORT, 64, 32)), false);
        assert!(after_pixel_rotation(&with_exif(&base, &t.bytes), 70_000, 64, 1).is_err());
        // No EXIF: nothing to fix.
        assert_eq!(after_pixel_rotation(&base, 32, 64, 1).unwrap(), base);
        assert!(after_pixel_rotation(&base, 32, 64, 0).is_err());
    }

    /// A 3x2 grid of distinct pixels: no rotation or mirror maps it onto itself.
    fn grid() -> RgbaImage {
        RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 * 40, y as u8 * 90, 7, 255]))
    }

    /// How the viewer displays stored pixels with an EXIF orientation (`decode`).
    fn display(img: &RgbaImage, orientation: u16) -> RgbaImage {
        let mut img = DynamicImage::ImageRgba8(img.clone());
        img.apply_orientation(Orientation::from_exif(orientation as u8).unwrap());
        img.into_rgba8()
    }

    fn rotate_cw(img: &RgbaImage, quarters: u8) -> RgbaImage {
        (0..quarters % 4).fold(img.clone(), |img, _| imageops::rotate90(&img))
    }

    #[test]
    fn compose_table_all_8x4() {
        // (orientation, user quarters) -> (new orientation, pixel quarters), the table in
        // `compose`'s documentation.
        #[rustfmt::skip]
        let table: [[(u16, u8); 4]; 8] = [
            [(1, 0), (1, 1), (1, 2), (1, 3)], // 1
            [(2, 0), (2, 3), (2, 2), (2, 1)], // 2
            [(1, 2), (1, 3), (1, 0), (1, 1)], // 3
            [(2, 2), (2, 1), (2, 0), (2, 3)], // 4
            [(2, 1), (2, 0), (2, 3), (2, 2)], // 5
            [(1, 1), (1, 2), (1, 3), (1, 0)], // 6
            [(2, 3), (2, 2), (2, 1), (2, 0)], // 7
            [(1, 3), (1, 0), (1, 1), (1, 2)], // 8
        ];
        let stored = grid();
        for o in 1..=8u16 {
            for q in 0..4u8 {
                let target = rotate_cw(&display(&stored, o), q);
                let (new_o, p) = compose(o, q);
                assert_eq!(
                    (new_o, p),
                    table[usize::from(o - 1)][usize::from(q)],
                    "({o}, {q})"
                );
                assert_eq!(display(&rotate_cw(&stored, p), new_o), target, "({o}, {q})");
                // Turns count modulo 4.
                assert_eq!(compose(o, q + 4), (new_o, p));
                // The tag alone can show the same thing.
                assert_eq!(
                    display(&stored, compose_tag_only(o, q)),
                    target,
                    "({o}, {q})"
                );
            }
        }
        // Values outside 1..=8 read as 1, as in `decode::exif_orientation`.
        assert_eq!(compose(0, 1), (1, 1));
        assert_eq!(compose(9, 3), (1, 3));
        assert_eq!(compose_tag_only(42, 1), 6);
    }

    #[test]
    fn mirrored_rotation_save_keeps_the_mirror() {
        // compose -> turn the pixels -> after_pixel_rotation(new orientation) shows the
        // target for every stored orientation (mirrored ones included), with or without
        // the tag.
        let base = sample("gradient-64x32.jpg");
        let stored = grid();
        for o in 1..=8u16 {
            for q in 0..4u8 {
                let target = rotate_cw(&display(&stored, o), q);
                let (new_o, p) = compose(o, q);
                let rotated = rotate_cw(&stored, p);
                let (w, h) = rotated.dimensions();
                for with_tag in [true, false] {
                    let t = tiff(o % 2 == 0, with_tag.then_some(o), Some((LONG, 3, 2)), true);
                    let j = with_exif(&base, &t.bytes);
                    let out = after_pixel_rotation(&j, w, h, new_o).unwrap();
                    let written = read_orientation(&out);
                    assert_eq!(written, new_o, "({o}, {q}, tag {with_tag})");
                    assert_eq!(display(&rotated, written), target, "({o}, {q})");
                    let exif = exif_of(&out);
                    assert!(exif.get_field(Tag::Compression, In::THUMBNAIL).is_none());
                    let x = exif.get_field(Tag::PixelXDimension, In::PRIMARY).unwrap();
                    assert_eq!(x.value.get_uint(0), Some(w));
                }
            }
        }
        // No EXIF at all: the mirror still gets written.
        let out = after_pixel_rotation(&base, 32, 64, 2).unwrap();
        assert_eq!(read_orientation(&out), 2);
    }

    #[test]
    fn edits_the_viewer_cannot_read_back_are_errors() {
        // An Exif IFD pointer past the block: the patcher can still edit IFD0, but
        // kamadak-exif rejects the whole EXIF and the viewer would show orientation 1.
        let base = sample("gradient-64x32.jpg");
        for be in [false, true] {
            let mut t = tiff(be, Some(1), None, false);
            let bad = if be {
                [0, 0, 0xFF, 0xF0]
            } else {
                [0xF0, 0xFF, 0, 0]
            };
            t.bytes[t.exif_ptr_at..t.exif_ptr_at + 4].copy_from_slice(&bad);
            let j = with_exif(&base, &t.bytes);
            assert_eq!(read_orientation(&j), 1);
            assert!(set_orientation(&j, 6).is_err(), "be {be}");
            assert!(after_pixel_rotation(&j, 32, 64, 2).is_err(), "be {be}");
            // 1 is what the viewer reads anyway.
            assert_eq!(read_orientation(&set_orientation(&j, 1).unwrap()), 1);
        }
    }

    /// A small EXIF JPEG for mutation: SOI, APP1 (LE TIFF with a thumbnail), SOF, SOS,
    /// entropy data, EOI.
    fn small_exif_jpeg() -> Vec<u8> {
        let t = tiff(false, Some(6), Some((SHORT, 16, 8)), true);
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&t.bytes);
        let mut j = vec![0xFF, 0xD8];
        j.extend(segment(0xE1, &payload));
        j.extend(segment(0xC0, &[8, 0, 8, 0, 16, 1, 1, 0x11, 0]));
        j.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
        j.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD9]);
        j
    }

    #[test]
    fn hostile_exif_never_panics() {
        let valid = small_exif_jpeg();
        let after_app1 = 2 + u16::from_be_bytes([valid[4], valid[5]]) as usize + 2;
        assert_eq!(read_orientation(&valid), 6);
        assert!(after_pixel_rotation(&valid, 8, 16, 1).is_ok());
        // xorshift32 with a fixed seed: fast and the same every run.
        let mut state = 0x9E37_79B9u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for round in 0..10_000 {
            let mut j = valid.clone();
            match round % 4 {
                // A few random bytes changed anywhere (mostly inside the TIFF block).
                0 | 1 => {
                    for _ in 0..1 + next() % 6 {
                        let at = (next() as usize) % j.len();
                        j[at] = next() as u8;
                    }
                }
                // Cut short.
                2 => j.truncate((next() as usize) % j.len()),
                // Random TIFF block of random length in an APP1 after SOI.
                _ => {
                    let len = (next() % 200) as usize;
                    let mut payload = b"Exif\0\0".to_vec();
                    let header: &[u8] = if next() % 2 == 0 {
                        b"II\x2a\0"
                    } else {
                        b"MM\0\x2a"
                    };
                    payload.extend_from_slice(header);
                    payload.extend((0..len).map(|_| {
                        let r = next();
                        if r % 3 == 0 {
                            (r >> 8) as u8 % 16
                        } else {
                            (r >> 8) as u8
                        }
                    }));
                    j = vec![0xFF, 0xD8];
                    j.extend(segment(0xE1, &payload));
                    j.extend_from_slice(&valid[after_app1..]);
                }
            }
            let _ = read_orientation(&j);
            let _ = set_orientation(&j, 1 + (next() % 8) as u16);
            let _ = after_pixel_rotation(
                &j,
                next() % 70_000,
                next() % 70_000,
                1 + (next() % 8) as u16,
            );
            let _ = crate::jpeg_check::inspect(&j);
        }
    }
}
