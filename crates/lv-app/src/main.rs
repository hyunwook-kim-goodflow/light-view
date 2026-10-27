// Release builds are GUI-subsystem (no console window); debug builds keep stderr.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod clipboard;
mod commands;
mod crash;
mod delete;
mod exif_card;
mod fullscreen;
mod input;
mod instance;
mod menu;
mod overlay;
mod placement;
mod render;
mod routing;
mod save;
mod settings_win;
mod shell;
mod start;
mod strings;
mod theme;
mod timing;
mod toolbar;
mod tooltip;
mod uia;
mod uia_provider;
mod update;
mod viewer;
mod watch;
mod window;
mod worker;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use lv_core::portable::{self, DataDir};
use lv_core::settings::load_settings;
use windows::core::w;
use windows::Win32::System::Diagnostics::Debug::{
    SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
};
use windows::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

use crate::instance::{acquire_or_forward, Launch};

/// `light-view.exe --uninstall-cleanup`: deletes the worker's AppContainer profile and
/// exits with 0, whatever happened, without showing anything.
const UNINSTALL_CLEANUP: &str = "--uninstall-cleanup";

fn main() -> anyhow::Result<()> {
    let start = Instant::now();
    quiet_system_errors();
    // SAFETY: plain call with valid flags, made before any other DLL is loaded on demand.
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)
    }
    .context("SetDefaultDllDirectories")?;
    // SAFETY: plain call with a static string, before any window exists.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(w!("goodflow.LightView")) } {
        eprintln!("light-view: could not set the AppUserModelID: {e}");
    }
    let mut args = std::env::args_os().skip(1).peekable();
    // The uninstaller runs this before it removes the files (installer/light-view.iss).
    if args.peek().is_some_and(|a| a == UNINSTALL_CLEANUP) {
        worker::delete_profile();
        return Ok(());
    }
    let mut bench = false;
    while let Some(flag) = args.next_if(|a| a == "--timing" || a == "--bench") {
        timing::enable(start);
        bench |= flag == "--bench";
    }
    let initial = args.next().map(PathBuf::from);
    // `--bench` (lv-bench): default settings, nothing saved, no single-instance check,
    // so a measurement neither depends on nor changes the user's settings and windows.
    let data = data_dir();
    let settings_path = if bench {
        None
    } else {
        window::settings_path(&data)
    };
    // Crash records (ruling R6); a measurement run leaves none.
    if !bench {
        if let Some(dir) = window::crash_dir(&data) {
            crash::install(&dir);
        }
    }
    let settings = settings_path
        .as_deref()
        .map(load_settings)
        .unwrap_or_default();
    strings::set(settings.language);
    // Held until the window closes, so later launches forward to it.
    let _guard = if settings.open_in_new_window || bench {
        None
    } else {
        match timing::step("instance-ms", || acquire_or_forward(initial.as_deref())) {
            Launch::Forwarded => return Ok(()),
            Launch::Primary(guard) => Some(guard),
        }
    };
    // COM for the UI thread, once (the shell's calls and the Open dialog), held until
    // the window is gone.
    let _com = shell::ComApartment::init();
    window::run(initial, settings, settings_path)
}

/// Where this copy keeps settings and crash records: beside the exe when `portable.txt` is
/// there (spec §9), else in the user profile.
fn data_dir() -> DataDir {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(portable::data_dir(exe.parent()?)))
        .unwrap_or(DataDir::Installed)
}

/// Turns off Windows' "System Error" boxes (a missing DLL, say) and the crash dialog for
/// this process. HEIF workers inherit the mode (the sandbox does not pass
/// CREATE_DEFAULT_ERROR_MODE), which matters because a worker missing `heif.dll` fails
/// before its own code runs: it must exit and count as a start failure, not block on a
/// dialog the user has to dismiss.
fn quiet_system_errors() {
    // SAFETY: plain call with valid flags; it only changes this process's error mode.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Diagnostics::Debug::{
        GetErrorMode, SetErrorMode, THREAD_ERROR_MODE,
    };

    #[test]
    fn system_error_dialogs_are_off() {
        // Start from the default mode: the test runner may have inherited a quiet one.
        // SAFETY: plain calls; the previous mode is put back below.
        let previous = unsafe { SetErrorMode(THREAD_ERROR_MODE(0)) };
        super::quiet_system_errors();
        // SAFETY: plain calls, as above.
        let mode = unsafe { GetErrorMode() };
        unsafe { SetErrorMode(previous) };
        assert_eq!(mode & 0b11, 0b11, "error mode {mode:#x}");
    }
}
