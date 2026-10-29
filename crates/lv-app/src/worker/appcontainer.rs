//! The pool's use of the worker's AppContainer (spec §6.1, ruling R2): looked up once,
//! dropped for good after a start in it fails. The container itself lives in
//! [`lv_sandbox::appcontainer`].

use std::sync::atomic::Ordering;

#[cfg(not(test))]
use lv_sandbox::appcontainer::profile_sid;
#[cfg(test)]
use lv_sandbox::appcontainer::profile_sid_named;
use lv_sandbox::appcontainer::{for_worker, Container};
use lv_sandbox::process::Worker;
use lv_sandbox::sandbox::log;

use super::WorkerPool;

impl WorkerPool {
    /// The AppContainer for this pool's workers, if they can run in one (ruling R2). Looked
    /// up and logged once, by the first worker start (normally on the pool's start thread).
    pub(super) fn container(&self) -> Option<&Container> {
        if self.container_failed.load(Ordering::Relaxed) {
            return None;
        }
        self.container
            .get_or_init(|| {
                #[cfg(test)]
                let profile = || profile_sid_named(self.profile_name);
                #[cfg(not(test))]
                let profile = profile_sid;
                for_worker(&self.exe, profile)
            })
            .as_ref()
    }

    /// Notes a worker that had to start without the AppContainer it was given.
    pub(super) fn check_container(&self, worker: &Worker) {
        if let Some(e) = &worker.sandboxed.container_failure {
            if !self.container_failed.swap(true, Ordering::Relaxed) {
                log(&format!(
                    "start in the AppContainer failed ({e}); workers run without it"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::OsStr;
    use std::ops::Deref;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    use windows::Win32::Foundation::{BOOL, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenIsAppContainer, TOKEN_QUERY};
    use windows::Win32::System::JobObjects::IsProcessInJob;
    use windows::Win32::System::Threading::OpenProcessToken;

    use lv_core::decode::DecodeError;
    use lv_sandbox::appcontainer::{folder_readable, profile_sid, PROFILE_NAME};

    use crate::worker::tests::{
        assert_decodes, assert_rotated, jpeg_sample, single_worker, win32k_lockdown, worker_exe,
        Process,
    };
    use crate::worker::{PoolError, WorkerPool};

    const MAX_PIXELS: u64 = 100_000_000;
    /// Not a valid profile name, so creating the profile fails.
    const BAD_PROFILE: &str = "no\\such//profile";

    fn unstarted(exe: PathBuf, profile: &'static str, every_args: &[&str]) -> WorkerPool {
        let pool =
            WorkerPool::unstarted(exe, 1, MAX_PIXELS, Duration::from_secs(10), &[], every_args);
        WorkerPool {
            profile_name: profile,
            ..pool
        }
    }

    /// A started pool whose workers are ended and waited for on drop, a failed assertion
    /// included, so the folder they run from can be deleted afterwards.
    struct Running {
        pool: WorkerPool,
        processes: Vec<Process>,
    }

    impl Running {
        fn start(pool: WorkerPool) -> Running {
            pool.start_idle_slots();
            let processes = pool
                .workers()
                .iter()
                .map(|w| Process::open(w.pid))
                .collect();
            Running { pool, processes }
        }

        fn new(exe: PathBuf, profile: &'static str, every_args: &[&str]) -> Running {
            Running::start(unstarted(exe, profile, every_args))
        }
    }

    impl Deref for Running {
        type Target = WorkerPool;

        fn deref(&self) -> &WorkerPool {
            &self.pool
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.pool.shutdown();
            for process in &self.processes {
                let ended = process.wait_for_exit(Duration::from_secs(5));
                if !ended && !std::thread::panicking() {
                    panic!("a worker did not end after shutdown");
                }
            }
        }
    }

    fn in_appcontainer(pid: u32) -> bool {
        let process = Process::open(pid);
        let mut token = HANDLE::default();
        // SAFETY: valid process handle; `token` receives a new handle.
        unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) }.expect("OpenProcessToken");
        let token = Process(token); // closes the token handle on drop
        let (mut flag, mut len) = (0u32, 0u32);
        // SAFETY: TokenIsAppContainer is one DWORD, written to `flag`.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenIsAppContainer,
                Some(std::ptr::from_mut(&mut flag).cast()),
                4,
                &mut len,
            )
        }
        .expect("GetTokenInformation");
        flag != 0
    }

    /// A temporary copy of the worker and its DLLs in a folder that grants `ALL APPLICATION
    /// PACKAGES` read and execute access, as the installer will.
    fn staged_worker() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("lv-m5-ac-")
            .tempdir()
            .expect("temp folder");
        let built = worker_exe();
        assert!(
            !folder_readable(&dir.path().join("lv-worker.exe")).unwrap(),
            "a fresh temp folder grants AppContainers nothing"
        );
        let status = Command::new("icacls")
            .arg(dir.path())
            .args(["/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/q"])
            .status()
            .expect("icacls");
        assert!(status.success(), "icacls failed");
        for entry in std::fs::read_dir(built.parent().unwrap()).unwrap() {
            let path = entry.unwrap().path();
            let ext = path.extension().and_then(OsStr::to_str).unwrap_or("");
            if ext.eq_ignore_ascii_case("dll") || path == built {
                std::fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
            }
        }
        let exe = dir.path().join("lv-worker.exe");
        assert!(folder_readable(&exe).unwrap());
        (dir, exe)
    }

