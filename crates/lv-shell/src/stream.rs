//! Reading Explorer's stream with a cap.

use windows::core::Result;
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::System::Com::{IStream, STATFLAG_NONAME, STATSTG};

/// The most bytes any handler reads from Explorer's stream: 64 MiB. iPhone HEIC photos are 1
/// to 10 MB and 48 MP ones up to about 25 MB; a larger file gets no thumbnail, preview or
/// details. Far under the app's HEIC cap (about 1 GiB), which would let one file take
/// gigabytes inside explorer.exe or the search indexer.
pub(crate) const READ_CAP: u64 = 64 * 1024 * 1024;

/// Bytes asked of the stream per read.
const CHUNK: usize = 256 * 1024;

/// The stream's bytes from its current position to its end, or an error when there are more
/// than `cap` (`E_FAIL`, as for a stream that fails). Never allocates more than `cap` bytes, and
/// reads at most one byte past the cap: a stream whose reported size is over the cap is
/// refused before any read.
pub fn read_capped(stream: &IStream, cap: u64) -> Result<Vec<u8>> {
    let size = size_of(stream);
    if size.is_some_and(|size| size > cap) {
        return Err(E_FAIL.into());
    }
    read_capped_with(
        |buf| {
            let mut got = 0u32;
            let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            // SAFETY: `buf` holds at least `len` writable bytes; `got` is a local.
            unsafe { stream.Read(buf.as_mut_ptr().cast(), len, Some(&mut got)) }.ok()?;
            Ok(got as usize)
        },
        cap,
        size,
    )
}

/// The stream's total size if it tells it (a hint: the read loop trusts only what it reads).
fn size_of(stream: &IStream) -> Option<u64> {
    let mut stat = STATSTG::default();
    // SAFETY: a valid stream and a local out-struct; NONAME allocates no name to free.
    unsafe { stream.Stat(&mut stat, STATFLAG_NONAME) }.ok()?;
    Some(stat.cbSize)
}

/// The read loop of [`read_capped`]: `read` fills a buffer and says how many bytes it put
/// there (0 at the end). `size_hint` (at most `cap`) is reserved exactly up front; past it the
/// buffer grows by doubling, never beyond `cap`.
fn read_capped_with(
    mut read: impl FnMut(&mut [u8]) -> Result<usize>,
    cap: u64,
    size_hint: Option<u64>,
) -> Result<Vec<u8>> {
    let first = usize::try_from(size_hint.unwrap_or(0).min(cap)).map_err(|_| E_FAIL)?;
    let mut data = Vec::new();
    data.try_reserve_exact(first).map_err(|_| E_FAIL)?;
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let room = cap - data.len() as u64;
        // At the cap, one probe byte tells whether the stream ends there.
        let want = room.clamp(1, CHUNK as u64) as usize;
        let got = read(&mut chunk[..want])?;
        if got == 0 {
            return Ok(data);
        }
        if got > want || room == 0 {
            return Err(E_FAIL.into());
        }
        let need = data.len() + got;
        if need > data.capacity() {
            // `need <= cap` (`got <= room`), so this stays within the cap.
            let grown = (data.capacity() * 2).max(need).min(cap as usize);
            data.try_reserve_exact(grown - data.len())
                .map_err(|_| E_FAIL)?;
        }
        data.extend_from_slice(&chunk[..got]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::STREAM_SEEK_CUR;
    use windows::Win32::UI::Shell::SHCreateMemStream;

    fn mem_stream(bytes: &[u8]) -> IStream {
        // SAFETY: plain call; the stream copies `bytes`.
        unsafe { SHCreateMemStream(Some(bytes)) }.expect("SHCreateMemStream")
    }

    fn position(stream: &IStream) -> u64 {
        let mut pos = 0u64;
        // SAFETY: valid stream and a local out-pointer.
        unsafe { stream.Seek(0, STREAM_SEEK_CUR, Some(&mut pos)) }.expect("Seek");
        pos
    }

    #[test]
    fn reads_a_stream_whole() {
        let bytes: Vec<u8> = (0..700_000u32).map(|i| i as u8).collect();
        let stream = mem_stream(&bytes);
        assert_eq!(
            read_capped(&stream, 700_000).unwrap(),
            bytes,
            "exactly the cap"
        );
        let stream = mem_stream(&bytes);
        assert_eq!(read_capped(&stream, u64::MAX).unwrap(), bytes);
        let empty = mem_stream(&[]);
        assert_eq!(read_capped(&empty, 10).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn stream_over_cap_is_refused() {
        let cap = 300_000u64;
        let stream = mem_stream(&vec![7u8; cap as usize + 1]);
        assert!(read_capped(&stream, cap).is_err());
        assert!(position(&stream) <= cap, "read past the cap");
    }

    /// A stream that never ends and whose size is unknown: the loop stops one byte past the
    /// cap, and never holds more than the cap.
    #[test]
    fn endless_stream_is_refused_at_the_cap() {
        let cap = 1_000_000u64;
        let mut read = 0u64;
        let result = read_capped_with(
            |buf| {
                buf.fill(1);
                read += buf.len() as u64;
                Ok(buf.len())
            },
            cap,
            None,
        );
        assert!(result.is_err());
        assert_eq!(read, cap + 1, "reads up to the cap plus one probe byte");
    }

    #[test]
    fn a_wrong_size_hint_changes_nothing() {
        let data = vec![3u8; 5000];
        let mut at = 0;
        let mut source = |buf: &mut [u8]| {
            let n = buf.len().min(data.len() - at).min(999);
            buf[..n].copy_from_slice(&data[at..at + n]);
            at += n;
            Ok(n)
        };
        let got = read_capped_with(&mut source, 5000, Some(u64::MAX)).unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn a_read_error_is_an_error() {
        let result = read_capped_with(
            |_| Err(windows::Win32::Foundation::E_ACCESSDENIED.into()),
            10,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_stream_claiming_more_than_asked_is_refused() {
        let result = read_capped_with(|buf| Ok(buf.len() + 1), 10, None);
        assert!(result.is_err());
    }

    /// A source of `len` bytes in reads of at most 999.
    fn source(len: usize) -> impl FnMut(&mut [u8]) -> Result<usize> {
        let mut left = len;
        move |buf: &mut [u8]| {
            let n = buf.len().min(left).min(999);
            buf[..n].fill(5);
            left -= n;
            Ok(n)
        }
    }

    #[test]
    fn the_buffer_never_grows_past_the_cap() {
        let cap = 1_000_003u64;
        let got = read_capped_with(source(cap as usize), cap, None).unwrap();
        assert_eq!(got.len() as u64, cap);
        assert!(got.capacity() as u64 <= cap, "capacity {}", got.capacity());
        // With the size known, exactly that is reserved and never grown.
        let got = read_capped_with(source(123_456), cap, Some(123_456)).unwrap();
        assert_eq!((got.len(), got.capacity()), (123_456, 123_456));
        // A size hint over the cap reserves the cap at most.
        let got = read_capped_with(source(10), cap, Some(u64::MAX)).unwrap();
        assert!(got.capacity() as u64 <= cap);
    }
}
