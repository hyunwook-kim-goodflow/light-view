//! The primary image's displayed size, read from the boxes alone (no decoding).

use super::parse::{full_box, parse, BoxRef, Reader};

/// The size of the primary image as shown: its `ispe` (the last one, as libheif reads it),
/// then each `clap` (cropped as libheif's decoder crops) and `irot` (an odd number of quarter
/// turns swaps the sides) associated with it, in `ipma` order. `imir` keeps the size. EXIF
/// Orientation is not applied (HEIF transforms are what the app and the thumbnails apply).
/// Read-only; the input is untrusted. The primary item's associations are walked one at a
/// time, nothing is collected per association.
pub fn display_size(heif: &[u8]) -> Result<(u32, u32), String> {
    let file = parse(heif)?;
    // `ispe` is descriptive, so its place among the associations is free.
    let mut ispe = None;
    for prop in file.walk_props(file.primary) {
        let prop = prop?;
        if &prop.typ == b"ispe" {
            ispe = Some(prop);
        }
    }
    let mut size = ispe_size(heif, &ispe.ok_or("HEIF primary item has no ispe")?)?;
    for prop in file.walk_props(file.primary) {
        let prop = prop?;
        match &prop.typ {
            b"clap" => size = clap_size(heif, &prop, size)?,
            b"irot" => match heif.get(prop.body) {
                Some(&angle) if prop.body < prop.end => {
                    if angle & 1 == 1 {
                        size = (size.1, size.0);
                    }
                }
                _ => return Err("irot box is empty".into()),
            },
            _ => {}
        }
    }
    Ok(size)
}

/// `ispe`'s width and height, both nonzero.
fn ispe_size(data: &[u8], ispe: &BoxRef) -> Result<(u32, u32), String> {
    let (_, _, mut r) = full_box(data, ispe)?;
    let size = (r.u32()?, r.u32()?);
    if size.0 == 0 || size.1 == 0 {
        return Err("HEIF ispe is empty".into());
    }
    Ok(size)
}

/// The size `clap` crops a `width` x `height` image to, as libheif 1.23's decoder does
/// (`Box_clap::get_crop`, then clamped to the image). An aperture outside the image or a
/// zero denominator is an error.
fn clap_size(
    data: &[u8],
    clap: &BoxRef,
    (width, height): (u32, u32),
) -> Result<(u32, u32), String> {
    let mut r = Reader::new(data, clap.body..clap.end)?;
    let mut fields = [0u32; 8];
    for f in &mut fields {
        *f = r.u32()?;
    }
    let [w_n, w_d, h_n, h_d, x_n, x_d, y_n, y_d] = fields;
    // The offsets' numerators are signed. libheif rounds the left edge down but the top
    // edge to nearest.
    let width = crop_span(width, (w_n, w_d), (x_n as i32, x_d), Edge::Down)?;
    let height = crop_span(height, (h_n, h_d), (y_n as i32, y_d), Edge::Nearest)?;
    Ok((width, height))
}

/// How libheif rounds the first edge of a crop.
#[derive(Clone, Copy)]
enum Edge {
    /// `Fraction::round_down`: towards zero.
    Down,
    /// `Fraction::round`: half up, then towards zero.
    Nearest,
}

