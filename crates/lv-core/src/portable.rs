//! Where light-view keeps its data (spec §9, ruling R6).
//!
//! An installed copy keeps its settings in `%APPDATA%\light-view` and its crash records in
//! `%LOCALAPPDATA%\light-view\crashes`. A portable copy is marked by a `portable.txt`
//! file beside the exe (the portable zip ships one, the installer does not); it keeps
//! everything in a `data` folder beside the exe and leaves the user profile alone.

use std::path::{Path, PathBuf};

/// The file beside the exe that makes a copy portable. Its content does not matter.
pub const MARKER: &str = "portable.txt";

/// The folder beside a portable exe that holds its data.
pub const PORTABLE_FOLDER: &str = "data";

/// The folder light-view uses inside `%APPDATA%` and `%LOCALAPPDATA%`.
pub const APP_FOLDER: &str = "light-view";

const SETTINGS_FILE: &str = "settings.toml";
const CRASHES_FOLDER: &str = "crashes";
const LIVE_BACKUPS_FOLDER: &str = "live-backups";

/// Where this copy keeps its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataDir {
    /// `portable.txt` is beside the exe: everything lives in this folder (`<exe dir>\data`).
    Portable(PathBuf),
    /// An installed copy: settings in `%APPDATA%\light-view`, crash records in
    /// `%LOCALAPPDATA%\light-view\crashes`.
    Installed,
}

/// The data folder for the exe in `exe_dir`: portable when `exe_dir\portable.txt` is a file.
pub fn data_dir(exe_dir: &Path) -> DataDir {
    if exe_dir.join(MARKER).is_file() {
        DataDir::Portable(exe_dir.join(PORTABLE_FOLDER))
    } else {
        DataDir::Installed
    }
}

impl DataDir {
    pub fn is_portable(&self) -> bool {
        matches!(self, DataDir::Portable(_))
    }

    /// The settings file. `roaming_app_data` resolves `%APPDATA%` and is asked only for an
    /// installed copy; `None` there means there is no settings file to use.
    pub fn settings_file(
        &self,
        roaming_app_data: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        Some(self.root(roaming_app_data)?.join(SETTINGS_FILE))
    }

    /// The crash record folder (ruling R6). `local_app_data` resolves `%LOCALAPPDATA%` and
    /// is asked only for an installed copy.
    pub fn crash_dir(&self, local_app_data: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
        Some(self.root(local_app_data)?.join(CRASHES_FOLDER))
    }

    /// The folder listing the folders that hold replace backups not yet committed, for
    /// crash recovery at the next start. Like the crash records it stays on this PC:
    /// `%LOCALAPPDATA%\light-view\live-backups`, or `data\live-backups` when portable.
    pub fn live_backups_dir(
        &self,
        local_app_data: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        Some(self.root(local_app_data)?.join(LIVE_BACKUPS_FOLDER))
    }

    fn root(&self, known_folder: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
        match self {
            DataDir::Portable(dir) => Some(dir.clone()),
            DataDir::Installed => Some(known_folder()?.join(APP_FOLDER)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> Option<PathBuf> {
        panic!("a portable copy must not look up a profile folder")
    }

    #[test]
    fn portable_marker_moves_data_dir() {
        let exe_dir = tempfile::tempdir().unwrap();
        std::fs::write(exe_dir.path().join("portable.txt"), "").unwrap();

        let data = data_dir(exe_dir.path());
        let expected = exe_dir.path().join("data");
        assert_eq!(data, DataDir::Portable(expected.clone()));
        assert!(data.is_portable());
        assert_eq!(
            data.settings_file(never),
            Some(expected.join("settings.toml"))
        );
        assert_eq!(data.crash_dir(never), Some(expected.join("crashes")));
        assert_eq!(
            data.live_backups_dir(never),
            Some(expected.join("live-backups"))
        );
    }

    #[test]
    fn installed_uses_appdata() {
        let exe_dir = tempfile::tempdir().unwrap();
        // A folder named like the marker is not the marker.
        std::fs::create_dir(exe_dir.path().join("portable.txt")).unwrap();

        let data = data_dir(exe_dir.path());
        assert_eq!(data, DataDir::Installed);
        assert!(!data.is_portable());
        let roaming = PathBuf::from(r"C:\Users\u\AppData\Roaming");
        let local = PathBuf::from(r"C:\Users\u\AppData\Local");
        assert_eq!(
            data.settings_file(|| Some(roaming.clone())),
            Some(roaming.join("light-view").join("settings.toml"))
        );
        assert_eq!(
            data.crash_dir(|| Some(local.clone())),
            Some(local.join("light-view").join("crashes"))
        );
        assert_eq!(
            data.live_backups_dir(|| Some(local.clone())),
            Some(local.join("light-view").join("live-backups"))
        );
        // No profile folder: no settings file, no crash records.
        assert_eq!(data.settings_file(|| None), None);
        assert_eq!(data.crash_dir(|| None), None);
        assert_eq!(data.live_backups_dir(|| None), None);
    }
}
