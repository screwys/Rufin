use super::*;

#[uniffi::export]
impl AndroidBrowseQuery {
    pub fn activity_sensitive(&self) -> bool {
        let settings = self.settings.load();
        let display = settings.library_list(library_list_key(&self.route, self.category));
        let mut request = BrowseRequest::new(&self.route, self.category, None, None, &display, 0);
        request.sort = self.sort;
        request.activity_sensitive()
            || (self.release_section.is_none()
                && matches!(
                    self.route,
                    Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
                )
                && BrowseRequest::new(
                    &self.route,
                    CollectionCategory::Tracks,
                    None,
                    None,
                    &settings.library_list(LibraryListKey::ArtistTracks),
                    0,
                )
                .activity_sensitive())
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
        match self.route {
            Route::Tracks
            | Route::AlbumDetail(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_)
            | Route::ArtistFavoriteTracks(_)
            | Route::AlbumArtistFavoriteTracks(_) => true,
            Route::Favorites | Route::GenreDetail(_) | Route::MoodDetail(_) => {
                self.category == CollectionCategory::Tracks
            }
            _ => false,
        }
    }

    pub fn supports_favorite_filter(&self) -> bool {
        match self.route {
            Route::Tracks
            | Route::Albums
            | Route::Artists
            | Route::AlbumArtists
            | Route::AlbumDetail(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_) => true,
            Route::GenreDetail(_) | Route::MoodDetail(_) => {
                self.category == CollectionCategory::Tracks
            }
            _ => false,
        }
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
        runtime
            .spawn(async move {
                if matches!(self.route, Route::Folders { .. }) {
                    return self.folder_sections().await;
                }
                if let Some(query) = self.track_query().await.map_err(error)? {
                    return Ok((query)
                        .sections(&self.database, &ReadCancellation::new())
                        .await
                        .map_err(error)?
                        .into_iter()
                        .map(|section| AndroidScrollSection {
                            title: section.title,
                            index: section.index,
                        })
                        .collect());
                }
                let sections = match self.query().await.map_err(error)? {
                    BrowseQuery::Collection(query) => {
                        query
                            .sections(&self.database, &ReadCancellation::new())
                            .await
                    }
                    BrowseQuery::Releases(query) => match self.release_section {
                        Some(ArtistReleaseSection::Class(class)) => {
                            query
                                .sections(&self.database, class, &ReadCancellation::new())
                                .await
                        }
                        _ => Ok(Vec::new()),
                    },
                    _ => Ok(Vec::new()),
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
            .map_err(error)?
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
                let selections = self
                    .home_rows()
                    .await
                    .map_err(error)?
                    .iter()
                    .filter(|(row, _)| row.section_id == section_id)
                    .map(|(_, selection)| selection.clone())
                    .collect();
                self.playback.play_selection(
                    rufin_core::playback::PlayRequest::captured(
                        TrackSelection::Concat(selections),
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
        runtime
            .spawn(async move {
                if matches!(self.route, Route::Folders { .. }) {
                    let (folders, tracks) = self.folder_counts().await.map_err(error)?;
                    return Ok(folders as u64 + tracks.max(0) as u64);
                }
                if let Some(query) = self.track_query().await.map_err(error)? {
                    return Ok((query)
                        .count(&self.database, &ReadCancellation::new())
                        .await
                        .map_err(error)?
                        .max(0) as u64);
                }
                let count = match self.query().await.map_err(error)? {
                    BrowseQuery::Collection(query) => query
                        .count(&self.database, &ReadCancellation::new())
                        .await
                        .map_err(error)?,
                    BrowseQuery::Releases(_) => {
                        let counts = self.release_counts().await?;
                        match self.release_section {
                            Some(ArtistReleaseSection::Class(class)) => {
                                counts[release_index(class)]
                            }
                            Some(ArtistReleaseSection::AppearsOn) => 0,
                            None => counts.iter().sum(),
                        }
                        .max(0) as usize
                    }
                    _ if matches!(self.route, Route::Home) => {
                        self.home_rows().await.map_err(error)?.len()
                    }
                    _ if matches!(self.route, Route::Search) => {
                        self.rows(0, usize::MAX).await.map_err(error)?.len()
                    }
                    _ => 0,
                };
                Ok(count as u64)
            })
            .await
            .map_err(error)?
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
        runtime
            .spawn(async move {
                self.rows(
                    usize::try_from(offset).map_err(error)?,
                    limit.min(256) as usize,
                )
                .await
                .map_err(error)
            })
            .await
            .map_err(error)?
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
                if matches!(self.route, Route::Folders { .. }) {
                    return self
                        .folder_play(
                            index,
                            media_uri,
                            track_key,
                            placement,
                            shuffled,
                            context_title,
                        )
                        .await;
                }
                let index = usize::try_from(index).map_err(error)?;
                let cancel = ReadCancellation::new();
                let track_key = match track_key {
                    Some(key) => library::TrackKey::from_raw(key),
                    None => self
                        .database
                        .catalog_identity(library::CatalogKind::Track, &media_uri, &cancel)
                        .await
                        .map(|identity| {
                            identity.map(|identity| library::TrackKey::from_raw(identity.key))
                        })
                        .map_err(error)?
                        .ok_or_else(|| error("Track is no longer available"))?,
                };
                let (selection, anchor, entry, title, context) = if self.album_detail_layout() {
                    let album = album_key
                        .map(library::AlbumKey::from_raw)
                        .ok_or_else(|| error("Album is no longer available"))?;
                    let mut query = CatalogQuery::tracks(TrackScope::Album(album));
                    query.subject = library::CatalogSubject::Tracks {
                        scope: TrackScope::Album(album),
                        sort: Some(self.album_tracks.sort_key.catalog_sort()),
                    };
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
                } else if let Some(query) = self.track_query().await.map_err(error)? {
                    let entry = if matches!(
                        query.subject,
                        library::CatalogSubject::Tracks {
                            scope: library::TrackScope::Playlist(_),
                            ..
                        }
                    ) {
                        Some(serde_json::from_str(&row_key).map_err(error)?)
                    } else {
                        None
                    };
                    (
                        query.clone().selection(),
                        index,
                        entry,
                        context_title,
                        self.context_id(),
                    )
                } else {
                    let rows = self.rows(0, usize::MAX).await.map_err(error)?;
                    let keys = rows
                        .into_iter()
                        .filter_map(|row| row.track_key())
                        .collect::<Vec<_>>();
                    let anchor = keys
                        .iter()
                        .position(|key| *key == track_key)
                        .ok_or_else(|| error("Track is no longer in this route"))?;
                    (
                        library::CatalogQuery::keys(
                            library::CatalogKind::Track,
                            None,
                            (keys).iter().map(|key| key.raw()).collect(),
                        )
                        .selection(),
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
