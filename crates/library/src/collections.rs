//! Album and artist views, collection choices and album release lookups. Rows, orders, totals
//! and pictures come from the resident Catalog.

use sqlx::FromRow;

use crate::catalog::views::Context;
use crate::{
    AlbumKey, ArtistKey, Catalog, FolderKey, GenreKey, LibraryError, LibraryField, LibraryResult,
    ListKind, ListRead, Read, ReadCancellation, SourceKey,
};

#[derive(Clone, Debug)]
pub struct GenreChoice {
    pub genre_key: GenreKey,
    pub object_id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AlbumReleaseClass {
    Album,
    Ep,
    Single,
    Collection,
    Other,
}

impl AlbumReleaseClass {
    const ALL: [Self; 5] = [
        Self::Album,
        Self::Ep,
        Self::Single,
        Self::Collection,
        Self::Other,
    ];

    /// Its place in an artist's releases, and its code in `albums.flags`.
    pub(crate) fn code(self) -> u16 {
        self as u16
    }

    /// The class a resident album record holds.
    pub(crate) fn of(flags: u16) -> Self {
        let code = (flags & crate::catalog::snapshot::RELEASE_MASK)
            >> crate::catalog::snapshot::RELEASE_SHIFT;
        Self::ALL
            .get(usize::from(code))
            .copied()
            .unwrap_or(Self::Album)
    }

    /// The class an `albums.flags` column holds.
    pub(crate) fn stored(flags: i64) -> Self {
        Self::ALL
            .get((flags >> crate::schema::ALBUM_RELEASE_SHIFT & 0b111) as usize)
            .copied()
            .unwrap_or(Self::Album)
    }
}
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollectionSummary {
    pub album_count: i64,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumLinks {
    pub album_artists: Vec<AlbumArtistLink>,
    pub genres: Vec<AlbumGenreLink>,
}

/// The full view of one album, for detail headers and editors: its row and its whole metadata.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumView {
    pub row: crate::AlbumRow,
    pub metadata: crate::AlbumMetadata,
}

/// The full view of one artist, for detail headers and editors.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtistView {
    pub row: crate::ArtistRow,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FolderChoice {
    pub folder_key: FolderKey,
    pub object_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, FromRow, PartialEq)]
pub struct AlbumReleaseCandidate {
    pub album_key: AlbumKey,
    pub media_uri: String,
    pub lookup_identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlbumReleaseResult {
    Found { release_types: Vec<String> },
    Missing,
}

impl Catalog {
    /// Letter sections of names in the order given.
    pub async fn ordered_name_sections<'a>(
        &self,
        names: impl Iterator<Item = &'a str>,
        _cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        Ok(crate::catalog::views::sections(
            names.map(|name| Some(name.to_owned())),
        ))
    }

    /// Genres of a source's music folder whose names match `filter`, by title: how many match
    /// and the ones in `range`.
    pub async fn genre_choices(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        range: std::ops::Range<usize>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, Vec<GenreChoice>)> {
        let read = ListRead {
            folder,
            filter: filter.to_owned(),
            ..ListRead::new(ListKind::Genres, Some(source), LibraryField::Title)
        };
        let order = self.order(&Read::List(read), cancellation).await?;
        let snapshot = self.snapshot();
        let choices = order
            .items(range)
            .into_iter()
            .filter_map(|item| match item {
                crate::Item::Genre(key) => {
                    let genre = snapshot.tags.get(key.raw())?;
                    Some(GenreChoice {
                        genre_key: key,
                        object_id: snapshot.text(genre.object).to_owned(),
                        name: snapshot.text(genre.name).to_owned(),
                    })
                }
                _ => None,
            })
            .collect();
        Ok((order.len(), choices))
    }

