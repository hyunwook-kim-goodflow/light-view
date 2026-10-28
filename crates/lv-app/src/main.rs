// Release builds are GUI-subsystem (no console window); debug builds keep stderr.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod clipboard;
mod commands;
mod crash;
mod delete;
mod exif_card;
mod explorer_sort;
mod fullscreen;
mod input;
mod instance;
#[cfg(test)]
mod long_paths;
mod menu;
mod overlay;
mod placement;
mod render;
mod resize;
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

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use lv_core::portable::{self, DataDir};
use lv_core::settings::{load_settings, sweep_stale_temps};
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
/// `light-view.exe --resize "<path>"`: Explorer's 크기 조정 verb (spec §3, ruling R4).
const RESIZE: &str = "--resize";

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
    let mut raw = std::env::args_os().skip(1).peekable();
    // The uninstaller runs this before it removes the files (installer/light-view.iss).
    if raw.peek().is_some_and(|a| a == UNINSTALL_CLEANUP) {
        worker::delete_profile();
        return Ok(());
    }
    let args = parse_args(raw);
    if args.timing {
        timing::enable(start);
    }
    let bench = args.bench;
    let initial = args.initial.clone();
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
    // A crash between writing and renaming leaves a `settings.toml.<pid>.tmp` behind.
    if let Some(path) = &settings_path {
        sweep_stale_temps(path, std::time::SystemTime::now());
    }
    let mut settings = settings_path
        .as_deref()
        .map(load_settings)
        .unwrap_or_default();
    // lv-bench's WebP resize case: the defaults, but WebP (nothing is saved in `--bench`).
    if args.bench_webp {
        settings.resize = Some(lv_core::settings::ResizeMemory {
            format: lv_core::settings::ResizeFormat::WebP,
            ..Default::default()
        });
    }
    strings::set(settings.language);
    // Originals a crashed save left under their backup names come back (ruling R2).
    if !bench {
        let first = initial
            .as_deref()
            .or(args.resize.first().map(PathBuf::as_path));
        if let Some(dir) = first.and_then(photo_folder) {
            timing::step("recover-ms", || resize::save::recover_backups(&dir));
        }
    }
    let live_list = if bench {
        None
    } else {
        window::live_backups_dir(&data)
    };
    if let Some(list) = &live_list {
        resize::save::live::init(list.clone());
    }
    // Held until the window closes, so later launches forward to it. "새 창" applies to
    // viewing only: every resize launch goes to the one window (spec §10, R4).
    let _guard = if (settings.open_in_new_window && !args.is_resize()) || bench {
        None
    } else {
        let key = instance::own_instance_key();
        let items = forwarded_items(&args);
        match timing::step("instance-ms", || acquire_or_forward(&items, key.as_deref())) {
            Launch::Forwarded => return Ok(()),
            Launch::Primary(guard) => Some(guard),
        }
    };
    // Every folder a save that did not finish listed is recovered, whatever this launch
    // opens. Only a launch that keeps running does it (a forwarding one would exit in
    // the middle), on a thread of its own since a listed folder may be slow to reach.
    if let Some(list) = live_list {
        let recover = move || {
            resize::save::live::recover_listed(&list, resize::save::recover_folder);
        };
        if let Err(e) = std::thread::Builder::new()
            .name("lv-recover".into())
            .spawn(recover)
        {
            eprintln!("light-view: backup recovery not started: {e}");
        }
    }
    // COM for the UI thread, once (the shell's calls and the Open dialog), held until
    // the window is gone.
    let _com = shell::ComApartment::init();
    window::run(initial, args.resize, settings, settings_path)
}

/// What this launch hands to a running window.
fn forwarded_items(args: &Args) -> Vec<instance::Forwarded> {
    if args.is_resize() {
        args.resize
            .iter()
            .cloned()
            .map(instance::Forwarded::Resize)
            .collect()
    } else {
        args.initial
            .iter()
            .cloned()
            .map(instance::Forwarded::Open)
            .collect()
    }
}

