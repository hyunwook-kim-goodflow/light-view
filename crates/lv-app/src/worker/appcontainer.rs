//! The AppContainer the decoder worker runs in when it can (spec §6.1, ruling R2).
//!
//! An AppContainer process passes an access check only where the object's DACL also names
//! its package SID or `ALL APPLICATION PACKAGES` (S-1-15-2-1). User files carry neither, so a
//! worker taken over by a hostile file can no longer read them, and with no capabilities it
//! has no network. The worker still needs read and execute access to its own folder (the
//! exe and the DLLs it imports). The installer grants that to `ALL APPLICATION PACKAGES`;
//! a folder without it (a dev build in `target\`, a copied portable folder) makes
//! [`for_worker`] answer `None`, and the worker runs in the low integrity sandbox alone.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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

use super::sandbox::log;

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

impl Drop for OwnedSid {
    fn drop(&mut self) {
        // SAFETY: the SID came from CreateAppContainerProfile or
        // DeriveAppContainerSidFromAppContainerName, which document FreeSid; freed once.
        unsafe { FreeSid(self.0) };
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

/// The AppContainer to start `exe` in: the profile's `sid` (from [`profile_sid`]) when there
/// is one and the exe's folder lets AppContainers read and run it, else `None`. Says which
/// in the log (the pool asks once).
pub fn for_worker(exe: &Path, sid: Option<OwnedSid>) -> Option<OwnedSid> {
    let sid = sid?;
    match folder_readable(&sid, exe) {
        Ok(true) => {
            log("workers run in an AppContainer");
            Some(sid)
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

/// Whether AppContainer `sid` may read and execute `exe`, its folder and every DLL in it
/// (by their DACLs: an allow entry for `sid` or `ALL APPLICATION PACKAGES` covering
/// read+execute, and no deny entry for either touching it).
pub fn folder_readable(sid: &OwnedSid, exe: &Path) -> Result<bool, String> {
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
        if !grants_read_execute(&path, &[sid.as_psid(), any_package])? {
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

/// Whether `path`'s DACL lets one of `sids` read and execute it. Deny entries count wherever
/// they are, which can only make the answer more cautious.
fn grants_read_execute(path: &Path, sids: &[PSID]) -> Result<bool, String> {
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
        if !sids
            .iter()
            .any(|&sid| unsafe { EqualSid(sid, ace_sid) }.is_ok())
        {
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

    fn pool_with(exe: PathBuf, profile: &'static str, every_args: &[&str]) -> WorkerPool {
        let pool =
            WorkerPool::unstarted(exe, 1, MAX_PIXELS, Duration::from_secs(10), &[], every_args);
        let pool = WorkerPool {
            profile_name: profile,
            ..pool
        };
        pool.start_idle_slots();
        pool
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
    /// PACKAGES` read and execute access, as the installer does.
    fn staged_worker() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("lv-m5-ac-")
            .tempdir()
            .expect("temp folder");
        let built = worker_exe();
        let sid = profile_sid().expect("worker profile");
        assert!(
            !folder_readable(&sid, &dir.path().join("lv-worker.exe")).unwrap(),
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
        assert!(folder_readable(&sid, &exe).unwrap());
        (dir, exe)
    }

    /// Ends the pool's workers and waits for them, so their folder can be deleted.
    fn finish(pool: WorkerPool, pids: &[u32]) {
        let processes: Vec<Process> = pids.iter().map(|&pid| Process::open(pid)).collect();
        pool.shutdown();
        drop(pool);
        for process in processes {
            assert!(process.wait_for_exit(Duration::from_secs(5)));
        }
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
        let pool = pool_with(worker_exe(), BAD_PROFILE, &[]);
        assert!(pool.container().is_none(), "the bad profile is refused");
        let pid = single_worker(&pool).pid;
        assert!(!in_appcontainer(pid));
        assert_decodes(&pool);
        finish(pool, &[pid]);
    }

    #[test]
    fn appcontainer_worker_cannot_read_user_file() {
        let user_dir = tempfile::Builder::new()
            .prefix("lv-m5-probe-")
            .tempdir()
            .expect("temp folder");
        let profile = std::env::var_os("USERPROFILE").expect("USERPROFILE");
        assert!(
            user_dir.path().starts_with(&profile),
            "TEMP is under USERPROFILE"
        );
        let secret = user_dir.path().join("secret.txt");
        std::fs::write(&secret, b"user data").unwrap();
        let secret = secret.to_str().unwrap();
        assert!(
            !secret.contains(' '),
            "the probe path goes on the command line unquoted"
        );
        let probe_arg = format!("--test-mode=probe-read={secret}");

        // In the low integrity sandbox alone the worker can read user files.
        let low = pool_with(worker_exe(), BAD_PROFILE, &[&probe_arg]);
        let low_pid = single_worker(&low).pid;
        assert_eq!(probe(&low), "probe-read: opened");
        finish(low, &[low_pid]);

        let (folder, exe) = staged_worker();
        let boxed = pool_with(exe.clone(), PROFILE_NAME, &[&probe_arg]);
        let boxed_pid = single_worker(&boxed).pid;
        assert!(
            in_appcontainer(boxed_pid),
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
        finish(boxed, &[boxed_pid]);

        // HEIC still opens in the AppContainer, which adds to the other layers.
        let heic = pool_with(exe, PROFILE_NAME, &[]);
        let worker = single_worker(&heic);
        let heic_pid = worker.pid;
        assert!(in_appcontainer(heic_pid));
        assert!(win32k_lockdown(&heic), "mitigations kept");
        let mut in_job = BOOL(0);
        let process = Process::open(heic_pid);
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { IsProcessInJob(process.0, worker.job, &mut in_job) }.expect("IsProcessInJob");
        assert!(in_job.as_bool(), "Job kept");
        drop(process);
        assert_decodes(&heic);
        assert_rotated(
            heic.rotate_jpeg(1, jpeg_sample("gradient-64x32.jpg")),
            (32, 64),
        );
        finish(heic, &[heic_pid]);
        drop(folder);
    }
}