    /// Track URIs of a source's latest albums, album by album.
    pub async fn latest_album_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let read = ListRead {
            folder,
            descending: true,
            ..ListRead::new(ListKind::Albums, Some(source), LibraryField::DateAdded)
        };
        let order = self.order(&Read::List(read), cancellation).await?;
        let tracks = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let folder = folder.and_then(|folder| snapshot.folders.slot(folder.raw()));
            let mut albums = order
                .items(0..album_limit.clamp(1, 100))
                .into_iter()
                .filter_map(|item| match item {
                    crate::Item::Album(key) => snapshot
                        .albums
                        .slot(key.raw())
                        .map(|slot| (key.raw(), slot)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            albums.sort_unstable();
            let mut members = context.album_tracks(
                &albums.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(),
                folder,
            );
            albums
                .into_iter()
                .flat_map(|(_, slot)| members.remove(&slot).unwrap_or_default())
                .filter_map(|slot| snapshot.tracks.key_of(slot))
                .collect::<Vec<_>>()
        };
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&tracks)?)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn album_release_candidates(
        &self,
        source: SourceKey,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumReleaseCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(100);
        self.loaded().await;
        let mut candidates = Vec::new();
        let mut after = 0;
        while candidates.len() < limit {
            let page: Vec<AlbumReleaseCandidate> = {
                let (_permit, mut connection) = self.acquire_general(cancellation).await?;
                sqlx::query_as("SELECT album_key,media_uri,release_identity lookup_identity FROM albums WHERE source_key=?1 AND album_key>?2 AND release_identity IS NOT NULL AND release_lookup_identity IS NOT release_identity ORDER BY album_key LIMIT 256")
                    .bind(source)
                    .bind(after)
                    .fetch_all(&mut *connection)
                    .await?
            };
            let Some(last) = page.last() else { break };
            after = last.album_key.raw();
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let derived = self.covers(&context);
            candidates.extend(
                page.into_iter()
                    .filter(|candidate| {
                        snapshot
                            .albums
                            .slot(candidate.album_key.raw())
                            .is_some_and(|slot| derived.albums[slot as usize].tracks > 0)
                    })
                    .take(limit - candidates.len()),
            );
        }
        Ok(candidates)
    }

    pub async fn accept_album_release_result(
        &self,
        source: SourceKey,
        album: AlbumKey,
        lookup_identity: &str,
        result: AlbumReleaseResult,
    ) -> LibraryResult<Option<AlbumKey>> {
        if lookup_identity.is_empty()
            || matches!(&result,AlbumReleaseResult::Found { release_types } if release_types.is_empty() || release_types.iter().any(|value|value.trim().is_empty()))
        {
            return Err(LibraryError::InvalidRequest(
                "invalid Album release lookup result".to_string(),
            ));
        }
        let mut write = self.write().await?;
        let Some(mut record) = write
            .get::<crate::catalog::records::Album>(album.raw())
            .await?
            .filter(|record| record.source == source)
        else {
            return Ok(None);
        };
        if record.release_identity.as_deref() != Some(lookup_identity)
            || record.release_lookup_identity.as_deref() == Some(lookup_identity)
        {
            return Ok(None);
        }
        write.keep();
        if let AlbumReleaseResult::Found { release_types } = result {
            sqlx::query("INSERT INTO album_release_observations(album_key,lookup_identity,release_types) VALUES(?1,?2,jsonb(?3)) ON CONFLICT(album_key) DO UPDATE SET lookup_identity=excluded.lookup_identity,release_types=excluded.release_types").bind(album).bind(lookup_identity).bind(serde_json::to_string(&release_types)?).execute(write.connection().await?).await?;
        }
        record.release_lookup_identity = Some(lookup_identity.to_owned());
        write.put(record);
        crate::catalog::albums::resolve(&mut write, &[album.raw()].into()).await?;
        write.commit().await?;
        Ok(Some(album))
    }

    /// A source's music folders by name, at most 256, for the music-folder chooser.
    pub fn music_folders(&self, source: SourceKey) -> Vec<FolderChoice> {
        use crate::catalog::snapshot::Packed;
        let snapshot = self.snapshot();
        let Some(owner) = snapshot.source_slot(source) else {
            return Vec::new();
        };
        let mut folders = snapshot
            .folders
            .iter()
            .filter(|(_, folder)| folder.source == owner && folder.indexed())
            .map(|(_, folder)| {
                (
                    snapshot.name_sort(folder.name, folder.sort),
                    folder.key,
                    folder,
                )
            })
            .collect::<Vec<_>>();
        folders.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        folders.truncate(256);
        folders
            .into_iter()
            .map(|(.., folder)| FolderChoice {
                folder_key: FolderKey::from_raw(i64::from(folder.key)),
                object_id: snapshot.text(folder.object).to_owned(),
                name: snapshot.text(folder.name).to_owned(),
            })
            .collect()
    }
}
