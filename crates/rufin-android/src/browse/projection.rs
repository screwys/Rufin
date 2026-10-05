use super::*;

impl AndroidBrowseQuery {
    pub(super) async fn history_rows(
        &self,
        offset: usize,
        limit: usize,
    ) -> library::LibraryResult<Vec<AndroidBrowseRow>> {
        let current = if self.history_current_only {
            Some(&self.selected().source_id)
        } else {
            None
        };
        let rows = self
            .database
            .activity_history(
                current,
                &self.filter,
                self.descending,
                &ReadCancellation::new(),
            )
            .await?;
        Ok(rows
            .into_iter()
            .skip(offset)
            .take(limit)
            .enumerate()
            .map(|(index, row)| {
                let fields = self
                    .display
                    .row_fields
                    .iter()
                    .map(|field| AndroidFieldValue {
                        id: serde_json::to_value(field)
                            .expect("Field serialization")
                            .as_str()
                            .expect("Field name")
                            .to_string(),
                        text: if *field == LibraryField::RowIndex {
                            (offset + index + 1).to_string()
                        } else {
                            rufin_core::settings::presentation::history_field(&row, *field)
                        },
                    })
                    .collect();
                AndroidBrowseRow {
                    kind: "track".into(),
                    playback_context_id: None,
                    key: row.media_uri.clone(),
                    artist: row.artist.clone(),
                    album: row.album.clone(),
                    fields,
                    media_uri: row.media_uri,
                    title: row.title,
                    subtitle: format!("{} · {}", row.artist, row.album),
                    favorite: row.favorite,
                    duration_millis: row.duration_millis.max(0) as u64,
                    detail_route: None,
                    pin: None,
                    section: String::new(),
                    section_id: String::new(),
                    section_kind: String::new(),
                    section_refreshable: false,
                    year: row.year,
                    track_count: 1,
                    source_id: None,
                    source_name: row.source_name.unwrap_or_default(),
                    last_played: row.last_played,
                    writable: false,
                    downloaded: false,
                    artwork_identity: row.artwork_binding,
                }
            })
            .collect())
    }

    fn track_field_values(
        &self,
        index: usize,
        text: impl Fn(LibraryField) -> String,
    ) -> Vec<AndroidFieldValue> {
        self.selected_track_fields()
            .iter()
            .map(|field| AndroidFieldValue {
                id: serde_json::to_value(field)
                    .expect("Field serialization")
                    .as_str()
                    .expect("Field name")
                    .to_string(),
                text: if *field == LibraryField::RowIndex {
                    (index + 1).to_string()
                } else {
                    text(*field)
                },
            })
            .collect()
    }

    pub(super) fn track_row(&self, track: library::TrackRow, index: usize) -> AndroidBrowseRow {
        let fields = self.track_field_values(index, |field| {
            rufin_core::settings::presentation::track_field(&track, field)
        });
        let mut row: AndroidBrowseRow = track.into();
        row.fields = fields;
        row
    }

    pub(super) fn selected_track_fields(&self) -> &[LibraryField] {
        if self.album_detail_layout() {
            &self.display.detail_track_fields
        } else {
            &self.detail_track_settings().row_fields
        }
    }

