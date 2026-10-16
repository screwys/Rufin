use super::*;

impl AndroidBrowseQuery {
    fn track_field_values(
        &self,
        index: usize,
        text: impl Fn(LibraryField) -> String,
    ) -> Vec<AndroidFieldValue> {
        self.selected_track_fields()
            .iter()
            .map(|field| AndroidFieldValue {
                id: *field,
                text: if *field == LibraryField::RowIndex {
                    (index + 1).to_string()
                } else {
                    text(*field)
                },
            })
            .collect()
    }

    pub(super) fn track_row(&self, track: library::TrackView, index: usize) -> AndroidBrowseRow {
        let fields = self.track_field_values(index, |field| {
            rufin_core::settings::presentation::track_field(&track, field)
        });
        AndroidBrowseRow {
            fields,
            ..track.into()
        }
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
    ) -> library::LibraryResult<&Vec<(AndroidBrowseRow, TrackSelection)>> {
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
                        CatalogQuery::tracks(TrackScope::Album(row.album.album_key)).selection();
                    let mut row: AndroidBrowseRow = row.into();
                    home_section(&mut row, library::HomeBlockKind::Showcase);
                    rows.push((row, selection));
                }
                for row in home.explore {
                    let selection =
                        CatalogQuery::tracks(TrackScope::Album(row.album.album_key)).selection();
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
                        let selection = library::CatalogQuery::keys(
                            library::CatalogKind::Track,
                            None,
                            vec![row.track.track_key.raw()],
                        )
                        .selection();
                        let mut row: AndroidBrowseRow = row.into();
                        home_section(&mut row, block);
                        rows.push((row, selection));
                    }
                    for row in section.albums {
                        let selection =
                            CatalogQuery::tracks(TrackScope::Album(row.album.album_key))
                                .selection();
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
                        let selection = library::CatalogQuery::keys(
                            library::CatalogKind::Track,
                            None,
                            vec![row.track.track_key.raw()],
                        )
                        .selection();
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push((row, selection));
                    }
                    for row in section.rows.albums {
                        let selection =
                            CatalogQuery::tracks(TrackScope::Album(row.album.album_key))
                                .selection();
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push((row, selection));
                    }
                }
                for row in home.genres {
                    let selection =
                        CatalogQuery::tracks(TrackScope::Genre(row.genre_key)).selection();
                    rows.push((
                        AndroidBrowseRow {
                            section: localization::tr("Featured genres"),
                            section_id: "Genres".into(),
                            section_kind: "genres".into(),
                            ..AndroidBrowseRow::new(AndroidBrowseItem::Genre { row })
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
                    .filter(|(row, _)| row.matches_filter(&filter))
                    .collect())
            })
            .await
    }

    pub(super) fn catalog_rows(&self, rows: CatalogRows, offset: usize) -> Vec<AndroidBrowseRow> {
        match rows {
            CatalogRows::Tracks(rows) => rows
                .into_iter()
                .enumerate()
                .map(|(index, row)| self.track_row(row, offset + index))
                .collect(),
            CatalogRows::PlaylistEntries(rows) => rows
                .into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let fields = self.track_field_values(offset + index, |field| {
                        rufin_core::settings::presentation::track_field(&row.track, field)
                    });
                    AndroidBrowseRow {
                        fields,
                        ..AndroidBrowseRow::new(AndroidBrowseItem::PlaylistEntry { row })
                    }
                })
                .collect(),
            CatalogRows::Folders(_) => unreachable!("folder projection is handled above"),
            CatalogRows::Albums(rows) => rows.into_iter().map(Into::into).collect(),
            CatalogRows::AlbumDetails(rows) => rows
                .into_iter()
                .map(|item| match item {
                    library::AlbumDetailRow::Album { album, .. } => {
                        let mut row: AndroidBrowseRow = album.into();
                        row.album_header = true;
                        row
                    }
                    library::AlbumDetailRow::Track { track, index, .. } => {
                        self.track_row(track, index)
                    }
                })
                .collect(),
            CatalogRows::Artists(rows) => rows
                .into_iter()
                .map(|row| artist_row(row, matches!(self.route, Route::AlbumArtists)))
                .collect(),
            CatalogRows::Genres(rows) => rows.into_iter().map(Into::into).collect(),
            CatalogRows::Moods(rows) => rows.into_iter().map(Into::into).collect(),
            CatalogRows::Playlists(rows) => rows.into_iter().map(Into::into).collect(),
            CatalogRows::SmartPlaylists(rows) => rows.into_iter().map(Into::into).collect(),
        }
    }

    pub(super) async fn rows(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AndroidBrowseRow>, library::LibraryError> {
        if matches!(self.route, Route::Folders { .. }) {
            return self.folder_rows(offset, limit).await;
        }
        let cancel = ReadCancellation::new();
        match self.query().await? {
            BrowseQuery::Tracks(query) | BrowseQuery::Collection(query) => Ok(self.catalog_rows(
                query.rows(&self.database, offset, limit, &cancel).await?,
                offset,
            )),
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
