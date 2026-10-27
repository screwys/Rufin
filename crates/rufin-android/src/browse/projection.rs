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
                let OpenedCollection::Home(home) = self.opened_page(&cancel).await?.opened() else {
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

    pub(super) async fn rows(
        &self,
        offset: usize,
        limit: usize,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<AndroidBrowseRow>> {
        let page = self.opened_page(cancel).await?;
        if matches!(page.opened(), OpenedCollection::Home(_)) {
            return Ok(self
                .home_rows(cancel)
                .await?
                .iter()
                .skip(offset)
                .take(limit)
                .map(|(row, _)| row.clone())
                .collect());
        }
        // Folder pages number their tracks after the folders.
        let first_track = match page.opened() {
            OpenedCollection::Folders(page) => page.folder_count,
            _ => 0,
        };
        Ok(page
            .rows(offset, limit, cancel)
            .await?
            .into_iter()
            .zip(offset..)
            .map(|(row, position)| {
                let index = position.saturating_sub(first_track);
                match row {
                    Row::Track(track) => self.track_row(track, index),
                    Row::Entry(row) => AndroidBrowseRow {
                        fields: self.track_field_values(index, |field| {
                            rufin_core::settings::presentation::track_field(&row.track, field)
                        }),
                        ..AndroidBrowseRow::new(AndroidBrowseItem::PlaylistEntry { row })
                    },
                    Row::Album(album) => album.into(),
                    Row::AlbumHeading(album) => AndroidBrowseRow {
                        album_header: true,
                        ..album.into()
                    },
                    Row::AlbumTrack { track, index } => self.track_row(track, index),
                    Row::Release { album, group } => release_row(album, group),
                    Row::Artist {
                        artist,
                        album_artist,
                    } => artist_row(artist, album_artist),
                    Row::Genre(row) => row.into(),
                    Row::Mood(row) => row.into(),
                    Row::PlaylistItem(row) => row.into(),
                    Row::Folder(folder) => {
                        self.folder_row(folder.object_id, folder.name, folder.artwork_binding)
                    }
                }
            })
            .collect())
    }
}