    fn probe(pool: &WorkerPool) -> String {
        match pool.decode_heif(vec![0; 16]) {
            Err(DecodeError::Corrupt(msg)) => msg,
            other => panic!(
                "expected a probe answer, got {:?}",
                other.map(|d| (d.0, d.1))
            ),
        }
    }

    #[test]
    fn worker_falls_back_without_appcontainer() {
        // A folder AppContainers cannot read: the profile is not even asked for.
        if !folder_readable(&worker_exe()).unwrap() {
            assert!(for_worker(&worker_exe(), || panic!("profile asked for")).is_none());
        }
        // A readable folder but no profile (creating it fails).
        let (_folder, exe) = staged_worker();
        let pool = Running::new(exe, BAD_PROFILE, &[]);
        assert!(pool.container().is_none(), "the bad profile is refused");
        let pid = single_worker(&pool).pid;
        assert!(!in_appcontainer(pid));
        assert!(win32k_lockdown(&pool));
        assert_decodes(&pool);
    }

    #[test]
    fn failed_appcontainer_start_keeps_mitigations() {
        let pool = unstarted(worker_exe(), PROFILE_NAME, &[]);
        let sid = profile_sid().expect("worker profile");
        // No LOCALAPPDATA: every start in this container fails.
        assert!(pool.container.set(Some(Container::broken(sid))).is_ok());
        let pool = Running::start(pool);
        let pid = single_worker(&pool).pid;
        assert!(!in_appcontainer(pid));
        assert!(win32k_lockdown(&pool), "only the AppContainer was dropped");
        assert_eq!(pool.slot_states(), vec![(0, false)], "not a start failure");
        assert!(pool.container_failed.load(Ordering::Relaxed));
        assert!(pool.container().is_none(), "later workers do not try it");
        assert_decodes(&pool);
        assert_rotated(
            pool.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")),
            (32, 64),
        );
    }

    #[test]
    fn appcontainer_worker_cannot_read_user_file() {
        let profile = PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE"));
        let mut builder = tempfile::Builder::new();
        builder.prefix("lv-m5-probe-");
        let user_dir = if std::env::temp_dir().starts_with(&profile) {
            builder.tempdir()
        } else {
            builder.tempdir_in(&profile)
        }
        .expect("temp folder under USERPROFILE");
        let secret = user_dir.path().join("secret file.txt");
        std::fs::write(&secret, b"user data").unwrap();
        // Quoted as one argument: the path has a space.
        let probe_arg = format!("\"--test-mode=probe-read={}\"", secret.display());

        // In the low integrity sandbox alone the worker can read user files.
        {
            let low = Running::new(worker_exe(), BAD_PROFILE, &[&probe_arg]);
            assert_eq!(probe(&low), "probe-read: opened");
        }

        let (_folder, exe) = staged_worker();
        {
            let boxed = Running::new(exe.clone(), PROFILE_NAME, &[&probe_arg]);
            assert!(
                in_appcontainer(single_worker(&boxed).pid),
                "the staged worker runs in the AppContainer"
            );
            let answer = probe(&boxed);
            assert!(
                answer.starts_with("probe-read: ") && answer.ends_with("(os error 5)"),
                "access denied expected: {answer}"
            );
            // Each rotation's single-use worker is boxed in too.
            match boxed.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")) {
                Err(PoolError::Failed(DecodeError::Corrupt(msg))) => {
                    assert!(
                        msg.ends_with("(os error 5)"),
                        "access denied expected: {msg}"
                    )
                }
                Err(e) => panic!("expected a probe answer, got {e}"),
                Ok(_) => panic!("expected a probe answer, got a reply"),
            }
        }

        // HEIC still opens in the AppContainer, which adds to the other layers.
        let heic = Running::new(exe, PROFILE_NAME, &[]);
        let worker = single_worker(&heic);
        assert!(in_appcontainer(worker.pid));
        assert!(win32k_lockdown(&heic), "mitigations kept");
        let mut in_job = BOOL(0);
        let process = Process::open(worker.pid);
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { IsProcessInJob(process.0, worker.job, &mut in_job) }.expect("IsProcessInJob");
        assert!(in_job.as_bool(), "Job kept");
        assert_decodes(&heic);
        assert_rotated(
            heic.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")),
            (32, 64),
        );
    }
}
