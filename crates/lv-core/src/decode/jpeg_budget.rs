//! The check every JPEG passes before it is decoded, on the full and the reduced-size
//! path alike (rulings R9–R11).
//!
//! JPEG decoders do work for every block of every scan, however little data the scan
//! has (they zero-fill what is missing, and progressive AC scans can skip any number of
//! blocks with one end-of-band run), and they cannot be cancelled. So a small file whose
//! headers promise a huge image, or many scans, keeps a decoding thread busy for minutes.
//! Before decoding, a frame is refused as `Corrupt` when:
//! - a sequential frame codes a component in two scans;
//! - a progressive frame has more than [`MAX_PROGRESSIVE_SCANS`] scans;
//! - a scan that codes every block of its components (any sequential scan, a progressive
//!   DC-first scan) has fewer bits of entropy-coded data than it has blocks (every block
//!   takes at least one bit);
//! - the coefficients its scans code, block by block (64 per sequential scan, `Se−Ss+1`
//!   per progressive one), add up to more than [`WORK_FACTOR`] full decodes of the frame.
//!   libjpeg's default progressive script codes at most 191 per block (gray), under 192.
//!
//! Markers are found the way decoders find them after a damaged segment: any `FF xx`
//! that is not a stuffed `FF00`, a fill byte or `RSTn`, so no scan a decoder would see is
//! missed. Segments are stepped over by their length, scans end at the next marker, and
//! the walk ends at EOI.

use super::{corrupt, DecodeError};
use crate::jpeg_markers::{is_frame_header, EOI, SOS};

/// The most scans a progressive frame may have (R10).
pub(super) const MAX_PROGRESSIVE_SCANS: usize = 64;

/// How many full decodes' worth of coefficients a frame's scans may code (R11).
pub(super) const WORK_FACTOR: u64 = 3;

/// A checked SOF0, SOF1 or SOF2 frame.
#[derive(Debug, Clone, Copy)]
pub(super) struct JpegFrame {
    pub progressive: bool,
    /// Every component's blocks over the MCU-padded image.
    pub blocks: u64,
}

/// Checks the JPEG in `bytes` (which starts with SOI). `Ok(None)` for a frame other than
/// SOF0, SOF1 or SOF2, which is left to the decoder; `Corrupt` as the module says, or
/// when the frame header is missing or malformed, there is no scan, or a scan header is
/// malformed or names a component the frame lacks.
pub(super) fn check(bytes: &[u8]) -> Result<Option<JpegFrame>, DecodeError> {
    let bad = |what: &str| Err(corrupt(what));
    let mut frame: Option<(bool, Sof)> = None;
    let mut seen: Vec<u8> = Vec::new();
    let (mut scans, mut work) = (0usize, 0u64);
    let mut at = 2;
    while let Some((pos, code)) = next_marker(bytes, at) {
        at = pos + 2;
        match code {
            EOI => break,
            // Standalone markers: SOI, TEM, RSTn (outside a scan).
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            _ => {}
        }
        let Some(len) = bytes.get(at..at + 2) else {
            break;
        };
        let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
        if len < 2 {
            continue;
        }
        let Some(payload) = bytes.get(at + 2..at + len) else {
            break;
        };
        at += len;
        if frame.is_none() && is_frame_header(code) {
            let progressive = match code {
                0xC0 | 0xC1 => false,
                0xC2 => true,
                _ => return Ok(None),
            };
            let Some(sof) = Sof::parse(payload) else {
                return bad("bad frame header");
            };
            frame = Some((progressive, sof));
            continue;
        }
        let (Some((progressive, sof)), SOS) = (&frame, code) else {
            continue;
        };
        scans += 1;
        if *progressive && scans > MAX_PROGRESSIVE_SCANS {
            return bad("too many scans");
        }
        let Some(header) = ScanHeader::parse(payload) else {
            return bad("bad scan header");
        };
        if !*progressive {
            if header.ids.iter().any(|id| seen.contains(id)) {
                return bad("a component in two scans");
            }
            seen.extend_from_slice(&header.ids);
        }
        let Some(blocks) = sof.scan_blocks(&header.ids) else {
            return bad("scan of an unknown component");
        };
        let codes_every_block = !*progressive || header.dc_first;
        if codes_every_block && scan_data_len(bytes, at).saturating_mul(8) < blocks {
            return bad("too little data for the image size");
        }
        let coefficients = if *progressive {
            header.coefficients
        } else {
            64
        };
        work = work.saturating_add(blocks.saturating_mul(coefficients));
    }
    let Some((progressive, sof)) = frame else {
        return bad("no frame header");
    };
    if scans == 0 {
        return bad("no scan");
    }
    let blocks = sof.blocks();
    if work > WORK_FACTOR.saturating_mul(64).saturating_mul(blocks) {
        return bad("too much work for the image size");
    }
    Ok(Some(JpegFrame {
        progressive,
        blocks,
    }))
}

