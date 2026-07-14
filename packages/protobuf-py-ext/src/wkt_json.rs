//! Well-known-type JSON string formatting (and, from step 4, parsing).
//!
//! Timestamp/Duration use pure integer civil-date math (Hinnant's algorithms),
//! avoiding any Python `datetime` round-trip. `FieldMask` uses byte-for-byte ports
//! of `protobuf._names.proto_camel_case`/`proto_snake_case`.

use pyo3::{PyResult, exceptions::PyValueError};

/// Parses an RFC 3339 timestamp string into (seconds, nanos), matching
/// `WktTimestamp.from_json` (strict: uppercase `T`, `Z` or `±HH:MM`, 1–9
/// fractional digits). `type_name` is used in error messages.
pub(crate) fn parse_timestamp(type_name: &str, text: &str) -> PyResult<(i64, i32)> {
    let invalid = || {
        PyValueError::new_err(format!(
            "cannot decode {type_name} from JSON: invalid RFC 3339 string"
        ))
    };
    let parsed = parse_rfc3339(text).ok_or_else(invalid)?;
    // Seconds within the day plus civil date, then apply the timezone offset.
    let day_seconds =
        i64::from(parsed.hour) * 3600 + i64::from(parsed.minute) * 60 + i64::from(parsed.second);
    let days = days_from_civil(parsed.year, parsed.month, parsed.day);
    let seconds = days * SECONDS_PER_DAY + day_seconds - parsed.offset_seconds;
    if !(TIMESTAMP_SECONDS_MIN..=TIMESTAMP_SECONDS_MAX).contains(&seconds) {
        return Err(PyValueError::new_err(format!(
            "cannot decode {type_name} from JSON: must be from \
             0001-01-01T00:00:00Z to 9999-12-31T23:59:59Z inclusive"
        )));
    }
    Ok((seconds, parsed.nanos))
}

/// Parses a Duration string into (seconds, nanos), matching
/// `WktDuration.from_json`.
pub(crate) fn parse_duration(type_name: &str, text: &str) -> PyResult<(i64, i32)> {
    let invalid = || PyValueError::new_err(format!("cannot decode {type_name} from JSON: {text}"));
    // `^(-?[0-9]+)(?:\.([0-9]{1,9}))?s$`
    let body = text.strip_suffix('s').ok_or_else(invalid)?;
    let (int_part, frac_part) = match body.split_once('.') {
        Some((int_part, frac_part)) => (int_part, Some(frac_part)),
        None => (body, None),
    };
    let negative = int_part.starts_with('-');
    let digits = int_part.strip_prefix('-').unwrap_or(int_part);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let seconds: i64 = int_part.parse().map_err(|_| invalid())?;
    if !(-DURATION_SECONDS_MAX..=DURATION_SECONDS_MAX).contains(&seconds) {
        return Err(invalid());
    }
    let mut nanos: i32 = 0;
    if let Some(frac) = frac_part {
        if frac.is_empty() || frac.len() > 9 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let mut padded = frac.to_string();
        while padded.len() < 9 {
            padded.push('0');
        }
        nanos = padded.parse::<i32>().map_err(|_| invalid())?;
        if seconds < 0 || (negative && seconds == 0) {
            nanos = -nanos;
        }
    }
    Ok((seconds, nanos))
}

struct Rfc3339 {
    year: i64,
    month: i64,
    day: i64,
    hour: u32,
    minute: u32,
    second: u32,
    nanos: i32,
    offset_seconds: i64,
}

/// Strictly parses `YYYY-MM-DDTHH:MM:SS(.fffffffff)?(Z|±HH:MM)`.
fn parse_rfc3339(text: &str) -> Option<Rfc3339> {
    let bytes = text.as_bytes();
    // Fixed-width date-time prefix: 19 chars `YYYY-MM-DDTHH:MM:SS`.
    if bytes.len() < 20 {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = text.get(range)?;
        if slice.bytes().all(|b| b.is_ascii_digit()) {
            slice.parse().ok()
        } else {
            None
        }
    };
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = digits(0..4)?;
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    let hour = u32::try_from(digits(11..13)?).ok()?;
    let minute = u32::try_from(digits(14..16)?).ok()?;
    let second = u32::try_from(digits(17..19)?).ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let mut rest = &text[19..];
    let mut nanos: i32 = 0;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let frac_len = after_dot.bytes().take_while(u8::is_ascii_digit).count();
        if frac_len == 0 || frac_len > 9 {
            return None;
        }
        let frac = &after_dot[..frac_len];
        let mut padded = frac.to_string();
        while padded.len() < 9 {
            padded.push('0');
        }
        nanos = padded.parse().ok()?;
        rest = &after_dot[frac_len..];
    }

    let offset_seconds = if rest == "Z" {
        0
    } else {
        // `±HH:MM`
        let sign = match rest.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        if rest.len() != 6 || rest.as_bytes()[3] != b':' {
            return None;
        }
        let off_hour: i64 = rest.get(1..3)?.parse().ok()?;
        let off_min: i64 = rest.get(4..6)?.parse().ok()?;
        if off_hour > 23 || off_min > 59 {
            return None;
        }
        sign * (off_hour * 3600 + off_min * 60)
    };

    Some(Rfc3339 {
        year,
        month,
        day,
        hour,
        minute,
        second,
        nanos,
        offset_seconds,
    })
}

