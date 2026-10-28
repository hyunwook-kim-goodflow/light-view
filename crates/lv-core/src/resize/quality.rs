//! Estimating a JPEG's quality setting from its quantization tables.
//!
//! IJG-style encoders (libjpeg, the `image` crate) scale the standard tables of
//! ITU-T T.81 Annex K by a factor derived from the quality; the estimate is the quality
//! whose scaled tables are closest to the file's. Other encoders (cameras, phones) get the
//! nearest IJG quality, which is what "match original" needs.

use crate::jpeg_markers::{Markers, SOS};

/// Define quantization table.
const DQT: u8 = 0xDB;

/// Annex K luminance table, natural (row-major) order.
#[rustfmt::skip]
const STD_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61,
    12, 12, 14, 19, 26, 58, 60, 55,
    14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62,
    18, 22, 37, 56, 68, 109, 103, 77,
    24, 35, 55, 64, 81, 104, 113, 92,
    49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// Annex K chrominance table, natural order.
#[rustfmt::skip]
const STD_CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99,
    18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// The natural-order index of the k-th value a DQT stores (zigzag order).
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// One quantization table in natural order, and whether it was stored with 16-bit values.
#[derive(Clone, Copy)]
struct Table {
    values: [u16; 64],
    wide: bool,
}

/// The IJG quality (1..=100) whose scaled standard tables are closest to `jpeg`'s
/// quantization tables (table 0 against the luminance table, table 1 if present against
/// the chrominance table; the sum of absolute differences, ties to the higher quality), or
/// `None` without a readable table 0 before the first scan.
pub fn estimate_jpeg_quality(jpeg: &[u8]) -> Option<u8> {
    let tables = read_tables(jpeg)?;
    let luma = tables[0]?;
    let error = |q: u8| {
        let mut sum = distance(&luma, &STD_LUMA, q);
        if let Some(chroma) = tables[1] {
            sum += distance(&chroma, &STD_CHROMA, q);
        }
        sum
    };
    // `min_by_key` keeps the first minimum: walking down prefers the higher quality.
    (1..=100u8).rev().min_by_key(|&q| error(q))
}

/// The tables (ids 0..=3) defined before the first scan; `None` if the markers are broken.
fn read_tables(jpeg: &[u8]) -> Option<[Option<Table>; 4]> {
    let mut tables = [None; 4];
    for marker in Markers::new(jpeg) {
        let marker = marker.ok()?;
        if marker.code == SOS {
            break;
        }
        if marker.code != DQT {
            continue;
        }
        let mut rest = marker.payload;
        while let Some((&pq_tq, body)) = rest.split_first() {
            let wide = pq_tq >> 4 == 1;
            let id = usize::from(pq_tq & 0x0F);
            if pq_tq >> 4 > 1 || id > 3 {
                return None;
            }
            let len = if wide { 128 } else { 64 };
            let data = body.get(..len)?;
            let mut values = [0u16; 64];
            for (k, &natural) in ZIGZAG.iter().enumerate() {
                values[natural] = if wide {
                    u16::from_be_bytes([data[2 * k], data[2 * k + 1]])
                } else {
                    u16::from(data[k])
                };
            }
            tables[id] = Some(Table { values, wide });
            rest = &body[len..];
        }
    }
    Some(tables)
}

/// Sum of absolute differences between `table` and `standard` scaled for quality `q` the
/// IJG way (clamped to 255 for an 8-bit table).
fn distance(table: &Table, standard: &[u16; 64], q: u8) -> u64 {
    let q = u32::from(q);
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let max = if table.wide { 32767 } else { 255 };
    table
        .values
        .iter()
        .zip(standard)
        .map(|(&got, &std)| {
            let want = ((u32::from(std) * scale + 50) / 100).clamp(1, max);
            u64::from(want.abs_diff(u32::from(got)))
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize::{encode, OutputFormat};
    use crate::test_fixtures::rgba_noise;

    #[test]
    fn quality_estimate_matches_encoder() {
        let rgba = rgba_noise(64, 48, 5);
        for q in [50u8, 75, 90, 95] {
            let jpeg = encode(&rgba, (64, 48), OutputFormat::Jpeg, q).unwrap();
            let got = estimate_jpeg_quality(&jpeg).expect("an estimate");
            assert!(got.abs_diff(q) <= 2, "quality {q} estimated as {got}");
        }
    }

    #[test]
    fn not_a_jpeg_gives_none() {
        assert_eq!(estimate_jpeg_quality(b"not a jpeg"), None);
        assert_eq!(estimate_jpeg_quality(&[0xFF, 0xD8, 0xFF, 0xD9]), None);
    }
}