/// The next marker at or after `from`: its offset (of the `FF`) and code. Skips stuffed
/// `FF00`, fill bytes and `RSTn`.
fn next_marker(bytes: &[u8], mut from: usize) -> Option<(usize, u8)> {
    loop {
        let i = from + bytes.get(from..)?.iter().position(|&b| b == 0xFF)?;
        match *bytes.get(i + 1)? {
            0xFF => from = i + 1,
            0x00 | 0xD0..=0xD7 => from = i + 2,
            code => return Some((i, code)),
        }
    }
}

/// The bytes of a scan's entropy-coded data, from its header's end (`start`) to the next
/// marker (or the end of the file).
pub(super) fn scan_data_len(bytes: &[u8], start: usize) -> u64 {
    let end = next_marker(bytes, start).map_or(bytes.len(), |(i, _)| i);
    end.saturating_sub(start) as u64
}

/// A frame header: its size and each component's id and sampling factors.
pub(super) struct Sof {
    width: u64,
    height: u64,
    /// (id, horizontal, vertical), every factor at least 1.
    components: Vec<(u8, u64, u64)>,
    h_max: u64,
    v_max: u64,
}

impl Sof {
    /// `None` if the SOF payload is malformed.
    pub(super) fn parse(sof: &[u8]) -> Option<Sof> {
        let height = u64::from(u16::from_be_bytes([*sof.get(1)?, *sof.get(2)?]));
        let width = u64::from(u16::from_be_bytes([*sof.get(3)?, *sof.get(4)?]));
        let count = usize::from(*sof.get(5)?);
        let components: Vec<(u8, u64, u64)> = sof
            .get(6..6 + 3 * count)?
            .chunks_exact(3)
            .map(|c| (c[0], u64::from(c[1] >> 4), u64::from(c[1] & 0xF)))
            .collect();
        if components.iter().any(|c| c.1 == 0 || c.2 == 0) {
            return None;
        }
        Some(Sof {
            width,
            height,
            h_max: components.iter().map(|c| c.1).max()?,
            v_max: components.iter().map(|c| c.2).max()?,
            components,
        })
    }

    fn mcus(&self) -> (u64, u64) {
        (
            self.width.div_ceil(8 * self.h_max),
            self.height.div_ceil(8 * self.v_max),
        )
    }

    /// Every component's blocks over the MCU-padded image.
    pub(super) fn blocks(&self) -> u64 {
        let (mx, my) = self.mcus();
        self.components.iter().map(|c| mx * c.1 * my * c.2).sum()
    }

    /// The blocks a scan of these components codes: MCU-padded when interleaved, the
    /// component's own (unpadded) blocks when alone. `None` for an unknown component.
    pub(super) fn scan_blocks(&self, ids: &[u8]) -> Option<u64> {
        let find = |id: &u8| self.components.iter().find(|c| c.0 == *id);
        if let [id] = ids {
            let (_, h, v) = find(id)?;
            let w = (self.width * h).div_ceil(self.h_max);
            let ht = (self.height * v).div_ceil(self.v_max);
            return Some(w.div_ceil(8) * ht.div_ceil(8));
        }
        let (mx, my) = self.mcus();
        ids.iter()
            .map(|id| find(id).map(|c| mx * c.1 * my * c.2))
            .sum()
    }
}

/// A scan header: its components, whether it is a DC-first scan (Ss = 0, Ah = 0), and
/// how many coefficients of each block it codes (`Se − Ss + 1`).
struct ScanHeader {
    ids: Vec<u8>,
    dc_first: bool,
    coefficients: u64,
}

