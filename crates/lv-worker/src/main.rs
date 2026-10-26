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
use lv_core::decode::{max_file_bytes, DecodeLimits};
use lv_heif::{decode_heif, ColorInfo, HeifError};
use lv_ipc::{
    max_request_data, max_response_pixels, read_request, write_response, FailKind, Request,
    Response, REQUEST_FIXED_LEN,
};

const EXIT_ERROR: u8 = 2;

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
    // libheif is built with ENABLE_PLUGIN_LOADING: heif_init (run by the first
    // `LibHeif::new`) LoadLibraryA's every DLL in the folder LIBHEIF_PLUGIN_PATH names (its
    // built-in plugin folder is empty). The app starts us with `SystemRoot` alone, but no
    // inherited variable may point libheif at foreign code. LIBHEIF_SECURITY_LIMITS can
    // switch off libheif's default limits; ours are set per decode, but drop it as well.
    std::env::remove_var("LIBHEIF_PLUGIN_PATH");
    std::env::remove_var("LIBHEIF_SECURITY_LIMITS");
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

/// Largest request payload the worker accepts: the file bytes the app may send (ruling R10;
/// the pool uses the same `max_request_data` rule) plus the request's fixed fields.
fn request_cap(max_pixels: u64) -> u64 {
    max_request_data(max_file_bytes(&DecodeLimits { max_pixels })) + REQUEST_FIXED_LEN
}

/// The pixel limit for one decode: the request's, within the worker's own, and small enough
/// that the image fits in one response (a larger one is `TooLarge`, not a failed write).
fn decode_limit(requested: u64, configured: u64) -> u64 {
    max_response_pixels(requested.min(configured))
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

        let response = decode(id, &data, decode_limit(max_pixels, config.max_pixels));
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

    use windows::Win32::System::Diagnostics::Debug::{
        SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
    };
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
        quiet_system_errors();
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

    /// No "System Error" box or crash dialog for this process. The app's mode, inherited at
    /// creation, already covers a DLL missing before `main`; this covers the rest even when
    /// the worker is started by something else.
    pub fn quiet_system_errors() {
        // SAFETY: plain call with valid flags; it only changes this process's error mode.
        unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
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

#[cfg(test)]
mod tests {
    use super::*;

    use lv_ipc::{MAX_PAYLOAD, MAX_RESPONSE_PIXELS};

    /// The default pixel limit (2^28), which any PC with 8 GB of memory or more gets.
    const DEFAULT_MAX_PIXELS: u64 = 1 << 28;

    #[test]
    fn request_cap_is_the_pools_data_cap_plus_fixed_fields() {
        assert_eq!(DecodeLimits::default().max_pixels, DEFAULT_MAX_PIXELS);
        // At the default the file size rule (2^30 + 1 MiB) is clamped to one frame.
        assert_eq!(request_cap(DEFAULT_MAX_PIXELS), MAX_PAYLOAD);
        assert_eq!(request_cap(u64::MAX), MAX_PAYLOAD);
        // Small limits keep lv-core's 64 MiB floor.
        assert_eq!(request_cap(1000), (64 << 20) + REQUEST_FIXED_LEN);
    }

    #[cfg(windows)]
    #[test]
    fn system_error_dialogs_are_off() {
        use windows::Win32::System::Diagnostics::Debug::{
            GetErrorMode, SetErrorMode, THREAD_ERROR_MODE,
        };

        // Start from the default mode: the test runner may have inherited a quiet one.
        // SAFETY: plain calls; the previous mode is put back below.
        let previous = unsafe { SetErrorMode(THREAD_ERROR_MODE(0)) };
        hardening::quiet_system_errors();
        // SAFETY: plain calls, as above.
        let mode = unsafe { GetErrorMode() };
        unsafe { SetErrorMode(previous) };
        assert_eq!(mode & 0b11, 0b11, "error mode {mode:#x}");
    }

    #[test]
    fn decode_limit_keeps_the_image_within_one_response() {
        assert_eq!(
            decode_limit(DEFAULT_MAX_PIXELS, DEFAULT_MAX_PIXELS),
            MAX_RESPONSE_PIXELS
        );
        assert!(decode_limit(u64::MAX, u64::MAX) * 4 + 16 <= MAX_PAYLOAD);
        assert_eq!(decode_limit(1000, 2000), 1000);
        assert_eq!(decode_limit(2000, 1000), 1000);
    }
}
