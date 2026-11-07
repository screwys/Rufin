//! The one keyed statement of a read: the Store columns of the rows a window shows, their
//! pictures and blur-hash previews, and the full reads of detail headers. Each kind of row is a
//! tagged part of one UNION ALL, present only when it has keys, and the statement steps on the
//! connection's raw handle. Playlist mosaics' previews follow, keyed by the composed candidate.

use std::collections::{BTreeSet, HashMap};

use super::rows::{Handle, Rows};
use super::views::{Art, Picture};
use crate::artwork::{AlbumCover, ArtworkCandidate, ArtworkImage};
use crate::{AlbumMetadata, LibraryResult, TrackAccess, TrackMetadata};

/// Columns after the part and the key; a part with fewer pads with NULL.
const COLUMNS: usize = 5;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Part {
    Track,
    View,
    AlbumText,
    AlbumView,
    ArtistView,
    TrackArt,
    AlbumArt,
    ArtistArt,
    AlbumCover,
    TagArt,
    TagMosaic,
    FolderArt,
    PlaylistArt,
    SmartArt,
}

const PARTS: [Part; 14] = [
    Part::Track,
    Part::View,
    Part::AlbumText,
    Part::AlbumView,
    Part::ArtistView,
    Part::TrackArt,
    Part::AlbumArt,
    Part::ArtistArt,
    Part::AlbumCover,
    Part::TagArt,
    Part::TagMosaic,
    Part::FolderArt,
    Part::PlaylistArt,
    Part::SmartArt,
];

/// The JSON object of preview hashes by family of the candidates in a binding.
fn previews(binding: &str, wanted: bool) -> String {
    if !wanted {
        return "NULL".into();
    }
    format!(
        "(SELECT json_group_object(preview.family,preview.hash) FROM json_each({binding}) candidate JOIN artwork_previews preview ON preview.family=json_extract(candidate.value,'$.key'))"
    )
}

