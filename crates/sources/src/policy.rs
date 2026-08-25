pub(crate) fn raw_item_id(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}

pub(crate) fn stable_hash(input: &str) -> u64 {
    input.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub(crate) fn normalized_date(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if let Some(prefix) = value.get(..10)
        && prefix.as_bytes().get(4) == Some(&b'-')
        && prefix.as_bytes().get(7) == Some(&b'-')
    {
        return Some(prefix.to_string());
    }
    Some(value)
}

pub(crate) fn unix_seconds(value: Option<String>) -> Option<i64> {
    let value = value?.trim().to_string();
    let date = value.get(..10)?;
    let time = value.get(11..19)?;
    let mut date = date.split('-');
    let year = date.next()?.parse::<i64>().ok()?;
    let month = date.next()?.parse::<i64>().ok()?;
    let day = date.next()?.parse::<i64>().ok()?;
    let mut time = time.split(':');
    let hour = time.next()?.parse::<i64>().ok()?;
    let minute = time.next()?.parse::<i64>().ok()?;
    let second = time.next()?.parse::<i64>().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let mut timestamp = days * 86_400 + hour * 3_600 + minute * 60 + second;
    let suffix = value.get(19..).unwrap_or_default();
    if let Some(sign_at) = suffix.find(['+', '-']) {
        let sign = if suffix.as_bytes().get(sign_at) == Some(&b'+') {
            1
        } else {
            -1
        };
        let offset = suffix.get(sign_at + 1..)?;
        let hours = offset.get(..2)?.parse::<i64>().ok()?;
        let minutes = offset.get(3..5)?.parse::<i64>().ok()?;
        timestamp -= sign * (hours * 3_600 + minutes * 60);
    }
    Some(timestamp)
}

pub(crate) fn u16_from_option(value: Option<i32>) -> u16 {
    value.unwrap_or_default().clamp(0, i32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_hashes_remain_stable() {
        for (provider, input, expected) in [
            ("Jellyfin", "https://music.example/", "521a0cd3e594763e"),
            (
                "Navidrome",
                "navidrome:https://music.example/rest/:Listener",
                "73c4e8e31079b4dc",
            ),
            ("Local", "/Music/Album/Track.flac", "b4b4e799309c6677"),
        ] {
            assert_eq!(
                format!("{:016x}", stable_hash(input)),
                expected,
                "{provider}"
            );
        }
    }

    #[test]
    fn provider_dates_keep_the_current_normalization() {
        for (input, expected) in [
            (None, None),
            (Some("  "), None),
            (Some(" 2025-04-03T12:00:00Z "), Some("2025-04-03")),
            (Some("2025-04"), Some("2025-04")),
            (Some("2025/04/03"), Some("2025/04/03")),
        ] {
            assert_eq!(
                normalized_date(input.map(str::to_string)),
                expected.map(str::to_string)
            );
        }
    }
}
