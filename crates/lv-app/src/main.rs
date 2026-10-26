// Release builds are GUI-subsystem (no console window); debug builds keep stderr.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod placement;
mod render;
mod theme;
mod toolbar;
mod tooltip;
mod window;

use std::path::PathBuf;

use anyhow::Context;
use windows::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

fn main() -> anyhow::Result<()> {
    // SAFETY: plain call with valid flags, made before any other DLL is loaded on demand.
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)
    }
    .context("SetDefaultDllDirectories")?;
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    window::run(initial)
}