/// Converts a (year, month, day) civil date to a day count since 1970-01-01.
/// Hinnant's `days_from_civil` algorithm (inverse of `civil_from_days`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// Bounds from `protobuf.wkt._mixin._const` (post-#18 integer bounds).
const NANOS_PER_SECOND_MAX: i32 = 999_999_999;
const DURATION_SECONDS_MAX: i64 = 315_576_000_000;
const TIMESTAMP_SECONDS_MIN: i64 = -62_135_596_800;
const TIMESTAMP_SECONDS_MAX: i64 = 253_402_300_799;

const SECONDS_PER_DAY: i64 = 86_400;

/// Formats a Timestamp as an RFC 3339 string, matching
/// `WktTimestamp.to_json_value`. Raises the same range errors.
pub(crate) fn timestamp_to_rfc3339(seconds: i64, nanos: i32) -> PyResult<String> {
    if !(TIMESTAMP_SECONDS_MIN..=TIMESTAMP_SECONDS_MAX).contains(&seconds) {
        return Err(PyValueError::new_err("timestamp seconds out of range"));
    }
    if !(0..=NANOS_PER_SECOND_MAX).contains(&nanos) {
        return Err(PyValueError::new_err("timestamp nanos out of range"));
    }

    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let secs_of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if nanos == 0 {
        out.push('Z');
    } else {
        out.push('.');
        out.push_str(&trim_nanos(nanos.unsigned_abs()));
        out.push('Z');
    }
    Ok(out)
}

/// Formats a Duration, matching `WktDuration.to_json_value`.
pub(crate) fn duration_to_json(seconds: i64, nanos: i32) -> PyResult<String> {
    if !(-DURATION_SECONDS_MAX..=DURATION_SECONDS_MAX).contains(&seconds) {
        return Err(PyValueError::new_err("duration seconds out of range"));
    }
    if !(-NANOS_PER_SECOND_MAX..=NANOS_PER_SECOND_MAX).contains(&nanos) {
        return Err(PyValueError::new_err("duration nanos out of range"));
    }
    if (seconds > 0 && nanos < 0) || (seconds < 0 && nanos > 0) {
        return Err(PyValueError::new_err(
            "duration seconds and nanos have different signs",
        ));
    }

    if nanos == 0 {
        return Ok(format!("{seconds}s"));
    }
    let mut text = format!("{seconds}.{}", trim_nanos(nanos.unsigned_abs()));
    if nanos < 0 && seconds == 0 {
        text.insert(0, '-');
    }
    text.push('s');
    Ok(text)
}

/// Trims a 9-digit nanosecond string to 3, 6, or 9 digits, matching the
/// pure-Python trimming (`nanos_str[3:] == "000000"` / `nanos_str[6:] == "000"`).
fn trim_nanos(nanos: u32) -> String {
    let full = format!("{nanos:09}");
    if &full[3..] == "000000" {
        full[..3].to_string()
    } else if &full[6..] == "000" {
        full[..6].to_string()
    } else {
        full
    }
}

/// Converts a day count since 1970-01-01 to a (year, month, day) civil date.
/// Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + i64::from(m <= 2);
    (year, m, d)
}

/// Byte-for-byte port of `protobuf._names.proto_snake_case`.
pub(crate) fn proto_snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_uppercase() {
            out.push('_');
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Byte-for-byte port of `protobuf._names.proto_camel_case`.
pub(crate) fn proto_camel_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    for (i, word) in snake.split('_').enumerate() {
        if i == 0 {
            out.push_str(word);
        } else if let Some(first) = word.chars().next() {
            for uc in first.to_uppercase() {
                out.push(uc);
            }
            out.push_str(&word[first.len_utf8()..]);
        }
    }
    out
}
