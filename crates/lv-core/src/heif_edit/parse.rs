//! Parsing the HEIF boxes a rotation edit needs: the box tree, `iloc`, `ipma`, `iinf`
//! and the property lists, with every length checked.

use std::ops::Range;

use super::{MAX_ASSOCIATIONS, MAX_BOXES, MAX_EXTENTS, MAX_IPMA_ENTRIES, MAX_ITEMS};

/// How a box stores its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SizeField {
    /// 32-bit `size`.
    Small,
    /// `size` = 1 and a 64-bit `largesize`.
    Large,
    /// `size` = 0: the box runs to the end of the file (top level only).
    ToEnd,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BoxRef {
    pub(super) start: usize,
    /// Offset of the payload (past the header, `largesize` and `uuid` extended type).
    pub(super) body: usize,
    pub(super) end: usize,
    pub(super) typ: [u8; 4],
    pub(super) size: SizeField,
}

/// An unsigned big-endian field of `size` bytes (0, 2, 4 or 8) at `at`; size 0 reads as 0.
#[derive(Clone, Copy, Debug)]
pub(super) struct Field {
    pub(super) at: usize,
    pub(super) size: u8,
    pub(super) value: u64,
}

#[derive(Debug)]
pub(super) struct Extent {
    pub(super) offset: Field,
    pub(super) length: u64,
}

#[derive(Debug)]
pub(super) struct IlocItem {
    pub(super) id: u32,
    pub(super) method: u8,
    pub(super) dref: u16,
    pub(super) base: Field,
    pub(super) extents: Vec<Extent>,
}

#[derive(Debug)]
pub(super) struct Iloc {
    pub(super) offset_size: u8,
    pub(super) items: Vec<IlocItem>,
}

#[derive(Debug)]
pub(super) struct IpmaEntry {
    pub(super) item: u32,
    /// Offset of the entry.
    pub(super) start: usize,
    /// Offset of the association count byte.
    pub(super) count_at: usize,
    /// Property indices (1-based, 0 = none); the essential bit is not needed here.
    pub(super) indices: Vec<u16>,
    /// Offset just past the entry.
    pub(super) end: usize,
}

#[derive(Debug)]
pub(super) struct Ipma {
    pub(super) version: u8,
    /// `flags & 1`: 15-bit property indices in 2 bytes instead of 7 bits in 1.
    pub(super) wide: bool,
    pub(super) entry_count_at: usize,
    pub(super) entry_count: u32,
    pub(super) entries: Vec<IpmaEntry>,
}

/// The parts of a HEIF file this module reads or edits.
#[derive(Debug)]
pub(super) struct Heif {
    pub(super) top: Vec<BoxRef>,
    pub(super) meta: BoxRef,
    pub(super) iinf: Option<BoxRef>,
    pub(super) iref: Option<BoxRef>,
    pub(super) iprp: BoxRef,
    pub(super) ipco: BoxRef,
    pub(super) ipma_box: BoxRef,
    /// The children of `ipco`; property index `i` is `props[i - 1]`.
    pub(super) props: Vec<BoxRef>,
    pub(super) ipma: Ipma,
    pub(super) primary: u32,
    pub(super) iloc: Iloc,
}

impl Heif {
    /// The properties associated with the primary item, in `ipma` order.
    pub(super) fn primary_props(&self) -> Result<Vec<BoxRef>, String> {
        self.props_of(self.primary)
    }

    /// The nonzero property indices associated with `item`, in `ipma` order.
    pub(super) fn indices_of(&self, item: u32) -> impl Iterator<Item = u16> + '_ {
        self.ipma
            .entries
            .iter()
            .filter(move |e| e.item == item)
            .flat_map(|e| e.indices.iter().copied())
            .filter(|&i| i != 0)
    }

    /// The properties associated with `item`, in `ipma` order.
    pub(super) fn props_of(&self, item: u32) -> Result<Vec<BoxRef>, String> {
        self.walk_props(item).collect()
    }

    /// The properties associated with `item`, in `ipma` order, one at a time (an error for
    /// an index past `ipco`).
    pub(super) fn walk_props(
        &self,
        item: u32,
    ) -> impl Iterator<Item = Result<BoxRef, String>> + '_ {
        self.indices_of(item).map(|index| {
            self.props
                .get(usize::from(index) - 1)
                .copied()
                .ok_or_else(|| "ipma refers to a missing property".to_string())
        })
    }
}

