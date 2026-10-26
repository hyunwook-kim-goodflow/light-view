//! light-view's HEIF decode worker.
//!
//! Reads [`Request`]s from stdin and writes [`Response`]s to stdout (see `lv-ipc`). stdout
//! carries framed responses only; diagnostics go to stderr. Exit codes: 0 after `Shutdown` or
//! a clean EOF, 2 on a bad command line, a protocol error or a failed write (the app went
//! away). The worker never touches the file system or the network: the app sends the bytes.
//!
//! Usage: `lv-worker --max-pixels <N>` (debug builds also take
//! `--test-mode=hang|crash|die-at-start`).

use std::io::{self, BufWriter, Write};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

use lv_core::color::{premultiply_to_bgra, to_srgb_in_place, ColorSource};
use lv_heif::{decode_heif, ColorInfo, HeifError};
use lv_ipc::{read_request, write_response, FailKind, Request, Response};

const EXIT_ERROR: u8 = 2;
const MIB: u64 = 1024 * 1024;

/// Debug-only fault injection for the app's watchdog tests.
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestMode {
    /// Never answer the first request.
    Hang,
    /// `abort()` on the first request.
    Crash,
    /// Exit with code 3 before reading anything (a worker that cannot start).
    DieAtStart,
}

/// Exit code of `--test-mode=die-at-start`.
#[cfg(debug_assertions)]
const EXIT_DIE_AT_START: u8 = 3;

#[derive(Debug)]
struct Config {
    max_pixels: u64,
    #[cfg(debug_assertions)]
    test_mode: Option<TestMode>,
}

fn main() -> ExitCode {
    #[cfg(windows)]
    hardening::apply();

    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(msg) => {
            eprintln!("lv-worker: {msg}");
            eprintln!("usage: lv-worker --max-pixels <N>");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    #[cfg(debug_assertions)]
    if config.test_mode == Some(TestMode::DieAtStart) {
        return ExitCode::from(EXIT_DIE_AT_START);
    }
    serve(&config)
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut max_pixels = None;
    #[cfg(debug_assertions)]
    let mut test_mode = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--max-pixels" => {
                let value = args.next().ok_or("--max-pixels needs a value")?;
                match value.parse::<u64>() {
                    Ok(n) if n > 0 => max_pixels = Some(n),
                    _ => return Err(format!("invalid --max-pixels value {value:?}")),
                }
            }
            #[cfg(debug_assertions)]
            "--test-mode=hang" => test_mode = Some(TestMode::Hang),
            #[cfg(debug_assertions)]
            "--test-mode=crash" => test_mode = Some(TestMode::Crash),
            #[cfg(debug_assertions)]
            "--test-mode=die-at-start" => test_mode = Some(TestMode::DieAtStart),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Config {
        max_pixels: max_pixels.ok_or("--max-pixels is required")?,
        #[cfg(debug_assertions)]
        test_mode,
    })
}

/// Largest request payload the worker accepts: room for an `N`-pixel image's file bytes
/// (ruling R10, the same rule as the app's file size limit) plus the frame's fixed fields.
fn request_cap(max_pixels: u64) -> u64 {
    max_pixels
        .saturating_mul(4)
        .saturating_add(MIB)
        .max(64 * MIB)
        .saturating_add(64)
}

fn serve(config: &Config) -> ExitCode {
    let cap = request_cap(config.max_pixels);
    let mut input = io::stdin().lock();
    let mut output = BufWriter::new(io::stdout().lock());
    loop {
        let (id, max_pixels, data) = match read_request(&mut input, cap) {
            Ok(None) | Ok(Some(Request::Shutdown)) => return ExitCode::SUCCESS,
            Ok(Some(Request::DecodeHeif {
                id,
                max_pixels,
                data,
            })) => (id, max_pixels, data),
            Err(e) => {
                eprintln!("lv-worker: protocol error, exiting: {e}");
                return ExitCode::from(EXIT_ERROR);
            }
        };

        #[cfg(debug_assertions)]
        match config.test_mode {
            Some(TestMode::Hang) => loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            },
            Some(TestMode::Crash) => std::process::abort(),
            Some(TestMode::DieAtStart) | None => {}
        }

        let response = decode(id, &data, max_pixels.min(config.max_pixels));
        drop(data);
        if let Err(e) = write_response(&mut output, &response).and_then(|()| output.flush()) {
            eprintln!("lv-worker: cannot write response, exiting: {e}");
            return ExitCode::from(EXIT_ERROR);
        }
    }
}

