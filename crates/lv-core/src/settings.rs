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

/// UI language choice. "auto" follows the Windows UI language; an unknown value in the
/// file, or one that is not a string, falls back to `Auto`.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LangSetting {
    #[default]
    Auto,
    Ko,
    En,
}

impl<'de> Deserialize<'de> for LangSetting {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Any value is accepted, so a wrong type never resets the whole file.
        Ok(match toml::Value::deserialize(d)? {
            toml::Value::String(s) if s == "ko" => LangSetting::Ko,
            toml::Value::String(s) if s == "en" => LangSetting::En,
            _ => LangSetting::Auto,
        })
    }
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
    pub language: LangSetting,
    /// Look for a new release once a day (spec §6.6). `None`: not asked yet, so no
    /// check is made until the user answers.
    pub update_check: Option<bool>,
    /// When the last update check started, in seconds since the Unix epoch.
    pub last_update_check: Option<u64>,
    /// The last resize settings (spec §8); `None` until the resize screen was used.
    pub resize: Option<ResizeMemory>,
    /// Which side of the resize screen the sidebar is on.
    pub sidebar_side: SidebarSide,
}

/// Where the resize screen's sidebar is (spec §4).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SidebarSide {
    Left,
    #[default]
    Right,
}

impl<'de> Deserialize<'de> for SidebarSide {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match toml::Value::deserialize(d)? {
            toml::Value::String(s) if s == "left" => SidebarSide::Left,
            _ => SidebarSide::Right,
        })
    }
}

/// The resize screen's tabs: what the result must meet.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResizeTab {
    /// 해상도: a side at most `long_edge` pixels.
    #[default]
    Resolution,
    /// 용량: at most `max_mb_tenths` / 10 MB, the long edge at most `limit_long_edge`.
    #[serde(rename = "filesize")]
    FileSize,
    /// 비율: `percent` of each side.
    Scale,
}

/// Which side the 해상도 tab's number limits.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResizeAxis {
    #[default]
    #[serde(rename = "longedge")]
    LongEdge,
    Width,
    Height,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResizeFormat {
    #[default]
    Jpeg,
    Png,
    #[serde(rename = "webp")]
    WebP,
}

/// The resize screen's last settings: every tab's number, the format, the quality and
/// the location choice. How to save (new file or replace) is deliberately not here:
/// every session starts with "새 파일" (spec §8).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeMemory {
    pub tab: ResizeTab,
    /// The 해상도 tab's limit in pixels, on `axis`.
    pub long_edge: u32,
    pub axis: ResizeAxis,
    /// The 용량 tab's limit in tenths of a megabyte (1 MB = 1,000,000 bytes).
    pub max_mb_tenths: u32,
    /// The 용량 tab's optional long-edge limit.
    pub limit_long_edge: Option<u32>,
    /// The 비율 tab's percentage, 1..=100.
    pub percent: u8,
    pub format: ResizeFormat,
    /// `None`: 원본 수준; else a fixed quality, 60..=100.
    pub quality: Option<u8>,
    pub strip_gps: bool,
}

impl Default for ResizeMemory {
    fn default() -> Self {
        ResizeMemory {
            tab: ResizeTab::Resolution,
            long_edge: 1920,
            axis: ResizeAxis::LongEdge,
            max_mb_tenths: 20,
            limit_long_edge: None,
            percent: 50,
            format: ResizeFormat::Jpeg,
            quality: None,
            strip_gps: true,
        }
    }
}

/// The lowest fixed quality (spec §10's floor).
pub const MIN_QUALITY: u8 = 60;

