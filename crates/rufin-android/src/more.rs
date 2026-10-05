use std::sync::Arc;

use rufin_core::runtime::{ProductHandles, ReleaseHistory, ReleaseUpdate, ReleaseUpdateHandle};

use crate::host::{AndroidError, error};

#[derive(Clone, PartialEq, uniffi::Record)]
pub struct AndroidReleaseNote {
    pub version: String,
    pub date: String,
    pub url: String,
    pub body: String,
    pub html: String,
}

#[derive(Clone, PartialEq, uniffi::Record)]
pub struct AndroidReleaseHistory {
    pub notes: Vec<AndroidReleaseNote>,
    pub installed_version: String,
    pub available_version: Option<String>,
    pub updating_version: Option<String>,
    pub ready_version: Option<String>,
    pub error: Option<String>,
    pub notification_version: Option<String>,
}

impl From<ReleaseHistory> for AndroidReleaseHistory {
    fn from(history: ReleaseHistory) -> Self {
        Self {
            notes: history
                .notes
                .iter()
                .map(|note| {
                    let mut html = String::new();
                    pulldown_cmark::html::push_html(
                        &mut html,
                        pulldown_cmark::Parser::new_ext(
                            &note.body,
                            pulldown_cmark::Options::ENABLE_TABLES
                                | pulldown_cmark::Options::ENABLE_STRIKETHROUGH,
                        ),
                    );
                    AndroidReleaseNote {
                        version: note.version.clone(),
                        date: note.date.clone(),
                        url: note.url.clone(),
                        body: note.body.clone(),
                        html,
                    }
                })
                .collect(),
            installed_version: history.installed_version,
            available_version: history.available_version,
            updating_version: None,
            ready_version: None,
            error: None,
            notification_version: None,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidActivityPeriod {
    pub year: i32,
    pub month: Option<u8>,
}

#[derive(uniffi::Record)]
pub struct AndroidActivityTotals {
    pub plays: i64,
    pub duration_millis: i64,
    pub tracks: i64,
    pub artists: i64,
}

impl From<library::ActivityTotals> for AndroidActivityTotals {
    fn from(value: library::ActivityTotals) -> Self {
        Self {
            plays: value.plays,
            duration_millis: value.duration_millis,
            tracks: value.tracks,
            artists: value.artists,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidOverviewItem {
    pub kind: String,
    pub media_uri: String,
    pub source_id: Option<String>,
    pub title: String,
    pub subtitle: String,
    pub plays: i64,
    pub artwork_identity: Option<Vec<u8>>,
    pub detail_route: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidGenreActivity {
    pub name: String,
    pub plays: i64,
}

#[derive(uniffi::Record)]
pub struct AndroidActivityOverview {
    pub title: String,
    pub key: String,
    pub totals: AndroidActivityTotals,
    pub previous: AndroidActivityTotals,
    pub tracks: Vec<AndroidOverviewItem>,
    pub albums: Vec<AndroidOverviewItem>,
    pub artists: Vec<AndroidOverviewItem>,
    pub genres: Vec<AndroidGenreActivity>,
}

#[derive(uniffi::Object)]
pub struct AndroidMore {
    database: Arc<library::Database>,
    settings: rufin_core::SettingsHandle,
    artwork: artwork::Artwork,
    runtime: tokio::runtime::Handle,
    releases: ReleaseUpdateHandle,
    history: tokio::sync::watch::Sender<AndroidReleaseHistory>,
}

impl AndroidMore {
    pub fn new(
        products: &ProductHandles,
        initial: ReleaseHistory,
        events: async_channel::Receiver<ReleaseUpdate>,
    ) -> Self {
        let (sender, _) = tokio::sync::watch::channel(AndroidReleaseHistory::from(initial));
        let history = sender.clone();
        products.runtime.spawn(async move {
            while let Ok(event) = events.recv().await {
                sender.send_if_modified(|current| {
                    let previous = current.clone();
                    match event {
                        ReleaseUpdate::Refreshed {
                            history,
                            notification_version,
                        } => {
                            *current = history.into();
                            current.notification_version = notification_version;
                        }
                        ReleaseUpdate::Updating { version } => {
                            current.updating_version = Some(version);
                            current.error = None;
                        }
                        ReleaseUpdate::Ready { version } => {
                            current.ready_version = Some(version);
                            current.updating_version = None;
                        }
                        ReleaseUpdate::Updated { version, .. } => {
                            current.installed_version = version;
                            current.available_version = None;
                            current.updating_version = None;
                        }
                        ReleaseUpdate::Failed { error, .. } => {
                            current.error = Some(error);
                            current.updating_version = None;
                        }
                        ReleaseUpdate::Restarting { .. } => {}
                    }
                    *current != previous
                });
            }
        });
        Self {
            database: products.library.clone(),
            settings: products.settings.clone(),
            artwork: products.artwork.clone(),
            runtime: products.runtime.clone(),
            releases: products.release_updates.clone(),
            history,
        }
    }
}

#[uniffi::export]
impl AndroidMore {
    pub async fn startup_activity_period(
        &self,
    ) -> Result<Option<AndroidActivityPeriod>, AndroidError> {
        let today = glib::DateTime::now_local().map_err(error)?;
        let periods = self
            .settings
            .load()
            .activity_overview
            .startup_periods(&today);
        if periods.iter().all(Option::is_none) {
            return Ok(None);
        }
        let database = self.database.clone();
        let months = self
            .runtime
            .spawn(async move { database.activity_months().await })
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(periods
            .into_iter()
            .flatten()
            .find_map(|period| match period {
                library::CalendarActivityPeriod::Month { year, month }
                    if months.contains(&rufin_core::activity::period_key(period)) =>
                {
                    Some(AndroidActivityPeriod {
                        year,
                        month: Some(month),
                    })
                }
                library::CalendarActivityPeriod::Year(year)
                    if months
                        .iter()
                        .any(|month| month.starts_with(&format!("{year}-"))) =>
                {
                    Some(AndroidActivityPeriod { year, month: None })
                }
                _ => None,
            }))
    }

    pub async fn mark_activity_opened(
        &self,
        year: i32,
        month: Option<u8>,
    ) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || {
                settings.update_preferences(|settings| {
                    match month {
                        Some(month) => {
                            settings.activity_overview.opened_month =
                                Some(format!("{year:04}-{month:02}"))
                        }
                        None => settings.activity_overview.opened_year = Some(year),
                    }
                    Ok(())
                })
            })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn subscribe_releases(&self) -> Arc<AndroidReleaseSubscription> {
        Arc::new(AndroidReleaseSubscription {
            state: tokio::sync::Mutex::new((self.history.subscribe(), true)),
        })
    }

    pub fn check_releases_automatically(&self) {
        self.releases.check();
    }

    pub async fn mark_release_seen(&self, version: String) -> Result<(), AndroidError> {
        let releases = self.releases.clone();
        let seen = version.clone();
        self.runtime
            .spawn_blocking(move || releases.mark_seen(seen))
            .await
            .map_err(error)?
            .map_err(error)?;
        self.history.send_if_modified(|history| {
            if history.notification_version.as_deref() == Some(version.as_str()) {
                history.notification_version = None;
                true
            } else {
                false
            }
        });
        Ok(())
    }

    pub async fn check_releases(&self) -> bool {
        self.releases.check_now().recv().await.unwrap_or(false)
    }

    pub fn activity_preferences(&self) -> String {
        serde_json::to_string(&self.settings.load().activity_overview)
            .expect("Activity preferences serialization")
    }

    pub async fn set_activity_preference(
        &self,
        field: String,
        value_json: String,
    ) -> Result<String, AndroidError> {
        let value = serde_json::from_str(&value_json).map_err(error)?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || {
                settings
                    .set_activity_overview_field(field, value)
                    .map_err(error)?;
                serde_json::to_string(&settings.load().activity_overview).map_err(error)
            })
            .await
            .map_err(error)?
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
        let database = self.database.clone();
        let settings = self.settings.load().activity_overview;
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
                let report = database
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
        let database = self.database.clone();
        self.runtime
            .spawn(async move { database.activity_months().await })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn activity_overview(
        &self,
        year: i32,
        month: Option<u8>,
    ) -> Result<AndroidActivityOverview, AndroidError> {
        let database = self.database.clone();
        let period = match month {
            Some(month) => library::CalendarActivityPeriod::Month { year, month },
            None => library::CalendarActivityPeriod::Year(year),
        };
        let report = self
            .runtime
            .spawn(async move {
                database
                    .activity_overview(period, 10, &library::ReadCancellation::new())
                    .await
            })
            .await
            .map_err(error)?
            .map_err(error)?;
        let source =
            |uri: &str| library::source_entity_parts(uri).map(|(source, _, _)| source.to_string());
        Ok(AndroidActivityOverview {
            title: rufin_core::activity::period_label(period),
            key: rufin_core::activity::period_key(period),
            totals: report.totals.into(),
            previous: report.previous.into(),
            tracks: report
                .tracks
                .into_iter()
                .map(|row| AndroidOverviewItem {
                    source_id: source(&row.media_uri),
                    kind: "track".into(),
                    media_uri: row.media_uri,
                    title: row.title,
                    subtitle: format!("{} · {}", row.artist, row.album),
                    plays: row.play_count,
                    artwork_identity: row.artwork_binding,
                    detail_route: None,
                })
                .collect(),
            albums: report
                .albums
                .into_iter()
                .map(|row| AndroidOverviewItem {
                    source_id: source(&row.media_uri),
                    kind: "album".into(),
                    detail_route: Some(
                        serde_json::to_string(&rufin_core::route::Route::AlbumDetail(
                            row.media_uri.clone(),
                        ))
                        .expect("Album route serialization"),
                    ),
                    media_uri: row.media_uri,
                    title: row.title,
                    subtitle: row.display_artist,
                    plays: row.play_count,
                    artwork_identity: row.artwork_binding,
                })
                .collect(),
            artists: report
                .artists
                .into_iter()
                .map(|row| AndroidOverviewItem {
                    source_id: source(&row.media_uri),
                    kind: "artist".into(),
                    detail_route: Some(
                        serde_json::to_string(&rufin_core::route::Route::ArtistDetail(
                            row.media_uri.clone(),
                        ))
                        .expect("Artist route serialization"),
                    ),
                    media_uri: row.media_uri,
                    title: row.name,
                    subtitle: String::new(),
                    plays: row.play_count,
                    artwork_identity: row.artwork_binding,
                })
                .collect(),
            genres: report
                .genres
                .into_iter()
                .map(|(name, plays)| AndroidGenreActivity { name, plays })
                .collect(),
        })
    }
}

#[derive(uniffi::Object)]
pub struct AndroidReleaseSubscription {
    state: tokio::sync::Mutex<(tokio::sync::watch::Receiver<AndroidReleaseHistory>, bool)>,
}

#[uniffi::export]
impl AndroidReleaseSubscription {
    pub async fn next(&self) -> Result<AndroidReleaseHistory, AndroidError> {
        let mut state = self.state.lock().await;
        if state.1 {
            state.1 = false;
        } else {
            state.0.changed().await.map_err(error)?;
        }
        Ok(state.0.borrow_and_update().clone())
    }
}