/// Decodes one file to premultiplied sRGB BGRA8. Panics anywhere in the pipeline (the
/// libheif wrapper, qcms) become `Corrupt`; crashes in C code end the process instead.
fn decode(id: u64, data: &[u8], max_pixels: u64) -> Response {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let image = decode_heif(data, max_pixels)?;
        let mut rgba = image.rgba;
        let source = match &image.color {
            ColorInfo::None => ColorSource::Srgb,
            ColorInfo::Icc(profile) => ColorSource::Icc(profile),
            ColorInfo::Nclx {
                primaries,
                transfer,
            } => ColorSource::Nclx {
                primaries: *primaries,
                transfer: *transfer,
            },
        };
        to_srgb_in_place(&mut rgba, source);
        Ok::<_, HeifError>((image.width, image.height, premultiply_to_bgra(rgba)))
    }));
    let kind = match result {
        Ok(Ok((width, height, bgra))) => {
            return Response::Image {
                id,
                width,
                height,
                bgra,
            }
        }
        Ok(Err(HeifError::TooLarge { width, height })) => FailKind::TooLarge { width, height },
        Ok(Err(HeifError::Corrupt(msg))) => FailKind::Corrupt(msg),
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            FailKind::Corrupt(format!("decode panicked: {what}"))
        }
    };
    Response::Failed { id, kind }
}

#[cfg(windows)]
mod hardening {
    //! Process-wide restrictions the worker puts on itself before reading any input.

    use windows::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    use windows::Win32::System::Threading::{
        ProcessDynamicCodePolicy, ProcessExtensionPointDisablePolicy, ProcessImageLoadPolicy,
        ProcessStrictHandleCheckPolicy, SetProcessMitigationPolicy, PROCESS_MITIGATION_POLICY,
    };

    // Each PROCESS_MITIGATION_*_POLICY structure below is a single DWORD of flag bits.
    /// PROCESS_MITIGATION_DYNAMIC_CODE_POLICY: ProhibitDynamicCode.
    const PROHIBIT_DYNAMIC_CODE: u32 = 1 << 0;
    /// PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY: DisableExtensionPoints.
    const DISABLE_EXTENSION_POINTS: u32 = 1 << 0;
    /// PROCESS_MITIGATION_IMAGE_LOAD_POLICY: NoRemoteImages | NoLowMandatoryLabelImages.
    const NO_REMOTE_OR_LOW_LABEL_IMAGES: u32 = (1 << 0) | (1 << 1);
    /// PROCESS_MITIGATION_STRICT_HANDLE_CHECK_POLICY: RaiseExceptionOnInvalidHandleReference
    /// | HandleExceptionsPermanentlyEnabled.
    const STRICT_HANDLE_CHECKS: u32 = (1 << 0) | (1 << 1);

    /// Restricts later DLL loads to the exe folder and System32, then applies the
    /// self-imposed mitigation policies. Failures are reported on stderr and ignored.
    ///
    /// `heif.dll` and `libde265.dll` are static imports, already loaded by the time this
    /// runs, so the image load policy cannot block them.
    pub fn apply() {
        // SAFETY: plain flags argument; no pointers involved.
        let dirs = unsafe {
            SetDefaultDllDirectories(
                LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR,
            )
        };
        if let Err(e) = dirs {
            eprintln!("lv-worker: SetDefaultDllDirectories failed: {e}");
        }
        let policies = [
            (
                "dynamic code",
                ProcessDynamicCodePolicy,
                PROHIBIT_DYNAMIC_CODE,
            ),
            (
                "extension points",
                ProcessExtensionPointDisablePolicy,
                DISABLE_EXTENSION_POINTS,
            ),
            (
                "image load",
                ProcessImageLoadPolicy,
                NO_REMOTE_OR_LOW_LABEL_IMAGES,
            ),
            (
                "strict handle checks",
                ProcessStrictHandleCheckPolicy,
                STRICT_HANDLE_CHECKS,
            ),
        ];
        for (name, policy, flags) in policies {
            if let Err(e) = set_policy(policy, flags) {
                eprintln!("lv-worker: mitigation policy '{name}' not applied: {e}");
            }
        }
    }

    fn set_policy(policy: PROCESS_MITIGATION_POLICY, flags: u32) -> windows::core::Result<()> {
        // SAFETY: every policy used here takes a 4-byte structure that is just a flags
        // DWORD; `flags` outlives the call and the length matches.
        unsafe {
            SetProcessMitigationPolicy(
                policy,
                std::ptr::from_ref(&flags).cast(),
                std::mem::size_of::<u32>(),
            )
        }
    }
}
