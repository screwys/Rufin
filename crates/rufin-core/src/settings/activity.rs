use super::app::ActivityOverviewSettings;
use library::CalendarActivityPeriod;

impl ActivityOverviewSettings {
    /// Eligible reports at launch, with the yearly report first when windows overlap.
    pub fn startup_periods(&self, today: &glib::DateTime) -> [Option<CalendarActivityPeriod>; 2] {
        let (year, month, day) = (today.year(), today.month(), today.day_of_month() as u32);
        let last_year_day = glib::DateTime::from_local(year, 12, 31, 12, 0, 0.0)
            .expect("valid calendar year")
            .day_of_year() as u32;
        let year_day = today.day_of_year() as u32;
        let yearly = if !self.yearly_enabled {
            None
        } else if year_day.saturating_add(self.yearly_days_before_end) >= last_year_day {
            Some(year)
        } else if year_day <= self.yearly_days_after_end {
            Some(year - 1)
        } else {
            None
        }
        .filter(|year| Some(*year) != self.opened_year)
        .map(CalendarActivityPeriod::Year);

        let last_day = glib::DateTime::from_local(year, month, 1, 12, 0, 0.0)
            .and_then(|date| date.add_months(1))
            .and_then(|date| date.add_days(-1))
            .expect("valid calendar month")
            .day_of_month() as u32;
        let current = CalendarActivityPeriod::Month {
            year,
            month: month as u8,
        };
        let monthly = if !self.monthly_enabled {
            None
        } else if day.saturating_add(self.monthly_days_before_end) >= last_day {
            Some(current)
        } else if day <= self.monthly_days_after_end {
            Some(current.previous())
        } else {
            None
        }
        .filter(|period| match period {
            CalendarActivityPeriod::Month { year, month } => {
                self.opened_month.as_deref() != Some(format!("{year:04}-{month:02}").as_str())
            }
            _ => false,
        });
        [yearly, monthly]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: i32, day: i32) -> glib::DateTime {
        glib::DateTime::from_local(year, month, day, 12, 0, 0.0).unwrap()
    }

    #[test]
    fn monthly_window_includes_both_ends_and_short_months() {
        let settings = ActivityOverviewSettings::default();
        let august = Some(CalendarActivityPeriod::Month {
            year: 2026,
            month: 8,
        });
        assert_eq!(settings.startup_periods(&date(2026, 8, 30))[1], None);
        assert_eq!(settings.startup_periods(&date(2026, 8, 31))[1], august);
        assert_eq!(settings.startup_periods(&date(2026, 9, 1))[1], august);
        assert_eq!(settings.startup_periods(&date(2026, 9, 5))[1], august);
        assert_eq!(settings.startup_periods(&date(2026, 9, 6))[1], None);
        for (year, last) in [(2026, 28), (2028, 29)] {
            assert_eq!(
                settings.startup_periods(&date(year, 2, last))[1],
                Some(CalendarActivityPeriod::Month { year, month: 2 })
            );
        }
    }

    #[test]
    fn yearly_window_crosses_year_boundary_and_reports_open_once() {
        let mut settings = ActivityOverviewSettings::default();
        let yearly = Some(CalendarActivityPeriod::Year(2026));
        assert_eq!(settings.startup_periods(&date(2026, 12, 24))[0], None);
        assert_eq!(settings.startup_periods(&date(2026, 12, 25))[0], yearly);
        assert_eq!(settings.startup_periods(&date(2027, 1, 7))[0], yearly);
        assert_eq!(settings.startup_periods(&date(2027, 1, 8))[0], None);
        settings.opened_year = Some(2026);
        let december = Some(CalendarActivityPeriod::Month {
            year: 2026,
            month: 12,
        });
        assert_eq!(
            settings.startup_periods(&date(2027, 1, 1)),
            [None, december]
        );
        settings.opened_month = Some("2026-12".into());
        assert_eq!(settings.startup_periods(&date(2027, 1, 5)), [None, None]);
    }

    #[test]
    fn custom_dates_and_independent_switches_apply_at_startup() {
        let mut settings = ActivityOverviewSettings {
            monthly_days_before_end: 2,
            monthly_days_after_end: 2,
            yearly_days_before_end: 11,
            yearly_days_after_end: 3,
            ..Default::default()
        };
        assert!(settings.startup_periods(&date(2026, 8, 29))[1].is_some());
        assert!(settings.startup_periods(&date(2026, 9, 3))[1].is_none());
        assert!(settings.startup_periods(&date(2026, 12, 20))[0].is_some());
        assert!(settings.startup_periods(&date(2027, 1, 4))[0].is_none());
        settings.monthly_enabled = false;
        assert_eq!(
            settings.startup_periods(&date(2026, 12, 31)),
            [Some(CalendarActivityPeriod::Year(2026)), None]
        );
        settings.monthly_enabled = true;
        settings.yearly_enabled = false;
        assert_eq!(
            settings.startup_periods(&date(2026, 12, 31)),
            [
                None,
                Some(CalendarActivityPeriod::Month {
                    year: 2026,
                    month: 12
                })
            ]
        );
    }
}
