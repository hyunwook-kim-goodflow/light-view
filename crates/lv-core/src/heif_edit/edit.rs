//! Writing a rotation: the byte edit (patches, insertions, grown sizes, moved `iloc`
//! offsets), adding an `irot`, and the self-check of every result.

use super::parse::{name, parse, BoxRef, Field, Heif, SizeField};
use super::{extent_range, irot_angle, primary_irot, OVERFLOW, SHIFTABLE_TOP_LEVEL};

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
pub(super) fn add_irot(
    heif: &[u8],
    file: &Heif,
    targets: &[u32],
    angle: u8,
) -> Result<Vec<u8>, String> {
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
pub(super) fn verify(
    src: &[u8],
    old: &Heif,
    out: &[u8],
    angle: u8,
    added_to: &[u32],
) -> Result<(), String> {
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
