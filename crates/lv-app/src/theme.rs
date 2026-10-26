//! goodflow color tokens (spec §4.1) and the system light/dark preference.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

/// An opaque sRGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn to_d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: f32::from(self.0) / 255.0,
            g: f32::from(self.1) / 255.0,
            b: f32::from(self.2) / 255.0,
            a: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub paper: Rgb,
    pub surface: Rgb,
    pub ink: Rgb,
    pub ink2: Rgb,
    pub hairline: Rgb,
    pub highlighter: Rgb,
    pub wash: Rgb,
    pub on_highlighter: Rgb,
}

pub const LIGHT: Palette = Palette {
    paper: Rgb(0xFB, 0xFA, 0xF6),
    surface: Rgb(0xFF, 0xFF, 0xFF),
    ink: Rgb(0x1C, 0x1B, 0x19),
    ink2: Rgb(0x69, 0x65, 0x5D),
    hairline: Rgb(0xE9, 0xE6, 0xDE),
    highlighter: Rgb(0xD6, 0xF0, 0x4A),
    wash: Rgb(0xED, 0xF8, 0xAE),
    on_highlighter: Rgb(0x1C, 0x1B, 0x19),
};

pub const DARK: Palette = Palette {
    paper: Rgb(0x13, 0x13, 0x11),
    surface: Rgb(0x1E, 0x1D, 0x1A),
    ink: Rgb(0xF3, 0xF1, 0xEA),
    ink2: Rgb(0xA7, 0xA3, 0x99),
    hairline: Rgb(0x2A, 0x29, 0x25),
    highlighter: Rgb(0x0F, 0x76, 0x6E),
    wash: Rgb(0x15, 0x52, 0x4C),
    on_highlighter: Rgb(0xF3, 0xF1, 0xEA),
};

pub fn palette(dark: bool) -> &'static Palette {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// `HKCU\...\Themes\Personalize\AppsUseLightTheme == 0`. Missing value means light.
pub fn system_prefers_dark() -> bool {
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` and `size` are live locals; `size` holds the byte size of `value`,
    // and RRF_RT_REG_DWORD restricts the write to a 4-byte DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::addr_of_mut!(value).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 0
}

pub fn is_color_set_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: for WM_SETTINGCHANGE a non-null lparam is a NUL-terminated wide string.
    let name = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
    name.is_ok_and(|s| s == "ImmersiveColorSet")
}

pub fn apply_title_bar(hwnd: HWND, dark: bool) {
    let value = BOOL::from(dark);
    let paper = palette(dark).paper;
    // COLORREF is 0x00BBGGRR.
    let caption = COLORREF(u32::from(paper.0) | u32::from(paper.1) << 8 | u32::from(paper.2) << 16);
    // SAFETY: `value` and `caption` are live locals and each size passed matches its
    // value. DWMWA_CAPTION_COLOR fails on Windows 10; that error is ignored.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            std::ptr::from_ref(&value).cast(),
            std::mem::size_of::<BOOL>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            std::ptr::from_ref(&caption).cast(),
            std::mem::size_of::<COLORREF>() as u32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(c: Rgb) -> f64 {
        0.2126 * linear(c.0) + 0.7152 * linear(c.1) + 0.0722 * linear(c.2)
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn on_highlighter_contrast_at_least_4_5() {
        for (name, p) in [("LIGHT", LIGHT), ("DARK", DARK)] {
            let ratio = contrast(p.highlighter, p.on_highlighter);
            assert!(ratio >= 4.5, "{name}: contrast {ratio:.2} < 4.5");
        }
    }

    #[test]
    fn contrast_of_black_on_white_is_21() {
        let ratio = contrast(Rgb(0, 0, 0), Rgb(255, 255, 255));
        assert!((ratio - 21.0).abs() < 1e-9, "{ratio}");
    }
}
