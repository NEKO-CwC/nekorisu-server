use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub fn rfc3339(ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

pub fn parse_rfc3339(text: &str) -> Option<i64> {
    OffsetDateTime::parse(text, &Rfc3339)
        .ok()
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// At most `max` characters of `text`, and whether it was cut. 0 = no limit.
pub fn cut(text: &str, max: usize) -> (String, bool) {
    if max == 0 || text.chars().count() <= max {
        return (text.to_string(), false);
    }
    (text.chars().take(max).collect(), true)
}

pub fn today_utc() -> String {
    let d = OffsetDateTime::now_utc().date();
    format!("{:04}{:02}{:02}", d.year(), d.month() as u8, d.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_trip() {
        assert_eq!(rfc3339(1_794_700_800_000), "2026-11-15T00:00:00Z");
        assert_eq!(parse_rfc3339("2026-11-15T00:00:00Z"), Some(1_794_700_800_000));
        assert_eq!(parse_rfc3339("nope"), None);
    }

    #[test]
    fn cuts_by_characters() {
        assert_eq!(cut("héllo", 3), ("hél".to_string(), true));
        assert_eq!(cut("héllo", 5), ("héllo".to_string(), false));
        assert_eq!(cut("héllo", 0), ("héllo".to_string(), false));
    }
}
