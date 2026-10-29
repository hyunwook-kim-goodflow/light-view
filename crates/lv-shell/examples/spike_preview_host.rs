//! SPIKE (M7 Task 1b) harness: does what Explorer's preview pane does for one file, from
//! this process, without touching Explorer or the user's preview-pane setting.
//!
//! `spike_preview_host <file> <out.bmp> [inproc]`:
//! 1. finds the preview handler the shell would use (`AssocQueryStringW`,
//!    `ASSOCSTR_SHELLEXTENSION` with the preview-handler shellex GUID) and whether it is
//!    listed under `PreviewHandlers` (HKCU and HKLM);
//! 2. creates it out of process (`CoCreateInstance(CLSCTX_LOCAL_SERVER)`, which starts the
//!    surrogate named by the class's `AppID`, prevhost.exe);
//! 3. `IInitializeWithStream::Initialize` with a read-only stream on the file, `SetWindow`
//!    on a window of ours, `SetRect`, `DoPreview`;
//! 4. lists the prevhost.exe processes, captures our window only (`PrintWindow`) to
//!    `out.bmp` and prints its centre pixel; then `Unload`.
//!
//! With `inproc`, step 2 uses `CLSCTX_INPROC_SERVER` instead (a check of the handler's own
//! code in this process; not what Explorer does).

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPFILEHEADER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{
    CLSIDFromString, CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ, STGM_SHARE_DENY_WRITE,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
use windows::Win32::UI::Shell::{
    AssocQueryStringW, IPreviewHandler, SHCreateStreamOnFileEx, ASSOCF_INIT_DEFAULTTOSTAR,
    ASSOCSTR_SHELLEXTENSION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW,
    TranslateMessage, CW_USEDEFAULT, MSG, PM_REMOVE, WINDOW_EX_STYLE, WNDCLASSW,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

const PREVIEW_SHELLEX: &str = "{8895b1c6-b41f-4c1c-a562-0d564250836f}";
const WIDTH: i32 = 400;
const HEIGHT: i32 = 300;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let inproc = args.get(3).map(String::as_str) == Some("inproc");
    if args.len() < 3 || (args.len() > 3 && !inproc) || args.len() > 4 {
        eprintln!("usage: spike_preview_host <file> <out.bmp> [inproc]");
        std::process::exit(2);
    }
    if let Err(e) = run(&args[1], &args[2], inproc) {
        println!("FAILED: {e} (hresult {:#010x})", e.code().0);
        std::process::exit(1);
    }
}

fn run(file: &str, out_bmp: &str, inproc: bool) -> windows::core::Result<()> {
    // SAFETY: first COM call on this thread.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let ext = std::path::Path::new(file)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let clsid_text = assoc_handler(&ext)?;
    println!("handler for {ext}: {clsid_text}");
    for (name, root) in [("HKCU", HKEY_CURRENT_USER), ("HKLM", HKEY_LOCAL_MACHINE)] {
        println!(
            "PreviewHandlers[{name}] {clsid_text} = {:?}",
            reg_sz(
                root,
                "Software\\Microsoft\\Windows\\CurrentVersion\\PreviewHandlers",
                &clsid_text
            )
        );
    }
    let clsid_w = wide(&clsid_text);
    // SAFETY: NUL-terminated "{...}" string.
    let clsid: GUID = unsafe { CLSIDFromString(PCWSTR(clsid_w.as_ptr()))? };
    println!("prevhost before: {:?}", prevhosts());

    let hwnd = create_window()?;
    let (context, context_name) = if inproc {
        (CLSCTX_INPROC_SERVER, "CLSCTX_INPROC_SERVER")
    } else {
        (CLSCTX_LOCAL_SERVER, "CLSCTX_LOCAL_SERVER")
    };
    let t = Instant::now();
    // SAFETY: COM initialised; a valid CLSID.
    let created: windows::core::Result<IPreviewHandler> =
        unsafe { CoCreateInstance(&clsid, None, context) };
    println!(
        "CoCreateInstance({context_name}) -> {} in {:?}",
        match &created {
            Ok(_) => "S_OK".to_string(),
            Err(e) => format!("{:#010x} {}", e.code().0, e.message()),
        },
        t.elapsed()
    );
    println!("prevhost after create: {:?}", prevhosts());
    let handler = created?;

    let path = wide(file);
    // SAFETY: NUL-terminated path; read-only stream.
    let stream = unsafe {
        SHCreateStreamOnFileEx(
            PCWSTR(path.as_ptr()),
            (STGM_READ | STGM_SHARE_DENY_WRITE).0,
            0,
            false,
            None,
        )?
    };
    let init: IInitializeWithStream = handler.cast()?;
    // SAFETY: valid stream; STGM_READ mode.
    unsafe { init.Initialize(&stream, STGM_READ.0)? };
    println!("Initialize: ok");
    let rect = RECT {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: HEIGHT,
    };
    // SAFETY: our own window; a valid RECT.
    unsafe {
        handler.SetWindow(hwnd, &rect)?;
        handler.SetRect(&rect)?;
    }
    let t = Instant::now();
    // SAFETY: COM call on the handler we created.
    let shown = unsafe { handler.DoPreview() };
    println!(
        "DoPreview -> {:?} in {:?}",
        shown.as_ref().map_err(|e| format!("{:#010x}", e.code().0)),
        t.elapsed()
    );
    pump(Duration::from_millis(1500));
    match capture(hwnd, out_bmp) {
        Ok(pixel) => println!("window capture {out_bmp}: centre pixel (r,g,b) = {pixel:?}"),
        Err(e) => println!("window capture failed: {e}"),
    }
    // SAFETY: COM call on the handler we created.
    unsafe { handler.Unload()? };
    drop(init);
    drop(handler);
    pump(Duration::from_millis(500));
    println!("prevhost at end: {:?}", prevhosts());
    shown
}

fn assoc_handler(ext: &str) -> windows::core::Result<String> {
    let ext_w = wide(ext);
    let extra = wide(PREVIEW_SHELLEX);
    let mut buf = [0u16; 128];
    let mut len = buf.len() as u32;
    // SAFETY: NUL-terminated inputs; `buf` holds `len` u16s.
    unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_DEFAULTTOSTAR,
            ASSOCSTR_SHELLEXTENSION,
            PCWSTR(ext_w.as_ptr()),
            PCWSTR(extra.as_ptr()),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()?
    };
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(&buf[..end]))
}

