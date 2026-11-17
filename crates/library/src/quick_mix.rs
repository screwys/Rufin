//! Quick mix: the tracks each entry's next press plays, and the count and covers Home shows for
//! them. A press plays an entry's tracks in the shuffle of a seed; Home shows the albums that
//! shuffle starts with, so the covers are what plays.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use sqlx::SqliteConnection;

use crate::catalog::views::{Context, Inputs, shuffled, smart_period};
use crate::{
    Catalog, FolderKey, LibraryResult, ReadCancellation, SmartPlaylistDefinition,
    SmartPlaylistSort, TrackKey,
};

/// The most albums an entry's cover shows.
const COVER_ALBUMS: usize = 4;

/// An entry as Home shows it: how many tracks its press plays and the first track of each
/// album they start with, the picks of its mosaic.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuickMixFace {
    pub(crate) entry: u32,
    pub(crate) tracks: u32,
    pub(crate) covers: Vec<TrackKey>,
}

/// What one entry's face was built from: its rules, its seed, the music folder, and of the
/// Catalog's stamps and the day only those its rules read. Listens are none of them: a face
/// holds through plays, and rules that read plays see them on the next day or the entry's next
/// press, sync or edit. Opening Home again with nothing it keys on changed only looks it up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct QuickMixKey {
    definition: u64,
    seed: i64,
    folder: Option<FolderKey>,
    stamps: [u64; 5],
    day: Option<i64>,
}

impl QuickMixKey {
    fn of(
        definition: &SmartPlaylistDefinition,
        seed: i64,
        folder: Option<FolderKey>,
        stamps: [u64; 5],
        now: i64,
    ) -> LibraryResult<Self> {
        use crate::SmartPlaylistRuleField as F;
        use crate::SmartPlaylistRuleOperator as O;
        let reads = |fields: &[F]| definition.rules().any(|rule| fields.contains(&rule.field));
        // Stamps in the order of `Snapshot::stamps`: facts, states, plays, downloads, playlists.
        let marks =
            reads(&[F::Favorite, F::Rating]) || definition.sort_field == SmartPlaylistSort::Rating;
        let playlists = reads(&[F::Playlist]);
        let dated = definition.reads_plays()
            || reads(&[F::DateAdded, F::LastPlayed])
            || definition
                .rules()
                .any(|rule| matches!(rule.operator, O::InTheLast | O::NotInTheLast));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(definition)?.hash(&mut hasher);
        Ok(Self {
            definition: hasher.finish(),
            seed,
            folder,
            stamps: [
                stamps[0],
                if marks { stamps[1] } else { 0 },
                0,
                0,
                if playlists { stamps[4] } else { 0 },
            ],
            day: dated.then(|| now.div_euclid(86_400)),
        })
    }
}

impl Context<'_> {
    /// An entry's tracks in music folder `folder`, each with its place in seed `seed`'s
    /// shuffle, and how many of the shuffle a press takes. A limited entry in an order of its
    /// own is cut to its limit in that order first; a random one takes the first of the shuffle.
    pub(crate) fn quick_mix_tracks(
        &self,
        definition: &SmartPlaylistDefinition,
        inputs: &Inputs,
        folder: Option<u32>,
        seed: i64,
    ) -> (Vec<(i64, u32)>, Option<usize>) {
        if definition.limit.is_some() && definition.sort_field != SmartPlaylistSort::Random {
            let tracks = self
                .evaluate(definition, inputs, folder)
                .into_iter()
                .filter_map(|slot| {
                    Some((shuffled(self.snapshot.tracks.live(slot)?.key, seed), slot))
                })
                .collect();
            return (tracks, None);
        }
        let tracks = self
            .matching(definition, inputs, folder)
            .map(|(slot, record)| (shuffled(record.key, seed), slot))
            .collect();
        (tracks, definition.limit)
    }

    /// How many tracks an entry's press plays, and the first track of each album they start
    /// with, without sorting the rest.
    fn quick_mix_face(
        &self,
        definition: &SmartPlaylistDefinition,
        inputs: &Inputs,
        folder: Option<u32>,
        seed: i64,
    ) -> (usize, Vec<u32>) {
        let (mut tracks, take) = self.quick_mix_tracks(definition, inputs, folder, seed);
        if let Some(take) = take.filter(|take| *take < tracks.len()) {
            tracks.select_nth_unstable(take.saturating_sub(1));
            tracks.truncate(take);
        }
        // An album's place and cover are its first track's; the first albums are those of the
        // lowest, as the press orders them.
        let mut albums = HashMap::<u32, (i64, u32)>::new();
        for (rank, slot) in &tracks {
            if let Some(album) = self
                .snapshot
                .tracks
                .live(*slot)
                .and_then(|record| self.album_of(record))
            {
                let first = albums.entry(album).or_insert((*rank, *slot));
                *first = (*first).min((*rank, *slot));
            }
        }
        let mut firsts = albums.into_values().collect::<Vec<_>>();
        if firsts.len() > COVER_ALBUMS {
            firsts.select_nth_unstable(COVER_ALBUMS - 1);
            firsts.truncate(COVER_ALBUMS);
        }
        firsts.sort_unstable();
        (
            tracks.len(),
            firsts.into_iter().map(|(_, slot)| slot).collect(),
        )
    }
}

