pub fn format_duration(seconds: u32) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes}:{seconds:02}")
}

pub fn format_duration_units(seconds: u32) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        return format!("{hours}h {minutes}m {seconds}s");
    }
    if minutes > 0 {
        return format!("{minutes}m {seconds}s");
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_track_duration() {
        assert_eq!(super::format_duration(0), "0:00");
        assert_eq!(super::format_duration(185), "3:05");
        assert_eq!(super::format_duration(3_661), "61:01");
    }

    #[test]
    fn formats_duration_units() {
        assert_eq!(super::format_duration_units(41), "41s");
        assert_eq!(super::format_duration_units(743), "12m 23s");
        assert_eq!(super::format_duration_units(4_421), "1h 13m 41s");
    }
}