impl ScanHeader {
    /// `None` if the SOS payload is malformed, names no component, or its spectral
    /// range is not within 0..=63.
    fn parse(sos: &[u8]) -> Option<ScanHeader> {
        let count = usize::from(*sos.first()?);
        let ids: Vec<u8> = sos
            .get(1..1 + 2 * count)?
            .iter()
            .step_by(2)
            .copied()
            .collect();
        let ss = *sos.get(1 + 2 * count)?;
        let se = *sos.get(2 + 2 * count)?;
        let ah = *sos.get(3 + 2 * count)? >> 4;
        if ids.is_empty() || ss > se || se > 63 {
            return None;
        }
        Some(ScanHeader {
            ids,
            dc_first: ss == 0 && ah == 0,
            coefficients: u64::from(se - ss) + 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::scaled_jpeg::tests::{sof, sos};

    /// One scan of a script: component ids, Ss, Se.
    type Scan = (&'static [u8], u8, u8);
    /// Each component's sampling factors.
    type Sampling = &'static [(u8, u8)];

    /// The work of a scan script over `sof`.
    fn script_work(sof: &Sof, script: &[Scan]) -> u64 {
        script
            .iter()
            .map(|(ids, ss, se)| sof.scan_blocks(ids).unwrap() * u64::from(se - ss + 1))
            .sum()
    }

    /// libjpeg's `jpeg_simple_progression` for YCbCr (ids 1, 2, 3).
    const YCBCR_SCRIPT: [Scan; 10] = [
        (&[1, 2, 3], 0, 0),
        (&[1], 1, 5),
        (&[3], 1, 63),
        (&[2], 1, 63),
        (&[1], 6, 63),
        (&[1], 1, 63),
        (&[1, 2, 3], 0, 0),
        (&[3], 1, 63),
        (&[2], 1, 63),
        (&[1], 1, 63),
    ];

    /// The same for one component.
    const GRAY_SCRIPT: [Scan; 6] = [
        (&[1], 0, 0),
        (&[1], 1, 5),
        (&[1], 6, 63),
        (&[1], 1, 63),
        (&[1], 0, 0),
        (&[1], 1, 63),
    ];

    #[test]
    fn libjpeg_default_progressive_scripts_are_within_the_budget() {
        let cases: [(Sampling, &[Scan]); 3] = [
            (&[(2, 2), (1, 1), (1, 1)], &YCBCR_SCRIPT),
            (&[(1, 1), (1, 1), (1, 1)], &YCBCR_SCRIPT),
            (&[(1, 1)], &GRAY_SCRIPT),
        ];
        for (sampling, script) in cases {
            // Odd sizes too, where MCU padding differs from a component's own blocks.
            for (w, h) in [(4000, 3000), (4001, 2999), (23168, 23168), (17, 9)] {
                let s = Sof::parse(&sof(w, h, sampling)).unwrap();
                let work = script_work(&s, script);
                let budget = WORK_FACTOR * 64 * s.blocks();
                assert!(work <= budget, "{sampling:?} {w}x{h}: {work} > {budget}");
            }
        }
    }

    #[test]
    fn markers_are_found_past_damage() {
        let seg = crate::jpeg_markers::tests::segment;
        let mut b = vec![0xFF, 0xD8];
        b.extend(seg(0xC0, &sof(64, 64, &[(1, 1)])));
        // Garbage between segments that a strict walker would stop at.
        b.extend([0x12, 0x34]);
        for _ in 0..2 {
            b.extend(seg(SOS, &sos(&[1], 0, 63, 0)));
            b.extend([0x55; 8]);
        }
        b.extend([0xFF, 0xD9]);
        assert!(matches!(check(&b), Err(DecodeError::Corrupt(_))));
        // The marker search skips stuffing, fill bytes and restarts.
        assert_eq!(
            next_marker(&[1, 0xFF, 0x00, 0xFF, 0xFF, 0xD3, 0xFF, 0xC4], 0),
            Some((6, 0xC4))
        );
        assert_eq!(next_marker(&[0xFF, 0x00, 0xFF], 0), None);
    }

    #[test]
    fn work_over_the_budget_is_corrupt() {
        let seg = crate::jpeg_markers::tests::segment;
        // Gray 64x64 progressive: 64 blocks. DC-first, then AC 1..63 scans.
        let file = |ac_scans: usize| {
            let mut b = vec![0xFF, 0xD8];
            b.extend(seg(0xC2, &sof(64, 64, &[(1, 1)])));
            b.extend(seg(SOS, &sos(&[1], 0, 0, 0)));
            b.extend([0x55; 8]);
            for _ in 0..ac_scans {
                b.extend(seg(SOS, &sos(&[1], 1, 63, 0)));
                b.push(0x55);
            }
            b.extend([0xFF, 0xD9]);
            b
        };
        // 1 + 3 * 63 coefficients per block is within 3 * 64; one more scan is not.
        assert!(check(&file(3)).unwrap().is_some());
        assert!(matches!(check(&file(4)), Err(DecodeError::Corrupt(_))));
        // A spectral range out of order or past 63 is a bad scan header.
        let mut b = vec![0xFF, 0xD8];
        b.extend(seg(0xC2, &sof(64, 64, &[(1, 1)])));
        b.extend(seg(SOS, &sos(&[1], 5, 4, 0)));
        b.extend([0xFF, 0xD9]);
        assert!(matches!(check(&b), Err(DecodeError::Corrupt(_))));
    }

    #[test]
    fn other_frame_kinds_are_left_to_the_decoder() {
        let seg = crate::jpeg_markers::tests::segment;
        let mut b = vec![0xFF, 0xD8];
        b.extend(seg(0xC3, &sof(64, 64, &[(1, 1)])));
        b.extend([0xFF, 0xD9]);
        assert!(matches!(check(&b), Ok(None)));
    }
}