/// The media URIs of `covers`, track keys in their order: a mosaic names its picks by them, and
/// only the Store holds them.
pub(crate) async fn cover_uris(
    connection: &mut SqliteConnection,
    covers: &[impl serde::Serialize],
) -> LibraryResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT track.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)",
    )
    .bind(serde_json::to_string(covers)?)
    .fetch_all(&mut *connection)
    .await?)
}

/// An entry's mosaic: `picks`, media URIs, composed as a smart playlist's. Home's tile and the
/// entry's page draw the same one, so either finds it composed by the other.
pub(crate) fn quick_mix_binding(
    definition: &SmartPlaylistDefinition,
    entry: u32,
    picks: Vec<String>,
) -> Option<Vec<u8>> {
    crate::artwork::mosaic_binding(
        definition
            .source_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        format!("quick-mix:{entry}"),
        picks,
    )
}

/// The listen windows `entries` read, read into inputs at `now`.
async fn inputs_of(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    entries: &[SmartPlaylistDefinition],
    now: i64,
) -> LibraryResult<Inputs> {
    let mut inputs = Inputs::default();
    let periods = entries.iter().filter_map(smart_period).collect();
    catalog
        .read_periods(connection, &mut inputs, periods, now)
        .await?;
    Ok(inputs)
}

impl Catalog {
    /// Each entry's face in music folder `folder` at `now`, entry `n` in the shuffle of
    /// `seeds[n]`, the entries that match nothing left out. Only entries whose held face no
    /// longer holds are evaluated.
    pub(crate) async fn quick_mix_faces(
        &self,
        connection: &mut SqliteConnection,
        folder: Option<FolderKey>,
        entries: &[SmartPlaylistDefinition],
        seeds: &[i64],
        now: i64,
    ) -> LibraryResult<Vec<QuickMixFace>> {
        let stamps = self.snapshot().stamps();
        let mut faces = Vec::with_capacity(entries.len());
        let mut missing = Vec::new();
        for (entry, (definition, seed)) in entries.iter().zip(seeds).enumerate() {
            let key = QuickMixKey::of(definition, *seed, folder, stamps, now)?;
            match self.held_quick_mix(&key) {
                Some(face) => faces.push((entry, face)),
                None => missing.push((entry, key)),
            }
        }
        if !missing.is_empty() {
            let reading = missing
                .iter()
                .map(|(entry, _)| entries[*entry].clone())
                .collect::<Vec<_>>();
            let inputs = inputs_of(self, connection, &reading, now).await?;
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let Some(slot) = context.folder_slot(folder, None) else {
                return Ok(Vec::new());
            };
            for (entry, key) in missing {
                let (tracks, covers) =
                    context.quick_mix_face(&entries[entry], &inputs, slot, seeds[entry]);
                let face = Arc::new(QuickMixFace {
                    entry: entry as u32,
                    tracks: u32::try_from(tracks).unwrap_or(u32::MAX),
                    covers: covers
                        .into_iter()
                        .filter_map(|cover| snapshot.tracks.key_of(cover))
                        .map(TrackKey::from_raw)
                        .collect(),
                });
                // Built from a newer Catalog than its key names, it answers no later read.
                if QuickMixKey::of(
                    &entries[entry],
                    seeds[entry],
                    folder,
                    snapshot.stamps(),
                    now,
                )? == key
                {
                    self.hold_quick_mix(key, face.clone());
                }
                faces.push((entry, face));
            }
        }
        faces.sort_by_key(|(entry, _)| *entry);
        Ok(faces
            .into_iter()
            .filter(|(_, face)| face.tracks > 0)
            .map(|(entry, face)| QuickMixFace {
                entry: entry as u32,
                ..QuickMixFace::clone(&face)
            })
            .collect())
    }

