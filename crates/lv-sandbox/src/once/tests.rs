use super::*;

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Instant;

use windows::Win32::Foundation::{BOOL, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenIsAppContainer, TOKEN_QUERY};
use windows::Win32::System::JobObjects::IsProcessInJob;
use windows::Win32::System::Threading::OpenProcessToken;

use crate::low_host::{is_low_integrity_in_job, run_test_at_low_integrity, staged_worker};
use crate::test_support::{worker_exe, Process};

const MAX_PIXELS: u64 = 100_000_000;
const DEADLINE: Duration = Duration::from_secs(10);
/// Set in the low integrity child test process: the worker exe it runs.
const CHILD_ENV: &str = "LV_SANDBOX_LOW_HOST_WORKER";

fn sample() -> Vec<u8> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/heif/rainbow-451x461.heic");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn thumbnail_request(id: u64) -> Request {
    Request::ThumbnailHeif {
        id,
        max_side: 256,
        max_pixels: MAX_PIXELS,
        data: sample(),
    }
}

/// Asserts a 256-pixel thumbnail of the 451x461 sample, answering request `id`.
fn assert_thumbnail(result: Result<Response, RunError>, id: u64) {
    match result {
        Ok(Response::Image {
            id: got,
            width,
            height,
            bgra,
        }) => {
            assert_eq!(got, id);
            assert_eq!(width.max(height), 256, "{width}x{height}");
            assert!(width >= 245, "aspect kept: {width}x{height}");
            assert_eq!(bgra.len(), width as usize * height as usize * 4);
        }
        other => panic!("expected an image, got {other:?}"),
    }
}

fn in_job(suspended: &Suspended) -> bool {
    let sandboxed = &suspended.sandboxed;
    let mut in_job = BOOL::default();
    // SAFETY: both handles are valid and owned by `sandboxed`.
    unsafe {
        IsProcessInJob(
            HANDLE(sandboxed.process.as_raw_handle()),
            HANDLE(sandboxed.job.as_raw_handle()),
            &mut in_job,
        )
    }
    .expect("IsProcessInJob");
    in_job.as_bool()
}

#[test]
fn run_once_decodes_heic() {
    assert_thumbnail(
        run_once(&worker_exe(), MAX_PIXELS, thumbnail_request(7), DEADLINE),
        7,
    );
    let decode = Request::DecodeHeif {
        id: 8,
        max_pixels: MAX_PIXELS,
        data: sample(),
    };
    match run_once(&worker_exe(), MAX_PIXELS, decode, DEADLINE) {
        Ok(Response::Image {
            id, width, height, ..
        }) => assert_eq!((id, width, height), (8, 451, 461)),
        other => panic!("expected an image, got {other:?}"),
    }
    // Garbage is the worker's answer, not a failure of the run.
    let garbage = Request::ThumbnailHeif {
        id: 9,
        max_side: 256,
        max_pixels: MAX_PIXELS,
        data: vec![0; 64],
    };
    assert!(matches!(
        run_once(&worker_exe(), MAX_PIXELS, garbage, DEADLINE),
        Ok(Response::Failed { id: 9, .. })
    ));
}

#[test]
fn run_once_times_out_and_kills_worker() {
    let mut watch = None;
    let started = Instant::now();
    let result = run(
        &worker_exe(),
        MAX_PIXELS,
        &["--test-mode=hang"],
        thumbnail_request(1),
        Duration::from_secs(1),
        |suspended| watch = Some(Process::open(suspended.sandboxed.pid)),
    );
    let took = started.elapsed();
    assert_eq!(result, Err(RunError::Timeout));
    assert!(
        took >= Duration::from_secs(1) && took < Duration::from_secs(3),
        "returned after {took:?}"
    );
    let watch = watch.expect("the worker started");
    assert!(
        watch.wait_for_exit(Duration::ZERO),
        "the worker outlived its deadline"
    );
}

#[test]
fn run_once_worker_is_born_in_job() {
    let mut seen = None;
    let result = run(
        &worker_exe(),
        MAX_PIXELS,
        &[],
        thumbnail_request(3),
        DEADLINE,
        |suspended| {
            seen = Some((
                suspended.born_in_job,
                in_job(suspended),
                Process::open(suspended.sandboxed.pid),
            ))
        },
    );
    let (born_in_job, in_job, process) = seen.expect("the worker started");
    assert!(born_in_job, "created inside the Job, not assigned later");
    assert!(in_job, "in the Job before it runs");
    assert_thumbnail(result, 3);
    assert!(
        process.wait_for_exit(Duration::ZERO),
        "the worker outlived its answer"
    );
}

