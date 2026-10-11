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
                if let Some(query) = self.song_query().await.map_err(error)? {
                    return Ok(self
                        .database
                        .song_sections(query, &ReadCancellation::new())
                        .await
                        .map_err(error)?
                        .into_iter()
                        .map(|section| AndroidScrollSection {
                            title: section.title,
                            index: section.index,
                        })
                        .collect());
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
                    Route::Home | Route::Search | Route::History => Ok(Vec::new()),
                    Route::Folders { .. } => {
                        unreachable!("Folder section projection is handled first")
                    }
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
                let rows = self.home_rows().await.map_err(error)?;
                let mut selections = Vec::new();
                for row in rows.iter().filter(|row| row.section_id == section_id) {
                    if let Some(key) = row.track_key {
                        selections.push(SongSelection::Tracks(Arc::from([
                            library::TrackKey::from_raw(key),
                        ])));
                    } else if row.kind == "album" {
                        if let Some(album) = self
                            .database
                            .album_row_by_media_uri(&row.media_uri, &ReadCancellation::new())
                            .await
                            .map_err(error)?
                        {
                            selections.push(
                                SongQuery::new(SongScope::Album(album.album_key)).selection(),
                            );
                        }
                    } else if let Some(route) = row.detail_route.as_deref() {
                        if let Route::GenreDetail(key) =
                            serde_json::from_str(route).map_err(error)?
                        {
                            selections.push(SongQuery::new(SongScope::Genre(key)).selection());
                        }
                    }
                }
                self.queue.play(
                    playback::PlayRequest::captured(
                        SongSelection::Concat(selections),
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
                if let Some(query) = self.song_query().await.map_err(error)? {
                    return Ok(self
                        .database
                        .song_count(query, &ReadCancellation::new())
                        .await
                        .map_err(error)?
                        .max(0) as u64);
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
                    Route::Home => return Ok(self.home_rows().await.map_err(error)?.len() as u64),
                    Route::Search | Route::History => {
                        return Ok(self.rows(0, usize::MAX).await.map_err(error)?.len() as u64);
                    }
                    Route::Folders { .. } => {
                        unreachable!("Folder count is handled before source count")
                    }
                    _ => Ok(0),
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
        let placement = queue_placement(&placement)?;
        let context_title = context_caption(&self.route, context_title);
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                if matches!(self.route, Route::Folders { .. }) {
                    return self
                        .folder_play(index, media_uri, placement, shuffled, context_title)
                        .await;
                }
                let index = usize::try_from(index).map_err(error)?;
                let cancel = ReadCancellation::new();
                let track = self
                    .database
                    .track_row_by_uri(&media_uri, &cancel)
                    .await
                    .map_err(error)?
                    .ok_or_else(|| error("Track is no longer available"))?;
                let (selection, anchor, entry, title, context) = if self.album_detail_layout() {
                    let album = track
                        .album_key
                        .ok_or_else(|| error("Album is no longer available"))?;
                    let mut query = SongQuery::new(SongScope::Album(album));
                    query.sort = Some(self.album_tracks.sort_key.track_sort());
                    query.descending = self.album_tracks.descending;
                    let title = Some(Arc::new(library::QueueContextTitle {
                        kind: Some("Album".into()),
                        title: track.album.clone().into(),
                    }));
                    (
                        query.selection(),
                        0,
                        None,
                        title,
                        format!(
                            "album:{}|query=|sort={:?}|descending={}",
                            track.album_media_uri.as_deref().unwrap_or_default(),
                            self.album_tracks.sort_key,
                            self.album_tracks.descending
                        )
                        .into(),
                    )
                } else if let Some(query) = self.song_query().await.map_err(error)? {
                    let entry = if matches!(query.scope, SongScope::Playlist(_)) {
                        self.database
                            .song_playlist_rows(query, index, 1, &cancel)
                            .await
                            .map_err(error)?
                            .first()
                            .map(|row| row.playlist_entry_key)
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
                        .filter(|row| row.kind == "track")
                        .filter_map(|row| row.track_key.map(library::TrackKey::from_raw))
                        .collect::<Vec<_>>();
                    let anchor = keys
                        .iter()
                        .position(|key| *key == track.track_key)
                        .ok_or_else(|| error("Track is no longer in this route"))?;
                    (
                        SongSelection::Tracks(keys.into()),
                        anchor,
                        None,
                        context_title,
                        self.context_id(),
                    )
                };
                self.queue.play(
                    playback::PlayRequest::captured(selection, anchor, placement, false)
                        .with_context(context)
                        .with_anchor(track.track_key, entry)
                        .with_context_title(title)
                        .shuffled(shuffled),
                );
                Ok(())
            })
            .await
            .map_err(error)?
    }
}
