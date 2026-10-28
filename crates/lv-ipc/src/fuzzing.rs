//! Entry function of the `ipc_frame` fuzz target in `fuzz/` (ruling R3), also replayed on
//! the seed corpus by lv-core's stable test `fuzz_corpus_replays`. Not part of the API.

use std::io::Cursor;

use crate::{read_request, read_response, write_request, write_response, Request, Response};

/// The payload limit the entry reads with: small, so a header that promises a large
/// payload costs the fuzzer no large allocation.
pub const FUZZ_MAX_PAYLOAD: u64 = 1 << 20;

/// Reads the input as a stream of requests and, separately, as a stream of responses,
/// until the end, an error, or 64 frames. Every frame read must write back and read back
/// as the same message. A `FitWebp` read must hold exactly `width * height * 4` bytes of
/// pixels, a start quality of 1 to 100 and a valid target; a `Fitted` read a non-zero size
/// and a quality of 1 to 100.
pub fn ipc_frame(data: &[u8]) {
    let mut r = Cursor::new(data);
    for _ in 0..64 {
        let Ok(Some(request)) = read_request(&mut r, FUZZ_MAX_PAYLOAD) else {
            break;
        };
        if let Request::FitWebp {
            width,
            height,
            start_quality,
            target,
            rgba,
            ..
        } = &request
        {
            assert!((1..=100).contains(start_quality));
            assert!(target.is_valid());
            assert_eq!(
                rgba.len() as u64,
                u64::from(*width) * u64::from(*height) * 4
            );
            assert!(*width > 0 && *height > 0);
        }
        let mut buf = Vec::new();
        write_request(&mut buf, &request).expect("a request read must write");
        let again = read_request(&mut Cursor::new(&buf), FUZZ_MAX_PAYLOAD);
        assert_eq!(again.ok().flatten(), Some(request));
    }
    let mut r = Cursor::new(data);
    for _ in 0..64 {
        let Ok(Some(response)) = read_response(&mut r, FUZZ_MAX_PAYLOAD) else {
            break;
        };
        if let Response::Fitted {
            width,
            height,
            quality,
            ..
        } = &response
        {
            assert!(*width > 0 && *height > 0 && (1..=100).contains(quality));
        }
        let mut buf = Vec::new();
        write_response(&mut buf, &response).expect("a response read must write");
        let again = read_response(&mut Cursor::new(&buf), FUZZ_MAX_PAYLOAD);
        assert_eq!(again.ok().flatten(), Some(response));
    }
}