impl Part {
    /// The part's columns and what it reads them from; `requested` is the key list.
    fn sql(self, previews_wanted: bool) -> (Vec<String>, String) {
        use crate::schema::LINK_ARTIST;
        let art = |table: &str, key: &str, column: &str| {
            (
                vec![
                    format!("item.{column}"),
                    previews(&format!("item.{column}"), previews_wanted),
                ],
                format!(
                    "JOIN {table} item ON item.{key}=requested.value WHERE item.{column} IS NOT NULL"
                ),
            )
        };
        let source_path = "CASE WHEN source.kind='local' THEN (SELECT file.path FROM files file WHERE file.source_key=track.source_key AND file.path=details.source_path) ELSE details.source_path END";
        match self {
            Self::Track => (
                vec![
                    "track.media_uri".into(),
                    "track.object_id".into(),
                    "track.release_date".into(),
                    source_path.into(),
                    "json_extract(details.metadata,'$.audio_properties')".into(),
                ],
                "JOIN tracks track ON track.track_key=requested.value LEFT JOIN track_details details ON details.track_key=track.track_key LEFT JOIN sources source ON source.source_key=track.source_key".into(),
            ),
            // What a full view adds to its row: the details JSON with the album's and first
            // artist's MBIDs and display artist, and the CUE window.
            Self::View => (
                vec![
                    format!("json_patch(coalesce(json(details.metadata),'{{}}'),json_object('album_display_artist',coalesce(nullif(album.display_artist,''),json_extract(details.metadata,'$.album_display_artist')),
                    'musicbrainz_album_id',coalesce(json_extract(album.metadata,'$.musicbrainz_release_id'),json_extract(details.metadata,'$.musicbrainz_album_id')),
                    'musicbrainz_release_group_id',coalesce(json_extract(album.metadata,'$.musicbrainz_release_group_id'),json_extract(details.metadata,'$.musicbrainz_release_group_id')),
                    'primary_artist_musicbrainz_id',coalesce((SELECT artist.musicbrainz_artist_id FROM track_links credit JOIN artists artist ON artist.artist_key=credit.target WHERE credit.track_key=track.track_key AND credit.role={LINK_ARTIST} ORDER BY credit.position LIMIT 1),json_extract(details.metadata,'$.primary_artist_musicbrainz_id'))))"),
                    "details.cue_path".into(),
                    "details.cue_start_millis".into(),
                    "details.cue_end_millis".into(),
                ],
                "JOIN tracks track ON track.track_key=requested.value LEFT JOIN track_details details ON details.track_key=track.track_key LEFT JOIN albums album ON album.album_key=track.album_key".into(),
            ),
            Self::AlbumText => (
                vec![
                    "album.release_date".into(),
                    "coalesce(date(album.date_added),album.date_added,date(album.first_seen_at,'unixepoch'))".into(),
                ],
                "JOIN albums album ON album.album_key=requested.value".into(),
            ),
            Self::AlbumView => (
                vec!["json_patch(json(album.metadata),json_object('title',album.title,'artist',album.display_artist,'sort_text',album.sort_text,'year',album.year,'release_date',album.release_date,'date_added',coalesce(date(album.date_added),album.date_added,date(album.first_seen_at,'unixepoch')),'is_compilation',json(CASE album.is_compilation WHEN 1 THEN 'true' WHEN 0 THEN 'false' END)))".into()],
                "JOIN albums album ON album.album_key=requested.value".into(),
            ),
            Self::ArtistView => (
                vec!["artist.musicbrainz_artist_id".into()],
                "JOIN artists artist ON artist.artist_key=requested.value".into(),
            ),
            Self::TrackArt => art("track_details", "track_key", "native_artwork"),
            Self::AlbumArt => art("albums", "album_key", "native_artwork"),
            Self::ArtistArt => art("artists", "artist_key", "native_artwork"),
            Self::AlbumCover => (
                vec![
                    "album.title".into(),
                    "album.display_artist".into(),
                    "json_extract(album.metadata,'$.musicbrainz_release_group_id')".into(),
                    "json_extract(album.metadata,'$.musicbrainz_release_id')".into(),
                ],
                "JOIN albums album ON album.album_key=requested.value".into(),
            ),
            Self::TagArt => art("tags", "tag_key", "native_artwork"),
            Self::FolderArt => art("folders", "folder_key", "native_artwork"),
            Self::TagMosaic => (
                vec!["(SELECT json_group_array(track.track_key ORDER BY pick.key) FROM json_each(item.mosaic) pick LEFT JOIN tracks track ON track.media_uri=pick.value)".into()],
                "JOIN tags item ON item.tag_key=requested.value WHERE item.mosaic IS NOT NULL".into(),
            ),
            Self::PlaylistArt | Self::SmartArt => {
                let (table, key) = if self == Self::PlaylistArt {
                    ("playlists", "playlist_key")
                } else {
                    ("smart_playlists", "smart_playlist_key")
                };
                let source = if self == Self::PlaylistArt {
                    "(SELECT source.object_id FROM sources source WHERE source.source_key=item.source_key)"
                } else {
                    "NULL"
                };
                (
                    vec![
                        "item.artwork_binding".into(),
                        previews("item.artwork_binding", previews_wanted),
                        "item.mosaic".into(),
                        source.into(),
                        "item.object_id".into(),
                    ],
                    format!("JOIN {table} item ON item.{key}=requested.value"),
                )
            }
        }
    }
}

/// The keys of each part one statement reads.
#[derive(Default)]
pub(crate) struct Want {
    keys: [BTreeSet<i64>; PARTS.len()],
    /// Bindings carry the blur-hash previews known for their candidates. Mosaic recipes are the
    /// candidates as stored.
    previews: bool,
}

impl Want {
    pub(crate) fn new(previews: bool) -> Self {
        Self {
            previews,
            ..Self::default()
        }
    }

    pub(crate) fn add(&mut self, part: Part, key: i64) {
        self.keys[part as usize].insert(key);
    }

