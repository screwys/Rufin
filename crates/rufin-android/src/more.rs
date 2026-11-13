use crate::host::{AndroidError, error};
use rufin_core::runtime::ProductHandles;
use std::sync::Arc;

#[derive(uniffi::Object)]
pub struct AndroidMore {
    catalog: Arc<library::Catalog>,
    settings: rufin_core::SettingsHandle,
    artwork: rufin_core::artwork::Artwork,
    runtime: tokio::runtime::Handle,
}
impl AndroidMore {
    pub fn new(products: &ProductHandles) -> Self {
        Self {
            catalog: products.library.clone(),
            settings: products.settings.clone(),
            artwork: products.artwork.clone(),
            runtime: products.runtime.clone(),
        }
    }
}
#[uniffi::export]
impl AndroidMore {
    pub async fn startup_activity_period(
        &self,
    ) -> Result<Option<library::CalendarActivityPeriod>, AndroidError> {
        let today = glib::DateTime::now_local().map_err(error)?;
        let settings = self.settings.load().application.activity_overview.clone();
        if settings.startup_periods(&today).iter().all(Option::is_none) {
            return Ok(None);
        }
        let months = self.activity_months().await?;
        Ok(settings.startup_period(&today, &months))
    }
    pub async fn export_overview_png(
        &self,
        year: i32,
        month: Option<u8>,
        dark: bool,
        foreground: u32,
        background: u32,
        accent: u32,
    ) -> Result<Vec<u8>, AndroidError> {
        let catalog = self.catalog.clone();
        let settings = self.settings.load().application.activity_overview.clone();
        let artwork = self.artwork.clone();
        let rgb = |color: u32| [(color >> 16) as u8, (color >> 8) as u8, color as u8];
        let appearance = rufin_core::activity::ReportAppearance {
            dark,
            foreground: rgb(foreground),
            background: rgb(background),
            accent: rgb(accent),
        };
        let period = match month {
            Some(month) => library::CalendarActivityPeriod::Month { year, month },
            None => library::CalendarActivityPeriod::Year(year),
        };
        self.runtime
            .spawn(async move {
                let report = catalog
                    .activity_overview(period, 10, &library::ReadCancellation::new())
                    .await
                    .map_err(error)?;
                rufin_core::activity::export_png(report, period, settings, appearance, artwork)
                    .await
                    .map_err(error)
            })
            .await
            .map_err(error)?
    }

    pub async fn activity_months(&self) -> Result<Vec<String>, AndroidError> {
        let catalog = self.catalog.clone();
        self.runtime
            .spawn(async move { catalog.activity_months().await })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn activity_overview(
        &self,
        year: i32,
        month: Option<u8>,
    ) -> Result<library::ActivityOverview, AndroidError> {
        let catalog = self.catalog.clone();
        let period = month.map_or(library::CalendarActivityPeriod::Year(year), |month| {
            library::CalendarActivityPeriod::Month { year, month }
        });
        self.runtime
            .spawn(async move {
                catalog
                    .activity_overview(period, 10, &library::ReadCancellation::new())
                    .await
            })
            .await
            .map_err(error)?
            .map_err(error)
    }
}
