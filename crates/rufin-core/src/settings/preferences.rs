use super::{
    ContextMenuSettings, ExternalSiteLinkSettings, HomeBlockKind, SettingsError, SettingsOwner, app,
};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ApplicationPreferences {
    pub private_mode: bool,
    pub cast_proxy_enabled: bool,
    pub external_metadata_enabled: bool,
    pub prefer_distinct_track_covers: bool,
    pub notifications_enabled: bool,
    pub control_notifications_enabled: bool,
    pub release_notifications_enabled: bool,
    pub release_check_interval_hours: u32,
    pub reduce_motion: bool,
    pub show_downloaded_badges: bool,
    pub new_playlist_current: bool,
    pub auto_dj_refill_threshold: u8,
    pub external_site_links: ExternalSiteLinkSettings,
    pub home_blocks: Vec<HomeBlockKind>,
    pub context_menu: ContextMenuSettings,
    pub activity_overview: app::ActivityOverviewSettings,
    pub backup: crate::runtime::BackupSettings,
    pub secret_storage_mode: secrets::SecretStorageMode,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ApplicationPreferenceEdit {
    PrivateMode {
        value: bool,
    },
    CastProxyEnabled {
        value: bool,
    },
    ExternalMetadataEnabled {
        value: bool,
    },
    PreferDistinctTrackCovers {
        value: bool,
    },
    NotificationsEnabled {
        value: bool,
    },
    ControlNotificationsEnabled {
        value: bool,
    },
    ReleaseNotificationsEnabled {
        value: bool,
    },
    ReleaseCheckIntervalHours {
        value: u32,
    },
    ReduceMotion {
        value: bool,
    },
    ShowDownloadedBadges {
        value: bool,
    },
    NewPlaylistCurrent {
        value: bool,
    },
    AutoDjRefillThreshold {
        value: u8,
    },
    ExternalSiteLinks {
        value: ExternalSiteLinkSettings,
    },
    HomeBlocks {
        value: Vec<HomeBlockKind>,
    },
    ContextMenu {
        value: ContextMenuSettings,
    },
    ActivityOverview {
        value: app::ActivityOverviewSettings,
    },
    Backup {
        value: crate::runtime::BackupSettings,
    },
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn application_preferences(&self) -> ApplicationPreferences {
        let ui = self.load();
        ApplicationPreferences {
            private_mode: ui.private_mode,
            cast_proxy_enabled: ui.cast_proxy_enabled,
            external_metadata_enabled: ui.external_metadata_enabled,
            prefer_distinct_track_covers: ui.prefer_distinct_track_covers,
            notifications_enabled: ui.notifications_enabled,
            control_notifications_enabled: ui.control_notifications_enabled,
            release_notifications_enabled: ui.release_notifications_enabled,
            release_check_interval_hours: ui.release_check_interval_hours,
            reduce_motion: ui.reduce_motion,
            show_downloaded_badges: ui.show_downloaded_badges,
            new_playlist_current: ui.new_playlist_current,
            auto_dj_refill_threshold: ui.auto_dj_refill_threshold,
            external_site_links: ui.external_site_links,
            home_blocks: ui.home_blocks,
            context_menu: ui.context_menu,
            activity_overview: ui.activity_overview,
            backup: ui.backup,
            secret_storage_mode: ui.secret_storage_mode,
        }
    }

    pub fn edit_application_preferences(
        &self,
        edit: ApplicationPreferenceEdit,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            match edit {
                ApplicationPreferenceEdit::PrivateMode { value } => ui.private_mode = value,
                ApplicationPreferenceEdit::CastProxyEnabled { value } => {
                    ui.cast_proxy_enabled = value
                }
                ApplicationPreferenceEdit::ExternalMetadataEnabled { value } => {
                    ui.external_metadata_enabled = value
                }
                ApplicationPreferenceEdit::PreferDistinctTrackCovers { value } => {
                    ui.prefer_distinct_track_covers = value
                }
                ApplicationPreferenceEdit::NotificationsEnabled { value } => {
                    ui.notifications_enabled = value
                }
                ApplicationPreferenceEdit::ControlNotificationsEnabled { value } => {
                    ui.control_notifications_enabled = value
                }
                ApplicationPreferenceEdit::ReleaseNotificationsEnabled { value } => {
                    ui.release_notifications_enabled = value
                }
                ApplicationPreferenceEdit::ReleaseCheckIntervalHours { value } => {
                    ui.release_check_interval_hours = value
                }
                ApplicationPreferenceEdit::ReduceMotion { value } => ui.reduce_motion = value,
                ApplicationPreferenceEdit::ShowDownloadedBadges { value } => {
                    ui.show_downloaded_badges = value
                }
                ApplicationPreferenceEdit::NewPlaylistCurrent { value } => {
                    ui.new_playlist_current = value
                }
                ApplicationPreferenceEdit::AutoDjRefillThreshold { value } => {
                    ui.auto_dj_refill_threshold = value
                }
                ApplicationPreferenceEdit::ExternalSiteLinks { value } => {
                    ui.external_site_links = value
                }
                ApplicationPreferenceEdit::HomeBlocks { value } => ui.home_blocks = value,
                ApplicationPreferenceEdit::ContextMenu { value } => ui.context_menu = value,
                ApplicationPreferenceEdit::ActivityOverview { mut value } => {
                    value.opened_month = ui.activity_overview.opened_month.clone();
                    value.opened_year = ui.activity_overview.opened_year;
                    ui.activity_overview = value;
                }
                ApplicationPreferenceEdit::Backup { mut value } => {
                    value.schedule.schedule_id = ui.backup.schedule.schedule_id.clone();
                    value.schedule.last_successful_at = ui.backup.schedule.last_successful_at;
                    ui.backup = value;
                }
            }
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn random_preferences(&self) -> super::RandomPlaySettings {
        self.load().random_play
    }

    pub fn set_random_preferences(
        &self,
        value: super::RandomPlaySettings,
    ) -> Result<super::RandomPlaySettings, SettingsError> {
        self.update_preferences(move |ui| {
            ui.random_play = value;
            Ok(())
        })?;
        Ok(self.random_preferences())
    }

    pub fn mark_activity_opened(
        &self,
        period: library::CalendarActivityPeriod,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.activity_overview.mark_opened(period);
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn activity_preferences(&self) -> app::ActivityOverviewSettings {
        self.load().activity_overview
    }

    pub fn set_activity_preferences(
        &self,
        value: app::ActivityOverviewSettings,
    ) -> Result<(), SettingsError> {
        self.edit_application_preferences(ApplicationPreferenceEdit::ActivityOverview { value })
    }
}
