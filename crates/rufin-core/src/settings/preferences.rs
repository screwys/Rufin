use std::sync::Arc;

use super::{PlayerPreferences, SettingsError, SettingsOwner, StoredSettings, app};

/// What the Android mirrors keep of the committed preferences.
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CommittedPreferences {
    pub player: PlayerPreferences,
    pub cast_network_interface: Option<String>,
    /// The sidebar routes the tabs show, and the pins the Pins page shows.
    pub route_items: Vec<super::SidebarRouteItemSettings>,
    pub pins: Vec<super::SidebarPin>,
}

/// Committed preferences for one observer. The first call returns the current value.
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct PreferenceSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<Arc<StoredSettings>>>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl PreferenceSubscription {
    pub async fn next(&self) -> Result<CommittedPreferences, SettingsError> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SettingsError::from(error.to_string()))?;
        let settings = Arc::clone(&changes.borrow_and_update());
        Ok(CommittedPreferences {
            player: PlayerPreferences::from(&settings.ui),
            cast_network_interface: settings.ui.cast_network_interface.clone(),
            route_items: settings.ui.sidebar.route_items.clone(),
            pins: (settings.ui.sidebar.pins_visible)
                .then(|| settings.ui.sidebar.pins.clone())
                .unwrap_or_default(),
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn subscribe_preferences(&self) -> Arc<PreferenceSubscription> {
        let mut changes = self.changes();
        changes.mark_changed();
        Arc::new(PreferenceSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }

    pub fn application_preferences(&self) -> app::ApplicationPreferences {
        self.file.load().ui.application.clone()
    }

    pub fn set_backup_preferences(
        &self,
        mut value: crate::backup::BackupSettings,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            value.schedule.schedule_id = ui.application.backup.schedule.schedule_id.clone();
            value.schedule.last_successful_at = ui.application.backup.schedule.last_successful_at;
            ui.application.backup = value;
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn random_preferences(&self) -> super::RandomPlaySettings {
        self.load().random_play.clone()
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
            ui.application.activity_overview.mark_opened(period);
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn activity_preferences(&self) -> app::ActivityOverviewSettings {
        self.load().application.activity_overview.clone()
    }

    pub fn set_activity_preferences(
        &self,
        value: app::ActivityOverviewSettings,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            let current = &mut ui.application.activity_overview;
            let mut value = value;
            value.opened_month = current.opened_month.clone();
            value.opened_year = current.opened_year;
            *current = value;
            Ok(())
        })
        .map_err(Into::into)
    }
}
