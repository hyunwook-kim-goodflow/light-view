//! The About dialog (ruling R4): an in-window dialog (`overlay::Prompt::About`) with the
//! version, a one-line license summary, [오픈소스 고지] and [닫기]. [오픈소스 고지]
//! opens `THIRD_PARTY_NOTICES.md` from the app's folder with its default program.

use std::cell::RefCell;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;

use crate::input;
use crate::overlay::{DialogTarget, Prompt, Toast};
use crate::shell;
use crate::strings::{t, tf, S};
use crate::window::App;

pub const NOTICES_FILE: &str = "THIRD_PARTY_NOTICES.md";

/// "버전 0.1.0".
pub fn version_line() -> String {
    tf(S::AboutVersion, &[&env!("CARGO_PKG_VERSION")])
}

pub fn license_line() -> &'static str {
    t(S::AboutLicense)
}

/// `THIRD_PARTY_NOTICES.md` next to the running exe.
pub fn notices_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(NOTICES_FILE))
}

/// Shows the dialog. It has no photo to ask about; its target is just the current one.
pub fn show(app: &RefCell<App>, hwnd: HWND) {
    let target = {
        let Ok(a) = app.try_borrow() else {
            return;
        };
        DialogTarget {
            path: a
                .viewer
                .current_path()
                .map(PathBuf::from)
                .unwrap_or_default(),
            generation: a.viewer.generation(),
        }
    };
    input::show_dialog(app, hwnd, Prompt::About, target);
}

/// [오픈소스 고지]: opens the notices, or says it could not.
pub fn open_notices(app: &RefCell<App>, hwnd: HWND) {
    let opened = notices_path().is_some_and(|p| p.is_file() && shell::open(hwnd, p.as_os_str()));
    if !opened {
        input::show_toast(app, hwnd, |now| Toast::new(t(S::ToastOpenFailed), now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_license_lines() {
        use crate::strings::{text, Lang};
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(version_line(), format!("{}{version}", t(S::AboutVersion)));
        assert_eq!(license_line(), t(S::AboutLicense));
        assert_eq!(text(Lang::Ko, S::AboutVersion), "버전 ");
        assert_eq!(
            text(Lang::Ko, S::AboutLicense),
            "MIT 또는 Apache-2.0 라이선스로 배포해요"
        );
        let p = notices_path().unwrap();
        assert!(p.ends_with(NOTICES_FILE));
    }
}