/// Bounds-checked big-endian reads inside `pos..end`.
pub(super) struct Reader<'a> {
    pub(super) data: &'a [u8],
    pub(super) pos: usize,
    pub(super) end: usize,
}

pub(super) fn truncated() -> String {
    "HEIF box is truncated".into()
}

impl<'a> Reader<'a> {
    pub(super) fn new(data: &'a [u8], range: Range<usize>) -> Result<Reader<'a>, String> {
        if range.start > range.end || range.end > data.len() {
            return Err(truncated());
        }
        Ok(Reader {
            data,
            pos: range.start,
            end: range.end,
        })
    }

    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.end)
            .ok_or_else(truncated)?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    pub(super) fn uint(&mut self, size: u8) -> Result<u64, String> {
        Ok(self
            .take(usize::from(size))?
            .iter()
            .fold(0u64, |v, &b| (v << 8) | u64::from(b)))
    }

    pub(super) fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u16(&mut self) -> Result<u16, String> {
        Ok(self.uint(2)? as u16)
    }

    pub(super) fn u32(&mut self) -> Result<u32, String> {
        Ok(self.uint(4)? as u32)
    }

    pub(super) fn field(&mut self, size: u8) -> Result<Field, String> {
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
pub(super) fn children(data: &[u8], range: Range<usize>, top: bool) -> Result<Vec<BoxRef>, String> {
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
pub(super) fn full_box<'a>(data: &'a [u8], b: &BoxRef) -> Result<(u8, u32, Reader<'a>), String> {
    let mut r = Reader::new(data, b.body..b.end)?;
    let vf = r.u32()?;
    Ok(((vf >> 24) as u8, vf & 0x00FF_FFFF, r))
}

pub(super) fn name(typ: &[u8; 4]) -> String {
    String::from_utf8_lossy(typ).into_owned()
}

/// The only `typ` box in `boxes`; more than one is an error.
pub(super) fn single(boxes: &[BoxRef], typ: &[u8; 4]) -> Result<Option<BoxRef>, String> {
    let mut found = boxes.iter().filter(|b| &b.typ == typ);
    let first = found.next().copied();
    if found.next().is_some() {
        return Err(format!("more than one {} box", name(typ)));
    }
    Ok(first)
}

pub(super) fn required(boxes: &[BoxRef], typ: &[u8; 4]) -> Result<BoxRef, String> {
    single(boxes, typ)?.ok_or_else(|| format!("no {} box", name(typ)))
}

pub(super) fn parse(data: &[u8]) -> Result<Heif, String> {
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

pub(super) fn parse_iloc(data: &[u8], b: &BoxRef) -> Result<Iloc, String> {
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
    if item_count as usize > MAX_ITEMS {
        return Err("too many iloc items".into());
    }
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
        // Exactly what this item may still add under the cap: no growth slack per item.
        let mut extents =
            Vec::with_capacity(usize::from(extent_count).min(MAX_EXTENTS - extents_total));
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

pub(super) fn parse_ipma(data: &[u8], b: &BoxRef) -> Result<Ipma, String> {
    let (version, flags, mut r) = full_box(data, b)?;
    let wide = flags & 1 != 0;
    let entry_count_at = r.pos;
    let entry_count = r.u32()?;
    if entry_count as usize > MAX_IPMA_ENTRIES {
        return Err("too many ipma entries".into());
    }
    let mut entries = Vec::new();
    let mut associations = 0usize;
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
        associations += usize::from(count);
        if associations > MAX_ASSOCIATIONS {
            return Err("too many ipma associations".into());
        }
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
pub(super) fn item_ids_of_type(
    data: &[u8],
    iinf: &BoxRef,
    typ: &[u8; 4],
) -> Result<Vec<u32>, String> {
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
