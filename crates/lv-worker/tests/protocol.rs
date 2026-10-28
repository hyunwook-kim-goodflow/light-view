//! Drives the real `lv-worker` executable over piped stdio.
//!
//! The worker resolves `heif.dll`/`libde265.dll`/`turbojpeg.dll`/`libwebp.dll` (and their DLLs) from its own folder only, so they must sit
//! next to `target\debug\lv-worker.exe` (`scripts/setup-deps.ps1` copies them there).

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use lv_ipc::{read_response, write_request, FailKind, FitTarget, Request, Response};

const WORKER: &str = env!("CARGO_BIN_EXE_lv-worker");
const MAX_PIXELS: u64 = 64_000_000;
const RESPONSE_CAP: u64 = MAX_PIXELS * 4 + 64 * 1024;

fn sample(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/heif")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn require_dlls() {
    let dir = PathBuf::from(WORKER).parent().unwrap().to_path_buf();
    for dll in [
        "heif.dll",
        "libde265.dll",
        "turbojpeg.dll",
        "spng.dll",
        "z.dll",
        "libwebp.dll",
        "libsharpyuv.dll",
    ] {
        assert!(
            dir.join(dll).exists(),
            "{dll} is missing from {}: the worker only loads it from its own folder. \
             Run `powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1`.",
            dir.display()
        );
    }
}

struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Worker {
    fn spawn(args: &[&str]) -> Worker {
        require_dlls();
        let mut child = Command::new(WORKER)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lv-worker");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Worker {
            child,
            stdin,
            stdout,
        }
    }

    fn start() -> Worker {
        Worker::spawn(&["--max-pixels", &MAX_PIXELS.to_string()])
    }

    fn send(&mut self, req: &Request) {
        let stdin = self.stdin.as_mut().unwrap();
        write_request(stdin, req).unwrap();
        stdin.flush().unwrap();
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        // The worker may exit before reading everything; a broken pipe is fine here.
        let _ = stdin.write_all(bytes);
        let _ = stdin.flush();
    }

    fn decode(&mut self, id: u64, max_pixels: u64, data: Vec<u8>) -> Response {
        self.send(&Request::DecodeHeif {
            id,
            max_pixels,
            data,
        });
        read_response(&mut self.stdout, RESPONSE_CAP)
            .expect("valid response")
            .expect("a response, not EOF")
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Waits up to 10 s for the worker to exit; kills it and panics otherwise.
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("worker did not exit within 10 s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Everything left on stdout until EOF.
    fn rest_of_stdout(&mut self) -> Vec<u8> {
        let mut rest = Vec::new();
        self.stdout.read_to_end(&mut rest).unwrap();
        rest
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the worker should send for `bytes`: the same decode and color pipeline, in process.
fn expected_bgra(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let img = lv_heif::decode_heif(bytes, MAX_PIXELS).expect("sample decodes");
    let mut rgba = img.rgba;
    let src = match &img.color {
        lv_heif::ColorInfo::None => lv_core::color::ColorSource::Srgb,
        lv_heif::ColorInfo::Icc(p) => lv_core::color::ColorSource::Icc(p),
        lv_heif::ColorInfo::Nclx {
            primaries,
            transfer,
        } => lv_core::color::ColorSource::Nclx {
            primaries: *primaries,
            transfer: *transfer,
        },
    };
    lv_core::color::to_srgb_in_place(&mut rgba, src);
    (
        img.width,
        img.height,
        lv_core::color::premultiply_to_bgra(rgba),
    )
}

#[test]
fn decodes_sample_over_pipes() {
    let mut w = Worker::start();
    match w.decode(7, MAX_PIXELS, sample("rainbow-451x461.heic")) {
        Response::Image {
            id,
            width,
            height,
            bgra,
        } => {
            assert_eq!((id, width, height), (7, 451, 461));
            assert_eq!(bgra.len(), 451 * 461 * 4);
        }
        other => panic!("expected Image, got {other:?}"),
    }
    // Alpha sample, and the worker keeps serving requests.
    match w.decode(8, MAX_PIXELS, sample("with-alpha-512x512.heic")) {
        Response::Image {
            id, width, height, ..
        } => assert_eq!((id, width, height), (8, 512, 512)),
        other => panic!("expected Image, got {other:?}"),
    }
    w.send(&Request::Shutdown);
    assert_eq!(w.wait().code(), Some(0));
}

/// stdout carries raw bytes: CR/LF-heavy pixel data arrives unchanged (no text-mode
/// translation), byte for byte equal to the in-process pipeline.
#[test]
fn stdout_is_binary_exact() {
    let bytes = sample("with-alpha-512x512.heic");
    let (ew, eh, expected) = expected_bgra(&bytes);
    let crlf = expected
        .iter()
        .filter(|&&b| b == b'\n' || b == b'\r')
        .count();
    assert!(crlf > 100, "sample has only {crlf} CR/LF bytes");

    let mut w = Worker::start();
    match w.decode(1, MAX_PIXELS, bytes) {
        Response::Image {
            width,
            height,
            bgra,
            ..
        } => {
            assert_eq!((width, height), (ew, eh));
            assert!(
                bgra == expected,
                "worker pixels differ from in-process decode"
            );
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

#[test]
fn corrupt_input_returns_failed() {
    let mut w = Worker::start();
    let mut truncated = sample("rainbow-451x461.heic");
    truncated.truncate(truncated.len() / 2);
    for (id, data) in [(1, truncated), (2, b"not a heif file at all".to_vec())] {
        match w.decode(id, MAX_PIXELS, data) {
            Response::Failed {
                id: got,
                kind: FailKind::Corrupt(msg),
            } => {
                assert_eq!(got, id);
                assert!(!msg.is_empty());
            }
            other => panic!("expected Failed(Corrupt), got {other:?}"),
        }
    }
    // Still alive after failures.
    assert!(matches!(
        w.decode(3, MAX_PIXELS, sample("rainbow-451x461.heic")),
        Response::Image { id: 3, .. }
    ));
}

#[test]
fn too_large_returns_failed_too_large() {
    let mut w = Worker::start();
    let expect_too_large = |r: Response, want_id: u64| match r {
        Response::Failed {
            id,
            kind: FailKind::TooLarge { width, height },
        } => assert_eq!((id, width, height), (want_id, 451, 461)),
        other => panic!("expected Failed(TooLarge), got {other:?}"),
    };
    // The request's own limit.
    expect_too_large(w.decode(5, 1000, sample("rainbow-451x461.heic")), 5);

    // The command-line limit wins over a larger per-request limit.
    let mut w = Worker::spawn(&["--max-pixels", "1000"]);
    expect_too_large(w.decode(6, u64::MAX, sample("rainbow-451x461.heic")), 6);
}

fn jpeg_sample(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/jpeg")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// (width, height) from a baseline or progressive JPEG's SOF segment.
fn jpeg_dims(jpeg: &[u8]) -> (u16, u16) {
    let mut i = 2;
    while i + 9 < jpeg.len() {
        let code = jpeg[i + 1];
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        if matches!(code, 0xC0..=0xC2) {
            let h = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]);
            let w = u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]);
            return (w, h);
        }
        i += 2 + len;
    }
    panic!("no SOF marker");
}

impl Worker {
    fn rotate(&mut self, id: u64, quarters: u8, max_pixels: u64, data: Vec<u8>) -> Response {
        self.send(&Request::RotateJpeg {
            id,
            quarters,
            max_pixels,
            data,
        });
        read_response(&mut self.stdout, RESPONSE_CAP)
            .expect("valid response")
            .expect("a response, not EOF")
    }
}

#[test]
fn rotate_over_pipes() {
    let source = jpeg_sample("gradient-64x32.jpg");
    let mut w = Worker::start();
    for (id, quarters, dims) in [(1, 1, (32, 64)), (2, 2, (64, 32)), (3, 3, (32, 64))] {
        match w.rotate(id, quarters, MAX_PIXELS, source.clone()) {
            Response::Bytes { id: got, data } => {
                assert_eq!(got, id);
                assert_eq!(&data[..2], &[0xFF, 0xD8]);
                assert_eq!(jpeg_dims(&data), dims, "{quarters} quarters");
                assert!(data.len() as u64 <= lv_ipc::max_bytes_response(source.len() as u64));
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }
    // Partial MCUs on both edges: not perfect.
    assert_eq!(
        w.rotate(4, 1, MAX_PIXELS, jpeg_sample("gradient-65x33.jpg")),
        Response::Failed {
            id: 4,
            kind: FailKind::NotPerfect
        }
    );
    // Data after EOI (ruling R7): refused, the bytes would be lost.
    let mut trailer = source.clone();
    trailer.extend_from_slice(b"motion photo video");
    assert_eq!(
        w.rotate(10, 1, MAX_PIXELS, trailer),
        Response::Failed {
            id: 10,
            kind: FailKind::HasTrailingData
        }
    );
    // Not a JPEG.
    match w.rotate(5, 1, MAX_PIXELS, b"not a jpeg".to_vec()) {
        Response::Failed {
            id: 5,
            kind: FailKind::Corrupt(msg),
        } => assert!(!msg.is_empty()),
        other => panic!("expected Failed(Corrupt), got {other:?}"),
    }
    // The request's pixel limit (64x32 = 2048 pixels).
    assert_eq!(
        w.rotate(6, 1, 2047, source.clone()),
        Response::Failed {
            id: 6,
            kind: FailKind::TooLarge {
                width: 0,
                height: 0
            }
        }
    );
    // The command-line limit wins over a larger per-request limit.
    let mut small = Worker::spawn(&["--max-pixels", "2047"]);
    assert!(matches!(
        small.rotate(7, 1, u64::MAX, source.clone()),
        Response::Failed {
            id: 7,
            kind: FailKind::TooLarge { .. }
        }
    ));
    // Still serving, and the HEIF path is unaffected.
    assert!(matches!(
        w.rotate(8, 1, MAX_PIXELS, source),
        Response::Bytes { id: 8, .. }
    ));
    assert!(matches!(
        w.decode(9, MAX_PIXELS, sample("rainbow-451x461.heic")),
        Response::Image { id: 9, .. }
    ));
}

impl Worker {
    fn fit_webp(
        &mut self,
        id: u64,
        src: (u32, u32),
        start_quality: u8,
        target: FitTarget,
        rgba: Vec<u8>,
    ) -> Response {
        self.send(&Request::FitWebp {
            id,
            width: src.0,
            height: src.1,
            start_quality,
            target,
            rgba,
        });
        read_response(&mut self.stdout, RESPONSE_CAP)
            .expect("valid response")
            .expect("a response, not EOF")
    }
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && &bytes[..4] == b"RIFF"
        && &bytes[8..12] == b"WEBP"
        && u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize + 8 == bytes.len()
}

/// A gradient with some texture, opaque.
fn picture(width: u32, height: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let t = ((x ^ y) & 15) as u8;
            px.extend_from_slice(&[
                (x * 200 / width) as u8 + t,
                (y * 200 / height) as u8 + t,
                ((x + y) * 100 / (width + height)) as u8 + t,
                255,
            ]);
        }
    }
    px
}

#[test]
fn fit_webp_answers_outcome() {
    let mut w = Worker::start();
    let px = picture(64, 48);
    // A resolution target: one encode at the start quality, at the target size.
    let first = match w.fit_webp(1, (64, 48), 80, FitTarget::LongEdge(32), px.clone()) {
        Response::Fitted {
            id: 1,
            width: 32,
            height: 24,
            quality: 80,
            data,
        } => data,
        other => panic!("expected Fitted 32x24 q80, got {other:?}"),
    };
    assert!(is_webp(&first));
    // Deterministic (ruling R1).
    assert!(matches!(
        w.fit_webp(2, (64, 48), 80, FitTarget::LongEdge(32), px.clone()),
        Response::Fitted { id: 2, data, .. } if data == first
    ));
    // A byte budget between the sizes at quality 95 and 60: the quality search runs here.
    let size_at = |w: &mut Worker, q: u8| match w.fit_webp(
        9,
        (64, 48),
        q,
        FitTarget::Percent(100),
        px.clone(),
    ) {
        Response::Fitted { data, .. } => data.len() as u64,
        other => panic!("{other:?}"),
    };
    let (high, low) = (size_at(&mut w, 95), size_at(&mut w, 60));
    assert!(low < high);
    let budget = low + (high - low) / 3;
    let target = FitTarget::MaxBytes {
        bytes: budget,
        long_edge: None,
    };
    match w.fit_webp(3, (64, 48), 95, target, px.clone()) {
        Response::Fitted {
            id: 3,
            width: 64,
            height: 48,
            quality,
            data,
        } => {
            assert!(data.len() as u64 <= budget, "{} > {budget}", data.len());
            assert!((60..95).contains(&quality), "quality {quality}");
            assert!(is_webp(&data));
        }
        other => panic!("expected Fitted, got {other:?}"),
    }
    // A budget nothing meets (the source is under the 800 px floor): CantFit.
    let target = FitTarget::MaxBytes {
        bytes: 10,
        long_edge: None,
    };
    match w.fit_webp(4, (64, 48), 90, target, px.clone()) {
        Response::CantFit { id: 4, smallest } => assert!(smallest > 10),
        other => panic!("expected CantFit, got {other:?}"),
    }
    // Over the worker's pixel limit: TooLarge with the source size, nothing encoded.
    let mut small = Worker::spawn(&["--max-pixels", "3000"]);
    assert_eq!(
        small.fit_webp(5, (64, 48), 80, FitTarget::LongEdge(32), px.clone()),
        Response::Failed {
            id: 5,
            kind: FailKind::TooLarge {
                width: 64,
                height: 48
            }
        }
    );
    // An output side over WebP's 16383 is TooLarge as well; a long source made short is not.
    assert_eq!(
        w.fit_webp(
            6,
            (16384, 1),
            80,
            FitTarget::Percent(100),
            vec![0; 16384 * 4]
        ),
        Response::Failed {
            id: 6,
            kind: FailKind::TooLarge {
                width: 16384,
                height: 1
            }
        }
    );
    assert!(matches!(
        w.fit_webp(
            7,
            (16384, 1),
            80,
            FitTarget::LongEdge(1000),
            vec![9; 16384 * 4]
        ),
        Response::Fitted {
            id: 7,
            width: 1000,
            height: 1,
            ..
        }
    ));
    // Still serving, and the other requests are unaffected.
    assert!(matches!(
        w.decode(8, MAX_PIXELS, sample("rainbow-451x461.heic")),
        Response::Image { id: 8, .. }
    ));
}

#[cfg(debug_assertions)]
#[test]
fn test_mode_wrong_kind_answers_with_the_other_kind() {
    let mut w = Worker::spawn(&["--max-pixels", "1000", "--test-mode=wrong-kind"]);
    assert!(matches!(
        w.decode(1, 1000, sample("rainbow-451x461.heic")),
        Response::Bytes { id: 1, .. }
    ));
    assert!(matches!(
        w.rotate(2, 1, 1000, jpeg_sample("gradient-64x32.jpg")),
        Response::Image { id: 2, .. }
    ));
    assert!(matches!(
        w.fit_webp(3, (1, 1), 80, FitTarget::LongEdge(1), vec![0; 4]),
        Response::Image { id: 3, .. }
    ));
}

#[test]
fn shutdown_exits_zero() {
    let mut w = Worker::start();
    w.send(&Request::Shutdown);
    assert_eq!(w.wait().code(), Some(0));
    assert!(w.rest_of_stdout().is_empty());
}

#[test]
fn clean_eof_exits_zero() {
    let mut w = Worker::start();
    w.close_stdin();
    assert_eq!(w.wait().code(), Some(0));
    assert!(w.rest_of_stdout().is_empty());
}

#[test]
fn garbage_protocol_exits_two() {
    let mut w = Worker::start();
    w.send_raw(b"GET / HTTP/1.1\r\n\r\n");
    assert_eq!(w.wait().code(), Some(2));
    assert!(w.rest_of_stdout().is_empty(), "no response for garbage");
}

#[test]
fn truncated_frame_exits_two() {
    let mut w = Worker::start();
    let mut frame = Vec::new();
    write_request(
        &mut frame,
        &Request::DecodeHeif {
            id: 1,
            max_pixels: MAX_PIXELS,
            data: vec![0; 100],
        },
    )
    .unwrap();
    frame.truncate(50);
    w.send_raw(&frame);
    w.close_stdin();
    assert_eq!(w.wait().code(), Some(2));
}

/// R10: request cap = min(max(N * 4 + 1 MiB, 64 MiB), MAX_PAYLOAD - 17) + 17 (the largest
/// fixed part, RotateJpeg's); with N = 1000 that is 64 MiB + 17.
#[test]
fn oversized_request_exits_two() {
    let cap: u64 = 64 * 1024 * 1024 + lv_ipc::REQUEST_FIXED_LEN;
    let header = |len: u64| {
        let mut h = b"LVW1".to_vec();
        h.push(0x01);
        h.extend_from_slice(&len.to_le_bytes());
        h
    };
    // One byte over the cap: rejected from the header alone, no response.
    let mut w = Worker::spawn(&["--max-pixels", "1000"]);
    w.send_raw(&header(cap + 1));
    assert_eq!(w.wait().code(), Some(2));
    assert!(w.rest_of_stdout().is_empty());

    // Exactly at the cap: read and answered (the zero bytes are not a HEIF file).
    let mut w = Worker::spawn(&["--max-pixels", "1000"]);
    let mut frame = header(cap);
    frame.extend_from_slice(&9u64.to_le_bytes());
    frame.extend_from_slice(&1000u64.to_le_bytes());
    frame.resize(13 + cap as usize, 0);
    w.send_raw(&frame);
    match read_response(&mut w.stdout, RESPONSE_CAP).unwrap().unwrap() {
        Response::Failed { id: 9, .. } => {}
        other => panic!("expected Failed for id 9, got {other:?}"),
    }
}

#[test]
fn missing_or_bad_max_pixels_exits_two() {
    for args in [
        &[][..],
        &["--max-pixels"][..],
        &["--max-pixels", "lots"][..],
        &["--max-pixels", "0"][..],
        &["--max-pixels", "100", "--bogus"][..],
    ] {
        let mut w = Worker::spawn(args);
        assert_eq!(w.wait().code(), Some(2), "args {args:?}");
    }
}

#[cfg(debug_assertions)]
#[test]
fn test_mode_crash_aborts_on_first_request() {
    let mut w = Worker::spawn(&["--max-pixels", "1000", "--test-mode=crash"]);
    w.send(&Request::DecodeHeif {
        id: 1,
        max_pixels: 1000,
        data: sample("rainbow-451x461.heic"),
    });
    let status = w.wait();
    assert!(!status.success(), "crash mode exited with {status:?}");
    assert_ne!(status.code(), Some(2));
    assert!(w.rest_of_stdout().is_empty());
}

#[cfg(debug_assertions)]
#[test]
fn test_mode_hang_never_answers() {
    let mut w = Worker::spawn(&["--max-pixels", "1000", "--test-mode=hang"]);
    w.send(&Request::DecodeHeif {
        id: 1,
        max_pixels: 1000,
        data: sample("rainbow-451x461.heic"),
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert!(w.child.try_wait().unwrap().is_none(), "hang mode exited");
}

#[cfg(debug_assertions)]
#[test]
fn test_mode_die_at_start_exits_without_reading() {
    let mut w = Worker::spawn(&["--max-pixels", "1000", "--test-mode=die-at-start"]);
    assert_eq!(w.wait().code(), Some(3));
    assert!(w.rest_of_stdout().is_empty());
}

#[cfg(not(debug_assertions))]
#[test]
fn test_mode_is_rejected_in_release() {
    for mode in [
        "--test-mode=crash",
        "--test-mode=hang",
        "--test-mode=die-at-start",
        "--test-mode=wrong-kind",
    ] {
        let mut w = Worker::spawn(&["--max-pixels", "1000", mode]);
        assert_eq!(w.wait().code(), Some(2), "{mode}");
    }
}
