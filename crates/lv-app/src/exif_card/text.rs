//! What the EXIF card says: the shooting date, camera, exposure, size and place.

use std::sync::Arc;

use lv_core::exif_info::ExifData;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

use crate::strings::{date_text, text, time_text, Lang, S};
use crate::toolbar::info_text;

/// What the card says, top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Plain(String),
    /// Shutter · aperture · ISO, on a wash pill.
    Exposure(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardText {
    /// The shooting date (serif, large), or "EXIF 정보가 없어요".
    pub heading: String,
    pub serif: bool,
    pub lines: Vec<Line>,
    /// There are tags to list ("전체 보기").
    pub can_expand: bool,
}

/// Local date and time: year, month, day, hour, minute.
pub type DateTime = (u16, u8, u8, u8, u8);

/// What the card is about: the shown photo's EXIF, its pixel size and file size, and
/// its modification time (shown only without EXIF, spec §4.6).
#[derive(Clone, Debug, Default)]
pub struct CardInfo {
    pub exif: Option<Arc<ExifData>>,
    pub file: Option<(u32, u32, u64)>,
    pub modified: Option<DateTime>,
}

/// `YYYY:MM:DD HH:MM[:SS]` as stored in EXIF; `None` when it is not a real date.
pub fn parse_taken(raw: &str) -> Option<DateTime> {
    let (date, time) = raw.trim().split_once(' ')?;
    let mut d = date.split(':').map(|p| p.parse::<u16>().ok());
    let mut tm = time.trim().split(':').map(|p| p.parse::<u8>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (h, min) = (tm.next()??, tm.next()??);
    let ok = d.next().is_none()
        && (1..=9999).contains(&y)
        && (1..=12).contains(&m)
        && (1..=31).contains(&day)
        && h < 24
        && min < 60;
    ok.then_some((y, m as u8, day as u8, h, min))
}

/// The date line and the time, e.g. ("2026. 10. 3", "오후 5:42"); an unreadable value
/// is shown as stored, without a time.
pub fn taken_lines(raw: &str, lang: Lang) -> (String, Option<String>) {
    match parse_taken(raw) {
        Some((y, m, d, h, min)) => (date_text(lang, y, m, d), Some(time_text(lang, h, min))),
        None => (raw.to_owned(), None),
    }
}

pub fn card_text(info: &CardInfo, lang: Lang) -> CardText {
    let file = info
        .file
        .map(|(w, h, bytes)| Line::Plain(info_text(w, h, bytes)));
    let Some(exif) = &info.exif else {
        let modified = info.modified.map(|(y, m, d, h, min)| {
            Line::Plain(format!(
                "{} {} {}",
                text(lang, S::ExifModified),
                date_text(lang, y, m, d),
                time_text(lang, h, min)
            ))
        });
        return CardText {
            heading: text(lang, S::ExifNone).to_owned(),
            serif: false,
            lines: file.into_iter().chain(modified).collect(),
            can_expand: false,
        };
    };
    let s = &exif.summary;
    let (date, time) = match &s.taken {
        Some(raw) => {
            let (date, time) = taken_lines(raw, lang);
            (Some(date), time)
        }
        None => (None, None),
    };
    let sub: Vec<&str> = time.iter().chain(&s.camera).map(String::as_str).collect();
    let mut lines: Vec<Line> = Vec::new();
    if !sub.is_empty() {
        lines.push(Line::Plain(sub.join(" \u{b7} ")));
    }
    lines.extend(s.exposure.clone().map(Line::Exposure));
    lines.extend(file);
    if let Some((lat, lon)) = s.gps {
        let label = text(lang, S::ExifLocation);
        lines.push(Line::Plain(format!("{label} {lat:.5}, {lon:.5}")));
    }
    let (heading, serif) = match date {
        Some(date) => (date, true),
        // No date: the first plain line moves up into the heading.
        None => match lines.first() {
            Some(Line::Plain(first)) => {
                let first = first.clone();
                lines.remove(0);
                (first, false)
            }
            _ => (String::new(), false),
        },
    };
    CardText {
        heading,
        serif,
        lines,
        can_expand: !exif.entries.is_empty(),
    }
}

/// A FILETIME (100 ns ticks since 1601, UTC) as local date and time.
pub fn local_time(filetime: u64) -> Option<DateTime> {
    let ft = FILETIME {
        dwLowDateTime: filetime as u32,
        dwHighDateTime: (filetime >> 32) as u32,
    };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: all three are live locals; the calls only read `ft`/`utc` and write the
    // SYSTEMTIME out-parameters.
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some((
        local.wYear,
        local.wMonth as u8,
        local.wDay as u8,
        local.wHour as u8,
        local.wMinute as u8,
    ))
}
