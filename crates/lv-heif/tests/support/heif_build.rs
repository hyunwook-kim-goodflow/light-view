//! Builds small HEIF files for the thumbnail tests without an HEVC encoder: the coded
//! images (`hvcC`, `ispe`, `clap`, `colr` and the HEVC data) are copied out of the samples
//! in `testdata/heif/`, and new files are muxed around them (grids of one tile repeated,
//! `thmb` thumbnails, `irot` and `imir` properties). Test code: it panics on bad input.

#![allow(dead_code)]

use std::ops::Range;

/// One HEVC-coded image copied from a sample.
#[derive(Clone)]
pub struct Coded {
    /// The whole `hvcC` box.
    pub hvcc: Vec<u8>,
    /// Coded size (`ispe`).
    pub size: (u32, u32),
    /// The whole `clap` box, if the sample's item has one.
    pub clap: Option<Vec<u8>>,
    /// The whole `colr` box, if the sample's item has one.
    pub colr: Option<Vec<u8>>,
    /// The HEVC data (all `iloc` extents, in order).
    pub data: Vec<u8>,
}

pub fn sample(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/heif")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The primary item of `rainbow-451x461.heic`: 452x462 coded, `clap` to 451x461, ICC.
pub fn rainbow() -> Coded {
    primary_of(&sample("rainbow-451x461.heic"))
}

/// The primary (colour) item of `with-alpha-512x512.heic`, without its alpha image.
pub fn alpha_sample_colour() -> Coded {
    primary_of(&sample("with-alpha-512x512.heic"))
}

// ---------------------------------------------------------------------------------------
// Reading the samples.

#[derive(Clone, Copy)]
struct Bx {
    typ: [u8; 4],
    start: usize,
    body: usize,
    end: usize,
}

fn be(data: &[u8], at: usize, n: usize) -> u64 {
    data[at..at + n]
        .iter()
        .fold(0, |acc, &b| (acc << 8) | u64::from(b))
}

fn boxes(data: &[u8], range: Range<usize>) -> Vec<Bx> {
    let mut out = Vec::new();
    let mut pos = range.start;
    while pos + 8 <= range.end {
        let size = be(data, pos, 4) as usize;
        let typ: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let (body, end) = match size {
            0 => (pos + 8, range.end),
            1 => (pos + 16, pos + be(data, pos + 8, 8) as usize),
            n => (pos + 8, pos + n),
        };
        out.push(Bx {
            typ,
            start: pos,
            body,
            end,
        });
        pos = end;
    }
    out
}

fn find(list: &[Bx], typ: &[u8; 4]) -> Bx {
    *list
        .iter()
        .find(|b| &b.typ == typ)
        .unwrap_or_else(|| panic!("no {} box", String::from_utf8_lossy(typ)))
}

fn primary_of(file: &[u8]) -> Coded {
    let meta = find(&boxes(file, 0..file.len()), b"meta");
    let in_meta = boxes(file, meta.body + 4..meta.end);
    let pitm = find(&in_meta, b"pitm");
    assert_eq!(file[pitm.body], 0, "pitm version");
    let primary = be(file, pitm.body + 4, 2) as u32;

    let iprp = find(&in_meta, b"iprp");
    let in_iprp = boxes(file, iprp.body..iprp.end);
    let props = boxes(
        file,
        find(&in_iprp, b"ipco").body..find(&in_iprp, b"ipco").end,
    );
    let mut coded = Coded {
        hvcc: Vec::new(),
        size: (0, 0),
        clap: None,
        colr: None,
        data: item_data(file, &find(&in_meta, b"iloc"), primary),
    };
    for index in ipma_indices(file, &find(&in_iprp, b"ipma"), primary) {
        let p = props[index - 1];
        let whole = file[p.start..p.end].to_vec();
        match &p.typ {
            b"hvcC" => coded.hvcc = whole,
            b"ispe" => {
                coded.size = (
                    be(file, p.body + 4, 4) as u32,
                    be(file, p.body + 8, 4) as u32,
                )
            }
            b"clap" => coded.clap = Some(whole),
            b"colr" => coded.colr = Some(whole),
            _ => {}
        }
    }
    assert!(
        !coded.hvcc.is_empty() && coded.size.0 > 0,
        "not an HEVC item"
    );
    coded
}

fn ipma_indices(file: &[u8], ipma: &Bx, item: u32) -> Vec<usize> {
    let version = file[ipma.body];
    let wide = file[ipma.body + 3] & 1 == 1;
    let mut pos = ipma.body + 4;
    let count = be(file, pos, 4);
    pos += 4;
    let mut out = Vec::new();
    for _ in 0..count {
        let id_len = if version < 1 { 2 } else { 4 };
        let id = be(file, pos, id_len) as u32;
        pos += id_len;
        let n = file[pos];
        pos += 1;
        for _ in 0..n {
            let index = if wide {
                let v = be(file, pos, 2) as usize & 0x7fff;
                pos += 2;
                v
            } else {
                let v = usize::from(file[pos] & 0x7f);
                pos += 1;
                v
            };
            if id == item && index != 0 {
                out.push(index);
            }
        }
    }
    out
}

fn item_data(file: &[u8], iloc: &Bx, item: u32) -> Vec<u8> {
    let version = file[iloc.body];
    let mut pos = iloc.body + 4;
    let (offset_size, length_size) = (usize::from(file[pos] >> 4), usize::from(file[pos] & 15));
    let (base_size, index_size) = (
        usize::from(file[pos + 1] >> 4),
        if version >= 1 {
            usize::from(file[pos + 1] & 15)
        } else {
            0
        },
    );
    pos += 2;
    let id_len = if version < 2 { 2 } else { 4 };
    let count = be(file, pos, id_len);
    pos += id_len;
    for _ in 0..count {
        let id = be(file, pos, id_len) as u32;
        pos += id_len;
        if version >= 1 {
            assert_eq!(be(file, pos, 2) & 15, 0, "only file-offset items");
            pos += 2;
        }
        pos += 2; // data_reference_index
        let base = be(file, pos, base_size) as usize;
        pos += base_size;
        let extents = be(file, pos, 2);
        pos += 2;
        let mut data = Vec::new();
        for _ in 0..extents {
            pos += index_size;
            let offset = be(file, pos, offset_size) as usize;
            pos += offset_size;
            let length = be(file, pos, length_size) as usize;
            pos += length_size;
            data.extend_from_slice(&file[base + offset..base + offset + length]);
        }
        if id == item {
            return data;
        }
    }
    panic!("item {item} not in iloc")
}

// ---------------------------------------------------------------------------------------
// Writing new files.

pub fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&(8 + payload.len() as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(payload);
    out
}

pub fn full(typ: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![version];
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(payload);
    bx(typ, &p)
}

pub fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut p = w.to_be_bytes().to_vec();
    p.extend_from_slice(&h.to_be_bytes());
    full(b"ispe", 0, 0, &p)
}