    pub(super) async fn home_rows(&self) -> Result<&Vec<AndroidBrowseRow>, library::LibraryError> {
        self.home_snapshot
            .get_or_try_init(|| async {
                let db = &self.database;
                let source = self.selected().source_key;
                let folder = self.selected().music_folder_key;
                let filter = &self.filter;
                let cancel = ReadCancellation::new();
                let home = db
                    .home_page(
                        source,
                        folder,
                        self.home_variations.0,
                        self.home_variations.1,
                        &self.blocks,
                        &cancel,
                    )
                    .await?;
                let mut rows = Vec::new();
                if let Some(row) = home.showcase {
                    let mut row: AndroidBrowseRow = row.into();
                    home_section(&mut row, library::HomeBlockKind::Showcase);
                    rows.push(row);
                }
                for row in home.explore {
                    let mut row: AndroidBrowseRow = row.into();
                    home_section(&mut row, library::HomeBlockKind::Explore);
                    rows.push(row);
                }
                for (block, section) in [
                    (library::HomeBlockKind::MostPlayed, home.most_played),
                    (library::HomeBlockKind::NewlyAdded, home.newly_added),
                    (library::HomeBlockKind::RecentlyPlayed, home.recently_played),
                    (
                        library::HomeBlockKind::RecentlyReleased,
                        home.recently_released,
                    ),
                ] {
                    for row in section.tracks {
                        let mut row: AndroidBrowseRow = row.into();
                        home_section(&mut row, block);
                        rows.push(row);
                    }
                    for row in section.albums {
                        let mut row: AndroidBrowseRow = row.into();
                        home_section(&mut row, block);
                        rows.push(row);
                    }
                }
                for section in home.provider_sections {
                    let id = format!("provider:{}", section.section_id);
                    let title = rufin_core::settings::presentation::home_provider_title(
                        &section.section_id,
                        section.title.as_deref(),
                    );
                    for row in section.rows.tracks {
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push(row);
                    }
                    for row in section.rows.albums {
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push(row);
                    }
                }
                let keys = home
                    .genres
                    .iter()
                    .map(|row| row.genre_key)
                    .collect::<Vec<_>>();
                let mut genre_downloads = std::collections::HashMap::new();
                for keys in keys.chunks(128) {
                    let facts = self
                        .database
                        .genre_rows(
                            self.selected().source_key,
                            keys,
                            self.selected().music_folder_key,
                            &ReadCancellation::new(),
                        )
                        .await?;
                    for fact in facts {
                        genre_downloads.insert(
                            fact.genre_key,
                            fact.track_count > 0 && fact.downloaded_count == fact.track_count,
                        );
                    }
                }
                for row in home.genres {
                    rows.push(AndroidBrowseRow {
                        kind: "genre".into(),
                        playback_context_id: Some(
                            rufin_core::playback::PlaybackTarget::Genre(row.genre_key).context_id(),
                        ),
                        fields: Vec::new(),
                        artist: String::new(),
                        album: String::new(),
                        key: row.object_id,
                        media_uri: String::new(),
                        title: row.name,
                        subtitle: String::new(),
                        favorite: false,
                        duration_millis: 0,
                        detail_route: Some(route_json(Route::GenreDetail(row.genre_key))),
                        pin: None,
                        section: localization::tr("Featured genres"),
                        section_id: "Genres".into(),
                        section_kind: "genres".into(),
                        section_refreshable: false,
                        year: None,
                        track_count: row.track_count.max(0) as u64,
                        source_id: None,
                        source_name: String::new(),
                        last_played: None,
                        writable: false,
                        downloaded: genre_downloads
                            .get(&row.genre_key)
                            .copied()
                            .unwrap_or(false),
                        artwork_identity: row.artwork_binding,
                    });
                }
                let provider = rows
                    .iter()
                    .any(|row| row.section_id.starts_with("provider:"));
                rows.sort_by_key(|row| {
                    if provider && row.section_id == "Showcase" {
                        return 0;
                    }
                    if row.section_id.starts_with("provider:") {
                        return 1;
                    }
                    self.blocks
                        .iter()
                        .position(|block| {
                            serde_json::to_value(block)
                                .expect("Home block name")
                                .as_str()
                                == Some(row.section_id.as_str())
                        })
                        .map_or(usize::MAX, |index| index + 2)
                });
                let filter = filter.to_lowercase();
                Ok(rows
                    .into_iter()
                    .filter(|row| {
                        format!("{} {}", row.title, row.subtitle)
                            .to_lowercase()
                            .contains(&filter)
                    })
                    .collect())
            })
            .await
    }

