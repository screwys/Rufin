use super::*;

#[uniffi::export]
impl AndroidBrowseQuery {
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
            route: route_json(Route::Folders { path: Vec::new() }),
        }];
        items.extend(
            path.iter()
                .enumerate()
                .map(|(index, item)| AndroidBreadcrumb {
                    title: item.name.clone(),
                    route: route_json(Route::Folders {
                        path: path[..=index].to_vec(),
                    }),
                }),
        );
        items
    }
    pub fn route_kind(&self) -> String {
        core_route_kind(&self.route)
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
                let db = &self.database;
                let selected_source = self.selected.as_ref().map(|selected| selected.source_key);
                let folder = self
                    .selected
                    .as_ref()
                    .and_then(|selected| selected.music_folder_key);
                let filter = &self.filter;

                let sort = self.sort;
                let descending = self.descending;
                let cancel = ReadCancellation::new();
                let sections = match &self.route {
                    Route::Albums | Route::Favorites
                        if !matches!(self.route, Route::Favorites)
                            || self.category == CollectionCategory::Albums =>
                    {
                        db.album_section_positions(
                            self.selected().source_key,
                            folder,
                            self.favorites(),
                            filter,
                            sort.album_sort(),
                            descending,
                            &cancel,
                        )
                        .await
                    }
                    Route::Artists | Route::AlbumArtists | Route::Favorites
                        if !matches!(self.route, Route::Favorites)
                            || self.category == CollectionCategory::Artists =>
                    {
                        db.artist_section_positions(
                            self.selected().source_key,
                            folder,
                            matches!(self.route, Route::AlbumArtists),
                            self.favorites(),
                            filter,
                            sort.artist_sort(),
                            descending,
                            &cancel,
                        )
                        .await
                    }
                    Route::ArtistDetail(uri)
                    | Route::AlbumArtistDetail(uri)
                    | Route::ArtistDiscography(uri)
                    | Route::AlbumArtistDiscography(uri)
                        if self.category == CollectionCategory::Albums
                            || matches!(
                                self.route,
                                Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_)
                            ) =>
                    {
                        let (_, _, object) = library::source_entity_parts(uri)
                            .ok_or_else(|| error("Artist identity is unavailable"))?;
                        let Some(key) = db
                            .artist_key_by_object(self.selected().source_key, &object, &cancel)
                            .await
                            .map_err(error)?
                        else {
                            return Ok(Vec::new());
                        };
                        match self.release_section {
                            Some(ArtistReleaseSection::Class(class)) => {
                                db.artist_release_scroll_sections(
                                    self.selected().source_key,
                                    key,
                                    matches!(
                                        self.route,
                                        Route::AlbumArtistDetail(_)
                                            | Route::AlbumArtistDiscography(_)
                                    ),
                                    folder,
                                    filter,
                                    sort.album_sort(),
                                    descending,
                                    class,
                                    &cancel,
                                )
                                .await
                            }
                            _ => Ok(Vec::new()),
                        }
                    }
                    Route::Genres => {
                        db.genre_section_positions(
                            self.selected().source_key,
                            folder,
                            filter,
                            sort.genre_sort(),
                            descending,
                            &cancel,
                        )
                        .await
                    }
                    Route::Moods => {
                        db.mood_section_positions(
                            self.selected().source_key,
                            folder,
                            filter,
                            sort.mood_sort(),
                            descending,
                            &cancel,
                        )
                        .await
                    }
                    Route::Playlists => {
                        db.playlist_section_positions(
                            selected_source,
                            folder,
                            sort.playlist_sort(),
                            descending,
                            filter,
                            &cancel,
                        )
                        .await
                    }
                    Route::SmartPlaylists => {
                        db.smart_playlist_section_positions(
                            selected_source,
                            folder,
                            sort.smart_playlist_sort(),
                            descending,
                            filter,
                            self.now,
                            &cancel,
                        )
                        .await
                    }
                    Route::PlaylistDetail(key) => {
                        db.playlist_track_section_positions(
                            *key,
                            folder,
                            sort.playlist_entry_sort(),
                            descending,
                            filter,
                            &cancel,
                        )
                        .await
                    }
                    Route::SmartPlaylistDetail(key) => {
                        db.smart_track_section_positions(
                            selected_source,
                            *key,
                            folder,
                            filter,
                            sort.track_sort(),
                            descending,
                            self.now,
                            &cancel,
                        )
                        .await
                    }
                    Route::Home | Route::Search | Route::History => Ok(Vec::new()),
                    Route::Folders { .. } => {
                        unreachable!("Folder section projection is handled first")
                    }
                    _ => {
                        db.track_section_positions(
                            &library::TrackQuery {
                                source: self.selected().source_key,
                                folder,
                                collection: self.collection(),
                                downloaded_only: self.downloaded(),
                                favorites_only: self.favorites(),
                            },
                            filter,
                            sort.track_sort(),
                            descending,
                            &cancel,
                        )
                        .await
                    }
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
        let context_title = context_caption(&self.route, context_title);
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let rows = self.rows(0, usize::MAX).await.map_err(error)?;
                let context = format!("home:{}:{section_id}", self.selected().source_id);
                let inputs = rows
                    .into_iter()
                    .filter(|row| row.section_id == section_id)
                    .filter_map(|row| {
                        let target = match row.kind.as_str() {
                            "track" => rufin_core::playback::PlaybackTarget::Track(row.media_uri),
                            "album" => rufin_core::playback::PlaybackTarget::Album(row.media_uri),
                            "genre" => {
                                let Route::GenreDetail(key) =
                                    serde_json::from_str(row.detail_route.as_deref()?).ok()?
                                else {
                                    return None;
                                };
                                rufin_core::playback::PlaybackTarget::Genre(key)
                            }
                            _ => return None,
                        };
                        Some(target.in_context(context.clone()).queue_input(
                            Some(self.selected().source_key),
                            self.selected().music_folder_key,
                        ))
                    })
                    .collect();
                self.queue.play(
                    playback::PlayRequest::captured(
                        library::QueueInput::Groups(inputs),
                        0,
                        placement,
                        false,
                    )
                    .with_context_title(context_title.clone())
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
                if matches!(self.route, Route::History) {
                    return Ok(self.history_rows(0, usize::MAX).await.map_err(error)?.len() as u64);
                }
                if matches!(self.route, Route::Folders { .. }) {
                    let (folders, tracks) = self.folder_counts().await.map_err(error)?;
                    return Ok(folders as u64 + tracks.max(0) as u64);
                }
                let db = &self.database;
                let selected_source = self.selected.as_ref().map(|selected| selected.source_key);
                let folder = self
                    .selected
                    .as_ref()
                    .and_then(|selected| selected.music_folder_key);

                let cancel = ReadCancellation::new();
                let filter = &self.filter;
                if self.album_detail_layout() {
                    return Ok(db
                        .album_detail_route_count(
                            self.selected().source_key,
                            folder,
                            self.favorites(),
                            filter,
                            self.sort.album_sort(),
                            self.descending,
                            &cancel,
                        )
                        .await
                        .map_err(error)?
                        .max(0) as u64);
                }
                let count = match &self.route {
                    Route::Albums | Route::Favorites
                        if !matches!(self.route, Route::Favorites)
                            || self.category == CollectionCategory::Albums =>
                    {
                        db.album_count(
                            self.selected().source_key,
                            folder,
                            self.favorites(),
                            filter,
                            self.sort.album_sort(),
                            &cancel,
                        )
                        .await
                    }
                    Route::Artists | Route::AlbumArtists | Route::Favorites
                        if !matches!(self.route, Route::Favorites)
                            || self.category == CollectionCategory::Artists =>
                    {
                        db.artist_count(
                            self.selected().source_key,
                            folder,
                            matches!(self.route, Route::AlbumArtists),
                            self.favorites(),
                            filter,
                            &cancel,
                        )
                        .await
                    }
                    Route::ArtistDetail(_)
                    | Route::AlbumArtistDetail(_)
                    | Route::ArtistDiscography(_)
                    | Route::AlbumArtistDiscography(_)
                        if self.category == CollectionCategory::Albums
                            || matches!(
                                self.route,
                                Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_)
                            ) =>
                    {
                        let counts = self.release_counts().await?;
                        Ok(match self.release_section {
                            Some(ArtistReleaseSection::Class(class)) => {
                                counts[release_index(class)]
                            }
                            Some(ArtistReleaseSection::AppearsOn) => 0,
                            None => counts.iter().sum(),
                        })
                    }
                    Route::Genres => {
                        db.genre_count(self.selected().source_key, folder, filter, &cancel)
                            .await
                    }
                    Route::Moods => {
                        db.mood_count(self.selected().source_key, folder, filter, &cancel)
                            .await
                    }
                    Route::Playlists => {
                        db.playlist_count(selected_source, folder, filter, &cancel)
                            .await
                    }
                    Route::SmartPlaylists => {
                        db.smart_playlist_count(selected_source, folder, filter, self.now, &cancel)
                            .await
                    }
                    Route::PlaylistDetail(key) => {
                        db.playlist_entries_count(*key, folder, filter, &cancel)
                            .await
                    }
                    Route::SmartPlaylistDetail(key) => {
                        db.smart_playlist_track_count(
                            selected_source,
                            *key,
                            folder,
                            filter,
                            self.now,
                            &cancel,
                        )
                        .await
                    }
                    Route::Home => return Ok(self.home_rows().await.map_err(error)?.len() as u64),
                    Route::Search | Route::History => {
                        return Ok(self.rows(0, usize::MAX).await.map_err(error)?.len() as u64);
                    }
                    Route::Folders { .. } => {
                        unreachable!("Folder count is handled before source count")
                    }
                    _ if self.downloaded() => {
                        db.query_tracks_count(
                            &library::TrackQuery {
                                source: self.selected().source_key,
                                folder,
                                collection: self.collection(),
                                favorites_only: self.favorites(),
                                downloaded_only: true,
                            },
                            filter,
                            &cancel,
                        )
                        .await
                    }
                    _ => match self.collection() {
                        Some(collection) => {
                            db.collection_tracks_count(
                                &collection,
                                folder,
                                filter,
                                self.favorites(),
                                &cancel,
                            )
                            .await
                        }
                        None => {
                            db.track_count(
                                self.selected().source_key,
                                folder,
                                self.favorites(),
                                filter,
                                &cancel,
                            )
                            .await
                        }
                    },
                }
                .map_err(error)?;
                Ok(count.max(0) as u64)
            })
            .await
            .map_err(error)?
    }

    pub fn sort_fields(&self) -> Vec<AndroidSortField> {
        let key = library_list_key(&self.route, self.category);
        rufin_core::settings::available_sort_fields(key)
            .iter()
            .map(|field| AndroidSortField {
                id: serde_json::to_value(field)
                    .expect("Sort field serialization")
                    .as_str()
                    .expect("Sort field name")
                    .to_string(),
                title: localization::tr(rufin_core::settings::presentation::library_field_title(
                    *field,
                )),
            })
            .collect()
    }

    pub fn display_settings(&self) -> AndroidBrowseDisplay {
        browse_display(library_list_key(&self.route, self.category), &self.display)
    }

    pub fn sort_selection(&self) -> AndroidSortSelection {
        AndroidSortSelection {
            id: serde_json::to_value(self.sort)
                .expect("Sort field serialization")
                .as_str()
                .expect("Sort field name")
                .to_string(),
            title: localization::tr(rufin_core::settings::presentation::library_field_title(
                self.sort,
            )),
            descending: self.descending,
        }
    }

    pub async fn page(
        self: Arc<Self>,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let mut rows = self
                    .rows(
                        usize::try_from(offset).map_err(error)?,
                        limit.min(256) as usize,
                    )
                    .await
                    .map_err(error)?;
                if matches!(self.route, Route::History) {
                    let uris = rows
                        .iter()
                        .map(|row| row.media_uri.clone())
                        .collect::<Vec<_>>();
                    let downloaded = self
                        .database
                        .downloaded_media_uris(&uris, &ReadCancellation::new())
                        .await
                        .map_err(error)?;
                    for row in &mut rows {
                        row.downloaded = downloaded.contains(&row.media_uri);
                    }
                }
                Ok(rows)
            })
            .await
            .map_err(error)?
    }

    pub async fn play(
        self: Arc<Self>,
        index: u64,
        media_uri: String,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        use rufin_core::playback::PlaybackTarget;
        let placement = queue_placement(&placement)?;
        let context_title = context_caption(&self.route, context_title);
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                if matches!(self.route, Route::History) {
                    let order = self
                        .history_rows(0, usize::MAX)
                        .await
                        .map_err(error)?
                        .into_iter()
                        .map(|row| row.media_uri)
                        .collect::<Vec<_>>();
                    let anchor = order
                        .iter()
                        .position(|uri| uri == &media_uri)
                        .ok_or_else(|| error("Track is no longer in this route"))?;
                    self.queue.play(
                        playback::PlayRequest::captured(
                            library::QueueInput::Uris {
                                order: order.into(),
                                context_id: format!(
                                    "history:{}:{}",
                                    if self.history_current_only {
                                        self.selected().source_id.as_str()
                                    } else {
                                        "all"
                                    },
                                    self.filter
                                )
                                .into(),
                                source_start: 0,
                            },
                            anchor,
                            placement,
                            false,
                        )
                        .with_context_title(context_title.clone())
                        .shuffled(shuffled),
                    );
                    return Ok(());
                }
                if matches!(self.route, Route::Folders { .. }) {
                    return self
                        .folder_play(index, media_uri, placement, shuffled, context_title.clone())
                        .await;
                }
                let folder = self.selected.as_ref().and_then(|selected| selected.music_folder_key);
                if self.album_detail_layout() {
                    let track = self
                        .database
                        .track_row_by_uri(&media_uri, &ReadCancellation::new())
                        .await
                        .map_err(error)?
                        .ok_or_else(|| error("Track is no longer available"))?;
                    let context_title = Some(Arc::new(library::QueueContextTitle {
                        kind: Some("Album".into()),
                        title: track.album.clone().into(),
                    }));
                    let album = track
                        .album_key
                        .ok_or_else(|| error("Album is no longer available"))?;
                    let album_uri = track
                        .album_media_uri
                        .ok_or_else(|| error("Album is no longer available"))?;
                    let context = format!(
                        "{}|query=|source={}|folder={folder:?}|sort={:?}|descending={}",
                        PlaybackTarget::Album(album_uri).context_id(),
                        self.selected().source_id,
                        self.album_tracks.sort_key,
                        self.album_tracks.descending,
                    );
                    self.queue.play(
                        playback::PlayRequest::ordered(
                            library::QueueInput::Query {
                                query: library::QueueQuery::Collection {
                                    collection: library::QueueCollection::AlbumKey(album),
                                    downloaded_only: false,
                                    favorites_only: false,
                                },
                                folder,
                                filter: String::new(),
                                sort: self.album_tracks.sort_key.track_sort(),
                                descending: self.album_tracks.descending,
                                context_id: context.into(),
                                anchor_uri: Some(media_uri),
                            },
                            0,
                            placement,
                            false,
                        )
                        .with_context_title(context_title.clone())
                        .shuffled(shuffled),
                    );
                    return Ok(());
                }
                let target = match &self.route {
                    Route::AlbumDetail(uri) => Some(PlaybackTarget::Album(uri.clone())),
                    Route::ArtistDetail(uri)
                    | Route::ArtistTracks(uri)
                    | Route::ArtistFavoriteTracks(uri)
                    | Route::ArtistDiscography(uri) => Some(PlaybackTarget::Artist(uri.clone())),
                    Route::AlbumArtistDetail(uri)
                    | Route::AlbumArtistTracks(uri)
                    | Route::AlbumArtistFavoriteTracks(uri)
                    | Route::AlbumArtistDiscography(uri) => Some(PlaybackTarget::AlbumArtist(uri.clone())),
                    Route::PlaylistDetail(key) => Some(PlaybackTarget::Playlist(*key)),
                    Route::SmartPlaylistDetail(key) => Some(PlaybackTarget::SmartPlaylist(*key)),
                    Route::GenreDetail(key) => Some(PlaybackTarget::Genre(*key)),
                    Route::MoodDetail(key) => Some(PlaybackTarget::Mood(*key)),
                    _ => None,
                };
                let context: Arc<str> = if let Some(target) = target {
                    format!(
                        "{}|query={}|source={}|folder={folder:?}|sort={:?}|descending={}|favorites={}|downloaded={}",
                        target.context_id(),
                        self.filter,
                        self.selected.as_ref().map(|selected| selected.source_id.as_str()).unwrap_or("rufin"),
                        self.sort,
                        self.descending,
                        self.favorites(),
                        self.downloaded(),
                    )
                } else {
                    format!("android:{}:{}:{}", self.selected().source_id, route_json(self.route.clone()), self.filter)
                }.into();
                if matches!(self.route, Route::Home | Route::Search | Route::History) {
                    let order = self
                        .rows(0, usize::MAX)
                        .await
                        .map_err(error)?
                        .into_iter()
                        .filter(|row| row.kind == "track")
                        .map(|row| row.media_uri)
                        .collect::<Vec<_>>();
                    let anchor = order
                        .iter()
                        .position(|uri| uri == &media_uri)
                        .ok_or_else(|| error("Track is no longer in this route"))?;
                    self.queue.play(
                        playback::PlayRequest::captured(
                            library::QueueInput::Uris {
                                order: order.into(),
                                context_id: context,
                                source_start: 0,
                            },
                            anchor,
                            placement,
                            false,
                        )
                        .with_context_title(context_title.clone())
                        .shuffled(shuffled),
                    );
                    return Ok(());
                }
                let anchor_entry = if let Route::PlaylistDetail(key) = self.route {
                    self.database
                        .playlist_entries_page(
                            key,
                            folder,
                            self.sort.playlist_entry_sort(),
                            self.descending,
                            &self.filter,
                            usize::try_from(index).map_err(error)?,
                            1,
                            &ReadCancellation::new(),
                        )
                        .await
                        .map_err(error)?
                        .first()
                        .map(|row| row.playlist_entry_key)
                } else {
                    None
                };
                let input = match self.route {
                    Route::PlaylistDetail(key) => library::QueueInput::PlaylistQuery {
                        key,
                        folder,
                        filter: self.filter.clone(),
                        sort: self.sort.playlist_entry_sort(),
                        descending: self.descending,
                        context_id: context,
                        anchor_entry,
                        anchor_uri: Some(media_uri),
                    },
                    _ => library::QueueInput::Query {
                        query: match self.route {
                            Route::SmartPlaylistDetail(key) => library::QueueQuery::SmartDisplay {
                                key,
                                source: Some(self.selected().source_key),
                            },
                            _ => library::TrackQuery {
                                source: self.selected().source_key,
                                collection: self.collection(),
                                folder,
                                downloaded_only: self.downloaded(),
                                favorites_only: self.favorites(),
                            }
                            .queue_query(),
                        },
                        folder,
                        filter: self.filter.clone(),
                        sort: self.sort.track_sort(),
                        descending: self.descending,
                        context_id: context,
                        anchor_uri: Some(media_uri),
                    },
                };
                self.queue.play(
                    playback::PlayRequest::captured(
                        input,
                        usize::try_from(index).map_err(error)?,
                        placement,
                        false,
                    )
                    .with_context_title(context_title.clone())
                    .shuffled(shuffled),
                );
                Ok(())
            })
            .await
            .map_err(error)?
    }
}
