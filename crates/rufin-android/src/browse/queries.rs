use super::*;

#[uniffi::export]
impl AndroidBrowseQuery {
    pub fn activity_sensitive(&self) -> bool {
        let settings = self.settings.load();
        let display = settings.library_list(library_list_key(&self.route, self.category));
        rufin_core::browse::uses_activity(&self.route, &display)
            || self.sort.catalog_sort().activity_sensitive()
            || (self.release_section.is_none()
                && matches!(
                    self.route,
                    Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
                )
                && rufin_core::browse::uses_activity(
                    &self.route,
                    &settings.library_list(LibraryListKey::ArtistTracks),
                ))
    }

    pub fn context_id(&self) -> String {
        let context = match &self.route {
            Route::AlbumDetail(uri) => format!("album:{uri}"),
            Route::ArtistDetail(uri)
            | Route::ArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::ArtistDiscography(uri) => format!("artist:{uri}"),
            Route::AlbumArtistDetail(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri)
            | Route::AlbumArtistDiscography(uri) => format!("album-artist:{uri}"),
            Route::GenreDetail(key) => format!("genre:{key}"),
            Route::MoodDetail(key) => format!("mood:{key}"),
            Route::PlaylistDetail(key) => format!("playlist:{key}"),
            Route::SmartPlaylistDetail(key) => format!("smart-playlist:{key}"),
            _ => format!(
                "android:{}",
                serde_json::to_string(&self.route).expect("Route serialization")
            ),
        };
        format!(
            "{context}|query={}|source={}|folder={:?}|sort={:?}|descending={}|favorites={}|downloaded={}",
            self.filter,
            self.selected
                .as_ref()
                .map_or("rufin", |selected| selected.source_id.as_str()),
            self.selected
                .as_ref()
                .and_then(|selected| selected.music_folder_key),
            self.sort,
            self.descending,
            self.favorites(),
            self.downloaded()
        )
    }

    pub fn home_section_title(&self, id: String) -> Option<String> {
        let block =
            serde_json::from_value::<library::HomeBlockKind>(serde_json::Value::String(id)).ok()?;
        Some(localization::tr(
            rufin_core::settings::presentation::home_block_title(block),
        ))
    }
    pub fn supports_downloaded_filter(&self) -> bool {
        rufin_core::browse::supports_downloaded_filter(&self.route, self.category)
    }

    pub fn supports_favorite_filter(&self) -> bool {
        rufin_core::browse::supports_favorite_filter(&self.route, self.category)
    }

    pub fn breadcrumbs(&self) -> Vec<AndroidBreadcrumb> {
        let Route::Folders { path } = &self.route else {
            return Vec::new();
        };
        let mut items = vec![AndroidBreadcrumb {
            title: localization::tr("Folders"),
            route: Route::Folders { path: Vec::new() },
        }];
        items.extend(
            path.iter()
                .enumerate()
                .map(|(index, item)| AndroidBreadcrumb {
                    title: item.name.clone(),
                    route: Route::Folders {
                        path: path[..=index].to_vec(),
                    },
                }),
        );
        items
    }

