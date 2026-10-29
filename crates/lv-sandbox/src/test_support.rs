//! Helpers for this crate's tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE,
};

/// `lv-worker.exe` next to the test binary's profile folder (`target/<profile>`).
pub(crate) fn worker_exe() -> PathBuf {
    let test_exe = std::env::current_exe().expect("current exe");
    // target/<profile>/deps/lv_sandbox-<hash>.exe
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .expect("test exe lives in target/<profile>/deps");
    let exe = profile_dir.join("lv-worker.exe");
    assert!(
        exe.is_file(),
        "{} is missing: run `cargo build -p lv-worker` first",
        exe.display()
    );
    exe
}

/// An owned process handle for `pid`, closed on drop.
pub(crate) struct Process(pub(crate) HANDLE);

impl Process {
    pub(crate) fn open(pid: u32) -> Process {
        // SAFETY: plain call; the returned handle is owned by `Process` and closed on drop.
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) }
                .unwrap_or_else(|e| panic!("OpenProcess({pid}): {e}"));
        Process(handle)
    }

    pub(crate) fn wait_for_exit(&self, timeout: Duration) -> bool {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `self.0` is a valid process handle with SYNCHRONIZE access.
        unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from OpenProcess and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
