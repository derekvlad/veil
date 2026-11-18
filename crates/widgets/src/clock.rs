//! Local time without pulling a date library into the workspace.

use std::time::{SystemTime, UNIX_EPOCH};

use veil::widgets::Parts;

/// Fields of the current local time, as the widgets render them.
pub fn now() -> Parts {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    parts_at(seconds)
}

/// Local time for a Unix timestamp; UTC if the C library cannot convert it.
pub fn parts_at(seconds: i64) -> Parts {
    let time: libc::time_t = seconds as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
    if converted {
        return Parts {
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            day: tm.tm_mday as u32,
            month: (tm.tm_mon + 1) as u32,
            year: (tm.tm_year + 1900) as u32,
            weekday: tm.tm_wday as u32,
        };
    }
    from_utc(seconds)
}

/// Plain civil date from a Unix timestamp, used when localtime is unavailable.
fn from_utc(seconds: i64) -> Parts {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    Parts {
        hour: (rest / 3600) as u32,
        minute: ((rest % 3600) / 60) as u32,
        second: (rest % 60) as u32,
        day,
        month,
        year,
        // 1970-01-01 was a Thursday, which `struct tm` calls 4.
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}

/// Howard Hinnant's `civil_from_days`, in days since 1970-01-01.
fn civil_from_days(days: i64) -> (u32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (if month <= 2 { y + 1 } else { y }) as u32;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_unix_time_to_civil_dates() {
        let parts = from_utc(0);
        assert_eq!((parts.year, parts.month, parts.day), (1970, 1, 1));
        assert_eq!(parts.weekday, 4);
        // 2026-11-18 15:04:05 UTC
        let parts = from_utc(1_795_014_245);
        assert_eq!((parts.year, parts.month, parts.day), (2026, 11, 18));
        assert_eq!((parts.hour, parts.minute, parts.second), (15, 4, 5));
        assert_eq!(parts.weekday, 3);
    }
}
