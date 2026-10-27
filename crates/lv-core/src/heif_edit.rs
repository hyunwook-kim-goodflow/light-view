//! Lossless rotation of HEIF files by editing the primary item's `irot` property.
//!
//! The coded image is never touched: a rotation save changes (or adds) the `irot`
//! transformative property of the primary item (ISO/IEC 23008-12 §6.5.10). For a grid
//! image that is the grid item; its tiles are left alone.
//!
//! **Rotation direction.** `irot.angle` turns the image *anti-clockwise* in units of 90°,
//! while the viewer's rotation commands are *clockwise* quarter turns. A user's `cw`
//! clockwise quarters on top of a stored `irot` of `ccw` give `(ccw + 4 - cw) mod 4`
//! ([`compose`]). A primary item with a mirror (`imir`) is refused: the order of `imir` and
//! `irot` in `ipma` decides which way the stored rotation must turn, and such files are
//! rare enough not to be worth the risk.
//!
//! **Alpha.** libheif applies a transformative property only to the item it is associated
//! with, so an `irot` on the primary item turns the colour planes but not the alpha
//! auxiliary image (`auxl`), and the two no longer line up. Other decoders may instead
//! apply the primary item's transforms to the composed image, so giving the alpha item its
//! own `irot` is not safe either. A primary item with an alpha image is refused. Other
//! auxiliary images (depth, gain maps) get the new `irot` too, as described below.
//!
//! **Editing.** Changing an existing `irot` rewrites one byte. That is refused when the
//! primary item has more than one `irot`, when a transformative property (`clap`, `imir`,
//! `irot`) follows its `irot` in `ipma` (a non-conforming order whose meaning would change),
//! and when the same property is also associated with one of the grid's tiles (`dimg`).
//! Adding one appends a 9-byte `irot` box at the end of `ipco` and an essential
//! association (1 or 2 bytes, following `ipma`'s flags) to the first `ipma` entry of the
//! primary item and of the items that must turn with it: its thumbnails (`thmb`) and its
//! non-alpha auxiliary images (`auxl`, e.g. depth or gain maps), as iPhone files share one
//! `irot`. Such an item that already has its own `irot` or `imir` is refused. An item
//! without an entry gets a new one, inserted in item id order. The size fields of `ipco`, `ipma`, `iprp` and `meta` (32-bit or `largesize`) grow
//! accordingly, and every `iloc` extent stored at a file offset (construction method 0,
//! data reference 0) that lies after an inserted byte is moved by the number of bytes
//! inserted before it: through `base_offset` when the item has a nonzero base and all its
//! extents move alike, else through each extent's offset. An offset that would not fit
//! its field is an error. `idat` (method 1) and item-offset (method 2) extents are relative
//! and stay as they are. A size change is refused when an item is stored at file offsets in
//! another data reference (a `url ` with the same-file flag still means this file, so it
//! would move unadjusted) or uses a reserved construction method, and, because only `iloc`
//! offsets are adjusted, when the file has top-level boxes other than `ftyp`, `meta`, `mdat`, `free` and `skip`
//! (a `moov`, for example, holds its own absolute offsets).
//!
//! Every result is re-parsed and checked before it is returned: every item's property list
//! is unchanged except for the intended `irot` additions, the primary item's `irot` reads
//! back, and every in-file extent's bytes are identical at their new place.
//!
//! The input is untrusted: every length is checked, box walking is bounded, nothing
//! panics, and allocations are bounded by the input size.

use std::ops::Range;

use crate::exif_edit::set_tiff_orientation_in_place;

/// Most boxes accepted in one container.
const MAX_BOXES: usize = 4096;
/// Most `iloc` extents accepted in one file (an extent can take zero bytes of `iloc`).
const MAX_EXTENTS: usize = 1 << 20;
/// Top-level boxes that hold no absolute file offsets of their own.
const SHIFTABLE_TOP_LEVEL: [&[u8; 4]; 5] = [b"ftyp", b"meta", b"mdat", b"free", b"skip"];

const MIRRORED: &str = "mirrored HEIF not supported for rotation save";
const SEVERAL_IROT: &str = "HEIF primary item has more than one irot";
const WITH_ALPHA: &str = "HEIF with an alpha plane not supported for rotation save";
/// `aux_type` values of an alpha plane (ISO/IEC 23008-12 and MIAF).
const ALPHA_URNS: [&[u8]; 2] = [
    b"urn:mpeg:hevc:2015:auxid:1",
    b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
];
const OVERFLOW: &str = "an iloc offset would overflow its field";
const SELF_CHECK: &str = "HEIF rotation self-check failed: ";

/// Parts of the errors that refuse a well-formed file by policy (a layout this module
/// does not edit) rather than because it is corrupt or an edit failed its self-check.
const REFUSALS: [&str; 10] = [
    MIRRORED,
    SEVERAL_IROT,
    WITH_ALPHA,
    OVERFLOW,
    "already has its own rotation or mirror",
    "not supported",
    "unsupported iloc",
    "cannot grow",
    "a size change cannot follow",
    "would have to change",
];

/// Whether `error`, from [`set_rotation`], [`read_rotation`] or
/// [`normalize_exif_orientation`], refuses a file this module chooses not to edit (it is
/// a policy refusal: the file may well be fine), rather than reporting a file it cannot
/// parse or an edit that failed its self-check.
pub fn is_refusal(error: &str) -> bool {
    !error.starts_with(SELF_CHECK)
        && (REFUSALS.iter().any(|r| error.contains(r))
            || error == "EXIF Orientation is not one SHORT"
            || error == "an item's data overlaps the edited boxes")
}

/// The `irot` angle (anti-clockwise quarters) after the user turns an image stored with
/// `stored_ccw` by `user_cw` clockwise quarter turns: `(stored_ccw + 4 - user_cw) mod 4`.
pub fn compose(stored_ccw: u8, user_cw: u8) -> u8 {
    (stored_ccw % 4 + 4 - user_cw % 4) % 4
}

/// The `irot` angle (anti-clockwise quarter turns, 0..=3) associated with the primary
/// item, 0 when it has none.
pub fn read_rotation(heif: &[u8]) -> Result<u8, String> {
    let file = parse(heif)?;
    match primary_irot(&file.primary_props()?)? {
        Some(irot) => irot_angle(heif, &irot),
        None => Ok(0),
    }
}

/// The primary item's only `irot`; more than one is an error (libheif applies them all).
fn primary_irot(props: &[BoxRef]) -> Result<Option<BoxRef>, String> {
    let mut irots = props.iter().filter(|p| &p.typ == b"irot");
    let first = irots.next().copied();
    if irots.next().is_some() {
        return Err(SEVERAL_IROT.into());
    }
    Ok(first)
}

/// `heif` with the primary item's `irot` set to `quarters_ccw` (0..=3) anti-clockwise
/// quarter turns, as the module documentation describes. An existing `irot` is changed in
/// place (also to 0); without one, 0 returns the input unchanged and other angles add one.
pub fn set_rotation(heif: &[u8], quarters_ccw: u8) -> Result<Vec<u8>, String> {
    if quarters_ccw > 3 {
        return Err(format!("irot angle {quarters_ccw} is not 0..=3"));
    }
    let file = parse(heif)?;
    let props = file.primary_props()?;
    if props.iter().any(|p| &p.typ == b"imir") {
        return Err(MIRRORED.into());
    }
    let refs = references(heif, &file)?;
    if has_alpha(heif, &file, &refs)? {
        return Err(WITH_ALPHA.into());
    }
    let (out, added_to) = match primary_irot(&props)? {
        Some(irot) => {
            check_in_place(&file, &refs, &props)?;
            irot_angle(heif, &irot)?;
            let mut out = heif.to_vec();
            // `irot_angle` checked that the byte exists. Keep its six reserved bits.
            out[irot.body] = (out[irot.body] & !3) | quarters_ccw;
            (out, Vec::new())
        }
        None if quarters_ccw == 0 => return Ok(heif.to_vec()),
        None => {
            let targets = rotation_targets(heif, &file, &refs)?;
            (add_irot(heif, &file, &targets, quarters_ccw)?, targets)
        }
    };
    verify(heif, &file, &out, quarters_ccw, &added_to).map_err(|e| format!("{SELF_CHECK}{e}"))?;
    Ok(out)
}

/// `heif` with the Orientation of every EXIF item (found through `iinf` and `iloc`) set to
/// 1 in place, so no viewer turns the image a second time on top of `irot`. The size never
/// changes. EXIF items that are absent, stored in `idat`, in another file or in more than
/// one extent, and EXIF without an Orientation tag, are left as they are.
pub fn normalize_exif_orientation(heif: &[u8]) -> Result<Vec<u8>, String> {
    let file = parse(heif)?;
    let Some(iinf) = file.iinf else {
        return Ok(heif.to_vec());
    };
    let mut out = heif.to_vec();
    for id in item_ids_of_type(heif, &iinf, b"Exif")? {
        let Some(item) = file.iloc.items.iter().find(|i| i.id == id) else {
            continue;
        };
        if let Some(tiff) = exif_tiff_range(heif, item)? {
            set_tiff_orientation_in_place(&mut out, tiff, 1)?;
        }
    }
    Ok(out)
}

