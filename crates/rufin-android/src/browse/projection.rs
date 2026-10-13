use super::*;

impl AndroidBrowseQuery {
    pub(super) async fn history_rows(
        &self,
        offset: usize,
        limit: usize,
    ) -> library::LibraryResult<Vec<AndroidBrowseRow>> {
        let query = self.song_query().await?.expect("History query");
        let rows = self
            .database
            .song_history_rows(query, offset, limit, &ReadCancellation::new())
            .await?;
        Ok(rows
            .into_iter()
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
                    track_key: Some(row.track_key.raw()),
                    album_key: None,
                    album_media_uri: None,
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
                    downloaded: row.is_downloaded,
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

    pub(super) fn track_row(&self, mut track: library::TrackRow, index: usize) -> AndroidBrowseRow {
        let fields = self.track_field_values(index, |field| {
            rufin_core::settings::presentation::track_field(&track, field)
        });
        let (album_key, album_media_uri) = if self.album_detail_layout() {
            (
                track.album_key.map(library::AlbumKey::raw),
                track.album_media_uri.take(),
            )
        } else {
            (None, None)
        };
        let mut row: AndroidBrowseRow = track.into();
        row.album_key = album_key;
        row.album_media_uri = album_media_uri;
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

    pub(super) async fn home_rows(
        &self,
    ) -> library::LibraryResult<&Vec<(AndroidBrowseRow, SongSelection)>> {
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
                    let selection =
                        SongQuery::new(SongScope::Album(row.album.album_key)).selection();
                    let mut row: AndroidBrowseRow = row.into();
                    home_section(&mut row, library::HomeBlockKind::Showcase);
                    rows.push((row, selection));
                }
                for row in home.explore {
                    let selection =
                        SongQuery::new(SongScope::Album(row.album.album_key)).selection();
                    let mut row: AndroidBrowseRow = row.into();
                    home_section(&mut row, library::HomeBlockKind::Explore);
                    rows.push((row, selection));
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
                        let selection = SongSelection::Tracks(Arc::from([row.track.track_key]));
                        let mut row: AndroidBrowseRow = row.into();
                        home_section(&mut row, block);
                        rows.push((row, selection));
                    }
                    for row in section.albums {
                        let selection =
                            SongQuery::new(SongScope::Album(row.album.album_key)).selection();
                        let mut row: AndroidBrowseRow = row.into();
                        home_section(&mut row, block);
                        rows.push((row, selection));
                    }
                }
                for section in home.provider_sections {
                    let id = format!("provider:{}", section.section_id);
                    let title = rufin_core::settings::presentation::home_provider_title(
                        &section.section_id,
                        section.title.as_deref(),
                    );
                    for row in section.rows.tracks {
                        let selection = SongSelection::Tracks(Arc::from([row.track.track_key]));
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push((row, selection));
                    }
                    for row in section.rows.albums {
                        let selection =
                            SongQuery::new(SongScope::Album(row.album.album_key)).selection();
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push((row, selection));
                    }
                }
                for row in home.genres {
                    let selection = SongQuery::new(SongScope::Genre(row.genre_key)).selection();
                    rows.push((
                        AndroidBrowseRow {
                            track_key: None,
                            album_key: None,
                            album_media_uri: None,
                            kind: "genre".into(),
                            playback_context_id: Some(format!("genre:{}", row.genre_key)),
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
                            downloaded: row.track_count > 0
                                && row.downloaded_count == row.track_count,
                            artwork_identity: row.artwork_binding,
                        },
                        selection,
                    ));
                }
                let provider = rows
                    .iter()
                    .any(|(row, _)| row.section_id.starts_with("provider:"));
                rows.sort_by_key(|(row, _)| {
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
                    .filter(|(row, _)| {
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
        if let Some(query) = self.song_query().await? {
            if matches!(query.scope, SongScope::Playlist(_)) {
                return Ok(self
                    .database
                    .song_playlist_rows(query, offset, limit, &ReadCancellation::new())
                    .await?
                    .into_iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let fields = self.track_field_values(offset + index, |field| {
                            rufin_core::settings::presentation::playlist_entry_field(&row, field)
                        });
                        AndroidBrowseRow {
                            track_key: Some(row.track_key.raw()),
                            album_key: None,
                            album_media_uri: None,
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
                    .collect());
            }
            return Ok(self
                .database
                .song_rows(query, offset, limit, &ReadCancellation::new())
                .await?
                .into_iter()
                .enumerate()
                .map(|(index, row)| self.track_row(row, offset + index))
                .collect());
        }
        let cancel = ReadCancellation::new();
        match self.query().await? {
            BrowseQuery::Collection(query) => Ok(
                match query.rows(&self.database, offset, limit, &cancel).await? {
                    CollectionRows::Albums(rows) => rows.into_iter().map(Into::into).collect(),
                    CollectionRows::AlbumDetails(rows) => rows
                        .into_iter()
                        .map(|item| match item {
                            library::AlbumDetailRow::Album { album, .. } => {
                                let mut row: AndroidBrowseRow = album.into();
                                row.kind = "album_header".into();
                                row
                            }
                            library::AlbumDetailRow::Track { track, index, .. } => {
                                self.track_row(track, index)
                            }
                        })
                        .collect(),
                    CollectionRows::Artists(rows) => rows
                        .into_iter()
                        .map(|row| artist_row(row, matches!(self.route, Route::AlbumArtists)))
                        .collect(),
                    CollectionRows::Genres(rows) => rows.into_iter().map(Into::into).collect(),
                    CollectionRows::Moods(rows) => rows.into_iter().map(Into::into).collect(),
                    CollectionRows::Playlists(rows) => rows.into_iter().map(Into::into).collect(),
                    CollectionRows::SmartPlaylists(rows) => {
                        rows.into_iter().map(Into::into).collect()
                    }
                },
            ),
            BrowseQuery::Releases(query) => {
                if matches!(self.release_section, Some(ArtistReleaseSection::AppearsOn)) {
                    return Ok(Vec::new());
                }
                let class = self.release_section.and_then(|section| match section {
                    ArtistReleaseSection::Class(class) => Some(class),
                    ArtistReleaseSection::AppearsOn => None,
                });
                Ok(query
                    .section_rows(&self.database, class, offset, limit, &cancel)
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
            _ if matches!(self.route, Route::Home) => Ok(self
                .home_rows()
                .await?
                .iter()
                .skip(offset)
                .take(limit)
                .map(|(row, _)| row.clone())
                .collect()),
            _ if matches!(self.route, Route::Search) => Ok(search_rows(
                self.database
                    .search(
                        self.selected().source_key,
                        self.selected().music_folder_key,
                        false,
                        &library::SearchRequest::with_limit(&self.filter, 60),
                        &cancel,
                    )
                    .await?,
            )
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect()),
            _ => Ok(Vec::new()),
        }
    }
}