    pub async fn section_positions(
        self: Arc<Self>,
    ) -> Result<Vec<AndroidScrollSection>, AndroidError> {
        if self.album_detail_layout() || matches!(self.route, Route::History) {
            return Ok(Vec::new());
        }
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            let sections = if let OpenedCollection::Folders(page) =
                self.opened(&cancel).await.map_err(error)?
            {
                page.projection
                    .sections(
                        &self.database,
                        self.selected().source_key,
                        &page.read,
                        &cancel,
                    )
                    .await
            } else if let Some(read) = self.members(&cancel).await.map_err(error)? {
                read.sections(&self.database, &cancel).await
            } else if let Some(read) = self.list(&cancel).await.map_err(error)? {
                if self.artist_release_identity().is_some() && self.release_section.is_none() {
                    return Ok(Vec::new());
                }
                if matches!(
                    self.route,
                    Route::Playlists | Route::SmartPlaylists | Route::PlaylistFolder { .. }
                ) {
                    read.playlist_items_sections(&self.database, &cancel).await
                } else {
                    read.sections(&self.database, &cancel).await
                }
            } else {
                Ok(Vec::new())
            }
            .map_err(error)?;
            Ok(sections
                .into_iter()
                .map(|section| AndroidScrollSection {
                    title: section.title,
                    index: section.index,
                })
                .collect())
        })
        .await
    }

    pub async fn play_section(
        self: Arc<Self>,
        section_id: String,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        if !matches!(self.route, Route::Home) {
            return Err(error("Sections belong to Home"));
        }
        let placement = queue_placement(&placement)?;
        let title = context_caption(&self.route, context_title);
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let cancel = ReadCancellation::new();
                let selections = self
                    .home_rows(&cancel)
                    .await
                    .map_err(error)?
                    .iter()
                    .filter(|(row, _)| row.section_id == section_id)
                    .map(|(_, selection)| selection.clone())
                    .collect();
                self.playback.play_selection(
                    rufin_core::playback::PlayRequest::captured(
                        MemberSelection::Concat(selections),
                        0,
                        placement,
                        false,
                    )
                    .with_context(format!("home:{}:{section_id}", self.selected().source_id).into())
                    .with_context_title(title)
                    .shuffled(shuffled),
                );
                Ok(())
            })
            .await
            .map_err(error)?
    }

    pub async fn count(self: Arc<Self>) -> Result<u64, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            let opened = self.opened(&cancel).await.map_err(error)?;
            let count = if matches!(opened, OpenedCollection::Home(_)) {
                self.home_rows(&cancel).await.map_err(error)?.len()
            } else {
                self.opened_count(opened)
            };
            Ok(count as u64)
        })
        .await
    }

    pub fn sort_fields(&self) -> Vec<AndroidSortField> {
        let key = library_list_key(&self.route, self.category);
        rufin_core::settings::available_sort_fields(key)
            .iter()
            .map(|field| AndroidSortField {
                id: *field,
                title: localization::tr(rufin_core::settings::presentation::library_field_title(
                    *field,
                )),
            })
            .collect()
    }

    pub fn display_settings(&self) -> AndroidBrowseDisplay {
        let mut display = self.display.clone();
        display.sort_key = self.sort;
        display.descending = self.descending;
        browse_display(library_list_key(&self.route, self.category), &display)
    }

    pub async fn page(
        self: Arc<Self>,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            self.rows(
                usize::try_from(offset).map_err(error)?,
                limit.min(256) as usize,
                &cancel,
            )
            .await
            .map_err(error)
        })
        .await
    }

    pub async fn play(
        self: Arc<Self>,
        index: u64,
        media_uri: String,
        track_key: Option<i64>,
        row_key: String,
        album_key: Option<i64>,
        album_media_uri: Option<String>,
        album_title: String,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let placement = queue_placement(&placement)?;
        let context_title = context_caption(&self.route, context_title);
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let index = usize::try_from(index).map_err(error)?;
                let cancel = ReadCancellation::new();
                let track_key = match track_key {
                    Some(key) => library::TrackKey::from_raw(key),
                    None => self
                        .database
                        .track(&media_uri, &cancel)
                        .await
                        .map_err(error)?
                        .map(|row| row.track_key)
                        .ok_or_else(|| error("Track is no longer available"))?,
                };
                let (selection, anchor, entry, title, context) = if self.album_detail_layout() {
                    let album = album_key
                        .map(library::AlbumKey::from_raw)
                        .ok_or_else(|| error("Album is no longer available"))?;
                    let mut query = MemberRead::new(Collection::Album(album));
                    query.sort = Some(self.album_tracks.sort_key.catalog_sort());
                    query.descending = self.album_tracks.descending;
                    let title = Some(Arc::new(library::QueueContextTitle {
                        kind: Some("Album".into()),
                        title: album_title.into(),
                    }));
                    (
                        query.selection(),
                        0,
                        None,
                        title,
                        format!(
                            "album:{}|query=|sort={:?}|descending={}",
                            album_media_uri.as_deref().unwrap_or_default(),
                            self.album_tracks.sort_key,
                            self.album_tracks.descending
                        ),
                    )
                } else if let Some(query) = self.members(&cancel).await.map_err(error)? {
                    let entry = if matches!(query.collection, Collection::Playlist(_)) {
                        Some(serde_json::from_str(&row_key).map_err(error)?)
                    } else {
                        None
                    };
                    (
                        query.clone().selection(),
                        if let OpenedCollection::Folders(page) =
                            self.opened(&cancel).await.map_err(error)?
                        {
                            index.saturating_sub(page.folder_count)
                        } else {
                            index
                        },
                        entry,
                        context_title,
                        self.context_id(),
                    )
                } else {
                    let rows = self.rows(0, usize::MAX, &cancel).await.map_err(error)?;
                    let keys = rows
                        .into_iter()
                        .filter_map(|row| row.track_key())
                        .collect::<Vec<_>>();
                    let anchor = keys
                        .iter()
                        .position(|key| *key == track_key)
                        .ok_or_else(|| error("Track is no longer in this route"))?;
                    (
                        MemberSelection::Keys {
                            source: None,
                            keys: keys.into(),
                        },
                        anchor,
                        None,
                        context_title,
                        self.context_id(),
                    )
                };
                self.playback.play_selection(
                    rufin_core::playback::PlayRequest::captured(
                        selection, anchor, placement, false,
                    )
                    .with_context(context.into())
                    .with_anchor(track_key, entry)
                    .with_context_title(title)
                    .shuffled(shuffled),
                );
                Ok(())
            })
            .await
            .map_err(error)?
    }
}