fn reg_sz(root: HKEY, key: &str, value: &str) -> Option<String> {
    let key_w = wide(key);
    let value_w = wide(value);
    let mut buf = [0u16; 256];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: NUL-terminated names; `buf` holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            root,
            PCWSTR(key_w.as_ptr()),
            PCWSTR(value_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// PIDs of running prevhost.exe processes.
fn prevhosts() -> Vec<u32> {
    let mut pids = Vec::new();
    // SAFETY: snapshot handle closed by the OwnedHandle-like drop below.
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return pids;
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: valid snapshot and a sized entry.
    let mut more = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
    while more {
        let end = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..end]).eq_ignore_ascii_case("prevhost.exe") {
            pids.push(entry.th32ProcessID);
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap, &mut entry) }.is_ok();
    }
    // SAFETY: the snapshot handle is ours.
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(snap) };
    pids
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwarding the window procedure's own arguments.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn create_window() -> windows::core::Result<HWND> {
    // SAFETY: our own module handle.
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("lvPreviewSpikeHost"),
        ..Default::default()
    };
    // SAFETY: a fully initialised class with static strings.
    unsafe { RegisterClassW(&class) };
    // SAFETY: registered class; top-level window owned by this thread.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("lvPreviewSpikeHost"),
            w!("light-view preview spike host"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIDTH + 40,
            HEIGHT + 60,
            None,
            None,
            instance,
            None,
        )
    }
}

fn pump(time: Duration) {
    let end = Instant::now() + time;
    let mut msg = MSG::default();
    while Instant::now() < end {
        // SAFETY: standard message loop on this thread.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Captures `hwnd` only (PrintWindow, full content) into a 24-bit BMP; returns the pixel
/// at the centre of the preview rectangle.
fn capture(hwnd: HWND, out: &str) -> windows::core::Result<(u8, u8, u8)> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: WIDTH + 40,
            biHeight: HEIGHT + 60,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let (w, h) = ((WIDTH + 40) as usize, (HEIGHT + 60) as usize);
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: GDI objects created and released here; `bits` points at w*h pixels.
    let pixels = unsafe {
        let screen = GetDC(hwnd);
        let dc = CreateCompatibleDC(screen);
        let bmp = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let old = SelectObject(dc, bmp);
        // PW_RENDERFULLCONTENT = 2: DWM content, including the out-of-process child.
        let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2));
        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), w * h * 4).to_vec();
        SelectObject(dc, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(dc);
        ReleaseDC(hwnd, screen);
        if !ok.as_bool() {
            return Err(windows::core::Error::from_win32());
        }
        pixels
    };
    // Bottom-up rows; the client area starts below the caption. Sample the window centre.
    let (cx, cy) = (w / 2, h / 2);
    let i = ((h - 1 - cy) * w + cx) * 4;
    let pixel = (pixels[i + 2], pixels[i + 1], pixels[i]);
    let file_header = BITMAPFILEHEADER {
        bfType: 0x4D42,
        bfSize: (14 + 40 + pixels.len()) as u32,
        bfOffBits: 14 + 40,
        ..Default::default()
    };
    let mut bytes = Vec::with_capacity(14 + 40 + pixels.len());
    // SAFETY: plain-old-data structs viewed as bytes (packed file header is 14 bytes).
    unsafe {
        bytes.extend_from_slice(std::slice::from_raw_parts(
            std::ptr::from_ref(&file_header).cast::<u8>(),
            14,
        ));
        bytes.extend_from_slice(std::slice::from_raw_parts(
            std::ptr::from_ref(&info.bmiHeader).cast::<u8>(),
            40,
        ));
    }
    bytes.extend_from_slice(&pixels);
    std::fs::write(out, bytes).map_err(|e| {
        windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string())
    })?;
    Ok(pixel)
}
