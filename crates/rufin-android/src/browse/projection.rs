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
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<&Vec<(AndroidBrowseRow, MemberSelection)>> {
        self.home_snapshot
            .get_or_try_init(|| async {
                let filter = &self.filter;
                let OpenedCollection::Home(home) = self.opened(&cancel).await? else {
                    unreachable!("Home rows")
                };
                let home = home.clone();
                let mut rows = Vec::new();
                if let Some(row) = home.showcase {
                    let selection =
                        MemberSelection::Collections(vec![Collection::Album(row.album.album_key)]);
                    let mut row: AndroidBrowseRow = row.into();
                    home_section(&mut row, library::HomeBlockKind::Showcase);
                    rows.push((row, selection));
                }
                for row in home.explore {
                    let selection =
                        MemberSelection::Collections(vec![Collection::Album(row.album.album_key)]);
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
                        let selection = MemberSelection::Keys {
                            source: None,
                            keys: vec![row.track.track_key].into(),
                        };
                        let mut row: AndroidBrowseRow = row.into();
                        home_section(&mut row, block);
                        rows.push((row, selection));
                    }
                    for row in section.albums {
                        let selection = MemberSelection::Collections(vec![Collection::Album(
                            row.album.album_key,
                        )]);
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
                        let selection = MemberSelection::Keys {
                            source: None,
                            keys: vec![row.track.track_key].into(),
                        };
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push((row, selection));
                    }
                    for row in section.rows.albums {
                        let selection = MemberSelection::Collections(vec![Collection::Album(
                            row.album.album_key,
                        )]);
                        let mut row: AndroidBrowseRow = row.into();
                        row.section = title.clone();
                        row.section_id = id.clone();
                        row.section_kind = "section".into();
                        rows.push((row, selection));
                    }
                }
                for row in home.genres {
                    let selection =
                        MemberSelection::Collections(vec![Collection::Genre(row.genre_key)]);
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

    pub(super) fn member_rows(&self, rows: Vec<MemberRow>, offset: usize) -> Vec<AndroidBrowseRow> {
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| match row {
                MemberRow::Track(track) => self.track_row(track, offset + index),
                MemberRow::Entry(row) => AndroidBrowseRow {
                    fields: self.track_field_values(offset + index, |field| {
                        rufin_core::settings::presentation::track_field(&row.track, field)
                    }),
                    ..AndroidBrowseRow::new(AndroidBrowseItem::PlaylistEntry { row })
                },
            })
            .collect()
    }

    pub(super) fn album_rows(&self, rows: Vec<library::AlbumDetailRow>) -> Vec<AndroidBrowseRow> {
        rows.into_iter()
            .map(|item| match item {
                library::AlbumDetailRow::Album { album, .. } => {
                    let mut row: AndroidBrowseRow = album.into();
                    row.album_header = true;
                    row
                }
                library::AlbumDetailRow::Track { track, index, .. } => self.track_row(track, index),
            })
            .collect()
    }

    pub(super) fn opened_count(&self, opened: &OpenedCollection) -> usize {
        match opened {
            OpenedCollection::Tracks(page)
            | OpenedCollection::Detail(Some(rufin_core::browse::DetailPage {
                content: DetailContent::Tracks(page),
                ..
            })) => page.page.count,
            OpenedCollection::Albums(page) => match &page.content {
                rufin_core::browse::AlbumListContent::Rows(page) => page.count,
                rufin_core::browse::AlbumListContent::Detail { page, .. } => page.count,
            },
            OpenedCollection::Artists { page, .. } => page.count,
            OpenedCollection::Genres { page, .. } => page.count,
            OpenedCollection::Moods { page, .. } => page.count,
            OpenedCollection::Playlists { page, .. } => page.count,
            OpenedCollection::SmartPlaylists { page, .. } => page.count,
            OpenedCollection::Folders(page) => page.folder_count + page.track_count,
            OpenedCollection::Detail(Some(rufin_core::browse::DetailPage {
                content: DetailContent::Releases { page, .. },
                ..
            })) => page.page.count,
            OpenedCollection::Search(rows) => {
                rows.tracks.len() + rows.albums.len() + rows.artists.len()
            }
            _ => 0,
        }
    }

    pub(super) async fn rows(
        &self,
        offset: usize,
        limit: usize,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<AndroidBrowseRow>> {
        let db = &self.database;
        Ok(match self.opened(&cancel).await? {
            OpenedCollection::Tracks(page)
            | OpenedCollection::Detail(Some(rufin_core::browse::DetailPage {
                content: DetailContent::Tracks(page),
                ..
            })) => self.member_rows(
                prepared_rows(
                    &page.page,
                    offset,
                    limit,
                    page.read.rows(db, offset, limit, cancel),
                )
                .await?,
                offset,
            ),
            OpenedCollection::Albums(opened) => match &opened.content {
                rufin_core::browse::AlbumListContent::Detail { index, page } => self.album_rows(
                    prepared_rows(page, offset, limit, index.rows(db, offset, limit, cancel))
                        .await?,
                ),
                rufin_core::browse::AlbumListContent::Rows(page) => prepared_rows(
                    page,
                    offset,
                    limit,
                    opened.read.albums(db, offset, limit, cancel),
                )
                .await?
                .into_iter()
                .map(|album| match self.release_section {
                    Some(ArtistReleaseSection::Class(class)) => {
                        release_row(album, release_index(class))
                    }
                    _ => album.into(),
                })
                .collect(),
            },
            OpenedCollection::Artists { read, page } => {
                prepared_rows(page, offset, limit, read.artists(db, offset, limit, cancel))
                    .await?
                    .into_iter()
                    .map(|row| artist_row(row, matches!(self.route, Route::AlbumArtists)))
                    .collect()
            }
            OpenedCollection::Genres { read, page } => {
                prepared_rows(page, offset, limit, read.genres(db, offset, limit, cancel))
                    .await?
                    .into_iter()
                    .map(Into::into)
                    .collect()
            }
            OpenedCollection::Moods { read, page } => {
                prepared_rows(page, offset, limit, read.moods(db, offset, limit, cancel))
                    .await?
                    .into_iter()
                    .map(Into::into)
                    .collect()
            }
            OpenedCollection::Playlists { read, page } => prepared_rows(
                page,
                offset,
                limit,
                read.playlist_items(db, offset, limit, cancel),
            )
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
            OpenedCollection::SmartPlaylists { read, page } => prepared_rows(
                page,
                offset,
                limit,
                read.playlist_items(db, offset, limit, cancel),
            )
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
            OpenedCollection::Detail(Some(rufin_core::browse::DetailPage {
                content: DetailContent::Releases { page, .. },
                ..
            })) => {
                let read = &page.query;
                prepared_rows(
                    &page.page,
                    offset,
                    limit,
                    read.albums(db, offset, limit, cancel),
                )
                .await?
                .into_iter()
                .enumerate()
                .map(|(index, album)| {
                    let mut end = 0;
                    let class = page
                        .counts
                        .iter()
                        .position(|count| {
                            end += count;
                            offset + index < end
                        })
                        .unwrap_or(4);
                    release_row(album, class)
                })
                .collect()
            }
            OpenedCollection::Folders(page) => {
                if offset == page.first_row_position
                    && limit.min(page.folder_count + page.track_count - offset)
                        == page.folders.len() + page.tracks.len()
                {
                    self.folder_seed(page)
                } else {
                    self.folder_rows(page, offset, limit, cancel).await?
                }
            }
            OpenedCollection::Home(_) => self
                .home_rows(&cancel)
                .await?
                .iter()
                .skip(offset)
                .take(limit)
                .map(|(row, _)| row.clone())
                .collect(),
            OpenedCollection::Search(rows) => search_rows(rows.clone())
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect(),
            OpenedCollection::Detail(None) => Vec::new(),
        })
    }
}

async fn prepared_rows<T: Clone>(
    page: &library::Page<T>,
    offset: usize,
    limit: usize,
    load: impl std::future::Future<Output = library::LibraryResult<Vec<T>>>,
) -> library::LibraryResult<Vec<T>> {
    if offset >= page.count {
        return Ok(Vec::new());
    }
    let end = offset.saturating_add(limit).min(page.count);
    let first = page.first_row_position;
    if offset >= first && end <= first + page.rows.len() {
        Ok(page.rows[offset - first..end - first].to_vec())
    } else {
        load.await
    }
}