#[test]
fn run_once_failures_are_errors() {
    let missing = worker_exe().with_file_name("no-such-worker.exe");
    assert!(matches!(
        run_once(&missing, MAX_PIXELS, thumbnail_request(1), DEADLINE),
        Err(RunError::Start(_))
    ));
    for (mode, id) in [("--test-mode=crash", 1), ("--test-mode=die-at-start", 2)] {
        let result = run(
            &worker_exe(),
            MAX_PIXELS,
            &[mode],
            thumbnail_request(id),
            DEADLINE,
            |_| {},
        );
        assert!(
            matches!(result, Err(RunError::Protocol(_))),
            "{mode}: {result:?}"
        );
    }
    // A request lv-ipc refuses to write: refused before any worker starts.
    let mut started = false;
    let invalid = Request::ThumbnailHeif {
        id: 4,
        max_side: 0,
        max_pixels: MAX_PIXELS,
        data: vec![],
    };
    let result = run(&worker_exe(), MAX_PIXELS, &[], invalid, DEADLINE, |_| {
        started = true
    });
    assert!(matches!(result, Err(RunError::Start(_))), "{result:?}");
    assert!(
        !started,
        "a worker started for a request that cannot be sent"
    );
}

#[test]
fn response_cap_fits_the_request() {
    let thumb = thumbnail_request(1);
    assert_eq!(response_cap(&thumb, MAX_PIXELS), 256 * 256 * 4 + 64 * 1024);
    let decode = Request::DecodeHeif {
        id: 1,
        max_pixels: MAX_PIXELS,
        data: vec![],
    };
    assert_eq!(
        response_cap(&decode, MAX_PIXELS),
        4 * MAX_PIXELS + 64 * 1024
    );
    // A rotation's answer is bounded by its input, as the app's pool bounds it.
    let rotate = Request::RotateJpeg {
        id: 1,
        quarters: 1,
        max_pixels: 1,
        data: vec![0; 10 << 20],
    };
    assert_eq!(
        response_cap(&rotate, 1),
        lv_ipc::FITTED_FIXED_LEN + lv_ipc::max_bytes_response(10 << 20)
    );
}

/// Whether the suspended worker's token is an AppContainer token.
fn in_appcontainer(suspended: &Suspended) -> bool {
    let mut token = HANDLE::default();
    let process = HANDLE(suspended.sandboxed.process.as_raw_handle());
    // SAFETY: valid process handle; `token` receives a new handle.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.expect("OpenProcessToken");
    // SAFETY: OpenProcessToken succeeded, so the handle is ours.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let (mut flag, mut len) = (0u32, 0u32);
    // SAFETY: TokenIsAppContainer is one DWORD, written to `flag`.
    unsafe {
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenIsAppContainer,
            Some(std::ptr::from_mut(&mut flag).cast()),
            4,
            &mut len,
        )
    }
    .expect("GetTokenInformation");
    flag != 0
}

#[test]
fn run_once_uses_the_appcontainer_where_the_folder_allows_it() {
    let (_folder, exe) = staged_worker(&worker_exe());
    let mut boxed = None;
    let result = run(
        &exe,
        MAX_PIXELS,
        &[],
        thumbnail_request(6),
        DEADLINE,
        |suspended| boxed = Some(in_appcontainer(suspended)),
    );
    assert_eq!(
        boxed,
        Some(true),
        "a medium integrity host gets the AppContainer"
    );
    assert_thumbnail(result, 6);
}

/// The child half of [`run_once_works_from_a_low_integrity_host_in_a_job`]: does nothing
/// unless that test started this process.
#[test]
fn low_integrity_host_child() {
    let Some(exe) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    assert!(is_low_integrity_in_job(), "a low integrity host in a Job");
    let mut seen = None;
    let result = run(
        Path::new(&exe),
        MAX_PIXELS,
        &[],
        thumbnail_request(5),
        DEADLINE,
        |suspended| seen = Some(in_job(suspended)),
    );
    assert_thumbnail(result, 5);
    assert_eq!(seen, Some(true), "the worker is in its own Job");
}

/// Like the Explorer extension in a low integrity host (prevhost may run so) that is itself
/// in a Job: the worker's folder grants AppContainers access, so the worker tries its
/// AppContainer first and falls back if the host cannot use it (ruling R2), and still
/// answers inside its own Job.
#[test]
fn run_once_works_from_a_low_integrity_host_in_a_job() {
    let (_folder, exe) = staged_worker(&worker_exe());
    run_test_at_low_integrity(
        "once::tests::low_integrity_host_child",
        CHILD_ENV,
        exe.as_os_str(),
    );
}