    /// The rows a picture is composed from.
    pub(crate) fn art(&mut self, art: Art) {
        match art {
            Art::None => {}
            Art::Track(key) => self.add(Part::TrackArt, key),
            Art::Album(key) => self.add(Part::AlbumArt, key),
            Art::Artist(key, borrowed) => {
                self.add(Part::ArtistArt, key);
                match borrowed {
                    Some(Picture::Track(key)) => self.add(Part::TrackArt, key),
                    Some(Picture::Album(key)) => self.add(Part::AlbumArt, key),
                    None => {}
                }
            }
            Art::Release(key, artist) => {
                self.add(Part::AlbumCover, key);
                if let Some((key, borrowed)) = artist {
                    self.art(Art::Artist(key, borrowed));
                }
            }
        }
    }
}

/// Text columns of a track row that the Catalog does not hold.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrackText {
    pub(crate) media_uri: String,
    pub(crate) object_id: Option<String>,
    pub(crate) release_date: Option<String>,
    pub(crate) source_path: Option<String>,
    pub(crate) audio: crate::AudioProperties,
}

/// A playlist's or smart playlist's own binding and the candidate its mosaic picks compose.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlaylistArt {
    pub(crate) candidates: Option<Vec<ArtworkCandidate>>,
    pub(crate) mosaic: Option<ArtworkCandidate>,
}

/// What one statement read, by part and key.
#[derive(Default)]
pub(crate) struct Got {
    pub(crate) tracks: HashMap<i64, TrackText>,
    /// A full view's details and its CUE window; the rest of the access its row fills.
    pub(crate) views: HashMap<i64, (TrackMetadata, TrackAccess)>,
    pub(crate) album_texts: HashMap<i64, (Option<String>, Option<String>)>,
    pub(crate) album_views: HashMap<i64, AlbumMetadata>,
    pub(crate) artist_views: HashMap<i64, Option<String>>,
    /// Stored pictures by art part and key.
    art: HashMap<(Part, i64), Vec<ArtworkCandidate>>,
    /// Playlists' and smart playlists' pictures, by whether smart and key.
    playlists: HashMap<(bool, i64), PlaylistArt>,
    /// The track keys of each tag's mosaic picks; a pick the Store lacks has none.
    pub(crate) mosaics: HashMap<i64, Vec<Option<i64>>>,
    /// The cover lookup of each album that has usable text or a MusicBrainz id.
    covers: HashMap<i64, AlbumCover>,
    previews: HashMap<String, String>,
}

impl Got {
    /// A row's picture as the Catalog chose it. An artist lists its own pictures, then the one
    /// it borrows, then its name lookups. An album's cover lookup comes before its artist's.
    fn candidates(&self, art: Art) -> Option<Vec<ArtworkCandidate>> {
        let stored = |part, key| self.art.get(&(part, key));
        let picture = |picture: Picture| match picture {
            Picture::Track(key) => stored(Part::TrackArt, key),
            Picture::Album(key) => stored(Part::AlbumArt, key),
        };
        let mut candidates = match art {
            Art::None => return None,
            Art::Track(key) => stored(Part::TrackArt, key)?.clone(),
            Art::Album(key) => stored(Part::AlbumArt, key)?.clone(),
            Art::Artist(key, borrowed) => {
                let own = stored(Part::ArtistArt, key)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let lookup = |candidate: &&ArtworkCandidate| {
                    matches!(
                        candidate.image,
                        ArtworkImage::Artist { .. } | ArtworkImage::Album(_)
                    )
                };
                let candidates = own
                    .iter()
                    .filter(|candidate| !lookup(candidate))
                    .chain(borrowed.and_then(picture).into_iter().flatten())
                    .chain(own.iter().filter(lookup))
                    .cloned()
                    .collect::<Vec<_>>();
                if candidates.is_empty() {
                    return None;
                }
                candidates
            }
            Art::Release(key, artist) => {
                let cover = self.covers.get(&key).cloned();
                let candidates = cover
                    .map(|cover| ArtworkCandidate::new(ArtworkImage::Album(cover), None))
                    .into_iter()
                    .chain(
                        artist
                            .and_then(|(key, borrowed)| self.candidates(Art::Artist(key, borrowed)))
                            .into_iter()
                            .flatten(),
                    )
                    .collect::<Vec<_>>();
                if candidates.is_empty() {
                    return None;
                }
                candidates
            }
        };
        self.fill(&mut candidates);
        Some(candidates)
    }

