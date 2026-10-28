//! Size and memory rules both sides of the pipe share: what one request or answer may carry
//! and what a worker's Job allows.

use crate::{MAX_PAYLOAD, MAX_REQUEST_DATA, MAX_RESPONSE_PIXELS};

const MIB: u64 = 1024 * 1024;
/// Floor of [`decoder_memory`].
const MIN_DECODER_MEMORY: u64 = 256 * MIB;
/// libwebp's worst case per pixel for one lossy encode, its output and the returned copy
/// included (measured 33 bytes for 12 MP of noise with alpha at quality 100; see lv-webp).
const WEBP_BYTES_PER_PIXEL: u64 = 40;
/// The worker's own memory beside an encode (code, libheif's and TurboJPEG's tables, pipe
/// buffers).
const WEBP_OVERHEAD: u64 = 32 * MIB;
/// The largest width or height a WebP file can store (libwebp's `WEBP_MAX_DIMENSION`).
pub const WEBP_MAX_DIMENSION: u32 = 16383;

/// The most file bytes the app sends and the worker accepts in one request: the app's file
/// size cap (`lv_core::decode::max_file_bytes`), clamped to what one frame can carry. Both
/// sides use this one rule, so a file the app sends is never one the worker refuses unread.
pub fn max_request_data(file_cap: u64) -> u64 {
    file_cap.min(MAX_REQUEST_DATA)
}

/// The most bytes a `Bytes` answer to a request carrying `input_len` file bytes may hold:
/// `input_len + 1 MiB` (a lossless JPEG transform keeps the markers and re-encodes the same
/// coefficients, so the output stays close to the input; a lossy WebP is far smaller than
/// its RGBA input), never above `MAX_PAYLOAD - 16`.
pub fn max_bytes_response(input_len: u64) -> u64 {
    input_len.saturating_add(MIB).min(MAX_PAYLOAD - 16)
}

/// The pixel limit a worker can honour: `max_pixels`, clamped so that the decoded image
/// still fits in one `Image` response. A larger image is reported as too large instead.
pub fn max_response_pixels(max_pixels: u64) -> u64 {
    max_pixels.min(MAX_RESPONSE_PIXELS)
}

/// Memory libheif may allocate for one decode of at most `max_pixels` pixels (its
/// `max_total_memory`): twice the RGBA output (YCbCr planes, the RGB(A) image and conversion
/// scratch), at least 256 MiB so that small limits still take ordinary files.
pub fn decoder_memory(max_pixels: u64) -> u64 {
    max_pixels.saturating_mul(8).max(MIN_DECODER_MEMORY)
}

/// The worker's Job memory cap: what libheif may allocate ([`decoder_memory`]) plus the
/// worker's own RGBA copy of the image (4 bytes per pixel), so libheif reaching its own
/// limit is an error it reports, never the Job killing the worker. This is
/// `max(12 * max_pixels, 256 MiB + 4 * max_pixels)`.
pub fn worker_memory_limit(max_pixels: u64) -> u64 {
    decoder_memory(max_pixels).saturating_add(max_pixels.saturating_mul(4))
}

/// What one `EncodeWebp` of `pixels` pixels may take in the worker: the request's RGBA
/// (4 bytes per pixel), libwebp's worst case (40 bytes per pixel) and 32 MiB for the rest
/// of the worker.
pub fn webp_encode_memory(pixels: u64) -> u64 {
    pixels
        .saturating_mul(4 + WEBP_BYTES_PER_PIXEL)
        .saturating_add(WEBP_OVERHEAD)
}

/// Whether a worker started with `max_pixels` encodes a `width` x `height` WebP: no side over
/// WebP's [`WEBP_MAX_DIMENSION`], within the pixel limit and, by [`webp_encode_memory`],
/// within its Job ([`worker_memory_limit`]). Otherwise the answer is `TooLarge`. With the
/// default limit (2^28 pixels, a 3 GiB Job) that is about 72 MP; a worker limited to 12 MP
/// (a 302 MiB Job) takes about 6 MP.
pub fn webp_encode_fits(width: u32, height: u32, max_pixels: u64) -> bool {
    let pixels = u64::from(width) * u64::from(height);
    width <= WEBP_MAX_DIMENSION
        && height <= WEBP_MAX_DIMENSION
        && pixels <= max_pixels
        && webp_encode_memory(pixels) <= worker_memory_limit(max_pixels)
}
