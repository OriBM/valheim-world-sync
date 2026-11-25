//! Hashing, atomic file writes and UTC time helpers.
//!
//! Times here are for display and naming. Correctness never depends on a
//! client's clock (I6): staleness uses GitHub's clock only.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, IoContext, Result};

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// SHA-256 (hex) and size of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = File::open(path).at(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).at(path)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex(&h.finalize()), total))
}

/// Write a file so that a crash leaves either the old or the new content, never half.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Invalid(format!("no parent folder for {}", path.display())))?;
    std::fs::create_dir_all(dir).at(dir)?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    {
        let mut f = File::create(&tmp).at(&tmp)?;
        f.write_all(bytes).at(&tmp)?;
        f.sync_all().at(&tmp)?;
    }
    std::fs::rename(&tmp, path).at(path)
}

/// Seconds since the Unix epoch, UTC.
pub type UnixTime = i64;

/// This PC's clock. Only for display, names and local timeouts (see I6).
pub fn now_unix() -> UnixTime {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let d = d as i64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn split(t: UnixTime) -> (i64, u32, u32, i64, i64, i64) {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    (y, m, d, s / 3600, (s % 3600) / 60, s % 60)
}

/// `2026-11-25T19:40:12Z`
pub fn format_rfc3339(t: UnixTime) -> String {
    let (y, m, d, hh, mm, ss) = split(t);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `20261125-194012Z`, for folder names.
pub fn format_compact(t: UnixTime) -> String {
    let (y, m, d, hh, mm, ss) = split(t);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}Z")
}

/// `20261125T194012Z`, for asset names.
pub fn format_asset_stamp(t: UnixTime) -> String {
    let (y, m, d, hh, mm, ss) = split(t);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

fn digits(s: &str, start: usize, end: usize) -> Option<i64> {
    let part = s.get(start..end)?;
    if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// Parse an RFC 3339 timestamp such as GitHub's `2026-11-25T19:40:12Z`.
pub fn parse_rfc3339(s: &str) -> Option<UnixTime> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d) = (digits(s, 0, 4)?, digits(s, 5, 7)?, digits(s, 8, 10)?);
    let (h, mi, se) = (digits(s, 11, 13)?, digits(s, 14, 16)?, digits(s, 17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let c = *b.get(i)?;
    let offset = match c {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        b'+' | b'-' if i + 6 == b.len() && b[i + 3] == b':' => {
            let o = digits(s, i + 1, i + 3)? * 3600 + digits(s, i + 4, i + 6)? * 60;
            if c == b'+' { o } else { -o }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    Some(days * 86_400 + h * 3600 + mi * 60 + se.min(59) - offset)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Parse an HTTP `Date` header (IMF-fixdate), e.g. `Tue, 25 Nov 2026 19:40:12 GMT`.
pub fn parse_http_date(s: &str) -> Option<UnixTime> {
    let p: Vec<&str> = s.split_ascii_whitespace().collect();
    if p.len() != 6 || !p[5].eq_ignore_ascii_case("GMT") {
        return None;
    }
    let d: u32 = p[1].parse().ok()?;
    let m = MONTHS.iter().position(|x| x.eq_ignore_ascii_case(p[2]))? as u32 + 1;
    let y: i64 = p[3].parse().ok()?;
    let hms: Vec<&str> = p[4].split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let (h, mi, se): (i64, i64, i64) = (hms[0].parse().ok()?, hms[1].parse().ok()?, hms[2].parse().ok()?);
    if !(1..=31).contains(&d) || !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..=60).contains(&se) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + se.min(59))
}

/// `Tue, 25 Nov 2026 19:40:12 GMT`
pub fn format_http_date(t: UnixTime) -> String {
    let (y, m, d, hh, mm, ss) = split(t);
    let dow = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][t.div_euclid(86_400).rem_euclid(7) as usize];
    format!(
        "{dow}, {d:02} {} {y:04} {hh:02}:{mm:02}:{ss:02} GMT",
        MONTHS[(m - 1) as usize]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for z in [-800_000i64, -1, 0, 1, 10_957, 20_417, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn rfc3339() {
        let t = parse_rfc3339("2026-11-25T19:40:12Z").unwrap();
        assert_eq!(format_rfc3339(t), "2026-11-25T19:40:12Z");
        assert_eq!(parse_rfc3339("2026-11-25T21:40:12+02:00"), Some(t));
        assert_eq!(parse_rfc3339("2026-11-25T19:40:12.123Z"), Some(t));
        assert_eq!(format_compact(t), "20261125-194012Z");
        assert_eq!(format_asset_stamp(t), "20261125T194012Z");
        for bad in [
            "",
            "2026-11-25",
            "2026-13-25T19:40:12Z",
            "2026-11-25T19:40:12",
            "x026-11-25T19:40:12Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }

    #[test]
    fn http_date() {
        let t = parse_http_date("Wed, 25 Nov 2026 19:40:12 GMT").unwrap();
        assert_eq!(Some(t), parse_rfc3339("2026-11-25T19:40:12Z"));
        assert_eq!(format_http_date(t), "Wed, 25 Nov 2026 19:40:12 GMT");
        assert_eq!(parse_http_date("25 Nov 2026"), None);
    }

    #[test]
    fn sha256_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