/// The pixels a clean aperture `n / d` with offset `o_n / o_d` keeps on a side of `len`:
/// `first = edge(o + (len - 1)/2 - (n/d - 1)/2)`, `last = round(n/d - 1 + first)`, clamped
/// to `0..len`. Exact (i128, no overflow); libheif's int32 fractions only lose precision on
/// values far beyond real files.
fn crop_span(
    len: u32,
    (n, d): (u32, u32),
    (o_n, o_d): (i32, u32),
    edge: Edge,
) -> Result<u32, String> {
    if d == 0 || o_d == 0 {
        return Err("HEIF clap has a zero denominator".into());
    }
    let (len, n, d, o_n, o_d) = (
        i128::from(len),
        i128::from(n),
        i128::from(d),
        i128::from(o_n),
        i128::from(o_d),
    );
    // first = num / den over the common denominator 2·d·o_d.
    let den = 2 * d * o_d;
    let num = 2 * o_n * d + (len - 1) * d * o_d - (n - d) * o_d;
    // Rust's `/` truncates towards zero, as C++'s does.
    let first = match edge {
        Edge::Down => num / den,
        Edge::Nearest => (2 * num + den) / (2 * den),
    };
    // round(n/d - 1 + first) = round((n - d + first·d) / d).
    let last = (2 * (n - d + first * d) + d) / (2 * d);
    let (first, last) = (first.max(0), last.min(len - 1));
    if last < first {
        return Err("HEIF clap lies outside the image".into());
    }
    u32::try_from(last - first + 1).map_err(|_| "HEIF clap is too large".into())
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

    fn ispe(w: u32, h: u32) -> Vec<u8> {
        let mut p = w.to_be_bytes().to_vec();
        p.extend_from_slice(&h.to_be_bytes());
        full(b"ispe", &p)
    }

    /// `clap` with a clean aperture of `w` x `h` (whole numbers) and offsets `dx`/2, `dy`/2.
    fn clap(w: u32, h: u32, dx: i32, dy: i32) -> Vec<u8> {
        let mut p = Vec::new();
        for v in [w, 1, h, 1] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        p.extend_from_slice(&dx.to_be_bytes());
        p.extend_from_slice(&2u32.to_be_bytes());
        p.extend_from_slice(&dy.to_be_bytes());
        p.extend_from_slice(&2u32.to_be_bytes());
        bx(b"clap", &p)
    }

    /// A HEIF with item 1 primary and `props` associated with it in this order.
    fn heif(props: &[Vec<u8>]) -> Vec<u8> {
        let mut hdlr = vec![0; 4];
        hdlr.extend_from_slice(b"pict");
        hdlr.extend_from_slice(&[0; 13]);
        let mut meta = full(b"hdlr", &hdlr);
        meta.extend(full(b"pitm", &1u16.to_be_bytes()));
        // iloc v0, 4-byte offsets and lengths, item 1 with no extent.
        meta.extend(full(b"iloc", &[0x44, 0, 0, 1, 0, 1, 0, 0, 0, 0]));
        let mut ipma = 1u32.to_be_bytes().to_vec();
        ipma.extend_from_slice(&1u16.to_be_bytes());
        ipma.push(props.len() as u8);
        ipma.extend((1..=props.len() as u8).map(|i| i | 0x80));
        let mut iprp = bx(b"ipco", &props.concat());
        iprp.extend(full(b"ipma", &ipma));
        meta.extend(bx(b"iprp", &iprp));
        let mut out = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        out.extend(full(b"meta", &meta));
        out
    }

    #[test]
    fn samples_report_their_displayed_size() {
        let size = |name| display_size(&sample(name)).unwrap();
        // ispe 452x462 cropped by clap.
        assert_eq!(size("rainbow-451x461.heic"), (451, 461));
        // The same with irot 90 after the clap.
        assert_eq!(size("rainbow-451x461-irot90.heic"), (461, 451));
        // The grid item's own ispe; its tiles are 452x462.
        assert_eq!(size("grid-4032x3024.heic"), (4032, 3024));
        assert_eq!(size("grid-4032x3024-thumbs.heic"), (4032, 3024));
        assert_eq!(size("with-alpha-512x512.heic"), (512, 512));
    }

    #[test]
    fn transforms_apply_in_ipma_order() {
        let irot = |a: u8| bx(b"irot", &[a]);
        let imir = bx(b"imir", &[1]);
        assert_eq!(display_size(&heif(&[ispe(40, 30)])), Ok((40, 30)));
        assert_eq!(display_size(&heif(&[ispe(40, 30), irot(1)])), Ok((30, 40)));
        assert_eq!(display_size(&heif(&[ispe(40, 30), irot(2)])), Ok((40, 30)));
        assert_eq!(display_size(&heif(&[ispe(40, 30), irot(3)])), Ok((30, 40)));
        assert_eq!(display_size(&heif(&[ispe(40, 30), imir])), Ok((40, 30)));
        // Crop then turn, and turn then crop (the crop is of the turned image).
        let crop = clap(20, 10, 0, 0);
        assert_eq!(
            display_size(&heif(&[ispe(40, 30), crop.clone(), irot(1)])),
            Ok((10, 20))
        );
        assert_eq!(
            display_size(&heif(&[ispe(40, 30), irot(1), crop])),
            Ok((20, 10))
        );
    }

    #[test]
    fn clap_crops_like_libheif() {
        // 452x462 to 451x461 shifted by -1/2: the rainbow sample's crop.
        assert_eq!(
            display_size(&heif(&[ispe(452, 462), clap(451, 461, -1, -1)])),
            Ok((451, 461))
        );
        // An aperture larger than the image, or pushed off it, is clamped to the image.
        assert_eq!(
            display_size(&heif(&[ispe(40, 30), clap(100, 100, 0, 0)])),
            Ok((40, 30))
        );
        assert_eq!(
            display_size(&heif(&[ispe(40, 30), clap(20, 10, 40, 30)])),
            Ok((10, 5))
        );
        // A fractional aperture: libheif keeps round(5.6 - 1 + floor(47.2)) - 47 + 1 = 6
        // pixels of 5.6 (49.5 - 2.3 = 47.2; 47 + 4.6 = 51.6 rounds to 52).
        let mut fractional = Vec::new();
        for v in [28u32, 5, 28, 5, 0, 1, 0, 1] {
            fractional.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(
            display_size(&heif(&[ispe(100, 100), bx(b"clap", &fractional)])),
            Ok((6, 6))
        );
        // Off the image entirely, or a zero denominator: an error.
        assert!(display_size(&heif(&[ispe(40, 30), clap(4, 4, 200, 0)])).is_err());
        let mut zero = clap(20, 10, 0, 0);
        zero[12..16].copy_from_slice(&0u32.to_be_bytes());
        assert!(display_size(&heif(&[ispe(40, 30), zero])).is_err());
    }

    #[test]
    fn missing_or_empty_sizes_are_errors() {
        assert!(display_size(&heif(&[])).is_err(), "no ispe");
        assert!(display_size(&heif(&[ispe(0, 30)])).is_err(), "zero width");
        assert!(
            display_size(&heif(&[bx(b"ispe", &[0; 6])])).is_err(),
            "short"
        );
        assert!(display_size(b"not a heif").is_err());
        assert!(display_size(&[]).is_err());
        // With two ispe (on the same item), the last counts, as libheif reads them.
        assert_eq!(
            display_size(&heif(&[ispe(1, 1), ispe(40, 30)])),
            Ok((40, 30))
        );
        // A clap before the ispe still crops the ispe size.
        assert_eq!(
            display_size(&heif(&[clap(20, 10, 0, 0), ispe(40, 30)])),
            Ok((20, 10))
        );
    }

    #[test]
    fn truncated_samples_never_panic() {
        let whole = sample("rainbow-451x461-irot90.heic");
        for len in 0..whole.len().min(2000) {
            let _ = display_size(&whole[..len]);
        }
    }
}