    /// Fills the preview hashes the Store knows for candidates without one.
    fn fill(&self, candidates: &mut [ArtworkCandidate]) {
        for candidate in candidates {
            if candidate.blur_hash.is_none() {
                candidate.blur_hash = self.previews.get(&candidate.key).cloned();
            }
        }
    }

    /// The binding a row shows.
    pub(crate) fn binding(&self, art: Art) -> LibraryResult<Option<Vec<u8>>> {
        self.candidates(art)
            .map(|candidates| encode(&candidates))
            .transpose()
    }

    /// A tag's or folder's own binding with previews.
    pub(crate) fn own(&self, part: Part, key: i64) -> LibraryResult<Option<Vec<u8>>> {
        self.art
            .get(&(part, key))
            .map(|candidates| {
                let mut candidates = candidates.clone();
                self.fill(&mut candidates);
                encode(&candidates)
            })
            .transpose()
    }

    /// A playlist's own binding with previews, without its mosaic.
    pub(crate) fn playlist_binding(&self, key: i64) -> LibraryResult<Option<Vec<u8>>> {
        self.playlists
            .get(&(false, key))
            .and_then(|art| art.candidates.clone())
            .map(|mut candidates| {
                self.fill(&mut candidates);
                encode(&candidates)
            })
            .transpose()
    }

    /// A playlist row's binding with previews: its own picture, then its mosaic. None when it
    /// has neither.
    pub(crate) fn playlist(&self, smart: bool, key: i64) -> LibraryResult<Option<Vec<u8>>> {
        let Some(art) = self.playlists.get(&(smart, key)) else {
            return Ok(None);
        };
        let mut candidates = art.candidates.clone().unwrap_or_default();
        candidates.extend(art.mosaic.clone());
        if candidates.is_empty() {
            return Ok(None);
        }
        self.fill(&mut candidates);
        encode(&candidates).map(Some)
    }
}

pub(crate) fn encode(candidates: &[ArtworkCandidate]) -> LibraryResult<Vec<u8>> {
    Ok(serde_json::to_vec(candidates)?)
}

fn candidates(rows: &Rows<'_>, column: i32) -> LibraryResult<Option<Vec<ArtworkCandidate>>> {
    rows.blob(column)
        .map(|binding| serde_json::from_slice(&binding))
        .transpose()
        .map_err(Into::into)
}