/// The TIFF block of an EXIF item: past the 4-byte `exif_tiff_header_offset` and an optional
/// JPEG-style `Exif\0\0` prefix. `None` for an item stored in `idat`, in another file or in
/// more than one extent.
fn exif_tiff_range(heif: &[u8], item: &IlocItem) -> Result<Option<Range<usize>>, String> {
    let [extent] = item.extents.as_slice() else {
        return Ok(None);
    };
    if item.method != 0 || item.dref != 0 {
        return Ok(None);
    }
    let range = extent_range(heif.len(), item.base.value, extent)
        .ok_or("EXIF item lies outside the file")?;
    let header = heif
        .get(range.start..range.start + 4)
        .ok_or("EXIF item is too short")?;
    let offset = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let mut tiff_start = usize::try_from(offset)
        .ok()
        .and_then(|o| (range.start + 4).checked_add(o))
        .filter(|&s| s <= range.end)
        .ok_or("EXIF item's TIFF offset is out of bounds")?;
    // Some writers keep the JPEG-style prefix without counting it in the offset.
    if heif.get(tiff_start..tiff_start + 6) == Some(b"Exif\0\0") {
        tiff_start += 6;
    }
    Ok(Some(tiff_start..range.end))
}

/// The TIFF block of the first readable EXIF item of a HEIF file, for read-only use. `None`
/// when there is none or the file does not parse.
pub(crate) fn exif_item_tiff(heif: &[u8]) -> Option<&[u8]> {
    let file = parse(heif).ok()?;
    let iinf = file.iinf?;
    for id in item_ids_of_type(heif, &iinf, b"Exif").ok()? {
        let Some(item) = file.iloc.items.iter().find(|i| i.id == id) else {
            continue;
        };
        if let Ok(Some(range)) = exif_tiff_range(heif, item) {
            return heif.get(range);
        }
    }
    None
}

/// How a box stores its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SizeField {
    /// 32-bit `size`.
    Small,
    /// `size` = 1 and a 64-bit `largesize`.
    Large,
    /// `size` = 0: the box runs to the end of the file (top level only).
    ToEnd,
}

#[derive(Clone, Copy, Debug)]
struct BoxRef {
    start: usize,
    /// Offset of the payload (past the header, `largesize` and `uuid` extended type).
    body: usize,
    end: usize,
    typ: [u8; 4],
    size: SizeField,
}

/// An unsigned big-endian field of `size` bytes (0, 2, 4 or 8) at `at`; size 0 reads as 0.
#[derive(Clone, Copy, Debug)]
struct Field {
    at: usize,
    size: u8,
    value: u64,
}

#[derive(Debug)]
struct Extent {
    offset: Field,
    length: u64,
}

#[derive(Debug)]
struct IlocItem {
    id: u32,
    method: u8,
    dref: u16,
    base: Field,
    extents: Vec<Extent>,
}

#[derive(Debug)]
struct Iloc {
    offset_size: u8,
    items: Vec<IlocItem>,
}

#[derive(Debug)]
struct IpmaEntry {
    item: u32,
    /// Offset of the entry.
    start: usize,
    /// Offset of the association count byte.
    count_at: usize,
    /// Property indices (1-based, 0 = none); the essential bit is not needed here.
    indices: Vec<u16>,
    /// Offset just past the entry.
    end: usize,
}

#[derive(Debug)]
struct Ipma {
    version: u8,
    /// `flags & 1`: 15-bit property indices in 2 bytes instead of 7 bits in 1.
    wide: bool,
    entry_count_at: usize,
    entry_count: u32,
    entries: Vec<IpmaEntry>,
}

/// The parts of a HEIF file this module reads or edits.
#[derive(Debug)]
struct Heif {
    top: Vec<BoxRef>,
    meta: BoxRef,
    iinf: Option<BoxRef>,
    iref: Option<BoxRef>,
    iprp: BoxRef,
    ipco: BoxRef,
    ipma_box: BoxRef,
    /// The children of `ipco`; property index `i` is `props[i - 1]`.
    props: Vec<BoxRef>,
    ipma: Ipma,
    primary: u32,
    iloc: Iloc,
}

impl Heif {
    /// The properties associated with the primary item, in `ipma` order.
    fn primary_props(&self) -> Result<Vec<BoxRef>, String> {
        self.props_of(self.primary)
    }

    /// The nonzero property indices associated with `item`, in `ipma` order.
    fn indices_of(&self, item: u32) -> impl Iterator<Item = u16> + '_ {
        self.ipma
            .entries
            .iter()
            .filter(move |e| e.item == item)
            .flat_map(|e| e.indices.iter().copied())
            .filter(|&i| i != 0)
    }

    /// The properties associated with `item`, in `ipma` order.
    fn props_of(&self, item: u32) -> Result<Vec<BoxRef>, String> {
        let mut out = Vec::new();
        for entry in self.ipma.entries.iter().filter(|e| e.item == item) {
            for &index in entry.indices.iter().filter(|&&i| i != 0) {
                let prop = self
                    .props
                    .get(usize::from(index) - 1)
                    .ok_or("ipma refers to a missing property")?;
                out.push(*prop);
            }
        }
        Ok(out)
    }
}

/// Bounds-checked big-endian reads inside `pos..end`.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

fn truncated() -> String {
    "HEIF box is truncated".into()
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], range: Range<usize>) -> Result<Reader<'a>, String> {
        if range.start > range.end || range.end > data.len() {
            return Err(truncated());
        }
        Ok(Reader {
            data,
            pos: range.start,
            end: range.end,
        })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.end)
            .ok_or_else(truncated)?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn uint(&mut self, size: u8) -> Result<u64, String> {
        Ok(self
            .take(usize::from(size))?
            .iter()
            .fold(0u64, |v, &b| (v << 8) | u64::from(b)))
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(self.uint(2)? as u16)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(self.uint(4)? as u32)
    }

    fn field(&mut self, size: u8) -> Result<Field, String> {
        let at = self.pos;
        Ok(Field {
            at,
            size,
            value: self.uint(size)?,
        })
    }
}

/// The boxes in `data[range]`, which they must fill exactly. `size` = 0 is accepted only at
/// the top level.
fn children(data: &[u8], range: Range<usize>, top: bool) -> Result<Vec<BoxRef>, String> {
    let mut out = Vec::new();
    let mut at = range.start;
    while at < range.end {
        if out.len() >= MAX_BOXES {
            return Err("too many HEIF boxes".into());
        }
        let mut r = Reader::new(data, at..range.end)?;
        let size32 = r.u32()?;
        let typ: [u8; 4] = r.take(4)?.try_into().map_err(|_| truncated())?;
        let (size, kind) = match size32 {
            1 => (r.uint(8)?, SizeField::Large),
            0 if top => ((range.end - at) as u64, SizeField::ToEnd),
            0 => return Err("nested HEIF box with size 0".into()),
            n => (u64::from(n), SizeField::Small),
        };
        if &typ == b"uuid" {
            r.take(16)?;
        }
        let end = usize::try_from(size)
            .ok()
            .and_then(|s| at.checked_add(s))
            .filter(|&e| e >= r.pos && e <= range.end)
            .ok_or("HEIF box size out of bounds")?;
        out.push(BoxRef {
            start: at,
            body: r.pos,
            end,
            typ,
            size: kind,
        });
        at = end;
    }
    Ok(out)
}

/// A full box's version, flags and a reader over the rest of its payload.
fn full_box<'a>(data: &'a [u8], b: &BoxRef) -> Result<(u8, u32, Reader<'a>), String> {
    let mut r = Reader::new(data, b.body..b.end)?;
    let vf = r.u32()?;
    Ok(((vf >> 24) as u8, vf & 0x00FF_FFFF, r))
}

fn name(typ: &[u8; 4]) -> String {
    String::from_utf8_lossy(typ).into_owned()
}

/// The only `typ` box in `boxes`; more than one is an error.
fn single(boxes: &[BoxRef], typ: &[u8; 4]) -> Result<Option<BoxRef>, String> {
    let mut found = boxes.iter().filter(|b| &b.typ == typ);
    let first = found.next().copied();
    if found.next().is_some() {
        return Err(format!("more than one {} box", name(typ)));
    }
    Ok(first)
}

fn required(boxes: &[BoxRef], typ: &[u8; 4]) -> Result<BoxRef, String> {
    single(boxes, typ)?.ok_or_else(|| format!("no {} box", name(typ)))
}

