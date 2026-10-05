pub use rufin_core::settings::presentation::{format_duration, format_duration_units};

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
