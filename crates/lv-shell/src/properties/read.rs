//! What the details handler shows of a HEIF file, read from its bytes alone: the container's
//! boxes (`lv_core::heif_edit`) and its EXIF item (`lv_core::exif_info`). Pure Rust, no
//! decoding, nothing outside the bytes given.

use lv_core::exif_info::camera_facts;
use lv_core::heif_edit::display_size;

/// 100 ns ticks per second (FILETIME's unit).
const TICKS_PER_SECOND: i64 = 10_000_000;

/// A file's details. The size is the displayed one (after `clap` and `irot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeifProps {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) date_taken: Option<DateTaken>,
    pub(crate) make: Option<String>,
    pub(crate) model: Option<String>,
}

/// EXIF `DateTimeOriginal` as FILETIME ticks (100 ns since 1601-01-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DateTaken {
    /// UTC: `OffsetTimeOriginal` said where the local time was.
    Utc(u64),
    /// The local wall time where it was taken, its offset unknown (read as if it were UTC).
    /// Windows treats such a time as the machine's local time, as its own photo handlers do.
    Local(u64),
}

/// The details of the HEIF file `bytes`, or `None` when its boxes do not give a size (not a
/// HEIF, or corrupt). EXIF that is missing or unreadable only leaves its facts out.
pub(crate) fn heif_properties(bytes: &[u8]) -> Option<HeifProps> {
    let (width, height) = display_size(bytes).ok()?;
    let facts = camera_facts(bytes).unwrap_or_default();
    let date_taken = facts.date_time_original.as_deref().and_then(|text| {
        let local = wall_ticks(text)?;
        match facts
            .offset_time_original
            .as_deref()
            .and_then(offset_seconds)
        {
            Some(offset) => {
                let utc = i64::try_from(local).ok()? - offset * TICKS_PER_SECOND;
                u64::try_from(utc).ok().map(DateTaken::Utc)
            }
            None => Some(DateTaken::Local(local)),
        }
    });
    Some(HeifProps {
        width,
        height,
        date_taken,
        make: facts.make,
        model: facts.model,
    })
}

/// `YYYY:MM:DD HH:MM:SS` (years 1601 to 9999, a real calendar date) as FILETIME ticks, read
/// as UTC. Anything else, the all-zero "unknown" date included, is `None`.
fn wall_ticks(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() != 19 || b[4] != b':' || b[7] != b':' || b[10] != b' ' {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = &b[range];
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| digits.iter().fold(0, |v, d| v * 10 + i64::from(d - b'0')))
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1601..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day) - days_from_civil(1601, 1, 1);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    u64::try_from(seconds * TICKS_PER_SECOND).ok()
}

/// `+HH:MM` or `-HH:MM` (up to 23:59) as seconds east of UTC.
fn offset_seconds(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 6 || b[3] != b':' || !b[1..3].iter().chain(&b[4..6]).all(u8::is_ascii_digit) {
        return None;
    }
    let sign = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours = i64::from(b[1] - b'0') * 10 + i64::from(b[2] - b'0');
    let minutes = i64::from(b[4] - b'0') * 10 + i64::from(b[5] - b'0');
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 3600 + minutes * 60))
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the given proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seconds from 1601-01-01 (FILETIME's epoch) to 1970-01-01.
    const EPOCH_1601_TO_1970: i64 = 11_644_473_600;

    fn ticks_of_unix(seconds: i64) -> u64 {
        ((seconds + EPOCH_1601_TO_1970) * TICKS_PER_SECOND) as u64
    }

    #[test]
    fn exif_dates_become_filetime_ticks() {
        // 2024-05-05T22:08:09Z is Unix 1714946889.
        assert_eq!(
            wall_ticks("2024:05:05 22:08:09"),
            Some(ticks_of_unix(1_714_946_889))
        );
        assert_eq!(wall_ticks("1601:01:01 00:00:00"), Some(0));
        assert_eq!(wall_ticks("1970:01:01 00:00:00"), Some(ticks_of_unix(0)));
        assert_eq!(
            wall_ticks("2000:02:29 23:59:59"),
            Some(125_963_423_990_000_000)
        );
        assert!(wall_ticks("9999:12:31 23:59:59").is_some());
        for bad in [
            "0000:00:00 00:00:00",
            "    :  :     :  :  ",
            "2023:02:29 00:00:00",
            "1900:02:29 00:00:00",
            "2024:04:31 00:00:00",
            "2024:13:01 00:00:00",
            "2024:00:01 00:00:00",
            "2024:05:06 24:00:00",
            "2024:05:06 07:60:00",
            "2024:05:06 07:08:60",
            "1600:12:31 23:59:59",
            "2024-05-06 07:08:09",
            "2024:05:06T07:08:09",
            "2024:05:06 07:08:09.5",
            "2024:05:06 07:08",
            "+024:05:06 07:08:0",
            "",
        ] {
            assert_eq!(wall_ticks(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn utc_offsets() {
        assert_eq!(offset_seconds("+09:00"), Some(9 * 3600));
        assert_eq!(offset_seconds("-05:30"), Some(-(5 * 3600 + 30 * 60)));
        assert_eq!(offset_seconds("+00:00"), Some(0));
        for bad in [
            "09:00", "+9:00", "+09:0", "+24:00", "+09:60", "+09-00", "Z", "", "+09:00 ",
        ] {
            assert_eq!(offset_seconds(bad), None, "{bad:?}");
        }
    }
}