fn parse(data: &[u8]) -> Result<Heif, String> {
    let top = children(data, 0..data.len(), true)?;
    if top.first().map(|b| &b.typ) != Some(b"ftyp") {
        return Err("not a HEIF file: no leading ftyp box".into());
    }
    let meta = required(&top, b"meta")?;
    let (_, _, r) = full_box(data, &meta)?;
    let in_meta = children(data, r.pos..meta.end, false)?;

    let hdlr = required(&in_meta, b"hdlr")?;
    let (_, _, mut r) = full_box(data, &hdlr)?;
    r.take(4)?; // pre_defined
    if r.take(4)? != b"pict" {
        return Err("HEIF meta handler is not 'pict'".into());
    }

    let pitm = required(&in_meta, b"pitm")?;
    let (version, _, mut r) = full_box(data, &pitm)?;
    let primary = if version == 0 {
        u32::from(r.u16()?)
    } else {
        r.u32()?
    };

    let iloc = parse_iloc(data, &required(&in_meta, b"iloc")?)?;
    let iinf = single(&in_meta, b"iinf")?;
    let iref = single(&in_meta, b"iref")?;

    let iprp = required(&in_meta, b"iprp")?;
    let in_iprp = children(data, iprp.body..iprp.end, false)?;
    let ipco = required(&in_iprp, b"ipco")?;
    let ipma_box = required(&in_iprp, b"ipma")?;
    let props = children(data, ipco.body..ipco.end, false)?;
    let ipma = parse_ipma(data, &ipma_box)?;

    Ok(Heif {
        top,
        meta,
        iinf,
        iref,
        iprp,
        ipco,
        ipma_box,
        props,
        ipma,
        primary,
        iloc,
    })
}

fn parse_iloc(data: &[u8], b: &BoxRef) -> Result<Iloc, String> {
    let (version, _, mut r) = full_box(data, b)?;
    if version > 2 {
        return Err(format!("unsupported iloc version {version}"));
    }
    let sizes = r.u8()?;
    let more = r.u8()?;
    let (offset_size, length_size, base_size) = (sizes >> 4, sizes & 15, more >> 4);
    let index_size = if version >= 1 { more & 15 } else { 0 };
    for size in [offset_size, length_size, base_size, index_size] {
        if ![0, 4, 8].contains(&size) {
            return Err(format!("unsupported iloc field size {size}"));
        }
    }
    let item_count = if version < 2 {
        u32::from(r.u16()?)
    } else {
        r.u32()?
    };
    let mut items = Vec::new();
    let mut extents_total = 0usize;
    // Every item takes at least 6 bytes, so a false count runs out of data quickly.
    for _ in 0..item_count {
        let id = if version < 2 {
            u32::from(r.u16()?)
        } else {
            r.u32()?
        };
        let method = if version >= 1 {
            (r.u16()? & 15) as u8
        } else {
            0
        };
        let dref = r.u16()?;
        let base = r.field(base_size)?;
        let extent_count = r.u16()?;
        let mut extents = Vec::new();
        for _ in 0..extent_count {
            extents_total += 1;
            if extents_total > MAX_EXTENTS {
                return Err("too many iloc extents".into());
            }
            r.uint(index_size)?;
            let offset = r.field(offset_size)?;
            let length = r.uint(length_size)?;
            extents.push(Extent { offset, length });
        }
        items.push(IlocItem {
            id,
            method,
            dref,
            base,
            extents,
        });
    }
    Ok(Iloc { offset_size, items })
}

fn parse_ipma(data: &[u8], b: &BoxRef) -> Result<Ipma, String> {
    let (version, flags, mut r) = full_box(data, b)?;
    let wide = flags & 1 != 0;
    let entry_count_at = r.pos;
    let entry_count = r.u32()?;
    let mut entries = Vec::new();
    // Every entry takes at least 3 bytes.
    for _ in 0..entry_count {
        let start = r.pos;
        let item = if version < 1 {
            u32::from(r.u16()?)
        } else {
            r.u32()?
        };
        let count_at = r.pos;
        let count = r.u8()?;
        let mut indices = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            indices.push(if wide {
                r.u16()? & 0x7FFF
            } else {
                u16::from(r.u8()? & 0x7F)
            });
        }
        entries.push(IpmaEntry {
            item,
            start,
            count_at,
            indices,
            end: r.pos,
        });
    }
    if r.pos != b.end {
        return Err("ipma has trailing bytes".into());
    }
    Ok(Ipma {
        version,
        wide,
        entry_count_at,
        entry_count,
        entries,
    })
}

