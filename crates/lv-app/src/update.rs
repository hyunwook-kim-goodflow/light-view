//! The opt-in update check (spec §6.6, the app's only network use). After the first
//! photo is shown the window asks once, in an in-window dialog, whether to look for
//! new versions; nothing is checked until the user says yes. Then at most once a day a
//! background thread asks GitHub's Releases API (WinHTTP, HTTPS only, no redirects,
//! 10 s, at most 1 MB) for the latest release and, if it is newer, posts
//! `WM_APP_UPDATE`; the window shows "새 버전 X가 나왔어요 [보기]", and [보기] opens the
//! release page in the browser. Every failure (offline, 404 before the first release,
//! an odd answer) is dropped silently.
//!
//! Only `spawn_check` reaches the network, and only from its own thread: the UI thread
//! never waits on it (`update_check_never_blocks_ui`).

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use std::ffi::c_void;

use lv_core::settings::load_settings;
use serde_json::Value;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_DISABLE_COOKIES, WINHTTP_DISABLE_REDIRECTS, WINHTTP_FLAG_SECURE,
    WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3,
    WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_OPTION_SECURE_PROTOCOLS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::input;
use crate::overlay::{DialogTarget, Prompt, PromptResult, Toast};
use crate::shell;
use crate::strings::S;
use crate::window::App;

/// A newer release was found; `take_result` has it. (WM_APP + 1..=7 are taken.)
pub const WM_APP_UPDATE: u32 = WM_APP + 8;

const HOST: &str = "api.github.com";
const LATEST_PATH: &str = "/repos/goodflow/light-view/releases/latest";
/// The only pages [보기] opens.
const RELEASES_PREFIX: &str = "https://github.com/goodflow/light-view/releases/";
/// Larger answers are refused unread.
pub const MAX_RESPONSE: usize = 1 << 20;
/// The whole check, from start to the answer reaching the window.
const TIMEOUT: Duration = Duration::from_secs(10);
const DAY_SECS: u64 = 24 * 60 * 60;

