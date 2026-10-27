//! 사용자 설정 파일(TOML) 읽기·쓰기.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WindowPlacement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub toolbar_pinned: bool,
    pub window: Option<WindowPlacement>,
    pub open_in_new_window: bool,
    pub memory_budget_mb: Option<u32>,
    /// Save a view rotation without asking when leaving the photo.
    pub auto_save_rotation: bool,
}

/// 설정을 읽는다. 파일이 없으면 기본값, 파싱에 실패하면 원본을 `.bak`으로 옮기고 기본값을 돌려준다.
pub fn load_settings(path: &Path) -> Settings {
    // 없는 파일과 읽기 오류는 모두 기본값. 읽기 오류일 때는 원본을 건드리지 않는다.
    let Ok(text) = std::fs::read_to_string(path) else {
        return Settings::default();
    };
    match toml::from_str(&text) {
        Ok(settings) => settings,
        Err(_) => {
            // 손상된 설정은 버리지 않고 옆으로 옮겨 둔다. 옮기지 못해도 기본값으로 계속 간다.
            let _ = std::fs::rename(path, with_suffix(path, ".bak"));
            Settings::default()
        }
    }
}

/// 설정을 같은 폴더의 임시 파일에 쓴 뒤 대상 경로로 옮긴다. 폴더가 없으면 만든다.
pub fn save_settings(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let text =
        toml::to_string(s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = with_suffix(path, ".tmp");
    std::fs::write(&tmp, text)?;
    // Windows에서도 std::fs::rename은 기존 대상을 덮어쓴다.
    std::fs::rename(&tmp, path)
}

/// 파일 이름 끝에 `suffix`를 붙인 경로를 만든다 (`settings.toml` + `.bak` → `settings.toml.bak`).
fn with_suffix(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            toolbar_pinned: true,
            window: Some(WindowPlacement {
                x: 10,
                y: -20,
                width: 1280,
                height: 720,
                maximized: false,
            }),
            open_in_new_window: true,
            memory_budget_mb: Some(512),
            auto_save_rotation: true,
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        save_settings(&path, &sample()).unwrap();
        assert_eq!(load_settings(&path), sample());
    }

    #[test]
    fn missing_file_gives_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(load_settings(&path), Settings::default());
    }

    #[test]
    fn corrupt_file_is_backed_up_and_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let bak = dir.path().join("settings.toml.bak");
        std::fs::write(&path, "toolbar_pinned = [[[").unwrap();

        assert_eq!(load_settings(&path), Settings::default());
        assert!(bak.exists());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "toolbar_pinned = [[["
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            "toolbar_pinned = true\nfuture_key = \"x\"\n[future_table]\na = 1\n",
        )
        .unwrap();
        let s = load_settings(&path);
        assert!(s.toolbar_pinned);
        assert_eq!(s.window, None);
        // Settings files from before M3 ask before saving a rotation.
        assert!(!s.auto_save_rotation);
    }
}