/// The ids of the items of type `typ` in `iinf` (`infe` version 2 and 3 carry a type).
fn item_ids_of_type(data: &[u8], iinf: &BoxRef, typ: &[u8; 4]) -> Result<Vec<u32>, String> {
    let (version, _, mut r) = full_box(data, iinf)?;
    if version == 0 {
        r.u16()?;
    } else {
        r.u32()?;
    }
    let mut ids = Vec::new();
    for infe in children(data, r.pos..iinf.end, false)? {
        if &infe.typ != b"infe" {
            continue;
        }
        let (version, _, mut r) = full_box(data, &infe)?;
        let id = match version {
            2 => u32::from(r.u16()?),
            3 => r.u32()?,
            _ => continue,
        };
        r.u16()?; // item_protection_index
        if r.take(4)? == typ {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// One `iref` entry: `from` refers to `to` with `typ`.
struct Reference {
    typ: [u8; 4],
    from: u32,
    to: Vec<u32>,
}

impl Reference {
    fn to_item(&self, typ: &[u8; 4], item: u32) -> bool {
        &self.typ == typ && self.to.contains(&item)
    }
}

/// The entries of `iref` (none without one).
fn references(data: &[u8], file: &Heif) -> Result<Vec<Reference>, String> {
    let Some(iref) = file.iref else {
        return Ok(Vec::new());
    };
    let (version, _, r) = full_box(data, &iref)?;
    let id = |r: &mut Reader| {
        if version == 0 {
            r.u16().map(u32::from)
        } else {
            r.u32()
        }
    };
    let mut out = Vec::new();
    for reference in children(data, r.pos..iref.end, false)? {
        let mut r = Reader::new(data, reference.body..reference.end)?;
        let from = id(&mut r)?;
        let mut to = Vec::new();
        // Every id takes at least 2 bytes of the (bounded) box.
        for _ in 0..r.u16()? {
            to.push(id(&mut r)?);
        }
        out.push(Reference {
            typ: reference.typ,
            from,
            to,
        });
    }
    Ok(out)
}

/// Whether one of `item`'s `auxC` properties names an alpha plane.
fn is_alpha(data: &[u8], file: &Heif, item: u32) -> Result<bool, String> {
    for auxc in file.props_of(item)?.iter().filter(|p| &p.typ == b"auxC") {
        let (_, _, mut r) = full_box(data, auxc)?;
        let rest = r.take(r.end - r.pos)?;
        let aux_type = rest.split(|&b| b == 0).next().unwrap_or_default();
        if ALPHA_URNS.contains(&aux_type) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether an alpha auxiliary image (an `auxl` reference to the primary item from an item
/// whose `auxC` names an alpha plane) belongs to the primary item.
fn has_alpha(data: &[u8], file: &Heif, refs: &[Reference]) -> Result<bool, String> {
    for r in refs.iter().filter(|r| r.to_item(b"auxl", file.primary)) {
        if is_alpha(data, file, r.from)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The items a new `irot` goes on, in increasing id order: the primary item, its
/// thumbnails and its (non-alpha) auxiliary images. Errs when one of the others already
/// has its own `irot` or `imir`.
fn rotation_targets(data: &[u8], file: &Heif, refs: &[Reference]) -> Result<Vec<u32>, String> {
    let mut targets = vec![file.primary];
    for r in refs {
        if r.from == file.primary {
            continue;
        }
        let follows = r.to_item(b"thmb", file.primary)
            || (r.to_item(b"auxl", file.primary) && !is_alpha(data, file, r.from)?);
        if !follows {
            continue;
        }
        if file
            .props_of(r.from)?
            .iter()
            .any(|p| &p.typ == b"irot" || &p.typ == b"imir")
        {
            return Err(format!(
                "HEIF item {} already has its own rotation or mirror",
                r.from
            ));
        }
        targets.push(r.from);
    }
    targets.sort_unstable();
    targets.dedup();
    Ok(targets)
}

/// Refuses an in-place `irot` change whose meaning would not be a plain rotation of the
/// primary image: a transformative property after the `irot`, or the `irot` shared with a
/// tile of the grid.
fn check_in_place(file: &Heif, refs: &[Reference], props: &[BoxRef]) -> Result<(), String> {
    let at = props
        .iter()
        .position(|p| &p.typ == b"irot")
        .ok_or("no irot")?;
    if props[at + 1..]
        .iter()
        .any(|p| [b"clap", b"imir", b"irot"].contains(&&p.typ))
    {
        return Err("HEIF transformative properties after irot are not supported".into());
    }
    let index = file
        .indices_of(file.primary)
        .find(|&i| file.props.get(usize::from(i) - 1).map(|p| &p.typ) == Some(b"irot"))
        .ok_or("no irot")?;
    for r in refs
        .iter()
        .filter(|r| &r.typ == b"dimg" && r.from == file.primary)
    {
        for &tile in &r.to {
            if tile != file.primary && file.indices_of(tile).any(|i| i == index) {
                return Err("HEIF irot shared with a grid tile is not supported".into());
            }
        }
    }
    Ok(())
}

fn irot_angle(data: &[u8], irot: &BoxRef) -> Result<u8, String> {
    match data.get(irot.body) {
        Some(&b) if irot.body < irot.end => Ok(b & 3),
        _ => Err("irot box is empty".into()),
    }
}

/// The byte range of an extent at `base + offset`, where length 0 means "to the end of the
/// file"; `None` when it does not lie inside a file of `file_len` bytes.
fn extent_range(file_len: usize, base: u64, extent: &Extent) -> Option<Range<usize>> {
    let start = usize::try_from(base.checked_add(extent.offset.value)?).ok()?;
    let len = if extent.length == 0 {
        file_len.checked_sub(start)?
    } else {
        usize::try_from(extent.length).ok()?
    };
    let end = start.checked_add(len)?;
    (end <= file_len).then_some(start..end)
}

/// Byte overwrites (at offsets in the original) and insertions (before an original
/// offset).
#[derive(Default)]
struct Edit {
    patches: Vec<(usize, Vec<u8>)>,
    inserts: Vec<(usize, Vec<u8>)>,
}

impl Edit {
    /// Grows the size field of `b` by `by` bytes.
    fn grow(&mut self, b: &BoxRef, by: usize) -> Result<(), String> {
        let new = (b.end - b.start)
            .checked_add(by)
            .ok_or("HEIF box too large")?;
        match b.size {
            SizeField::Small => {
                let new = u32::try_from(new).map_err(|_| "HEIF box too large")?;
                self.patches.push((b.start, new.to_be_bytes().to_vec()));
            }
            SizeField::Large => self
                .patches
                .push((b.start + 8, (new as u64).to_be_bytes().to_vec())),
            SizeField::ToEnd => {}
        }
        Ok(())
    }

    /// Adds `by` to an `iloc` field.
    fn add_to(&mut self, f: &Field, by: u64) -> Result<(), String> {
        let v = f.value.checked_add(by).ok_or(OVERFLOW)?;
        let bytes = match f.size {
            4 => u32::try_from(v)
                .map_err(|_| OVERFLOW)?
                .to_be_bytes()
                .to_vec(),
            8 => v.to_be_bytes().to_vec(),
            _ => return Err("an iloc offset that has no field would have to change".into()),
        };
        self.patches.push((f.at, bytes));
        Ok(())
    }

    /// How far a byte at original offset `x` moves: the bytes inserted at or before it.
    fn shift(&self, x: u64) -> u64 {
        self.inserts
            .iter()
            .filter(|(at, _)| *at as u64 <= x)
            .map(|(_, bytes)| bytes.len() as u64)
            .sum()
    }

    fn apply(&self, src: &[u8]) -> Result<Vec<u8>, String> {
        let mut buf = src.to_vec();
        for (at, bytes) in &self.patches {
            buf.get_mut(*at..at + bytes.len())
                .ok_or("HEIF patch out of bounds")?
                .copy_from_slice(bytes);
        }
        let mut inserts: Vec<&(usize, Vec<u8>)> = self.inserts.iter().collect();
        inserts.sort_by_key(|(at, _)| *at);
        let added: usize = inserts.iter().map(|(_, b)| b.len()).sum();
        let mut out = Vec::with_capacity(buf.len() + added);
        let mut from = 0;
        for (at, bytes) in inserts {
            out.extend_from_slice(buf.get(from..*at).ok_or("HEIF insert out of bounds")?);
            out.extend_from_slice(bytes);
            from = *at;
        }
        out.extend_from_slice(&buf[from..]);
        Ok(out)
    }
}

/// `heif` with a new `irot` of `angle` associated with `targets` (increasing ids).
fn add_irot(heif: &[u8], file: &Heif, targets: &[u32], angle: u8) -> Result<Vec<u8>, String> {
    if let Some(b) = file
        .top
        .iter()
        .find(|b| !SHIFTABLE_TOP_LEVEL.contains(&&b.typ))
    {
        return Err(format!(
            "HEIF with a top-level {} box cannot grow",
            name(&b.typ)
        ));
    }
    if let Some(item) = file
        .iloc
        .items
        .iter()
        .find(|i| (i.method == 0 && i.dref != 0) || i.method > 2)
    {
        return Err(format!(
            "HEIF item {} is stored in a way a size change cannot follow",
            item.id
        ));
    }
    let index = file.props.len() + 1;
    let ipma = &file.ipma;
    let association = if ipma.wide {
        let index = u16::try_from(index)
            .ok()
            .filter(|&i| i <= 0x7FFF)
            .ok_or("ipco is full")?;
        (0x8000 | index).to_be_bytes().to_vec()
    } else {
        let index = u8::try_from(index)
            .ok()
            .filter(|&i| i <= 0x7F)
            .ok_or("ipco is full for 7-bit ipma indices")?;
        vec![0x80 | index]
    };

    let mut edit = Edit::default();
    let irot = vec![0, 0, 0, 9, b'i', b'r', b'o', b't', angle];
    let irot_len = irot.len();
    edit.inserts.push((file.ipco.end, irot));
    let mut ipma_growth = 0;
    let mut new_entries = 0u32;
    // `targets` is in increasing id order, so new entries at the same place stay sorted.
    for &item in targets {
        match ipma.entries.iter().find(|e| e.item == item) {
            Some(entry) => {
                let count = u8::try_from(entry.indices.len() + 1)
                    .map_err(|_| format!("HEIF item {item} has too many properties"))?;
                edit.patches.push((entry.count_at, vec![count]));
                ipma_growth += association.len();
                edit.inserts.push((entry.end, association.clone()));
            }
            None => {
                let mut new_entry = if ipma.version < 1 {
                    u16::try_from(item)
                        .map_err(|_| "item id does not fit ipma")?
                        .to_be_bytes()
                        .to_vec()
                } else {
                    item.to_be_bytes().to_vec()
                };
                new_entry.push(1);
                new_entry.extend_from_slice(&association);
                // Before the first entry of a larger id, keeping the entries sorted.
                let at = ipma
                    .entries
                    .iter()
                    .find(|e| e.item > item)
                    .map_or(file.ipma_box.end, |e| e.start);
                ipma_growth += new_entry.len();
                edit.inserts.push((at, new_entry));
                new_entries += 1;
            }
        }
    }
    if new_entries > 0 {
        let count = ipma
            .entry_count
            .checked_add(new_entries)
            .ok_or("ipma is full")?;
        edit.patches
            .push((ipma.entry_count_at, count.to_be_bytes().to_vec()));
    }
    edit.grow(&file.ipco, irot_len)?;
    edit.grow(&file.ipma_box, ipma_growth)?;
    edit.grow(&file.iprp, irot_len + ipma_growth)?;
    edit.grow(&file.meta, irot_len + ipma_growth)?;

    let mut moves = Vec::new();
    for item in &file.iloc.items {
        if item.method != 0 || item.dref != 0 {
            continue;
        }
        let mut shifts = Vec::with_capacity(item.extents.len());
        for extent in &item.extents {
            let start = item
                .base
                .value
                .checked_add(extent.offset.value)
                .ok_or(OVERFLOW)?;
            let len = if extent.length == 0 {
                (heif.len() as u64).saturating_sub(start)
            } else {
                extent.length
            };
            let end = start.saturating_add(len);
            if edit
                .inserts
                .iter()
                .any(|(at, _)| start < *at as u64 && (*at as u64) < end)
            {
                return Err("an item's data overlaps the edited boxes".into());
            }
            shifts.push(edit.shift(start));
        }
        moves.push((item, shifts));
    }
    for (item, shifts) in moves {
        let Some(&first) = shifts.first() else {
            continue;
        };
        if shifts.iter().all(|&s| s == 0) {
            continue;
        }
        let uniform = shifts.iter().all(|&s| s == first);
        if uniform && (item.base.value != 0 || file.iloc.offset_size == 0) {
            edit.add_to(&item.base, first)?;
        } else {
            for (extent, &s) in item.extents.iter().zip(&shifts) {
                if s != 0 {
                    edit.add_to(&extent.offset, s)?;
                }
            }
        }
    }
    edit.apply(heif)
}

/// Re-parses `out` and checks it against the original: every item's properties are
/// unchanged except for a new `irot` appended to the items in `added_to`, the primary item's
/// only `irot` reads `angle`, and every in-file extent holds the same bytes as before.
fn verify(src: &[u8], old: &Heif, out: &[u8], angle: u8, added_to: &[u32]) -> Result<(), String> {
    let new = parse(out)?;
    if new.primary != old.primary {
        return Err("primary item changed".into());
    }
    let types = |f: &Heif, item: u32| -> Result<Vec<[u8; 4]>, String> {
        Ok(f.props_of(item)?.iter().map(|p| p.typ).collect())
    };
    let mut items: Vec<u32> = old
        .ipma
        .entries
        .iter()
        .chain(&new.ipma.entries)
        .map(|e| e.item)
        .collect();
    items.sort_unstable();
    items.dedup();
    for item in items {
        let mut want = types(old, item)?;
        if added_to.contains(&item) {
            want.push(*b"irot");
        }
        if types(&new, item)? != want {
            return Err(format!("properties of item {item} changed"));
        }
    }
    match primary_irot(&new.primary_props()?)? {
        Some(irot) if irot_angle(out, &irot)? == angle => {}
        _ => return Err("irot does not read back".into()),
    }
    for &item in added_to {
        let irot = new
            .props_of(item)?
            .into_iter()
            .rfind(|p| &p.typ == b"irot")
            .ok_or("irot missing")?;
        if irot_angle(out, &irot)? != angle {
            return Err(format!("irot of item {item} does not read back"));
        }
    }
    if new.iloc.items.len() != old.iloc.items.len() {
        return Err("iloc item count changed".into());
    }
    for (a, b) in old.iloc.items.iter().zip(&new.iloc.items) {
        if (a.id, a.method, a.dref, a.extents.len()) != (b.id, b.method, b.dref, b.extents.len()) {
            return Err(format!("iloc item {} changed", a.id));
        }
        // A size change refuses items in other data references and reserved methods; the
        // in-place edit moves nothing.
        if a.method != 0 || a.dref != 0 {
            if a.base.value != b.base.value
                || a.extents
                    .iter()
                    .zip(&b.extents)
                    .any(|(x, y)| (x.offset.value, x.length) != (y.offset.value, y.length))
            {
                return Err(format!("relative extents of item {} changed", a.id));
            }
            continue;
        }
        for (x, y) in a.extents.iter().zip(&b.extents) {
            let Some(before) = extent_range(src.len(), a.base.value, x) else {
                continue; // Outside the original file: nothing to compare.
            };
            let same = extent_range(out.len(), b.base.value, y)
                .is_some_and(|after| out[after] == src[before.clone()]);
            if !same {
                return Err(format!("data of item {} moved incorrectly", a.id));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/heif")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    const PLAIN: &str = "rainbow-451x461.heic";
    const IROT90: &str = "rainbow-451x461-irot90.heic";
    const ALPHA: &str = "with-alpha-512x512.heic";

    fn differing(a: &[u8], b: &[u8]) -> Vec<usize> {
        assert_eq!(a.len(), b.len());
        (0..a.len()).filter(|&i| a[i] != b[i]).collect()
    }

    /// The bytes of every file-offset extent (None outside the file), per item, in `iloc`
    /// order.
    fn item_data(heif: &[u8]) -> Vec<(u32, Vec<Option<Vec<u8>>>)> {
        let file = parse(heif).unwrap();
        file.iloc
            .items
            .iter()
            .map(|item| {
                let data = item
                    .extents
                    .iter()
                    .map(|e| {
                        if item.method == 0 && item.dref == 0 {
                            extent_range(heif.len(), item.base.value, e).map(|r| heif[r].to_vec())
                        } else {
                            Some(vec![])
                        }
                    })
                    .collect();
                (item.id, data)
            })
            .collect()
    }

    // ---- A minimal HEIF builder for structures the samples do not have. ----

    fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(typ);
        b.extend_from_slice(payload);
        b
    }

    fn large_bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(typ);
        b.extend_from_slice(&((16 + payload.len()) as u64).to_be_bytes());
        b.extend_from_slice(payload);
        b
    }

    fn full(typ: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut p = ((u32::from(version) << 24) | flags).to_be_bytes().to_vec();
        p.extend_from_slice(payload);
        bx(typ, &p)
    }

    fn uint(v: u64, size: u8) -> Vec<u8> {
        v.to_be_bytes()[8 - usize::from(size)..].to_vec()
    }

    struct Item {
        id: u32,
        typ: [u8; 4],
        /// 0: in `mdat`; 1: in `idat`.
        method: u8,
        /// Stored as one extent per chunk.
        chunks: Vec<Vec<u8>>,
    }

    struct Spec {
        iloc_version: u8,
        offset_size: u8,
        length_size: u8,
        base_size: u8,
        index_size: u8,
        /// Store `mdat` items as base_offset (start of the item) + extent offsets.
        use_base: bool,
        ipma_version: u8,
        ipma_flags: u32,
        props: Vec<Vec<u8>>,
        /// (item, property indices) — all essential.
        assocs: Vec<(u32, Vec<u16>)>,
        items: Vec<Item>,
        primary: u32,
        large_meta: bool,
        extra_top: Vec<u8>,
        /// `iref` entries (type, from, to), written as a version 0 `iref` when not empty.
        refs: Vec<([u8; 4], u32, Vec<u32>)>,
    }

    fn spec() -> Spec {
        Spec {
            iloc_version: 1,
            offset_size: 4,
            length_size: 4,
            base_size: 0,
            index_size: 0,
            use_base: false,
            ipma_version: 0,
            ipma_flags: 0,
            props: vec![bx(b"ispe", &[0; 12])],
            assocs: vec![(1, vec![1])],
            items: vec![Item {
                id: 1,
                typ: *b"hvc1",
                method: 0,
                chunks: vec![b"coded-image-bytes".to_vec()],
            }],
            primary: 1,
            large_meta: false,
            extra_top: vec![],
            refs: vec![],
        }
    }

    fn build(s: &Spec) -> Vec<u8> {
        // Two passes: the field sizes do not depend on the offsets, so the first pass fixes
        // where `mdat` starts.
        let mut mdat_body = 0;
        for _ in 0..2 {
            let file = build_with(s, mdat_body);
            let mdat = file.windows(4).rposition(|w| w == b"mdat").unwrap() + 4;
            if mdat == mdat_body {
                return file;
            }
            mdat_body = mdat;
        }
        build_with(s, mdat_body)
    }

    fn build_with(s: &Spec, mdat_body: usize) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        let mut hdlr = vec![0; 4];
        hdlr.extend_from_slice(b"pict");
        hdlr.extend_from_slice(&[0; 13]);
        let pitm = full(b"pitm", 0, 0, &(s.primary as u16).to_be_bytes());

        let mut iinf = (s.items.len() as u16).to_be_bytes().to_vec();
        for item in &s.items {
            let mut p = (item.id as u16).to_be_bytes().to_vec();
            p.extend_from_slice(&[0, 0]);
            p.extend_from_slice(&item.typ);
            p.push(0);
            iinf.extend(full(b"infe", 2, 0, &p));
        }

        let v = s.iloc_version;
        let mut iloc = vec![
            (s.offset_size << 4) | s.length_size,
            (s.base_size << 4) | if v >= 1 { s.index_size } else { 0 },
        ];
        let count = s.items.len() as u32;
        iloc.extend(if v < 2 {
            uint(count.into(), 2)
        } else {
            uint(count.into(), 4)
        });
        let (mut mdat, mut idat) = (Vec::new(), Vec::new());
        for item in &s.items {
            iloc.extend(if v < 2 {
                uint(item.id.into(), 2)
            } else {
                uint(item.id.into(), 4)
            });
            if v >= 1 {
                iloc.extend(uint(item.method.into(), 2));
            }
            iloc.extend([0, 0]);
            let store = if item.method == 0 {
                &mut mdat
            } else {
                &mut idat
            };
            let item_start = store.len() + if item.method == 0 { mdat_body } else { 0 };
            let base = if s.use_base && item.method == 0 {
                item_start
            } else {
                0
            };
            iloc.extend(uint(base as u64, s.base_size));
            iloc.extend(uint(item.chunks.len() as u64, 2));
            for chunk in &item.chunks {
                let at = store.len() + if item.method == 0 { mdat_body } else { 0 };
                iloc.extend(uint(0, if v >= 1 { s.index_size } else { 0 }));
                iloc.extend(uint((at - base) as u64, s.offset_size));
                iloc.extend(uint(chunk.len() as u64, s.length_size));
                store.extend_from_slice(chunk);
            }
        }

        let mut ipma = (s.assocs.len() as u32).to_be_bytes().to_vec();
        for (item, indices) in &s.assocs {
            ipma.extend(if s.ipma_version < 1 {
                uint((*item).into(), 2)
            } else {
                uint((*item).into(), 4)
            });
            ipma.push(indices.len() as u8);
            for &i in indices {
                if s.ipma_flags & 1 != 0 {
                    ipma.extend((0x8000 | i).to_be_bytes());
                } else {
                    ipma.push(0x80 | i as u8);
                }
            }
        }
        let mut iprp = bx(b"ipco", &s.props.concat());
        iprp.extend(full(b"ipma", s.ipma_version, s.ipma_flags, &ipma));

        let mut meta = vec![0; 4];
        meta.extend(full(b"hdlr", 0, 0, &hdlr));
        meta.extend(pitm);
        meta.extend(full(b"iloc", v, 0, &iloc));
        meta.extend(full(b"iinf", 0, 0, &iinf));
        meta.extend(bx(b"iprp", &iprp));
        if !s.refs.is_empty() {
            let mut iref = Vec::new();
            for (typ, from, to) in &s.refs {
                let mut r = (*from as u16).to_be_bytes().to_vec();
                r.extend((to.len() as u16).to_be_bytes());
                for &t in to {
                    r.extend((t as u16).to_be_bytes());
                }
                iref.extend(bx(typ, &r));
            }
            meta.extend(full(b"iref", 0, 0, &iref));
        }
        if !idat.is_empty() {
            meta.extend(bx(b"idat", &idat));
        }
        let mut file = ftyp;
        file.extend(if s.large_meta {
            large_bx(b"meta", &meta)
        } else {
            bx(b"meta", &meta)
        });
        file.extend_from_slice(&s.extra_top);
        file.extend(bx(b"mdat", &mdat));
        file
    }

    /// The primary item's property types.
    fn primary_types(heif: &[u8]) -> Vec<[u8; 4]> {
        parse(heif)
            .unwrap()
            .primary_props()
            .unwrap()
            .iter()
            .map(|p| p.typ)
            .collect()
    }

    /// Adds irot 1 to `s`'s file and checks the result keeps every item's data.
    fn add_and_check(s: &Spec) -> Vec<u8> {
        let src = build(s);
        assert_eq!(read_rotation(&src).unwrap(), 0);
        let out = set_rotation(&src, 1).unwrap();
        assert_eq!(read_rotation(&out).unwrap(), 1);
        assert_eq!(item_data(&out), item_data(&src));
        out
    }

    // ---- Tests ----

    #[test]
    fn compose_turns_clockwise_quarters_into_anticlockwise_irot() {
        // One clockwise turn = three anti-clockwise ones.
        assert_eq!(compose(0, 1), 3);
        assert_eq!(compose(0, 2), 2);
        assert_eq!(compose(0, 3), 1);
        assert_eq!(compose(1, 1), 0);
        assert_eq!(compose(3, 3), 0);
        assert_eq!(compose(2, 0), 2);
        for stored in 0..4 {
            for user in 0..4 {
                // Turning back anti-clockwise restores the stored angle.
                assert_eq!(compose(compose(stored, user), 4 - user), stored);
            }
        }
    }

    #[test]
    fn read_rotation_of_samples() {
        assert_eq!(read_rotation(&sample(PLAIN)).unwrap(), 0);
        assert_eq!(read_rotation(&sample(ALPHA)).unwrap(), 0);
        assert_eq!(read_rotation(&sample(IROT90)).unwrap(), 1);
    }

    #[test]
    fn set_existing_irot_in_place() {
        let src = sample(IROT90);
        let out = set_rotation(&src, 3).unwrap();
        assert_eq!(read_rotation(&out).unwrap(), 3);
        let changed = differing(&src, &out);
        assert_eq!(changed.len(), 1);
        assert_eq!(&src[changed[0] - 4..changed[0]], b"irot");
        // Setting 0 keeps the property, with angle 0.
        let zero = set_rotation(&src, 0).unwrap();
        assert_eq!(zero.len(), src.len());
        assert_eq!(read_rotation(&zero).unwrap(), 0);
    }

    #[test]
    fn add_irot_shifts_iloc_offsets() {
        let src = sample(PLAIN);
        let out = set_rotation(&src, 1).unwrap();
        assert_eq!(out.len(), src.len() + 10);
        assert_eq!(read_rotation(&out).unwrap(), 1);
        assert_eq!(item_data(&out), item_data(&src));
        // The README recipe made the irot sample exactly this way.
        assert_eq!(out, sample(IROT90));
    }

    #[test]
    fn alpha_is_refused() {
        // libheif would turn the colour planes but not the alpha plane.
        assert_eq!(set_rotation(&sample(ALPHA), 1).unwrap_err(), WITH_ALPHA);
        assert_eq!(set_rotation(&sample(ALPHA), 0).unwrap_err(), WITH_ALPHA);
    }

    #[test]
    fn policy_refusals_are_told_from_corrupt_files() {
        let refused = set_rotation(&sample(ALPHA), 1).unwrap_err();
        assert!(is_refusal(&refused), "{refused}");
        for policy in [
            MIRRORED.to_string(),
            SEVERAL_IROT.to_string(),
            "HEIF with a top-level moov box cannot grow".to_string(),
            "HEIF irot shared with a grid tile is not supported".to_string(),
        ] {
            assert!(is_refusal(&policy), "{policy}");
        }
        let truncated = sample(PLAIN)[..200].to_vec();
        let corrupt = set_rotation(&truncated, 1).unwrap_err();
        assert!(!is_refusal(&corrupt), "{corrupt}");
        assert!(!is_refusal(&read_rotation(b"garbage").unwrap_err()));
        // A self-check failure is never a refusal, whatever it quotes.
        let self_check = format!("{SELF_CHECK}{MIRRORED}");
        assert!(!is_refusal(&self_check));
    }

    #[test]
    fn zero_without_irot_is_unchanged() {
        let src = sample(PLAIN);
        assert_eq!(set_rotation(&src, 0).unwrap(), src);
    }

    #[test]
    fn four_quarter_turns_restore_bytes() {
        let src = sample(IROT90);
        let mut file = src.clone();
        for _ in 0..4 {
            let next = compose(read_rotation(&file).unwrap(), 1);
            file = set_rotation(&file, next).unwrap();
        }
        assert_eq!(file, src);

        // Without an irot the first turn adds one; after four it stays, at angle 0.
        let plain = sample(PLAIN);
        let mut file = plain.clone();
        for _ in 0..4 {
            let next = compose(read_rotation(&file).unwrap(), 1);
            file = set_rotation(&file, next).unwrap();
        }
        assert_eq!(file.len(), plain.len() + 10);
        assert_eq!(read_rotation(&file).unwrap(), 0);
    }

    #[test]
    fn bad_angle_is_error() {
        assert!(set_rotation(&sample(PLAIN), 4).is_err());
    }

    #[test]
    fn idat_extents_not_shifted() {
        let mut s = spec();
        s.items.push(Item {
            id: 2,
            typ: *b"Exif",
            method: 1,
            chunks: vec![b"in-idat".to_vec()],
        });
        let src = build(&s);
        let out = add_and_check(&s);
        let (old, new) = (parse(&src).unwrap(), parse(&out).unwrap());
        let idat_offset = |f: &Heif| f.iloc.items[1].extents[0].offset.value;
        assert_eq!(idat_offset(&new), idat_offset(&old));
        // The mdat item moved by the 9-byte irot plus the 1-byte association.
        let mdat_offset = |f: &Heif| f.iloc.items[0].extents[0].offset.value;
        assert_eq!(mdat_offset(&new), mdat_offset(&old) + 10);
        // idat itself still holds the same bytes.
        let idat = |f: &[u8]| {
            let at = f.windows(4).position(|w| w == b"idat").unwrap();
            f[at + 4..at + 11].to_vec()
        };
        assert_eq!(idat(&out), b"in-idat");
        assert_eq!(idat(&src), b"in-idat");
    }

    #[test]
    fn offset_overflow_is_error() {
        // An extent stored at 0xFFFF_FFF8 in a 4-byte field (it lies past the end of this
        // small file) cannot move by 10 bytes.
        let mut s = spec();
        s.items.push(Item {
            id: 2,
            typ: *b"hvc1",
            method: 0,
            chunks: vec![b"x".to_vec()],
        });
        let mut src = build(&s);
        let file = parse(&src).unwrap();
        let at = file.iloc.items[1].extents[0].offset.at;
        src[at..at + 4].copy_from_slice(&0xFFFF_FFF8u32.to_be_bytes());
        let err = set_rotation(&src, 1).unwrap_err();
        assert_eq!(err, OVERFLOW);
        // An 8-byte field takes the same move.
        let mut s = spec();
        s.offset_size = 8;
        add_and_check(&s);
    }

    #[test]
    fn iloc_layouts_are_all_adjusted() {
        for version in [0, 1, 2] {
            for offset_size in [4, 8] {
                for length_size in [0, 4, 8] {
                    for index_size in [0, 4, 8] {
                        let mut s = spec();
                        s.iloc_version = version;
                        s.offset_size = offset_size;
                        s.length_size = length_size;
                        s.index_size = index_size;
                        if length_size == 0 {
                            // Length 0 = to the end of the file: one item, one extent.
                            s.items[0].chunks = vec![b"whole-rest".to_vec()];
                        } else {
                            s.items[0].chunks = vec![b"part-1".to_vec(), b"part-2".to_vec()];
                        }
                        add_and_check(&s);
                    }
                }
            }
        }
    }

    #[test]
    fn base_offset_moves_instead_of_extents() {
        for (base_size, offset_size) in [(4, 4), (8, 0), (8, 8), (4, 0)] {
            let mut s = spec();
            s.base_size = base_size;
            s.offset_size = offset_size;
            s.use_base = true;
            if offset_size != 0 {
                s.items[0].chunks = vec![b"part-1".to_vec(), b"part-2".to_vec()];
            }
            let src = build(&s);
            let out = add_and_check(&s);
            let (old, new) = (parse(&src).unwrap(), parse(&out).unwrap());
            let (a, b) = (&old.iloc.items[0], &new.iloc.items[0]);
            assert_eq!(b.base.value, a.base.value + 10);
            for (x, y) in a.extents.iter().zip(&b.extents) {
                assert_eq!(x.offset.value, y.offset.value);
            }
        }
    }

    #[test]
    fn data_spanning_the_edit_is_error() {
        // An extent from offset 0 to the end of the file (length 0) contains meta: inserting
        // into it would change the item's bytes.
        let mut src = build(&spec());
        let file = parse(&src).unwrap();
        let at = file.iloc.items[0].extents[0].offset.at;
        src[at..at + 8].copy_from_slice(&[0; 8]);
        assert!(set_rotation(&src, 1).unwrap_err().contains("overlaps"));
    }

    #[test]
    fn wide_ipma_indices_and_version_1() {
        let mut s = spec();
        s.ipma_version = 1;
        s.ipma_flags = 1;
        let src = build(&s);
        let out = add_and_check(&s);
        // 9-byte irot + 2-byte association.
        assert_eq!(out.len(), src.len() + 11);
        assert_eq!(primary_types(&out), vec![*b"ispe", *b"irot"]);
    }

    #[test]
    fn narrow_ipma_index_overflow_is_error() {
        let mut s = spec();
        s.props = (0..127).map(|_| bx(b"free", &[])).collect();
        let src = build(&s);
        assert_eq!(parse(&src).unwrap().props.len(), 127);
        assert!(set_rotation(&src, 1).is_err());
        // With 2-byte indices there is room.
        s.ipma_flags = 1;
        add_and_check(&s);
    }

    #[test]
    fn primary_without_ipma_entry_gets_one() {
        let mut s = spec();
        s.items.push(Item {
            id: 2,
            typ: *b"hvc1",
            method: 0,
            chunks: vec![b"other".to_vec()],
        });
        s.assocs = vec![(2, vec![1])];
        let src = build(&s);
        let out = add_and_check(&s);
        // Entry: 2-byte item id, count, 1-byte association.
        assert_eq!(out.len(), src.len() + 9 + 4);
        assert_eq!(primary_types(&out), vec![*b"irot"]);
        let ipma = parse(&out).unwrap().ipma;
        assert_eq!(ipma.entry_count, 2);
        // Inserted before item 2's entry, keeping item id order.
        let ids: Vec<u32> = ipma.entries.iter().map(|e| e.item).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(ipma.entries[1].indices, vec![1]);
    }

    #[test]
    fn grid_gets_irot_and_tiles_do_not() {
        // Primary item 1 is a grid in idat; items 2 and 3 are its tiles in mdat, sharing
        // property 1 (hvcC stand-in); the grid has ispe (2).
        let mut s = spec();
        s.props = vec![bx(b"hvcC", &[1, 2, 3]), bx(b"ispe", &[0; 12])];
        s.items = vec![
            Item {
                id: 1,
                typ: *b"grid",
                method: 1,
                chunks: vec![vec![0, 0, 0, 1, 0, 64, 0, 32]],
            },
            Item {
                id: 2,
                typ: *b"hvc1",
                method: 0,
                chunks: vec![b"tile-a".to_vec()],
            },
            Item {
                id: 3,
                typ: *b"hvc1",
                method: 0,
                chunks: vec![b"tile-b".to_vec()],
            },
        ];
        s.assocs = vec![(1, vec![2]), (2, vec![1]), (3, vec![1])];
        s.refs = vec![(*b"dimg", 1, vec![2, 3])];
        let out = add_and_check(&s);
        let file = parse(&out).unwrap();
        assert_eq!(primary_types(&out), vec![*b"ispe", *b"irot"]);
        assert_eq!(file.ipma.entries[1].indices, vec![1]);
        assert_eq!(file.ipma.entries[2].indices, vec![1]);
    }

    #[test]
    fn largesize_meta_is_grown() {
        let mut s = spec();
        s.large_meta = true;
        let src = build(&s);
        let out = add_and_check(&s);
        let size = |f: &[u8]| {
            let meta = parse(f).unwrap().meta;
            assert_eq!(meta.size, SizeField::Large);
            u64::from_be_bytes(f[meta.start + 8..meta.start + 16].try_into().unwrap())
        };
        assert_eq!(size(&out), size(&src) + 10);
    }

    #[test]
    fn unknown_top_level_box_refuses_growth_only() {
        let mut s = spec();
        s.extra_top = bx(b"moov", &[0; 8]);
        let src = build(&s);
        assert!(set_rotation(&src, 1).unwrap_err().contains("moov"));
        // An in-place change still works.
        s.props.push(bx(b"irot", &[1]));
        s.assocs = vec![(1, vec![1, 2])];
        let src = build(&s);
        let out = set_rotation(&src, 2).unwrap();
        assert_eq!(read_rotation(&out).unwrap(), 2);
    }

    fn item(id: u32, data: &[u8]) -> Item {
        Item {
            id,
            typ: *b"hvc1",
            method: 0,
            chunks: vec![data.to_vec()],
        }
    }

    fn auxc(aux_type: &[u8]) -> Vec<u8> {
        let mut p = aux_type.to_vec();
        p.push(0);
        full(b"auxC", 0, 0, &p)
    }

    const DEPTH: &[u8] = b"urn:mpeg:hevc:2015:auxid:2";

    #[test]
    fn only_alpha_auxiliary_images_are_refused() {
        // Item 2 is an auxiliary image of item 1 (iref auxl 2 -> 1) with auxC property 2.
        let with_aux = |aux_type: &[u8]| {
            let mut s = spec();
            s.props.push(auxc(aux_type));
            s.items.push(item(2, b"aux"));
            s.assocs = vec![(1, vec![1]), (2, vec![2])];
            s.refs = vec![(*b"auxl", 2, vec![1])];
            build(&s)
        };
        let alpha = with_aux(b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha");
        assert_eq!(set_rotation(&alpha, 1).unwrap_err(), WITH_ALPHA);
        let depth = with_aux(DEPTH);
        let out = set_rotation(&depth, 1).unwrap();
        assert_eq!(item_data(&out), item_data(&depth));
        assert_eq!(item_data(&depth)[1].1, vec![Some(b"aux".to_vec())]);
        // The depth map turns with the primary image.
        let file = parse(&out).unwrap();
        let types: Vec<_> = file.props_of(2).unwrap().iter().map(|p| p.typ).collect();
        assert_eq!(types, vec![*b"auxC", *b"irot"]);
    }

    #[test]
    fn thumbnail_and_depth_share_the_new_irot() {
        // 1 primary; 2 thumbnail (thmb 2 -> 1) without an ipma entry; 3 depth (auxl 3 -> 1);
        // 4 an unrelated item. Properties: 1 ispe, 2 auxC(depth).
        let mut s = spec();
        s.props.push(auxc(DEPTH));
        s.items
            .extend([item(2, b"thumb"), item(3, b"depth"), item(4, b"other")]);
        s.assocs = vec![(1, vec![1]), (3, vec![2]), (4, vec![1])];
        s.refs = vec![(*b"thmb", 2, vec![1]), (*b"auxl", 3, vec![1])];
        let src = build(&s);
        let out = add_and_check(&s);
        let file = parse(&out).unwrap();
        let types =
            |item| -> Vec<[u8; 4]> { file.props_of(item).unwrap().iter().map(|p| p.typ).collect() };
        assert_eq!(types(1), vec![*b"ispe", *b"irot"]);
        assert_eq!(types(2), vec![*b"irot"]);
        assert_eq!(types(3), vec![*b"auxC", *b"irot"]);
        assert_eq!(types(4), vec![*b"ispe"]);
        // One shared property, and the new entry for item 2 sits between 1 and 3.
        let ids: Vec<u32> = file.ipma.entries.iter().map(|e| e.item).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
        assert_eq!(file.ipma.entry_count, 4);
        assert_eq!(file.ipma.entries[1].indices, vec![3]);
        assert_eq!(file.props.len(), 3);
        // irot 9 + three 1-byte associations + new entry (2 id + 1 count).
        assert_eq!(out.len(), src.len() + 9 + 3 + 3);
        assert_eq!(read_rotation(&out).unwrap(), 1);
    }

    #[test]
    fn follower_with_own_rotation_or_mirror_is_refused() {
        for own in [bx(b"irot", &[2]), bx(b"imir", &[0])] {
            let mut s = spec();
            s.props.push(own);
            s.items.push(item(2, b"thumb"));
            s.assocs = vec![(1, vec![1]), (2, vec![2])];
            s.refs = vec![(*b"thmb", 2, vec![1])];
            let err = set_rotation(&build(&s), 1).unwrap_err();
            assert!(err.contains("own rotation"), "{err}");
        }
    }

    #[test]
    fn new_primary_entry_is_inserted_in_id_order() {
        let mut s = spec();
        s.items = vec![item(1, b"a"), item(2, b"primary"), item(3, b"c")];
        s.primary = 2;
        s.assocs = vec![(1, vec![1]), (3, vec![1])];
        let out = add_and_check(&s);
        let ids: Vec<u32> = parse(&out)
            .unwrap()
            .ipma
            .entries
            .iter()
            .map(|e| e.item)
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(primary_types(&out), vec![*b"irot"]);
    }

    /// `s` built, then item `index`'s data reference or construction method overwritten
    /// (iloc version 1, no base_offset field: they are the 4 bytes before the extent count).
    fn with_location(s: &Spec, index: usize, method: u16, dref: u16) -> Vec<u8> {
        assert_eq!((s.iloc_version, s.base_size), (1, 0));
        let mut f = build(s);
        let at = parse(&f).unwrap().iloc.items[index].base.at;
        f[at - 4..at - 2].copy_from_slice(&method.to_be_bytes());
        f[at - 2..at].copy_from_slice(&dref.to_be_bytes());
        f
    }

    #[test]
    fn other_data_references_and_reserved_methods_refuse_growth() {
        let mut s = spec();
        s.items.push(item(2, b"elsewhere"));
        for (method, dref) in [(0, 1), (3, 0), (15, 0)] {
            let f = with_location(&s, 1, method, dref);
            let err = set_rotation(&f, 1).unwrap_err();
            assert!(err.contains("size change"), "{err}");
        }
        // An in-place change moves nothing and is still allowed.
        s.props.push(bx(b"irot", &[1]));
        s.assocs = vec![(1, vec![1, 2])];
        let f = with_location(&s, 1, 0, 1);
        let out = set_rotation(&f, 3).unwrap();
        assert_eq!(differing(&f, &out).len(), 1);
    }

    #[test]
    fn transformative_property_after_irot_refuses_in_place_change() {
        let mut s = spec();
        s.props.push(bx(b"irot", &[1]));
        s.props.push(bx(b"clap", &[0; 32]));
        // Conforming order: clap then irot.
        s.assocs = vec![(1, vec![1, 3, 2])];
        assert_eq!(
            read_rotation(&set_rotation(&build(&s), 2).unwrap()).unwrap(),
            2
        );
        // irot then clap.
        s.assocs = vec![(1, vec![1, 2, 3])];
        let err = set_rotation(&build(&s), 2).unwrap_err();
        assert!(err.contains("after irot"), "{err}");
    }

    #[test]
    fn several_irots_are_refused() {
        let mut s = spec();
        s.props.push(bx(b"irot", &[1]));
        s.props.push(bx(b"irot", &[1]));
        s.assocs = vec![(1, vec![1, 2, 3])];
        let f = build(&s);
        assert_eq!(read_rotation(&f).unwrap_err(), SEVERAL_IROT);
        assert_eq!(set_rotation(&f, 0).unwrap_err(), SEVERAL_IROT);
    }

    #[test]
    fn irot_shared_with_a_tile_refuses_in_place_change() {
        // Grid 1 (idat) with tiles 2 and 3; property 2 is an irot.
        let mut s = spec();
        s.props = vec![bx(b"ispe", &[0; 12]), bx(b"irot", &[1])];
        s.items = vec![
            Item {
                id: 1,
                typ: *b"grid",
                method: 1,
                chunks: vec![vec![0, 0, 0, 1, 0, 64, 0, 32]],
            },
            item(2, b"tile-a"),
            item(3, b"tile-b"),
        ];
        s.refs = vec![(*b"dimg", 1, vec![2, 3])];
        s.assocs = vec![(1, vec![1, 2]), (2, vec![1]), (3, vec![1])];
        assert_eq!(
            read_rotation(&set_rotation(&build(&s), 3).unwrap()).unwrap(),
            3
        );
        s.assocs = vec![(1, vec![1, 2]), (2, vec![1]), (3, vec![1, 2])];
        let err = set_rotation(&build(&s), 3).unwrap_err();
        assert!(err.contains("tile"), "{err}");
    }

    #[test]
    fn mirrored_primary_is_refused() {
        let mut s = spec();
        s.props.push(bx(b"imir", &[0]));
        s.assocs = vec![(1, vec![1, 2])];
        assert_eq!(set_rotation(&build(&s), 1).unwrap_err(), MIRRORED);
    }

    /// A TIFF block with only IFD0 holding Orientation (and one other tag).
    fn tiff(be: bool, orientation: u16) -> Vec<u8> {
        let w16 = |v: u16| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        let w32 = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        let mut t = if be {
            b"MM\0\x2a".to_vec()
        } else {
            b"II\x2a\0".to_vec()
        };
        t.extend(w32(8));
        t.extend(w16(2));
        // ImageWidth LONG 1 = 640, Orientation SHORT 1.
        t.extend(w16(0x0100));
        t.extend(w16(4));
        t.extend(w32(1));
        t.extend(w32(640));
        t.extend(w16(0x0112));
        t.extend(w16(3));
        t.extend(w32(1));
        t.extend(w16(orientation));
        t.extend([0, 0]);
        t.extend(w32(0));
        t
    }

    fn with_exif_item(payload: Vec<u8>) -> Vec<u8> {
        let mut s = spec();
        s.items.push(Item {
            id: 2,
            typ: *b"Exif",
            method: 0,
            chunks: vec![payload],
        });
        build(&s)
    }

    fn orientation_in(heif: &[u8]) -> u32 {
        let at = heif
            .windows(4)
            .rposition(|w| w == b"MM\0\x2a" || w == b"II\x2a\0");
        let tiff = heif[at.unwrap()..].to_vec();
        let exif = exif::Reader::new().read_raw(tiff).unwrap();
        exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
            .unwrap()
            .value
            .get_uint(0)
            .unwrap()
    }

    #[test]
    fn exif_orientation_normalized() {
        // ISO layout: offset 6 past an "Exif\0\0" prefix, big-endian TIFF.
        let mut payload = 6u32.to_be_bytes().to_vec();
        payload.extend_from_slice(b"Exif\0\0");
        payload.extend(tiff(true, 6));
        let src = with_exif_item(payload);
        assert_eq!(orientation_in(&src), 6);
        let out = normalize_exif_orientation(&src).unwrap();
        assert_eq!(orientation_in(&out), 1);
        assert_eq!(differing(&src, &out).len(), 1);

        // Offset 0 with the TIFF right after it, little-endian.
        let mut payload = 0u32.to_be_bytes().to_vec();
        payload.extend(tiff(false, 8));
        let src = with_exif_item(payload);
        let out = normalize_exif_orientation(&src).unwrap();
        assert_eq!(orientation_in(&out), 1);
        assert_eq!(differing(&src, &out).len(), 1);

        // Offset 0 but the prefix is there anyway.
        let mut payload = 0u32.to_be_bytes().to_vec();
        payload.extend_from_slice(b"Exif\0\0");
        payload.extend(tiff(false, 3));
        let out = normalize_exif_orientation(&with_exif_item(payload)).unwrap();
        assert_eq!(orientation_in(&out), 1);

        // Already 1, and no EXIF at all: unchanged.
        let mut payload = 0u32.to_be_bytes().to_vec();
        payload.extend(tiff(true, 1));
        let src = with_exif_item(payload);
        assert_eq!(normalize_exif_orientation(&src).unwrap(), src);
        let plain = sample(PLAIN);
        assert_eq!(normalize_exif_orientation(&plain).unwrap(), plain);
    }

    #[test]
    fn hostile_boxes_never_panic() {
        let mut exif = 0u32.to_be_bytes().to_vec();
        exif.extend(tiff(false, 6));
        let mut grid = spec();
        grid.iloc_version = 2;
        grid.base_size = 8;
        grid.use_base = true;
        grid.ipma_flags = 1;
        let mut followers = spec();
        followers.props.push(auxc(DEPTH));
        followers
            .items
            .extend([item(2, b"thumb"), item(3, b"depth")]);
        followers.assocs = vec![(1, vec![1]), (3, vec![2])];
        followers.refs = vec![(*b"thmb", 2, vec![1]), (*b"auxl", 3, vec![1])];
        let seeds = [
            build(&followers),
            sample(PLAIN),
            sample(IROT90),
            sample(ALPHA),
            with_exif_item(exif),
            build(&grid),
        ];
        // xorshift32 with a fixed seed: fast and the same every run.
        let mut state = 0x2545_F491u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for round in 0..10_000 {
            let seed = &seeds[round % seeds.len()];
            let mut f = seed.clone();
            // Mostly inside meta, where the structure is.
            let meta_end = 1300.min(f.len());
            match next() % 4 {
                0 => {
                    for _ in 0..1 + next() % 6 {
                        let at = (next() as usize) % meta_end;
                        f[at] = next() as u8;
                    }
                }
                1 => {
                    // A size or offset-like word set to an edge value.
                    let at = (next() as usize) % (meta_end - 4);
                    let v =
                        [0, 1, 7, 8, 9, 0xFFFF_FFFF, 0x7FFF_FFFF, next()][(next() % 8) as usize];
                    f[at..at + 4].copy_from_slice(&v.to_be_bytes());
                }
                2 => f.truncate((next() as usize) % f.len()),
                _ => {
                    let at = (next() as usize) % meta_end;
                    f[at] ^= 1 << (next() % 8);
                }
            }
            let _ = read_rotation(&f);
            if let Ok(out) = set_rotation(&f, (next() % 4) as u8) {
                // Whatever is returned passed the self-check: it parses again and every
                // item keeps its bytes.
                assert!(parse(&out).is_ok());
                assert_eq!(item_data(&out), item_data(&f), "round {round}");
            }
            let _ = normalize_exif_orientation(&f);
        }
    }
}