/// The command line, after `--uninstall-cleanup` was ruled out.
#[derive(Debug, Default)]
struct Args {
    /// `--timing` (or `--bench`): print the start-up timings.
    timing: bool,
    /// `--bench` (lv-bench).
    bench: bool,
    /// `--bench-webp` (lv-bench's WebP resize case): `--bench` with the resize format WebP.
    bench_webp: bool,
    /// The photo or folder to open.
    initial: Option<PathBuf>,
    /// `--resize <path>…` (Explorer's 크기 조정 verb): the photos for the resize screen.
    resize: Vec<PathBuf>,
}

impl Args {
    fn is_resize(&self) -> bool {
        !self.resize.is_empty()
    }
}

fn parse_args(args: impl Iterator<Item = std::ffi::OsString>) -> Args {
    let mut args = args.peekable();
    let mut parsed = Args::default();
    while let Some(flag) =
        args.next_if(|a| a == "--timing" || a == "--bench" || a == "--bench-webp")
    {
        parsed.timing = true;
        parsed.bench |= flag != "--timing";
        parsed.bench_webp |= flag == "--bench-webp";
    }
    if args.next_if(|a| a == RESIZE).is_some() {
        parsed.resize = args.map(|a| shell::long_form(Path::new(&a))).collect();
    } else {
        parsed.initial = args.next().map(|a| shell::long_form(Path::new(&a)));
    }
    parsed
}

/// The folder of the photo (or the folder) the app was started with.
fn photo_folder(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        return Some(path.to_path_buf());
    }
    let parent = path.parent()?;
    Some(if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    })
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

    use super::{parse_args, Args};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn args(list: &[&str]) -> Args {
        parse_args(list.iter().map(OsString::from))
    }

    #[test]
    fn resize_flag_parses_paths() {
        // Explorer's verb: `light-view.exe --resize "<path>"`, once per file (R4); more
        // than one path after the flag is taken too.
        let a = args(&["--resize", r"C:\p\a.jpg"]);
        assert_eq!(a.resize, vec![PathBuf::from(r"C:\p\a.jpg")]);
        assert_eq!(a.initial, None);
        let a = args(&["--resize", "a.jpg", "b c.png"]);
        assert_eq!(
            a.resize,
            vec![PathBuf::from("a.jpg"), PathBuf::from("b c.png")]
        );
        assert!(a.is_resize());
        // Timing flags come first, as before.
        let a = args(&["--timing", "--resize", "a.jpg"]);
        assert!(a.timing && !a.bench);
        assert_eq!(a.resize, vec![PathBuf::from("a.jpg")]);
        // A plain launch is unchanged.
        let a = args(&["--bench", "x.jpg"]);
        assert!(a.timing && a.bench && !a.bench_webp && !a.is_resize());
        assert_eq!(a.initial, Some(PathBuf::from("x.jpg")));
        // lv-bench's WebP resize case.
        let a = args(&["--bench-webp", "--resize", "a.jpg"]);
        assert!(a.timing && a.bench && a.bench_webp);
        assert_eq!(a.resize, vec![PathBuf::from("a.jpg")]);
        assert_eq!(args(&[]).initial, None);
        // The flag with nothing after it opens nothing.
        let a = args(&["--resize"]);
        assert!(!a.is_resize() && a.initial.is_none());
        // `--resize` after a path is just another (ignored) argument.
        assert_eq!(
            args(&["a.jpg", "--resize"]).initial,
            Some(PathBuf::from("a.jpg"))
        );
    }

    #[test]
    fn recovery_looks_in_the_photo_folder() {
        use std::path::Path;
        let dir = tempfile::Builder::new().prefix("lv-m6-").tempdir().unwrap();
        assert_eq!(super::photo_folder(dir.path()).unwrap(), dir.path());
        assert_eq!(
            super::photo_folder(&dir.path().join("a.jpg")).unwrap(),
            dir.path()
        );
        assert_eq!(
            super::photo_folder(Path::new("a.jpg")).unwrap(),
            Path::new(".")
        );
    }

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
