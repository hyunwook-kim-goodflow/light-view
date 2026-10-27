//! The one JPEG marker walker behind [`crate::jpeg_extent`], [`crate::jpeg_check`] and
//! [`crate::exif_edit`].
//!
//! [`Markers`] yields the markers after SOI in order, each with its payload, and steps over
//! a scan's entropy-coded data (stuffed `0xFF00` bytes, `RSTn` markers and fill bytes) only
//! when asked for the marker after the SOS, so a caller that stops at the first scan never
//! reads the image data. It ends after EOI. It never panics, checks every length against
//! the input, and reads each byte a bounded number of times, so it runs in linear time.

/// Start of image.
pub(crate) const SOI: u8 = 0xD8;
/// End of image.
pub(crate) const EOI: u8 = 0xD9;
/// Start of scan.
pub(crate) const SOS: u8 = 0xDA;
pub(crate) const APP0: u8 = 0xE0;
pub(crate) const APP1: u8 = 0xE1;
pub(crate) const APP2: u8 = 0xE2;

/// The input is not a JPEG marker sequence (or ends before the next marker does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Malformed;

/// One marker. For a marker without a length (EOI, TEM, `RSTn`) `payload` is empty and
/// `end` is just past the marker code.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Marker<'a> {
    pub code: u8,
    /// The segment's bytes after its 2-byte length field.
    pub payload: &'a [u8],
    /// Offset just past the segment (for SOS: past its header, before the entropy data).
    pub end: usize,
}

/// Whether `code` is a frame header (SOF0..SOF15 except DHT, JPG and DAC).
pub(crate) fn is_frame_header(code: u8) -> bool {
    (0xC0..=0xCF).contains(&code) && !matches!(code, 0xC4 | 0xC8 | 0xCC)
}

pub(crate) struct Markers<'a> {
    data: &'a [u8],
    /// Where the next marker is expected.
    at: usize,
    /// The last marker was SOS: entropy data comes first.
    in_scan: bool,
    /// Past EOI or an error: yields nothing more.
    done: bool,
}

impl<'a> Markers<'a> {
    /// Walks `data`, which must start with SOI; if it does not, the first item is an error.
    pub(crate) fn new(data: &'a [u8]) -> Markers<'a> {
        Markers {
            data,
            at: 2,
            in_scan: false,
            done: false,
        }
    }

    fn next_marker(&mut self) -> Result<Marker<'a>, Malformed> {
        let data = self.data;
        if self.at == 2 && data.get(..2) != Some(&[0xFF, SOI][..]) {
            return Err(Malformed);
        }
        let mut i = self.at;
        if self.in_scan {
            i = skip_entropy_data(data, i).ok_or(Malformed)?;
            self.in_scan = false;
        }
        if *data.get(i).ok_or(Malformed)? != 0xFF {
            return Err(Malformed);
        }
        // Fill bytes before a marker code.
        while *data.get(i + 1).ok_or(Malformed)? == 0xFF {
            i += 1;
        }
        let code = data[i + 1];
        i += 2;
        match code {
            // Standalone markers: EOI, TEM, RST0..7.
            EOI | 0x01 | 0xD0..=0xD7 => {
                self.at = i;
                return Ok(Marker {
                    code,
                    payload: &[],
                    end: i,
                });
            }
            // A stuffed zero or a second SOI is not a marker here.
            0x00 | SOI => return Err(Malformed),
            _ => {}
        }
        let len_bytes = data.get(i..i + 2).ok_or(Malformed)?;
        let len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
        if len < 2 {
            return Err(Malformed);
        }
        let payload = data.get(i + 2..i + len).ok_or(Malformed)?;
        self.at = i + len;
        self.in_scan = code == SOS;
        Ok(Marker {
            code,
            payload,
            end: self.at,
        })
    }
}

impl<'a> Iterator for Markers<'a> {
    type Item = Result<Marker<'a>, Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.next_marker();
        self.done = !matches!(item, Ok(Marker { code, .. }) if code != EOI);
        Some(item)
    }
}

/// From the start of a scan's entropy-coded data, the offset of the next real marker (its
/// `0xFF`), or `None` if the data ends first.
fn skip_entropy_data(data: &[u8], mut i: usize) -> Option<usize> {
    loop {
        i += data.get(i..)?.iter().position(|&b| b == 0xFF)?;
        match *data.get(i + 1)? {
            // Stuffed 0xFF data byte, or a restart marker inside the scan.
            0x00 | 0xD0..=0xD7 => i += 2,
            // A fill byte: the marker starts at the next 0xFF.
            0xFF => i += 1,
            _ => return Some(i),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn segment(code: u8, payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, code];
        s.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        s.extend_from_slice(payload);
        s
    }

    #[test]
    fn walks_segments_scans_and_stops_after_eoi() {
        let mut j = vec![0xFF, SOI];
        j.extend(segment(APP0, b"JFIF\0"));
        j.extend_from_slice(&[0xFF, 0xFF]); // fill bytes
        j.extend(segment(0xC0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]));
        j.extend(segment(SOS, &[1, 1, 0, 0, 63, 0]));
        j.extend_from_slice(&[0x12, 0xFF, 0x00, 0xFF, 0xD3, 0xFF, 0xFF, EOI, 0xAA]);
        let codes: Vec<u8> = Markers::new(&j).map(|m| m.unwrap().code).collect();
        assert_eq!(codes, [APP0, 0xC0, SOS, EOI]);
        let eoi = Markers::new(&j).last().unwrap().unwrap();
        assert_eq!(eoi.end, j.len() - 1);
        // Stopping at SOS reads nothing of a broken scan.
        let mut cut = j.clone();
        cut.truncate(cut.len() - 4);
        let mut walk = Markers::new(&cut);
        assert!(walk.by_ref().take(3).all(|m| m.is_ok()));
        assert_eq!(walk.next().unwrap().unwrap_err(), Malformed);
        assert!(walk.next().is_none());
    }
}