/// `irot`: `ccw` anti-clockwise quarter turns.
pub fn irot(ccw: u8) -> Vec<u8> {
    bx(b"irot", &[ccw & 3])
}

/// `imir` with the given `axis` bit.
pub fn imir(axis: u8) -> Vec<u8> {
    bx(b"imir", &[axis & 1])
}

struct Item {
    id: u32,
    typ: [u8; 4],
    data: Range<usize>,
    props: Vec<(u16, bool)>,
}

/// A HEIF file under construction. Item ids start at 1, property indices at 1.
#[derive(Default)]
pub struct Builder {
    props: Vec<Vec<u8>>,
    items: Vec<Item>,
    refs: Vec<([u8; 4], u32, Vec<u32>)>,
    primary: u32,
    mdat: Vec<u8>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The index of `prop` in `ipco`, added once.
    fn prop(&mut self, prop: Vec<u8>) -> u16 {
        let at = match self.props.iter().position(|p| *p == prop) {
            Some(i) => i,
            None => {
                self.props.push(prop);
                self.props.len() - 1
            }
        };
        u16::try_from(at + 1).unwrap()
    }

    /// `bytes` in `mdat`, stored once.
    fn data(&mut self, bytes: &[u8]) -> Range<usize> {
        if let Some(at) = self
            .mdat
            .windows(bytes.len().max(1))
            .position(|w| w == bytes)
        {
            return at..at + bytes.len();
        }
        let start = self.mdat.len();
        self.mdat.extend_from_slice(bytes);
        start..self.mdat.len()
    }

    fn item(&mut self, typ: &[u8; 4], data: &[u8], props: Vec<(Vec<u8>, bool)>) -> u32 {
        let id = self.items.len() as u32 + 1;
        let data = self.data(data);
        let props = props
            .into_iter()
            .map(|(p, essential)| (self.prop(p), essential))
            .collect();
        self.items.push(Item {
            id,
            typ: *typ,
            data,
            props,
        });
        id
    }

    /// An `hvc1` item for `coded`: `hvcC`, `ispe`, `colr`, its `clap` when `clap` is set,
    /// then `transforms` (whole `irot`/`imir` boxes) in that order.
    pub fn image(&mut self, coded: &Coded, clap: bool, transforms: &[Vec<u8>]) -> u32 {
        let mut props = vec![
            (coded.hvcc.clone(), true),
            (ispe(coded.size.0, coded.size.1), false),
        ];
        if let Some(colr) = &coded.colr {
            props.push((colr.clone(), false));
        }
        if clap {
            props.push((coded.clap.clone().expect("sample has no clap"), true));
        }
        props.extend(transforms.iter().map(|t| (t.clone(), true)));
        self.item(b"hvc1", &coded.data, props)
    }