/// Reads every part of a statement on the connection's raw handle.
pub(crate) async fn run(
    connection: &mut sqlx::SqliteConnection,
    want: &Want,
) -> LibraryResult<Got> {
    let mut got = Got::default();
    if want.keys.iter().all(BTreeSet::is_empty) {
        return Ok(got);
    }
    let mut parts = Vec::new();
    let mut binds = Vec::new();
    for part in PARTS {
        let keys = &want.keys[part as usize];
        if keys.is_empty() {
            continue;
        }
        let (mut columns, from) = part.sql(want.previews);
        columns.resize(COLUMNS, "NULL".into());
        binds.push(serde_json::to_string(keys)?);
        parts.push(format!(
            "SELECT {},requested.value,{} FROM json_each(?{}) requested {from}",
            part as i64,
            columns.join(","),
            binds.len()
        ));
    }
    let sql = parts.join(" UNION ALL ");
    let handle = Handle::lock(connection).await?;
    let mut rows = handle.prepare(&sql)?;
    for (index, keys) in binds.iter().enumerate() {
        rows.bind_text(index as i32 + 1, keys)?;
    }
    fn text(rows: &Rows<'_>, column: i32) -> Option<String> {
        rows.optional_text(column).map(str::to_owned)
    }
    fn json<T: serde::de::DeserializeOwned + Default>(
        rows: &Rows<'_>,
        column: i32,
    ) -> LibraryResult<T> {
        Ok(match rows.optional_text(column) {
            Some(text) => serde_json::from_str(text)?,
            None => T::default(),
        })
    }
    while rows.step()? {
        let part = PARTS[rows.int(0) as usize];
        let key = rows.int(1);
        let previews = |rows: &Rows<'_>, got: &mut Got, column| -> LibraryResult<()> {
            if let Some(text) = rows.optional_text(column) {
                got.previews
                    .extend(serde_json::from_str::<HashMap<String, String>>(text)?);
            }
            Ok(())
        };
        match part {
            Part::Track => {
                got.tracks.insert(
                    key,
                    TrackText {
                        media_uri: rows.text(2).to_owned(),
                        object_id: text(&rows, 3),
                        release_date: text(&rows, 4),
                        source_path: text(&rows, 5),
                        audio: json(&rows, 6)?,
                    },
                );
            }
            Part::View => {
                got.views.insert(
                    key,
                    (
                        json(&rows, 2)?,
                        TrackAccess {
                            cue_path: text(&rows, 3),
                            cue_start_millis: rows.optional_int(4),
                            cue_end_millis: rows.optional_int(5),
                            ..TrackAccess::default()
                        },
                    ),
                );
            }
            Part::AlbumText => {
                got.album_texts
                    .insert(key, (text(&rows, 2), text(&rows, 3)));
            }
            Part::AlbumView => {
                got.album_views.insert(key, json(&rows, 2)?);
            }
            Part::ArtistView => {
                got.artist_views.insert(key, text(&rows, 2));
            }
            Part::TrackArt | Part::AlbumArt | Part::ArtistArt | Part::TagArt | Part::FolderArt => {
                previews(&rows, &mut got, 3)?;
                if let Some(stored) = candidates(&rows, 2)? {
                    got.art.insert((part, key), stored);
                }
            }
            Part::AlbumCover => {
                let text = |column| rows.optional_text(column).unwrap_or_default();
                if let Some(cover) = AlbumCover::new(
                    text(3),
                    text(2),
                    rows.optional_text(4),
                    rows.optional_text(5),
                ) {
                    got.covers.insert(key, cover);
                }
            }
            Part::TagMosaic => {
                got.mosaics.insert(key, json(&rows, 2)?);
            }
            Part::PlaylistArt | Part::SmartArt => {
                previews(&rows, &mut got, 3)?;
                let smart = part == Part::SmartArt;
                let art = PlaylistArt {
                    candidates: candidates(&rows, 2)?,
                    mosaic: crate::artwork::playlist_mosaic(
                        rows.optional_text(4),
                        (smart, text(&rows, 5), rows.text(6).to_owned()),
                    )?,
                };
                got.playlists.insert((smart, key), art);
            }
        }
    }
    drop(rows);
    // A mosaic's key comes from its picks, so its preview is read once the picks are known.
    let mosaics = got
        .playlists
        .values()
        .filter_map(|art| art.mosaic.as_ref().map(|mosaic| &mosaic.key))
        .collect::<Vec<_>>();
    if want.previews && !mosaics.is_empty() {
        let mut rows = handle.prepare("SELECT preview.family,preview.hash FROM json_each(?1) requested JOIN artwork_previews preview ON preview.family=requested.value")?;
        rows.bind_text(1, &serde_json::to_string(&mosaics)?)?;
        while rows.step()? {
            got.previews
                .insert(rows.text(0).to_owned(), rows.text(1).to_owned());
        }
    }
    Ok(got)
}

/// The bindings rows show for these pictures: composed candidates with their previews.
pub(crate) async fn bindings(
    connection: &mut sqlx::SqliteConnection,
    arts: &[Art],
) -> LibraryResult<Vec<Option<Vec<u8>>>> {
    let mut want = Want::new(true);
    for art in arts {
        want.art(*art);
    }
    let got = run(connection, &want).await?;
    arts.iter().map(|art| got.binding(*art)).collect()
}

/// The candidates of these pictures as stored, without previews, so showing a cover never
/// changes a mosaic recipe.
pub(crate) async fn recipes(
    connection: &mut sqlx::SqliteConnection,
    arts: &[Art],
) -> LibraryResult<Vec<Option<Vec<ArtworkCandidate>>>> {
    let mut want = Want::new(false);
    for art in arts {
        want.art(*art);
    }
    let got = run(connection, &want).await?;
    Ok(arts.iter().map(|art| got.candidates(*art)).collect())
}
