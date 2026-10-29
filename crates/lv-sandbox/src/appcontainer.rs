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

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    LocalFree, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GENERIC_ALL, GENERIC_EXECUTE, GENERIC_READ,
    HLOCAL,
};
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    CreateWellKnownSid, EqualSid, FreeSid, GetAce, WinBuiltinAnyPackageSid, ACCESS_ALLOWED_ACE,
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE, PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
};
use windows::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};

use crate::sandbox::log;

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
    #[cfg(any(test, feature = "test-support"))]
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
pub fn profile_sid_named(name: &str) -> Option<OwnedSid> {
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

/// Deletes the worker's AppContainer profile and the folders Windows made for it
/// (`light-view.exe --uninstall-cleanup`, run by the uninstaller). A profile that is not
/// there is fine; a failure is only logged.
pub fn delete_profile() {
    delete_profile_named(PROFILE_NAME);
}

fn delete_profile_named(name: &str) {
    let name_w = wide(name);
    // SAFETY: NUL-terminated name that outlives the call.
    if let Err(e) = unsafe { DeleteAppContainerProfile(PCWSTR(name_w.as_ptr())) } {
        log(&format!(
            "could not delete AppContainer profile {name:?}: {e}"
        ));
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

    #[test]
    fn uninstall_cleanup_deletes_profile() {
        // Not PROFILE_NAME: other tests start workers in that one.
        const NAME: &str = "goodflow.LightView.Worker.CleanupTest";
        drop(profile_sid_named(NAME).expect("profile created"));
        delete_profile_named(NAME);

        // Gone: creating it again succeeds instead of answering ERROR_ALREADY_EXISTS.
        let name_w = wide(NAME);
        // SAFETY: NUL-terminated strings that outlive the call; the SID is freed below.
        let created = unsafe {
            CreateAppContainerProfile(
                PCWSTR(name_w.as_ptr()),
                PCWSTR(name_w.as_ptr()),
                PCWSTR(name_w.as_ptr()),
                None,
            )
        };
        drop(OwnedSid(created.expect("the profile was deleted")));

        delete_profile_named(NAME);
        // Deleting a profile that is not there does not fail or panic.
        delete_profile_named(NAME);
    }
}