    /// A `grid` item of `cols` x `rows` copies of `tile` (no `clap` on the tiles), cropped
    /// to `width` x `height`, with the tile's `colr` and then `transforms`.
    pub fn grid(
        &mut self,
        tile: &Coded,
        (cols, rows): (u8, u8),
        (width, height): (u16, u16),
        transforms: &[Vec<u8>],
    ) -> u32 {
        let tiles: Vec<u32> = (0..u32::from(cols) * u32::from(rows))
            .map(|_| self.image(tile, false, &[]))
            .collect();
        let mut descriptor = vec![0, 0, rows - 1, cols - 1];
        descriptor.extend_from_slice(&width.to_be_bytes());
        descriptor.extend_from_slice(&height.to_be_bytes());
        let mut props = vec![(ispe(width.into(), height.into()), false)];
        if let Some(colr) = &tile.colr {
            props.push((colr.clone(), false));
        }
        props.extend(transforms.iter().map(|t| (t.clone(), true)));
        let grid = self.item(b"grid", &descriptor, props);
        self.refs.push((*b"dimg", grid, tiles));
        grid
    }

    /// An `Exif` item describing `of` (`cdsc`): `tiff` (a TIFF block, no `Exif\0\0` prefix)
    /// behind a zero `exif_tiff_header_offset`. Its data goes after what `mdat` holds so far.
    pub fn exif(&mut self, of: u32, tiff: &[u8]) -> u32 {
        let mut data = vec![0u8; 4];
        data.extend_from_slice(tiff);
        let id = self.item(b"Exif", &data, Vec::new());
        self.refs.push((*b"cdsc", id, vec![of]));
        id
    }

    /// Makes `thumb` a `thmb` thumbnail of `of`.
    pub fn thumbnail(&mut self, thumb: u32, of: u32) {
        self.refs.push((*b"thmb", thumb, vec![of]));
    }

    pub fn primary(&mut self, id: u32) {
        self.primary = id;
    }

    pub fn finish(&self) -> Vec<u8> {
        assert!(self.primary != 0, "no primary item");
        let mut ftyp = b"heic".to_vec();
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"mif1heic");
        let ftyp = bx(b"ftyp", &ftyp);
        // The meta box's size does not depend on the offsets in iloc.
        let probe = self.meta(0);
        let mdat_body = ftyp.len() + probe.len() + 8;
        let meta = self.meta(mdat_body);
        assert_eq!(meta.len(), probe.len());
        let mut out = ftyp;
        out.extend_from_slice(&meta);
        out.extend_from_slice(&bx(b"mdat", &self.mdat));
        out
    }

    fn meta(&self, mdat_body: usize) -> Vec<u8> {
        let mut hdlr = vec![0; 4];
        hdlr.extend_from_slice(b"pict");
        hdlr.extend_from_slice(&[0; 13]);
        let mut body = full(b"hdlr", 0, 0, &hdlr);
        body.extend(full(b"pitm", 0, 0, &(self.primary as u16).to_be_bytes()));

        let mut iinf = (self.items.len() as u16).to_be_bytes().to_vec();
        for item in &self.items {
            let mut infe = (item.id as u16).to_be_bytes().to_vec();
            infe.extend_from_slice(&[0, 0]);
            infe.extend_from_slice(&item.typ);
            infe.push(0);
            iinf.extend(full(b"infe", 2, 0, &infe));
        }
        body.extend(full(b"iinf", 0, 0, &iinf));

        let mut iref = Vec::new();
        for (typ, from, to) in &self.refs {
            let mut p = (*from as u16).to_be_bytes().to_vec();
            p.extend_from_slice(&(to.len() as u16).to_be_bytes());
            for id in to {
                p.extend_from_slice(&(*id as u16).to_be_bytes());
            }
            iref.extend(bx(typ, &p));
        }
        if !iref.is_empty() {
            body.extend(full(b"iref", 0, 0, &iref));
        }

        let ipco = bx(b"ipco", &self.props.concat());
        let mut ipma = (self.items.len() as u32).to_be_bytes().to_vec();
        for item in &self.items {
            ipma.extend_from_slice(&(item.id as u16).to_be_bytes());
            ipma.push(item.props.len() as u8);
            for &(index, essential) in &item.props {
                assert!(index < 128);
                ipma.push(index as u8 | if essential { 0x80 } else { 0 });
            }
        }
        let mut iprp = ipco;
        iprp.extend(full(b"ipma", 0, 0, &ipma));
        body.extend(bx(b"iprp", &iprp));

        // iloc version 0: 4-byte offsets and lengths, no base offset, one extent each.
        let mut iloc = vec![0x44, 0x00];
        iloc.extend_from_slice(&(self.items.len() as u16).to_be_bytes());
        for item in &self.items {
            iloc.extend_from_slice(&(item.id as u16).to_be_bytes());
            iloc.extend_from_slice(&[0, 0, 0, 1]);
            iloc.extend_from_slice(&((mdat_body + item.data.start) as u32).to_be_bytes());
            iloc.extend_from_slice(&(item.data.len() as u32).to_be_bytes());
        }
        body.extend(full(b"iloc", 0, 0, &iloc));
        full(b"meta", 0, 0, &body)
    }
}