impl<'de> Deserialize<'de> for ResizeMemory {
    /// Field by field: a missing or wrong value takes its default, so a hand-edited
    /// section never resets the rest of the file. Unknown keys (an old `replace`) are
    /// ignored.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = toml::Value::deserialize(d)?;
        let d = ResizeMemory::default();
        let Some(t) = value.as_table() else {
            return Ok(d);
        };
        let text = |k: &str| t.get(k).and_then(toml::Value::as_str);
        let int = |k: &str, lo: i64, hi: i64| {
            t.get(k)
                .and_then(toml::Value::as_integer)
                .filter(|v| (lo..=hi).contains(v))
        };
        let px = |k: &str| int(k, 1, 65_535).map(|v| v as u32);
        Ok(ResizeMemory {
            tab: match text("tab") {
                Some("filesize") => ResizeTab::FileSize,
                Some("scale") => ResizeTab::Scale,
                _ => d.tab,
            },
            long_edge: px("long_edge").unwrap_or(d.long_edge),
            axis: match text("axis") {
                Some("width") => ResizeAxis::Width,
                Some("height") => ResizeAxis::Height,
                _ => d.axis,
            },
            max_mb_tenths: int("max_mb_tenths", 1, 100_000).map_or(d.max_mb_tenths, |v| v as u32),
            limit_long_edge: px("limit_long_edge"),
            percent: int("percent", 1, 100).map_or(d.percent, |v| v as u8),
            format: match text("format") {
                Some("png") => ResizeFormat::Png,
                Some("webp") => ResizeFormat::WebP,
                _ => d.format,
            },
            quality: int("quality", i64::from(MIN_QUALITY), 100).map(|v| v as u8),
            strip_gps: t
                .get("strip_gps")
                .and_then(toml::Value::as_bool)
                .unwrap_or(d.strip_gps),
        })
    }
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
    let tmp = temp_path(path);
    let written = std::fs::write(&tmp, text).and_then(|()| {
        // Windows에서도 std::fs::rename은 기존 대상을 덮어쓴다.
        std::fs::rename(&tmp, path)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The temp file a save writes before renaming it into place: one per process
/// (`settings.toml.<pid>.tmp`), so windows in separate processes saving at once never
/// write into or rename each other's.
fn temp_path(path: &Path) -> std::path::PathBuf {
    with_suffix(path, &format!(".{}.tmp", std::process::id()))
}

/// A temp file younger than this may belong to a save running in another process.
const STALE_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// Deletes the `settings.toml.<pid>.tmp` files a process left behind when it crashed
/// between writing and renaming (a failed write or rename removes its own). Called at
/// start-up, before this process saves anything. Only that exact name pattern beside
/// `path` is touched, and only a file older than a minute or carrying this process's id
/// (a dead process that had the same id left it); a fresh file of another id may be a
/// save in progress. Returns how many were deleted.
pub fn sweep_stale_temps(path: &Path, now: std::time::SystemTime) -> usize {
    let (Some(dir), Some(base)) = (path.parent(), path.file_name()) else {
        return 0;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let base = base.to_string_lossy();
    let mut deleted = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| temp_pid(n, &base)) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= STALE_TEMP_AGE);
        if (old || pid == std::process::id()) && std::fs::remove_file(entry.path()).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// The process id in `<base>.<pid>.tmp` (the name `temp_path` makes), or `None`.
fn temp_pid(name: &str, base: &str) -> Option<u32> {
    let digits = name
        .strip_prefix(base)?
        .strip_prefix('.')?
        .strip_suffix(".tmp")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Changes only what `change` sets: reads the file as it is now (another window in
/// another process may have saved since this one started), applies `change`, and
/// writes it back atomically. Returns what was written.
pub fn update_settings(
    path: &Path,
    change: impl FnOnce(&mut Settings),
) -> std::io::Result<Settings> {
    let mut s = load_settings(path);
    change(&mut s);
    save_settings(path, &s)?;
    Ok(s)
}

impl Settings {
    /// Takes the fields this process does not apply while it runs (the new-window
    /// choice, the memory limit and the placement) from `written`, and the update
    /// answer and last check time, which any window may have written last. The
    /// language, the pin and auto-save stay as this window applied them.
    pub fn sync_from(&mut self, written: &Settings) {
        self.open_in_new_window = written.open_in_new_window;
        self.memory_budget_mb = written.memory_budget_mb;
        self.window = written.window.clone();
        self.update_check = written.update_check;
        self.last_update_check = written.last_update_check;
    }
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

    #[test]
    fn stale_temp_files_are_swept_at_start() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let now = SystemTime::now();
        let make = |name: &str, age: Duration| {
            let p = dir.path().join(name);
            std::fs::write(&p, "x").unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(now - age).unwrap();
            p
        };
        let other = std::process::id().wrapping_add(1);
        let old_other = make(
            &format!("settings.toml.{other}.tmp"),
            Duration::from_secs(120),
        );
        let fresh_other = make(&format!("settings.toml.{other}.tmp2"), Duration::ZERO);
        let fresh_live = make(
            &format!("settings.toml.{}.tmp", other.wrapping_add(1)),
            Duration::from_secs(5),
        );
        let fresh_own = make(
            &format!("settings.toml.{}.tmp", std::process::id()),
            Duration::ZERO,
        );
        // Names outside the pattern stay, however old.
        let keep: Vec<_> = [
            "settings.toml",
            "settings.toml.bak",
            "settings.toml..tmp",
            "settings.toml.12a.tmp",
            "settings.toml.12.TMP",
            "other.toml.12.tmp",
            "xsettings.toml.12.tmp",
        ]
        .iter()
        .map(|n| make(n, Duration::from_secs(3600)))
        .collect();
        assert_eq!(sweep_stale_temps(&path, now), 2);
        assert!(!old_other.exists());
        assert!(!fresh_own.exists());
        // A fresh file of another process may be a save in progress.
        assert!(fresh_live.exists());
        assert!(fresh_other.exists());
        for p in &keep {
            assert!(p.exists(), "{}", p.display());
        }
        // A missing folder is fine.
        assert_eq!(
            sweep_stale_temps(&dir.path().join("none").join("settings.toml"), now),
            0
        );
    }

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
            language: LangSetting::En,
            update_check: Some(true),
            last_update_check: Some(1_790_000_000),
            resize: Some(ResizeMemory::default()),
            sidebar_side: SidebarSide::Left,
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        save_settings(&path, &sample()).unwrap();
        assert_eq!(load_settings(&path), sample());
    }

    /// Windows in separate processes may save at the same moment: each writes its own
    /// temp file (`settings.toml.<pid>.tmp`), so neither renames the other's half-written
    /// one into place, and a temp file of another process is left alone.
    #[test]
    fn each_process_saves_through_its_own_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let own = dir
            .path()
            .join(format!("settings.toml.{}.tmp", std::process::id()));
        assert_eq!(temp_path(&path), own);
        let other = dir.path().join("settings.toml.4294967295.tmp");
        std::fs::write(&other, b"another process, mid-write").unwrap();
        save_settings(&path, &sample()).unwrap();
        assert_eq!(load_settings(&path), sample());
        assert!(!own.exists());
        assert_eq!(
            std::fs::read(&other).unwrap(),
            b"another process, mid-write"
        );
    }

    /// Two windows in separate processes ("새 창") each keep their own Settings: a
    /// save writes only its own fields, so neither undoes the other's change.
    #[test]
    fn field_saves_keep_another_writers_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        save_settings(&path, &Settings::default()).unwrap();
        // Window A starts; window B (another process) changes the language.
        let mut a = load_settings(&path);
        update_settings(&path, |s| s.language = LangSetting::En).unwrap();
        // A saves its placement: B's language stays.
        let place = sample().window;
        let written = update_settings(&path, |s| s.window = place.clone()).unwrap();
        assert_eq!(written.language, LangSetting::En);
        assert_eq!(written.window, place);
        assert_eq!(load_settings(&path), written);
        // And the other way: B saves a placement after A changed the memory limit.
        update_settings(&path, |s| s.memory_budget_mb = Some(1024)).unwrap();
        let written = update_settings(&path, |s| s.window = None).unwrap();
        assert_eq!(written.memory_budget_mb, Some(1024));
        assert_eq!(written.language, LangSetting::En);
        // A refreshes the fields it does not apply live from what was written; the
        // live ones stay as A has them.
        a.toolbar_pinned = true;
        a.sync_from(&written);
        assert_eq!(a.memory_budget_mb, Some(1024));
        assert_eq!(a.window, None);
        assert_eq!(a.language, LangSetting::Auto);
        assert!(a.toolbar_pinned);
    }

    /// The last resize settings come back on the next start; how to save (new file or
    /// replace) is never kept, so every session starts with "새 파일" (spec §8).
    #[test]
    fn settings_resize_memory_round_trips_without_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let memory = ResizeMemory {
            tab: ResizeTab::FileSize,
            long_edge: 1280,
            axis: ResizeAxis::Height,
            max_mb_tenths: 35,
            limit_long_edge: Some(2400),
            percent: 25,
            format: ResizeFormat::WebP,
            quality: Some(82),
            strip_gps: false,
        };
        let s = Settings {
            resize: Some(memory),
            sidebar_side: SidebarSide::Left,
            ..Settings::default()
        };
        save_settings(&path, &s).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("replace"), "{text}");
        assert!(text.contains("sidebar_side = \"left\""), "{text}");
        let back = load_settings(&path);
        assert_eq!(back.resize, Some(memory));
        assert_eq!(back.sidebar_side, SidebarSide::Left);
        // Defaults (spec §8, global constraints): 해상도 tab, long edge 1920, 2 MB,
        // 50 %, JPEG at the original's quality, location removed; sidebar on the right.
        let d = ResizeMemory::default();
        assert_eq!(
            (
                d.tab,
                d.long_edge,
                d.axis,
                d.max_mb_tenths,
                d.limit_long_edge
            ),
            (ResizeTab::Resolution, 1920, ResizeAxis::LongEdge, 20, None)
        );
        assert_eq!(
            (d.percent, d.format, d.quality),
            (50, ResizeFormat::Jpeg, None)
        );
        assert!(d.strip_gps);
        assert_eq!(Settings::default().sidebar_side, SidebarSide::Right);
        assert_eq!(Settings::default().resize, None);
    }

    /// A damaged or hand-edited resize section never resets the rest of the file: wrong
    /// values fall back to their defaults one by one.
    #[test]
    fn bad_resize_values_fall_back_without_losing_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            "toolbar_pinned = true\nsidebar_side = 3\n[resize]\ntab = \"nope\"\n\
             long_edge = \"big\"\npercent = 0\nmax_mb_tenths = 0\nformat = \"png\"\n\
             quality = 300\nreplace = true\n",
        )
        .unwrap();
        let s = load_settings(&path);
        assert!(s.toolbar_pinned);
        assert!(path.exists());
        assert_eq!(s.sidebar_side, SidebarSide::Right);
        let r = s.resize.expect("the section is kept");
        assert_eq!(r.tab, ResizeTab::Resolution);
        assert_eq!(r.long_edge, 1920);
        assert_eq!(r.percent, 50);
        assert_eq!(r.max_mb_tenths, 20);
        assert_eq!(r.format, ResizeFormat::Png);
        assert_eq!(r.quality, None);
        // Not a table at all: the defaults.
        std::fs::write(&path, "toolbar_pinned = true\nresize = 5\n").unwrap();
        let s = load_settings(&path);
        assert!(s.toolbar_pinned);
        assert_eq!(s.resize, Some(ResizeMemory::default()));
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
        assert_eq!(s.language, LangSetting::Auto);
        // Files from before M5 have not been asked about update checks yet.
        assert_eq!(s.update_check, None);
        assert_eq!(s.last_update_check, None);
    }

    /// The update answer is three-state: unanswered (no key), on, off. Another
    /// window's answer and check time are taken over on the next field save.
    #[test]
    fn update_answer_round_trips_and_syncs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        save_settings(&path, &Settings::default()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("update_check"), "{text}");
        for answer in [Some(true), Some(false)] {
            update_settings(&path, |s| s.update_check = answer).unwrap();
            assert_eq!(load_settings(&path).update_check, answer);
        }
        let mut a = Settings::default();
        update_settings(&path, |s| s.last_update_check = Some(42)).unwrap();
        let written = update_settings(&path, |s| s.toolbar_pinned = true).unwrap();
        a.sync_from(&written);
        assert_eq!(a.update_check, Some(false));
        assert_eq!(a.last_update_check, Some(42));
    }

    #[test]
    fn language_is_lowercase_and_unknown_falls_back_to_auto() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        for (text, want) in [
            ("language = \"ko\"\n", LangSetting::Ko),
            ("language = \"en\"\n", LangSetting::En),
            ("language = \"auto\"\n", LangSetting::Auto),
            ("language = \"fr\"\n", LangSetting::Auto),
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(load_settings(&path).language, want, "{text}");
        }
        // A value of another type falls back to Auto too, keeping the rest of the file.
        for text in [
            "toolbar_pinned = true
language = 1
",
            "toolbar_pinned = true
language = [\"ko\"]
",
            "toolbar_pinned = true
[language]
ko = true
",
        ] {
            std::fs::write(&path, text).unwrap();
            let s = load_settings(&path);
            assert_eq!(s.language, LangSetting::Auto, "{text}");
            assert!(s.toolbar_pinned, "{text}");
            assert!(path.exists(), "{text}");
        }
        for want in [LangSetting::Auto, LangSetting::Ko, LangSetting::En] {
            let s = Settings {
                language: want,
                ..Settings::default()
            };
            save_settings(&path, &s).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let name = format!("{want:?}").to_lowercase();
            assert!(text.contains(&format!("language = \"{name}\"")), "{text}");
        }
    }
}