    pub(super) async fn rows(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AndroidBrowseRow>, library::LibraryError> {
        if matches!(self.route, Route::History) {
            return self.history_rows(offset, limit).await;
        }
        if matches!(self.route, Route::Folders { .. }) {
            return self.folder_rows(offset, limit).await;
        }
        let db = &self.database;
        let selected_source = self.selected.as_ref().map(|selected| selected.source_key);
        let folder = self
            .selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        let cancel = ReadCancellation::new();
        let filter = &self.filter;
        let sort = self.sort;
        let descending = self.descending;
        if self.album_detail_layout() {
            let info = db
                .album_detail_route_page_info(
                    self.selected().source_key,
                    folder,
                    self.favorites(),
                    filter,
                    sort.album_sort(),
                    descending,
                    offset,
                    limit,
                    &cancel,
                )
                .await?;
            let ids = info.iter().map(|(uri, _)| uri.clone()).collect::<Vec<_>>();
            let (albums, tracks) = db
                .album_detail_route_rows(self.selected().source_key, &ids, folder, &cancel)
                .await?;
            let mut rows = std::collections::HashMap::with_capacity(ids.len());
            for album in albums {
                let mut row: AndroidBrowseRow = album.into();
                row.kind = "album_header".into();
                rows.insert(row.media_uri.clone(), row);
            }
            for track in tracks {
                let rank = info
                    .iter()
                    .find(|(uri, _)| uri == &track.media_uri)
                    .expect("Loaded Detail track identity")
                    .1;
                let row = self.track_row(track, rank.saturating_sub(1) as usize);
                rows.insert(row.media_uri.clone(), row);
            }
            return Ok(ids.into_iter().filter_map(|id| rows.remove(&id)).collect());
        }
        match &self.route {
            Route::Albums | Route::Favorites
                if !matches!(self.route, Route::Favorites)
                    || self.category == CollectionCategory::Albums =>
            {
                Ok(db
                    .album_page(
                        self.selected().source_key,
                        folder,
                        self.favorites(),
                        filter,
                        sort.album_sort(),
                        descending,
                        offset,
                        limit,
                        &cancel,
                    )
                    .await?
                    .into_iter()
                    .map(Into::into)
                    .collect())
            }
            Route::Artists | Route::AlbumArtists | Route::Favorites
                if !matches!(self.route, Route::Favorites)
                    || self.category == CollectionCategory::Artists =>
            {
                Ok(db
                    .artist_page(
                        self.selected().source_key,
                        folder,
                        matches!(self.route, Route::AlbumArtists),
                        self.favorites(),
                        filter,
                        sort.artist_sort(),
                        descending,
                        offset,
                        limit,
                        &cancel,
                    )
                    .await?
                    .into_iter()
                    .map(|row| artist_row(row, matches!(self.route, Route::AlbumArtists)))
                    .collect())
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
                let (_, _, object) = library::source_entity_parts(uri).ok_or_else(|| {
                    library::LibraryError::InvalidRequest("Artist identity is unavailable".into())
                })?;
                let Some(key) = db
                    .artist_key_by_object(self.selected().source_key, &object, &cancel)
                    .await?
                else {
                    return Ok(Vec::new());
                };
                if matches!(self.release_section, Some(ArtistReleaseSection::AppearsOn)) {
                    return Ok(Vec::new());
                }
                Ok(db
                    .artist_release_page(
                        self.selected().source_key,
                        key,
                        matches!(
                            self.route,
                            Route::AlbumArtistDetail(_) | Route::AlbumArtistDiscography(_)
                        ),
                        folder,
                        filter,
                        sort.album_sort(),
                        descending,
                        self.release_section.and_then(|section| match section {
                            ArtistReleaseSection::Class(class) => Some(class),
                            ArtistReleaseSection::AppearsOn => None,
                        }),
                        offset,
                        limit,
                        &cancel,
                    )
                    .await?
                    .into_iter()
                    .map(|(class, album)| {
                        let mut row: AndroidBrowseRow = album.into();
                        let (id, title) = ARTIST_RELEASE_GROUPS[release_index(class)];
                        row.section_id = id.into();
                        row.section = localization::tr(title);
                        row.section_kind = "artist_release".into();
                        row
                    })
                    .collect())
            }
            Route::Genres => Ok(db
                .genre_page(
                    self.selected().source_key,
                    folder,
                    filter,
                    sort.genre_sort(),
                    descending,
                    offset,
                    limit,
                    &cancel,
                )
                .await?
                .into_iter()
                .map(Into::into)
                .collect()),
            Route::Moods => Ok(db
                .mood_page(
                    self.selected().source_key,
                    folder,
                    filter,
                    sort.mood_sort(),
                    descending,
                    offset,
                    limit,
                    &cancel,
                )
                .await?
                .into_iter()
                .map(Into::into)
                .collect()),
            Route::Playlists => Ok(db
                .playlist_page(
                    selected_source,
                    folder,
                    sort.playlist_sort(),
                    descending,
                    filter,
                    offset,
                    limit,
                    &cancel,
                )
                .await?
                .into_iter()
                .map(Into::into)
                .collect()),
            Route::SmartPlaylists => Ok(db
                .smart_playlist_page(
                    selected_source,
                    folder,
                    sort.smart_playlist_sort(),
                    descending,
                    filter,
                    self.now,
                    offset,
                    limit,
                    &cancel,
                )
                .await?
                .into_iter()
                .map(Into::into)
                .collect()),
            Route::PlaylistDetail(key) => Ok(db
                .playlist_entries_page(
                    *key,
                    folder,
                    sort.playlist_entry_sort(),
                    descending,
                    filter,
                    offset,
                    limit,
                    &cancel,
                )
                .await?
                .into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let fields = self.track_field_values(offset + index, |field| {
                        rufin_core::settings::presentation::playlist_entry_field(&row, field)
                    });
                    AndroidBrowseRow {
                        kind: "track".into(),
                        playback_context_id: None,
                        fields,
                        artist: row.artist.clone(),
                        album: row.album.clone(),
                        key: serde_json::to_string(&row.playlist_entry_key)
                            .expect("Playlist entry serialization"),
                        media_uri: row.media_uri,
                        title: row.title,
                        subtitle: format!("{} · {}", row.artist, row.album),
                        favorite: row.favorite,
                        duration_millis: row.duration_millis.max(0) as u64,
                        detail_route: None,
                        pin: None,
                        section: String::new(),
                        section_id: String::new(),
                        section_kind: String::new(),
                        section_refreshable: false,
                        year: row.year,
                        track_count: 1,
                        source_id: None,
                        source_name: String::new(),
                        last_played: row.last_played,
                        writable: false,
                        downloaded: row.is_downloaded,
                        artwork_identity: row.artwork_binding,
                    }
                })
                .collect()),
            Route::SmartPlaylistDetail(key) => Ok(db
                .smart_playlist_sorted_track_page(
                    selected_source,
                    *key,
                    folder,
                    filter,
                    sort.track_sort(),
                    descending,
                    self.now,
                    offset,
                    limit,
                    &cancel,
                )
                .await?
                .into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let fields = self.track_field_values(offset + index, |field| {
                        rufin_core::settings::presentation::smart_track_field(&row, field)
                    });
                    AndroidBrowseRow {
                        kind: "track".into(),
                        playback_context_id: None,
                        fields,
                        artist: row.artist.clone(),
                        album: row.album.clone(),
                        key: row.media_uri.clone(),
                        media_uri: row.media_uri,
                        title: row.title,
                        subtitle: format!("{} · {}", row.artist, row.album),
                        favorite: row.favorite,
                        duration_millis: row.duration_millis.max(0) as u64,
                        detail_route: None,
                        pin: None,
                        section: String::new(),
                        section_id: String::new(),
                        section_kind: String::new(),
                        section_refreshable: false,
                        year: row.year,
                        track_count: 1,
                        source_id: None,
                        source_name: String::new(),
                        last_played: row.last_played,
                        writable: false,
                        downloaded: row.is_downloaded,
                        artwork_identity: row.artwork_binding,
                    }
                })
                .collect()),
            Route::History => unreachable!("History is handled before source projection"),
            Route::Home => Ok(self
                .home_rows()
                .await?
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect()),
            Route::Search => Ok(search_rows(
                db.search(
                    self.selected().source_key,
                    folder,
                    false,
                    &library::SearchRequest::with_limit(filter, 60),
                    &cancel,
                )
                .await?,
            )
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect()),
            Route::Folders { .. } => {
                unreachable!("Folder projection is handled before source rows")
            }
            _ => Ok(db
                .query_tracks_page(
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
                    offset,
                    limit,
                    &cancel,
                )
                .await?
                .into_iter()
                .enumerate()
                .map(|(index, track)| self.track_row(track, offset + index))
                .collect()),
        }
    }
}
