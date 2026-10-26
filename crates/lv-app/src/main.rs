// Release builds are GUI-subsystem (no console window); debug builds keep stderr.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod fullscreen;
mod input;
mod instance;
mod placement;
mod render;
mod theme;
mod toolbar;
mod tooltip;
mod viewer;
mod window;

use std::path::PathBuf;

use anyhow::Context;
use lv_core::settings::load_settings;
use windows::core::w;
use windows::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

use crate::instance::{acquire_or_forward, Launch};

fn main() -> anyhow::Result<()> {
    // SAFETY: plain call with valid flags, made before any other DLL is loaded on demand.
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)
    }
    .context("SetDefaultDllDirectories")?;
    // SAFETY: plain call with a static string, before any window exists.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(w!("goodflow.LightView")) } {
        eprintln!("light-view: could not set the AppUserModelID: {e}");
    }
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    let settings_path = window::settings_path();
    let settings = settings_path
        .as_deref()
        .map(load_settings)
        .unwrap_or_default();
    // Held until the window closes, so later launches forward to it.
    let _guard = if settings.open_in_new_window {
        None
    } else {
        match acquire_or_forward(initial.as_deref()) {
            Launch::Forwarded => return Ok(()),
            Launch::Primary(guard) => Some(guard),
        }
    };
    window::run(initial, settings, settings_path)
}