    /// The tracks a press of `entry` plays in music folder `folder`, in seed `seed`'s shuffle
    /// at `now`, and the albums they start with: what Home's face for the same seed shows.
    pub async fn quick_mix_press(
        &self,
        folder: Option<FolderKey>,
        entry: &SmartPlaylistDefinition,
        seed: i64,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<QuickMixPress> {
        let inputs = {
            let mut connection = self.acquire_loaded(cancellation).await?;
            inputs_of(self, &mut connection, std::slice::from_ref(entry), now).await?
        };
        // The Catalog is read before the Store; its guard holds no await.
        let (tracks, covers) = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let Some(folder) = context.folder_slot(folder, None) else {
                return Ok(QuickMixPress::default());
            };
            let (mut tracks, take) = context.quick_mix_tracks(entry, &inputs, folder, seed);
            tracks.sort_unstable();
            if let Some(take) = take {
                tracks.truncate(take);
            }
            // The first track of each album the press starts with: the picks of its mosaic.
            let mut albums = Vec::with_capacity(COVER_ALBUMS);
            let mut covers = Vec::with_capacity(COVER_ALBUMS);
            for (_, slot) in &tracks {
                if albums.len() == COVER_ALBUMS {
                    break;
                }
                if let Some(album) = snapshot
                    .tracks
                    .live(*slot)
                    .and_then(|record| context.album_of(record))
                    && !albums.contains(&album)
                {
                    albums.push(album);
                    covers.extend(snapshot.tracks.key_of(*slot));
                }
            }
            let tracks = tracks
                .into_iter()
                .filter_map(|(_, slot)| snapshot.tracks.key_of(slot))
                .map(TrackKey::from_raw)
                .collect::<Arc<[_]>>();
            (tracks, covers)
        };
        let mut connection = self.acquire_loaded(cancellation).await?;
        let covers = cover_uris(&mut connection, &covers).await?;
        Ok(QuickMixPress { tracks, covers })
    }

    /// A Quick mix page's header for entry `entry` of `source_id`, named `name`: what `press`
    /// lists, counted in music folder `folder`, under a mosaic of the tracks it starts with, as
    /// a smart playlist's header shows its members.
    pub fn quick_mix_header(
        &self,
        entry: u32,
        name: String,
        definition: SmartPlaylistDefinition,
        press: &QuickMixPress,
        folder: Option<FolderKey>,
    ) -> crate::Header {
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        // While the page holds its tracks, a download change counts them again.
        self.hold_quick_mix_page(entry, &press.tracks);
        let mut totals = crate::catalog::views::Totals::default();
        if let Some(folder) = context.folder_slot(folder, None) {
            for key in press.tracks.iter() {
                if let Some(record) = snapshot
                    .tracks
                    .get(key.raw())
                    .filter(|record| context.in_source_folder(record, folder))
                {
                    totals.add_member(record);
                }
            }
        }
        let summary = totals.summary();
        let artwork_binding = quick_mix_binding(&definition, entry, press.covers.clone());
        crate::Header::QuickMix(Box::new(QuickMixRow {
            entry,
            name,
            definition,
            track_count: summary.track_count,
            duration_millis: summary.duration_millis,
            downloaded_count: summary.downloaded_count,
            artwork_binding,
        }))
    }
}

/// A Quick mix page's header: the smart playlist header's facts for an entry that is no
/// smart playlist of the Store.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickMixRow {
    /// The entry's place among the Quick mix entries.
    pub entry: u32,
    pub name: String,
    pub definition: SmartPlaylistDefinition,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub artwork_binding: Option<Vec<u8>>,
}

/// What a Quick mix press plays, and the media URI of the first track of each album it starts
/// with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuickMixPress {
    pub tracks: Arc<[TrackKey]>,
    pub covers: Vec<String>,
}