pub type Version = (u32, u32, u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub html_url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateError {
    TooLarge,
    Json(String),
    /// `tag_name` is missing or not `v?MAJOR.MINOR.PATCH`.
    BadTag,
    /// `html_url` is missing or not a light-view release page.
    BadUrl,
    Status(u32),
    Network(String),
    TimedOut,
}

/// Reads the Releases API's `latest` answer: `Some` when it names a newer version than
/// `current`, `None` for the same or an older one (or a draft or pre-release). The
/// tag must be exactly `v?MAJOR.MINOR.PATCH` and the page a light-view release page.
pub fn parse_latest(json: &[u8], current: Version) -> Result<Option<Release>, UpdateError> {
    if json.len() > MAX_RESPONSE {
        return Err(UpdateError::TooLarge);
    }
    let v: Value = serde_json::from_slice(json).map_err(|e| UpdateError::Json(e.to_string()))?;
    let field = |key: &str| v.get(key).and_then(Value::as_str);
    let tag = field("tag_name").ok_or(UpdateError::BadTag)?;
    let version = parse_version(tag.strip_prefix('v').unwrap_or(tag)).ok_or(UpdateError::BadTag)?;
    let html_url = field("html_url")
        .filter(|u| release_url_ok(u))
        .ok_or(UpdateError::BadUrl)?;
    let flagged = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    if flagged("draft") || flagged("prerelease") || version <= current {
        return Ok(None);
    }
    Ok(Some(Release {
        version,
        html_url: html_url.to_owned(),
    }))
}

/// `MAJOR.MINOR.PATCH`: three decimal numbers without signs or leading zeros.
fn parse_version(s: &str) -> Option<Version> {
    let mut parts = s.split('.').map(|p| {
        let digits = !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        let leading_zero = p.len() > 1 && p.starts_with('0');
        if digits && !leading_zero {
            p.parse::<u32>().ok()
        } else {
            None
        }
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// A page under the light-view releases URL, in plain URL characters (no `..`).
fn release_url_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix(RELEASES_PREFIX) else {
        return false;
    };
    !rest.is_empty()
        && url.len() <= 512
        && !rest.contains("..")
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._/".contains(&b))
}

/// This build's version (`CARGO_PKG_VERSION`).
pub fn current_version() -> Option<Version> {
    parse_version(env!("CARGO_PKG_VERSION"))
}

pub fn version_text((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

/// Ask the question now? Only while it is unanswered, once per run, once a photo is
/// on screen and no other dialog is open.
pub fn should_ask(answer: Option<bool>, asked: bool, photo_shown: bool, dialog_open: bool) -> bool {
    answer.is_none() && !asked && photo_shown && !dialog_open
}

/// Check now? Only when the user said yes, and a day after the last check (or at once
/// if the clock went back before it).
pub fn check_due(answer: Option<bool>, last: Option<u64>, now: u64) -> bool {
    answer == Some(true) && last.is_none_or(|last| now < last || now - last >= DAY_SECS)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The window's update state.
#[derive(Debug, Default)]
pub struct UpdateState {
    /// The question was shown in this run.
    asked: bool,
    /// The page [보기] opens.
    release_url: Option<String>,
}

/// After a photo is shown: asks the question if it is time (see `should_ask`).
pub fn maybe_ask(app: &RefCell<App>, hwnd: HWND) {
    let target = {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        let Some(path) = a.settings_file().map(PathBuf::from) else {
            return;
        };
        let shown = a.viewer.has_file() && !a.viewer.pending();
        let open = a.overlay.dialog_open();
        if !should_ask(a.settings.update_check, a.update.asked, shown, open) {
            return;
        }
        // Another window may have been answered since this one started.
        let answer = load_settings(&path).update_check;
        if answer.is_some() {
            a.settings.update_check = answer;
            return;
        }
        a.update.asked = true;
        DialogTarget {
            path: a
                .viewer
                .current_path()
                .map(PathBuf::from)
                .unwrap_or_default(),
            generation: a.viewer.generation(),
        }
    };
    input::show_dialog(app, hwnd, Prompt::UpdateCheck, target);
}

/// The question was answered: only [알려 주세요] turns checks on (Enter and Esc say no).
pub fn on_answer(app: &RefCell<App>, hwnd: HWND, result: PromptResult) {
    let yes = result == PromptResult::NotifyMe;
    if let Ok(mut a) = app.try_borrow_mut() {
        a.persist(|s| s.update_check = Some(yes));
    }
    if yes {
        check_if_due(app, hwnd);
    }
}

/// Starts a check if checks are on and the last one is a day old. The time is saved
/// first, so a failing check is not retried until tomorrow either.
pub fn check_if_due(app: &RefCell<App>, hwnd: HWND) {
    let Ok(mut a) = app.try_borrow_mut() else {
        return;
    };
    let Some(path) = a.settings_file().map(PathBuf::from) else {
        return;
    };
    let now = unix_now();
    // The file as it is now: another window may have checked today.
    let saved = load_settings(&path);
    if !check_due(a.settings.update_check, saved.last_update_check, now) {
        return;
    }
    a.persist(|s| s.last_update_check = Some(now));
    drop(a);
    spawn_check(hwnd);
}

/// `WM_APP_UPDATE`: shows the toast for the release found, if checks are still on.
pub fn on_result(app: &RefCell<App>, hwnd: HWND) {
    let Some(release) = take_result() else {
        return;
    };
    {
        let Ok(mut a) = app.try_borrow_mut() else {
            return;
        };
        if a.settings.update_check != Some(true) {
            return;
        }
        a.update.release_url = Some(release.html_url);
    }
    let version = version_text(release.version);
    input::show_toast(app, hwnd, |now| Toast::update_available(&version, now));
}

/// [보기]: the release page in the browser.
pub fn open_release(app: &RefCell<App>, hwnd: HWND) {
    let url = app
        .try_borrow()
        .ok()
        .and_then(|a| a.update.release_url.clone());
    let opened = url.is_some_and(|u| release_url_ok(&u) && shell::open(hwnd, u.as_ref()));
    if !opened {
        input::show_toast(app, hwnd, |now| Toast::new(S::ToastOpenFailed, now));
    }
}

/// The release the last check found, handed from its thread to the window.
static FOUND: Mutex<Option<Release>> = Mutex::new(None);

fn take_result() -> Option<Release> {
    FOUND.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Starts a check on its own thread; returns at once. A newer release found within
/// `TIMEOUT` is posted to `hwnd` as `WM_APP_UPDATE`.
pub fn spawn_check(hwnd: HWND) {
    let Some(current) = current_version() else {
        return;
    };
    let target = hwnd.0 as isize;
    let spawned = spawn_with(TIMEOUT, current, fetch_latest, move |release| {
        *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(release);
        // SAFETY: plain call; posting to a window that is gone just fails.
        unsafe {
            let _ = PostMessageW(HWND(target as *mut _), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
        }
    });
    if let Err(e) = spawned {
        eprintln!("light-view: could not start the update check: {e}");
    }
}

/// Runs `fetch` and `parse_latest` on a new thread and hands a newer release to
/// `deliver` there, unless `timeout` (from now) passed first. Errors and late answers
/// are logged and dropped.
fn spawn_with<F, D>(
    timeout: Duration,
    current: Version,
    fetch: F,
    deliver: D,
) -> std::io::Result<()>
where
    F: FnOnce(Instant) -> Result<Vec<u8>, UpdateError> + Send + 'static,
    D: FnOnce(Release) + Send + 'static,
{
    let deadline = Instant::now() + timeout;
    std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let found = fetch(deadline).and_then(|body| parse_latest(&body, current));
            match found {
                Ok(Some(release)) if Instant::now() <= deadline => deliver(release),
                Ok(Some(_)) => eprintln!("light-view: update check answered too late"),
                Ok(None) => {}
                Err(e) => eprintln!("light-view: update check failed: {e:?}"),
            }
        })
        .map(drop)
}

/// GETs the latest release over HTTPS from `HOST` with WinHTTP. Blocking: called only
/// on `spawn_with`'s thread.
fn fetch_latest(deadline: Instant) -> Result<Vec<u8>, UpdateError> {
    let agent = HSTRING::from(format!("light-view/{}", env!("CARGO_PKG_VERSION")));
    let host = HSTRING::from(HOST);
    let path = HSTRING::from(LATEST_PATH);
    let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n"
        .encode_utf16()
        .collect();
    // SAFETY: WinHTTP calls on handles opened here and closed by `Internet`'s drop;
    // the strings and buffers outlive the calls that use them.
    unsafe {
        let session = Internet::open(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        // TLS 1.2 or later (older Windows 10 builds refuse the 1.3 flag: 1.2 then).
        let tls = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
        if session.set(WINHTTP_OPTION_SECURE_PROTOCOLS, tls).is_err() {
            session.set(
                WINHTTP_OPTION_SECURE_PROTOCOLS,
                WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2,
            )?;
        }
        let connect = Internet::open(WinHttpConnect(
            session.0,
            &host,
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ))?;
        let request = Internet::open(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        // Only api.github.com answers: no redirects (to any host), no cookies.
        request.set(
            WINHTTP_OPTION_REDIRECT_POLICY,
            WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
        )?;
        request.set(
            WINHTTP_OPTION_DISABLE_FEATURE,
            WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS,
        )?;
        request.limit(deadline)?;
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0).map_err(net)?;
        request.limit(deadline)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(net)?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(std::ptr::from_mut(&mut status).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(net)?;
        if status != 200 {
            return Err(UpdateError::Status(status));
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 16 * 1024];
        loop {
            request.limit(deadline)?;
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
            .map_err(net)?;
            if read == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > MAX_RESPONSE {
                return Err(UpdateError::TooLarge);
            }
        }
    }
}

fn net(e: windows::core::Error) -> UpdateError {
    UpdateError::Network(e.to_string())
}

/// A WinHTTP handle, closed on drop.
struct Internet(*mut c_void);

impl Internet {
    fn open(handle: *mut c_void) -> Result<Internet, UpdateError> {
        if handle.is_null() {
            Err(net(windows::core::Error::from_win32()))
        } else {
            Ok(Internet(handle))
        }
    }

    fn set(&self, option: u32, value: u32) -> Result<(), UpdateError> {
        // SAFETY: a live handle and a 4-byte value, as these options take.
        unsafe { WinHttpSetOption(Some(self.0), option, Some(&value.to_ne_bytes())) }.map_err(net)
    }

    /// Lets each step (name lookup, connect, send, receive) take at most the time left
    /// until `deadline`; an error once it has passed. (A slow server can still keep this
    /// thread a few steps' worth longer; `spawn_with` drops an answer that late.)
    fn limit(&self, deadline: Instant) -> Result<(), UpdateError> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(UpdateError::TimedOut);
        }
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: a live handle.
        unsafe { WinHttpSetTimeouts(self.0, ms, ms, ms, ms) }.map_err(net)
    }
}

impl Drop for Internet {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by WinHTTP and is closed once, here.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    const CURRENT: Version = (0, 1, 0);

    fn answer(tag: &str, url: &str) -> Vec<u8> {
        serde_json::json!({ "tag_name": tag, "html_url": url, "draft": false, "prerelease": false })
            .to_string()
            .into_bytes()
    }

    fn page(tag: &str) -> String {
        format!("{RELEASES_PREFIX}tag/{tag}")
    }

    #[test]
    fn update_response_is_parsed_strictly() {
        // A newer release, with or without the v.
        for tag in ["v0.2.0", "0.2.0", "v1.0.0", "v0.1.1"] {
            let got = parse_latest(&answer(tag, &page(tag)), CURRENT).unwrap();
            let want = parse_version(tag.trim_start_matches('v')).unwrap();
            assert_eq!(
                got,
                Some(Release {
                    version: want,
                    html_url: page(tag)
                }),
                "{tag}"
            );
        }
        // The same or an older version: nothing to tell.
        for tag in ["v0.1.0", "v0.0.9", "0.0.1"] {
            assert_eq!(
                parse_latest(&answer(tag, &page(tag)), CURRENT),
                Ok(None),
                "{tag}"
            );
        }
        // Pre-release and odd tags.
        for tag in [
            "v0.2.0-beta",
            "v0.2.0-rc.1",
            "v0.2.0+build",
            "v0.2",
            "v0.2.0.1",
            "V0.2.0",
            "vv0.2.0",
            "v00.2.0",
            "v0.2.-0",
            "v+0.2.0",
            "v 0.2.0",
            "",
            "v4294967296.0.0",
            "latest",
        ] {
            assert_eq!(
                parse_latest(&answer(tag, &page("x")), CURRENT),
                Err(UpdateError::BadTag),
                "{tag:?}"
            );
        }
        // A flagged pre-release or draft is not offered either.
        for flag in ["prerelease", "draft"] {
            let mut v: Value = serde_json::from_slice(&answer("v9.0.0", &page("v9.0.0"))).unwrap();
            v[flag] = Value::Bool(true);
            assert_eq!(
                parse_latest(v.to_string().as_bytes(), CURRENT),
                Ok(None),
                "{flag}"
            );
        }
        // Another host, another repository, another scheme, tricks.
        for url in [
            "https://evil.example/goodflow/light-view/releases/tag/v0.2.0",
            "https://github.com.evil.example/goodflow/light-view/releases/tag/v0.2.0",
            "https://github.com/someone/light-view/releases/tag/v0.2.0",
            "http://github.com/goodflow/light-view/releases/tag/v0.2.0",
            "https://github.com/goodflow/light-view/releases/",
            "https://github.com/goodflow/light-view/releases/../../../evil",
            "https://github.com/goodflow/light-view/releases/tag/v0.2.0?x=\"y\"",
            "https://github.com/goodflow/light-view/releases/tag/v0.2.0 calc.exe",
            "https://github.com/goodflow/light-view/releasesx/tag/v0.2.0",
            "",
        ] {
            assert_eq!(
                parse_latest(&answer("v0.2.0", url), CURRENT),
                Err(UpdateError::BadUrl),
                "{url}"
            );
        }
        // Missing or mistyped fields.
        for body in [
            r#"{"html_url": "https://github.com/goodflow/light-view/releases/tag/v0.2.0"}"#,
            r#"{"tag_name": 2, "html_url": "https://github.com/goodflow/light-view/releases/tag/v0.2.0"}"#,
        ] {
            assert_eq!(
                parse_latest(body.as_bytes(), CURRENT),
                Err(UpdateError::BadTag)
            );
        }
        assert_eq!(
            parse_latest(br#"{"tag_name": "v0.2.0"}"#, CURRENT),
            Err(UpdateError::BadUrl)
        );
        // Cut short, not JSON, or not an object (GitHub's 404 body is valid JSON
        // without the fields).
        let full = answer("v0.2.0", &page("v0.2.0"));
        for body in [&full[..full.len() / 2], b"", b"<html>", b"[]", b"null"] {
            assert!(parse_latest(body, CURRENT).is_err(), "{body:?}");
        }
        let not_found = br#"{"message":"Not Found","documentation_url":"https://docs.github.com"}"#;
        assert_eq!(parse_latest(not_found, CURRENT), Err(UpdateError::BadTag));
        // More than 1 MB is refused before parsing, even if valid.
        let mut v: Value = serde_json::from_slice(&full).unwrap();
        v["body"] = Value::String("x".repeat(MAX_RESPONSE));
        let big = v.to_string().into_bytes();
        assert!(big.len() > MAX_RESPONSE);
        assert_eq!(parse_latest(&big, CURRENT), Err(UpdateError::TooLarge));
        // This build's own version parses.
        assert!(current_version().is_some());
        assert_eq!(version_text((0, 12, 3)), "0.12.3");
    }

    /// The network is reached only from `spawn_with`'s own thread: spawning returns
    /// while the fetch still blocks, the fetch and the hand-off run on another thread,
    /// and an answer later than the time limit (or an error) never reaches the window.
    #[test]
    fn update_check_never_blocks_ui() {
        let ui = std::thread::current().id();
        let (go_tx, go_rx) = channel::<()>();
        let (fetch_tx, fetch_rx) = channel();
        let (got_tx, got_rx) = channel();
        let started = Instant::now();
        spawn_with(
            TIMEOUT,
            CURRENT,
            move |deadline| {
                fetch_tx
                    .send((std::thread::current().id(), deadline))
                    .unwrap();
                go_rx.recv().unwrap();
                Ok(answer("v0.2.0", &page("v0.2.0")))
            },
            move |release| got_tx.send((std::thread::current().id(), release)).unwrap(),
        )
        .unwrap();
        // Returned at once, while the fetch is still waiting for `go`.
        assert!(started.elapsed() < Duration::from_secs(1));
        let (fetch_thread, deadline) = fetch_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(fetch_thread, ui);
        assert!(deadline <= started + TIMEOUT + Duration::from_secs(1));
        assert!(got_rx.try_recv().is_err());
        go_tx.send(()).unwrap();
        let (deliver_thread, release) = got_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(deliver_thread, ui);
        assert_eq!(release.version, (0, 2, 0));

        // Too late: dropped.
        let (late_tx, late_rx) = channel();
        spawn_with(
            Duration::from_millis(20),
            CURRENT,
            |_| {
                std::thread::sleep(Duration::from_millis(200));
                Ok(answer("v0.2.0", &page("v0.2.0")))
            },
            move |r| late_tx.send(r).unwrap(),
        )
        .unwrap();
        assert!(late_rx.recv_timeout(Duration::from_secs(1)).is_err());
        // A failed fetch (404 before the first release) and a malformed answer: dropped.
        for result in [Err(UpdateError::Status(404)), Ok(b"{".to_vec())] {
            let (tx, rx) = channel();
            spawn_with(
                TIMEOUT,
                CURRENT,
                move |_| result,
                move |r| tx.send(r).unwrap(),
            )
            .unwrap();
            assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        }

        // Structure: the WinHTTP call is private to this file and its only caller is
        // the `spawn_with` call in `spawn_check`; no other module touches WinHTTP.
        let src = include_str!("update.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        let uses: Vec<&str> = code
            .lines()
            .filter(|l| l.contains("fetch_latest") && !l.trim_start().starts_with("//"))
            .collect();
        assert_eq!(uses.len(), 2, "{uses:?}");
        assert!(uses.iter().any(|l| l.contains("fn fetch_latest(")));
        assert!(uses
            .iter()
            .any(|l| l.contains("spawn_with(TIMEOUT, current, fetch_latest,")));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut others = Vec::new();
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("update.rs") {
                    others.push(p);
                }
            }
        }
        assert!(others.len() > 10);
        for p in others {
            let text = std::fs::read_to_string(&p).unwrap();
            for word in ["WinHttp", "fetch_latest", "spawn_with"] {
                assert!(!text.contains(word), "{} mentions {word}", p.display());
            }
        }
    }

    #[test]
    fn first_run_asks_once() {
        // Unanswered: asked once a photo is on screen and no dialog is open.
        assert!(!should_ask(None, false, false, false));
        assert!(!should_ask(None, false, true, true));
        assert!(should_ask(None, false, true, false));
        // Once shown in this run, not again (even if dismissed by another dialog).
        assert!(!should_ask(None, true, true, false));
        // Answered either way: never again.
        assert!(!should_ask(Some(true), false, true, false));
        assert!(!should_ask(Some(false), false, true, false));

        // The answer persists: the next run (a fresh load) does not ask.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(load_settings(&path).update_check, None);
        lv_core::settings::update_settings(&path, |s| s.update_check = Some(false)).unwrap();
        assert!(!should_ask(
            load_settings(&path).update_check,
            false,
            true,
            false
        ));

        // No check before a yes; then once a day.
        let now = 1_800_000_000;
        assert!(!check_due(None, None, now));
        assert!(!check_due(Some(false), None, now));
        assert!(check_due(Some(true), None, now));
        assert!(!check_due(Some(true), Some(now - 60), now));
        assert!(!check_due(Some(true), Some(now - DAY_SECS + 1), now));
        assert!(check_due(Some(true), Some(now - DAY_SECS), now));
        // The clock went back past the last check: check rather than wait for it.
        assert!(check_due(Some(true), Some(now + 3600), now));
    }

    #[test]
    fn update_message_is_free() {
        use crate::{commands, render, uia_provider, watch};
        let taken = [
            commands::WM_APP_IMAGE_READY,
            commands::WM_APP_SAVE_DONE,
            commands::WM_APP_DELETE_DONE,
            watch::WM_APP_FOLDER_CHANGED,
            watch::WM_APP_RESCAN_DONE,
            uia_provider::WM_APP_UIA,
            render::WM_APP_MIPS_READY,
        ];
        assert!(!taken.contains(&WM_APP_UPDATE));
    }

    /// The real API; the repository does not exist yet, so this shows that a 404 (or
    /// no network) comes back as an error to drop, within the time limit.
    #[test]
    #[ignore = "network"]
    fn live_check_fails_quietly() {
        let started = Instant::now();
        let got = fetch_latest(started + TIMEOUT)
            .and_then(|body| parse_latest(&body, current_version().unwrap()));
        eprintln!("live check: {got:?} in {:?}", started.elapsed());
        assert!(started.elapsed() < TIMEOUT + Duration::from_secs(25));
    }
}
