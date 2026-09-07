pub fn seekbar_target_seconds(value: f64, duration_seconds: u32) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(0.0, f64::from(duration_seconds)) as u32
}

#[cfg(test)]
mod tests {
    use super::seekbar_target_seconds;

    #[test]
    pub fn clamps_seekbar_targets_to_the_track_duration() {
        assert_eq!(seekbar_target_seconds(42.4, 180), 42);
        assert_eq!(seekbar_target_seconds(42.5, 180), 43);
        assert_eq!(seekbar_target_seconds(-10.0, 180), 0);
        assert_eq!(seekbar_target_seconds(220.0, 180), 180);
        assert_eq!(seekbar_target_seconds(f64::NAN, 180), 0);
    }
}
