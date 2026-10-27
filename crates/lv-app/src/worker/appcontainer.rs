//! The AppContainer the decoder worker runs in when it can (spec §6.1, ruling R2).
//!
//! An AppContainer process passes an access check only where the object's DACL also names
//! its package SID or `ALL APPLICATION PACKAGES` (S-1-15-2-1). User files carry neither, so a
//! worker taken over by a hostile file can no longer read them, and with no capabilities it
//! has no network. The worker still needs read and execute access to its own folder (the
//! exe and the DLLs it imports). The installer grants that to `ALL APPLICATION PACKAGES`;
//! a folder without it (a dev build in `target\`, a copied portable folder) makes
//! [`for_worker`] answer `None` without creating the profile, and the worker runs in the low
//! integrity sandbox alone.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    LocalFree, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GENERIC_ALL, GENERIC_EXECUTE, GENERIC_READ,
    HLOCAL,
};
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    CreateWellKnownSid, EqualSid, FreeSid, GetAce, WinBuiltinAnyPackageSid, ACCESS_ALLOWED_ACE,
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE, PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
};
use windows::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};

use super::process::Worker;
use super::sandbox::log;
use super::WorkerPool;

/// The worker's AppContainer profile. The app creates it on first use and keeps it.
pub const PROFILE_NAME: &str = "goodflow.LightView.Worker";

/// What an AppContainer needs on the worker's folder, exe and DLLs.
const READ_EXECUTE: u32 = FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0;

/// A SID allocated by the AppContainer APIs, freed with `FreeSid`.
pub struct OwnedSid(PSID);

// SAFETY: the SID is a heap block that nothing mutates after creation and that only `Drop`
// frees; reading it from several threads is fine.
unsafe impl Send for OwnedSid {}
// SAFETY: as above, shared access is read-only.
unsafe impl Sync for OwnedSid {}

impl OwnedSid {
    pub fn as_psid(&self) -> PSID {
        self.0
    }
}

/// Variables an AppContainer start passes on. Windows points `LOCALAPPDATA`, `TEMP` and
/// `TMP` at the container's own folder and refuses the start (ERROR_ENVVAR_NOT_FOUND) when
/// `LOCALAPPDATA` is missing.
const ENVIRONMENT: &[&str] = &["SystemRoot", "LOCALAPPDATA"];

/// The AppContainer a worker starts in, and what its environment holds.
pub struct Container {
    sid: OwnedSid,
    environment: &'static [&'static str],
}

impl Container {
    pub fn sid(&self) -> &OwnedSid {
        &self.sid
    }

    /// Names of the variables of ours the worker's environment holds.
    pub fn environment(&self) -> &'static [&'static str] {
        self.environment
    }

    /// A container whose start always fails (no `LOCALAPPDATA`), to test the fallback.
    #[cfg(test)]
    pub fn broken(sid: OwnedSid) -> Container {
        Container {
            sid,
            environment: &["SystemRoot"],
        }
    }
}

impl Drop for OwnedSid {
    fn drop(&mut self) {
        // SAFETY: the SID came from CreateAppContainerProfile or
        // DeriveAppContainerSidFromAppContainerName, which document FreeSid; freed once.
        unsafe { FreeSid(self.0) };
    }
}

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

/// The SID of the worker's AppContainer profile, creating the profile if needed. A
/// failure is logged.
pub fn profile_sid() -> Option<OwnedSid> {
    profile_sid_named(PROFILE_NAME)
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// The SID of AppContainer profile `name`: created, or derived if it already exists. A
/// failure is logged.
pub(super) fn profile_sid_named(name: &str) -> Option<OwnedSid> {
    match create_or_derive(name) {
        Ok(sid) => Some(sid),
        Err(e) => {
            log(&format!(
                "AppContainer profile {name:?} unavailable ({e}); workers run in the low integrity sandbox only"
            ));
            None
        }
    }
}

fn create_or_derive(name: &str) -> windows::core::Result<OwnedSid> {
    let name_w = wide(name);
    let display = wide("light-view worker");
    let description = wide("light-view image decoder (HEIF, lossless JPEG rotation)");
    // SAFETY: NUL-terminated strings that outlive the call; no capabilities. On success the
    // returned SID is ours to free.
    let created = unsafe {
        CreateAppContainerProfile(
            PCWSTR(name_w.as_ptr()),
            PCWSTR(display.as_ptr()),
            PCWSTR(description.as_ptr()),
            None,
        )
    };
    match created {
        Ok(sid) => Ok(OwnedSid(sid)),
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => {
            // SAFETY: NUL-terminated name that outlives the call; the SID is ours to free.
            let sid =
                unsafe { DeriveAppContainerSidFromAppContainerName(PCWSTR(name_w.as_ptr()))? };
            Ok(OwnedSid(sid))
        }
        Err(e) => Err(e),
    }
}

/// The AppContainer to start `exe` in, else `None`, saying which in the log (the pool asks
/// once). It needs `LOCALAPPDATA` set and `exe`'s folder letting `ALL APPLICATION PACKAGES`
/// read and run it; only then is `profile` asked for the SID (normally [`profile_sid`],
/// which creates the profile), so runs that cannot use it leave no profile behind.
pub fn for_worker(exe: &Path, profile: impl FnOnce() -> Option<OwnedSid>) -> Option<Container> {
    if std::env::var_os("LOCALAPPDATA").is_none() {
        log("LOCALAPPDATA is not set; workers run in the low integrity sandbox only");
        return None;
    }
    match folder_readable(exe) {
        Ok(true) => {
            let sid = profile()?;
            log("workers run in an AppContainer");
            Some(Container {
                sid,
                environment: ENVIRONMENT,
            })
        }
        Ok(false) => {
            let dir = exe.parent().unwrap_or(exe);
            log(&format!(
                "{} gives AppContainers no read access; workers run in the low integrity sandbox only",
                dir.display()
            ));
            None
        }
        Err(e) => {
            log(&format!(
                "cannot check the worker folder's access ({e}); workers run in the low integrity sandbox only"
            ));
            None
        }
    }
}

/// Whether every AppContainer may read and execute `exe`, its folder and every DLL in it
/// (by their DACLs: an allow entry for `ALL APPLICATION PACKAGES` covering read+execute,
/// and no deny entry for it touching that).
pub fn folder_readable(exe: &Path) -> Result<bool, String> {
    let dir = exe
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", exe.display()))?;
    let mut paths: Vec<PathBuf> = vec![dir.to_path_buf(), exe.to_path_buf()];
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        let is_dll = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"));
        if is_dll {
            paths.push(path);
        }
    }
    let any_package = any_package_sid().map_err(|e| format!("CreateWellKnownSid: {e}"))?;
    let any_package = PSID(any_package.as_ptr().cast_mut().cast());
    for path in paths {
        if !grants_read_execute(&path, any_package)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `ALL APPLICATION PACKAGES` (S-1-15-2-1) in a SID-aligned buffer.
fn any_package_sid() -> windows::core::Result<[u32; 17]> {
    // SECURITY_MAX_SID_SIZE is 68 bytes; u32 elements give the SID its 4-byte alignment.
    let mut buf = [0u32; 17];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf` holds `len` writable bytes.
    unsafe {
        CreateWellKnownSid(
            WinBuiltinAnyPackageSid,
            PSID::default(),
            PSID(buf.as_mut_ptr().cast()),
            &mut len,
        )?
    };
    Ok(buf)
}

/// A security descriptor from GetNamedSecurityInfoW, freed with LocalFree.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by GetNamedSecurityInfoW, which documents LocalFree; freed once.
        unsafe { LocalFree(HLOCAL(self.0 .0)) };
    }
}

/// File access rights an ACE mask grants, with the generic rights mapped.
fn file_rights(mask: u32) -> u32 {
    let mut rights = mask;
    if mask & GENERIC_ALL.0 != 0 {
        rights |= FILE_ALL_ACCESS.0;
    }
    if mask & GENERIC_READ.0 != 0 {
        rights |= FILE_GENERIC_READ.0;
    }
    if mask & GENERIC_EXECUTE.0 != 0 {
        rights |= FILE_GENERIC_EXECUTE.0;
    }
    rights
}

/// Whether `path`'s DACL lets `sid` read and execute it. Deny entries count wherever they
/// are, which can only make the answer more cautious.
fn grants_read_execute(path: &Path, sid: PSID) -> Result<bool, String> {
    let path_w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: NUL-terminated path; out-pointers to locals. On success `sd` is ours to free
    // and `dacl` points into it.
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path_w.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "GetNamedSecurityInfoW({}): {}",
            path.display(),
            windows::core::Error::from(status.to_hresult())
        ));
    }
    let _sd = Descriptor(sd);
    if dacl.is_null() {
        return Ok(true); // a NULL DACL grants everyone everything
    }
    let (mut allowed, mut denied) = (0u32, 0u32);
    // SAFETY: `dacl` is a valid ACL inside `_sd`, which lives until the end of the function.
    let count = u32::from(unsafe { (*dacl).AceCount });
    for index in 0..count {
        let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: valid ACL and an index below its ACE count; `ace` points into the ACL.
        unsafe { GetAce(dacl, index, &mut ace) }.map_err(|e| format!("GetAce: {e}"))?;
        // SAFETY: every ACE starts with an ACE_HEADER.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE.0 != 0 {
            continue;
        }
        let kind = u32::from(header.AceType);
        if kind != ACCESS_ALLOWED_ACE_TYPE && kind != ACCESS_DENIED_ACE_TYPE {
            continue;
        }
        // SAFETY: allowed and denied ACEs share ACCESS_ALLOWED_ACE's layout: header, mask,
        // then the SID starting at `SidStart`.
        let (mask, ace_sid) = unsafe {
            let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            (
                ace.Mask,
                PSID(std::ptr::from_ref(&ace.SidStart).cast_mut().cast()),
            )
        };
        // SAFETY: both SIDs are valid for the duration of the call.
        if unsafe { EqualSid(sid, ace_sid) }.is_err() {
            continue;
        }
        if kind == ACCESS_ALLOWED_ACE_TYPE {
            allowed |= file_rights(mask);
        } else {
            denied |= file_rights(mask);
        }
    }
    Ok(allowed & READ_EXECUTE == READ_EXECUTE && denied & READ_EXECUTE == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ops::Deref;
    use std::process::Command;
    use std::time::Duration;

    use windows::Win32::Foundation::{BOOL, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenIsAppContainer, TOKEN_QUERY};
    use windows::Win32::System::JobObjects::IsProcessInJob;
    use windows::Win32::System::Threading::OpenProcessToken;

    use lv_core::decode::DecodeError;

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
