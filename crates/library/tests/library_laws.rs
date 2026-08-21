use library::{
    AcceptedHomeChange, AcceptedLibraryChange, Album, AlbumId, AlbumRelations, Artist,
    ArtistCredit, ArtistId, CandidateBatch, CandidateFinish, CandidateHeader, FavoriteAcceptance,
    FavoriteItemId, Genre, GenreCredit, GenreId, HomeFacts, Libraries, Library,
    LocalComponentReplacement, Playlist, PlaylistAcceptance, PlaylistEdit, PlaylistEntry,
    PlaylistId, PlaylistSnapshot, ProviderFreshness, SourceId, SourceLibraryUpdate, Track,
    TrackData, TrackId, TrackRelations,
};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
const TRACKS: usize = 2;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    Local,
    Jellyfin,
    OpenSubsonic,
}
impl Slot {
    const ALL: [Self; 3] = [Self::Local, Self::Jellyfin, Self::OpenSubsonic];

    const fn index(self) -> usize {
        match self {
            Self::Local => 0,
            Self::Jellyfin => 1,
            Self::OpenSubsonic => 2,
        }
    }

    const fn is_remote(self) -> bool {
        !matches!(self, Self::Local)
    }

    fn source_id(self) -> SourceId {
        SourceId::new(match self {
            Self::Local => "local:law:shared",
            Self::Jellyfin => "jellyfin:law:shared",
            Self::OpenSubsonic => "opensubsonic:law:shared",
        })
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Jellyfin => "jellyfin",
            Self::OpenSubsonic => "opensubsonic",
        }
    }
}
struct LocalPlaylist {
    id: PlaylistId,
    entries: Vec<Occurrence>,
}
struct Occurrence {
    id: String,
    track: usize,
}
struct SourceModel {
    present: bool,
    local_tracks: [bool; TRACKS],
    titles: [String; TRACKS],
    favorites: [[bool; TRACKS]; 3],
    remote_entries: Vec<usize>,
    local_playlist: Option<LocalPlaylist>,
    freshness: Option<ProviderFreshness>,
}
impl SourceModel {
    fn new(slot: Slot) -> Self {
        Self {
            present: false,
            local_tracks: [true; TRACKS],
            titles: std::array::from_fn(|index| format!("{} track {index}", slot.prefix())),
            favorites: [[false; TRACKS]; 3],
            remote_entries: vec![0, 1, 0],
            local_playlist: None,
            freshness: None,
        }
    }
}
struct Model {
    sources: [SourceModel; 3],
}
impl Model {
    fn new() -> Self {
        Self {
            sources: std::array::from_fn(|index| SourceModel::new(Slot::ALL[index])),
        }
    }

    fn source(&self, slot: Slot) -> &SourceModel {
        &self.sources[slot.index()]
    }

    fn source_mut(&mut self, slot: Slot) -> &mut SourceModel {
        &mut self.sources[slot.index()]
    }
}
struct Harness {
    path: PathBuf,
    library: Option<Libraries>,
    loaded: [Option<Arc<Library>>; 3],
}
impl Harness {
    fn new(path: PathBuf) -> Self {
        Self {
            library: Some(Libraries::open(&path).expect("open law Library")),
            path,
            loaded: [None, None, None],
        }
    }

    fn library(&self) -> &Libraries {
        self.library.as_ref().expect("law Library is open")
    }

    fn loaded(&self, slot: Slot) -> &Arc<Library> {
        self.loaded[slot.index()]
            .as_ref()
            .expect("operation requires an accepted source")
    }

    fn restart(&mut self, model: &Model) {
        self.loaded = [None, None, None];
        drop(self.library.take());
        self.library = Some(Libraries::open(&self.path).expect("restart Library"));
        for slot in Slot::ALL {
            self.loaded[slot.index()] = model.source(slot).present.then(|| {
                self.library()
                    .load_source(&slot.source_id())
                    .expect("reload source after restart")
                    .expect("accepted source after restart")
            });
        }
    }

    fn reopened_product(&self, model: &Model) -> [Option<ProductSource>; 3] {
        let library = Libraries::open(&self.path).expect("open readback Library");
        Slot::ALL.map(|slot| {
            model.source(slot).present.then(|| {
                let loaded = library
                    .load_source(&slot.source_id())
                    .expect("read back source")
                    .expect("accepted source after readback");
                product_snapshot(&loaded, slot, model.source(slot))
            })
        })
    }
}
type ProductSource = (
    Vec<ProductTrack>,
    Vec<ProductPlaylist>,
    Option<ProviderFreshness>,
);
type ProductTrack = (String, String, bool, bool, bool);
type ProductPlaylist = (String, String, Vec<(String, String)>);
#[derive(Clone, Debug)]
enum Op {
    Refresh(Slot),
    RemoteExactPatch(Slot),
    LocalExactPatch(usize, usize),
    Favorite(Slot, usize, usize, bool),
    Playlist(Slot),
    Forget(Slot),
    CandidateDrop(Slot, bool),
    Restart,
}

fn slots() -> impl Strategy<Value = Slot> {
    prop_oneof![
        Just(Slot::Local),
        Just(Slot::Jellyfin),
        Just(Slot::OpenSubsonic)
    ]
}

fn remote_slots() -> impl Strategy<Value = Slot> {
    prop_oneof![Just(Slot::Jellyfin), Just(Slot::OpenSubsonic)]
}

fn operations() -> impl Strategy<Value = Op> {
    prop_oneof![
        slots().prop_map(Op::Refresh),
        remote_slots().prop_map(Op::RemoteExactPatch),
        (0usize..3, 0usize..TRACKS).prop_map(|value| Op::LocalExactPatch(value.0, value.1)),
        (slots(), 0usize..3, 0usize..TRACKS, any::<bool>())
            .prop_map(|value| { Op::Favorite(value.0, value.1, value.2, value.3) }),
        slots().prop_map(Op::Playlist),
        slots().prop_map(Op::Forget),
        (slots(), any::<bool>()).prop_map(|value| Op::CandidateDrop(value.0, value.1)),
        Just(Op::Restart),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 56,
        failure_persistence: Some(Box::new(
            FileFailurePersistence::WithSource("proptest-regressions")
        )),
        ..ProptestConfig::default()
    })]
    #[test]
    fn library_operations_preserve_the_public_product_laws(operations in prop::collection::vec(operations(), 1..=20)) {
        let directory = tempfile::tempdir().expect("temporary law directory");
        let mut harness = Harness::new(directory.path().join("library.db"));
        let mut model = Model::new();

        for (step, slot) in Slot::ALL.into_iter().enumerate() {
            refresh(&mut harness, &mut model, slot, 0);
            verify(&mut harness, &model, step);
        }
        for finished in [false, true] {
            candidate_drop(&harness, &model, Slot::Local, finished, 0);
            verify(&mut harness, &model, 0);
        }
        for (step, operation) in operations.iter().enumerate() {
            apply(&mut harness, &mut model, operation, i64::try_from(step).expect("step fits"));
            verify(&harness, &model, step);
        }
        harness.restart(&model);
        verify(&harness, &model, operations.len());
    }
}

fn apply(harness: &mut Harness, model: &mut Model, operation: &Op, timestamp: i64) {
    match operation {
        Op::Refresh(slot) => refresh(harness, model, *slot, timestamp),
        Op::RemoteExactPatch(slot) if model.source(*slot).present => {
            remote_exact_patch(harness, model, *slot, timestamp)
        }
        Op::LocalExactPatch(kind, index) if model.source(Slot::Local).present => {
            local_exact_patch(harness, model, *kind, *index, timestamp)
        }
        Op::Favorite(slot, kind, index, next_favorite)
            if model.source(*slot).present && has_track(*slot, model.source(*slot), *index) =>
        {
            favorite(harness, model, *slot, *kind, *index, *next_favorite)
        }
        Op::Playlist(slot) if model.source(*slot).present => playlist(harness, model, *slot),
        Op::Forget(slot) => forget(harness, model, *slot),
        Op::CandidateDrop(slot, finished) => {
            candidate_drop(harness, model, *slot, *finished, timestamp)
        }
        Op::Restart => harness.restart(model),
        Op::RemoteExactPatch(_) | Op::LocalExactPatch(..) | Op::Favorite(..) | Op::Playlist(..) => {
        }
    }
}

fn refresh(harness: &mut Harness, model: &mut Model, slot: Slot, accepted_at: i64) {
    let source = model.source(slot);
    let freshness = slot.is_remote().then(|| ProviderFreshness {
        version: 1,
        marker: format!("{}:{accepted_at}", slot.prefix()).into_bytes(),
    });
    let mut candidate = harness
        .library()
        .begin_source_candidate(CandidateHeader {
            source_id: slot.source_id(),
            input_digest: [u8::try_from(slot.index() + 1).expect("fixture source index"); 32],
        })
        .expect("begin refresh candidate");
    candidate
        .write(CandidateBatch::Albums(
            (0..TRACKS)
                .filter(|index| source_item(slot, *index) && has_track(slot, source, *index))
                .map(|index| album(slot, source, index))
                .collect(),
        ))
        .expect("write refresh Albums");
    candidate
        .write(CandidateBatch::Tracks(
            (0..TRACKS)
                .filter(|index| has_track(slot, source, *index))
                .map(|index| track(slot, source, index))
                .collect(),
        ))
        .expect("write refresh Tracks");
    candidate
        .write(CandidateBatch::Artists(
            (0..TRACKS)
                .filter(|index| source_item(slot, *index) && has_track(slot, source, *index))
                .map(|index| artist(slot, source, index))
                .collect(),
        ))
        .expect("write refresh Artists");
    candidate
        .write(CandidateBatch::Genres(
            (0..TRACKS)
                .filter(|index| source_item(slot, *index) && has_track(slot, source, *index))
                .map(|index| genre(slot, index))
                .collect(),
        ))
        .expect("write refresh Genres");
    if slot.is_remote() {
        candidate
            .write(CandidateBatch::Playlists(vec![remote_playlist(
                slot, source,
            )]))
            .expect("write refresh remote Playlist");
    }
    let current = harness.loaded[slot.index()].as_ref();
    let commit = candidate
        .finish(
            CandidateFinish {
                freshness: freshness.clone(),
                home: HomeFacts::RufinDefined,
                accepted_at,
            },
            current,
        )
        .and_then(|prepared| prepared.accept())
        .expect("accept refresh candidate");
    harness.loaded[slot.index()] = Some(commit.library);
    let source = model.source_mut(slot);
    source.present = true;
    source.freshness = freshness;
}

fn remote_exact_patch(harness: &mut Harness, model: &mut Model, slot: Slot, timestamp: i64) {
    let index = usize::try_from(timestamp.unsigned_abs() % TRACKS as u64).expect("track index");
    let source = model.source_mut(slot);
    source.titles[index] = format!("{} remote patch {timestamp}", slot.prefix());
    let update = SourceLibraryUpdate {
        tracks: vec![track(slot, source, index)],
        playlists: vec![remote_playlist(slot, source)],
        ..SourceLibraryUpdate::default()
    };
    let accepted = harness
        .loaded(slot)
        .accept_source_update(update)
        .expect("accept exact remote source patch")
        .expect("changed remote source facts");
    assert_eq!(accepted.home, AcceptedHomeChange::Rebuild);
    assert!(accepted.download_coverage_changed);
}

fn local_exact_patch(
    harness: &mut Harness,
    model: &mut Model,
    kind: usize,
    index: usize,
    timestamp: i64,
) {
    let source = model.source_mut(Slot::Local);
    let replacement = match (kind, source.local_tracks[index]) {
        (0, false) => {
            source.local_tracks[index] = true;
            source.titles[index] = format!("local exact add {timestamp}");
            LocalComponentReplacement {
                observed_at: timestamp,
                albums: vec![album(Slot::Local, source, index)],
                tracks: vec![track(Slot::Local, source, index)],
                artists: vec![artist(Slot::Local, source, index)],
                genres: vec![genre(Slot::Local, index)],
                ..LocalComponentReplacement::default()
            }
        }
        (1, true) => {
            source.titles[index] = format!("local exact update {timestamp}");
            LocalComponentReplacement {
                observed_at: timestamp,
                albums: vec![album(Slot::Local, source, index)],
                tracks: vec![track(Slot::Local, source, index)],
                artists: vec![artist(Slot::Local, source, index)],
                genres: vec![genre(Slot::Local, index)],
                ..LocalComponentReplacement::default()
            }
        }
        (2, true) => {
            source.local_tracks[index] = false;
            LocalComponentReplacement {
                observed_at: timestamp,
                removed_album_ids: vec![album_id(Slot::Local, index)],
                removed_track_ids: vec![track_id(Slot::Local, index)],
                removed_artist_ids: vec![artist_id(Slot::Local, index)],
                removed_genre_ids: vec![genre_id(Slot::Local, index)],
                ..LocalComponentReplacement::default()
            }
        }
        _ => return,
    };
    let accepted = harness
        .loaded(Slot::Local)
        .accept_local_component(replacement)
        .expect("accept exact Local patch")
        .expect("changed Local component");
    assert_eq!(accepted.home, AcceptedHomeChange::Rebuild);
    assert!(accepted.download_coverage_changed);
}

fn candidate_drop(harness: &Harness, model: &Model, slot: Slot, finished: bool, accepted_at: i64) {
    let mut candidate = harness
        .library()
        .begin_source_candidate(CandidateHeader {
            source_id: slot.source_id(),
            input_digest: [42; 32],
        })
        .expect("begin discarded candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![track(
            slot,
            model.source(slot),
            0,
        )]))
        .expect("write discarded candidate");
    if finished {
        let prepared = candidate
            .finish(
                CandidateFinish {
                    freshness: None,
                    home: HomeFacts::RufinDefined,
                    accepted_at,
                },
                harness.loaded[slot.index()].as_ref(),
            )
            .expect("finish discarded candidate");
        for slot in Slot::ALL {
            let expected = expected_product(model, slot);
            let resident = harness.loaded[slot.index()]
                .as_ref()
                .map(|loaded| product_snapshot(loaded, slot, model.source(slot)));
            let accepted = harness
                .library()
                .load_source(&slot.source_id())
                .expect("load accepted source while candidate is prepared")
                .as_ref()
                .map(|loaded| product_snapshot(loaded, slot, model.source(slot)));
            assert_eq!(resident, expected, "resident prepared witness, {slot:?}");
            assert_eq!(accepted, expected, "accepted prepared witness, {slot:?}");
        }
        drop(prepared);
    }
}

fn favorite(
    harness: &mut Harness,
    model: &mut Model,
    slot: Slot,
    kind: usize,
    index: usize,
    favorite: bool,
) {
    let item = match kind {
        0 => FavoriteItemId::Track(track_id(slot, index)),
        1 => FavoriteItemId::Album(album_id(slot, index)),
        2 => FavoriteItemId::Artist(artist_id(slot, index)),
        _ => unreachable!("favorite kind is bounded"),
    };
    let acceptance = if slot.is_remote() {
        FavoriteAcceptance::SourceAcknowledged {
            item: item.clone(),
            favorite,
        }
    } else {
        FavoriteAcceptance::RufinOwned {
            item: item.clone(),
            favorite,
        }
    };
    let accepted = harness
        .loaded(slot)
        .accept_favorite(acceptance)
        .expect("accept favorite");
    assert_eq!(accepted.home, AcceptedHomeChange::Favorite(item));
    assert!(accepted.download_coverage_changed);
    model.source_mut(slot).favorites[kind][index] = favorite;
}

fn playlist(harness: &mut Harness, model: &mut Model, slot: Slot) {
    if slot.is_remote() {
        let source = model.source_mut(slot);
        source.remote_entries = if source.remote_entries == [0, 1, 0] {
            vec![1, 0]
        } else {
            vec![0, 1, 0]
        };
        let accepted = harness
            .loaded(slot)
            .accept_playlist(PlaylistAcceptance::SourceSnapshot(remote_playlist(
                slot, source,
            )))
            .expect("accept exact remote playlist readback")
            .expect("changed remote playlist");
        assert_eq!(accepted.home, AcceptedHomeChange::Keep);
        assert!(accepted.download_coverage_changed);
        return;
    }

    if model.source(Slot::Local).local_playlist.is_none() {
        let tracks = (0..TRACKS)
            .filter(|index| has_track(Slot::Local, model.source(Slot::Local), *index))
            .collect::<Vec<_>>();
        if tracks.is_empty() {
            return;
        }
        let change = harness
            .loaded(Slot::Local)
            .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::Create {
                name: "Law Local Playlist".to_string(),
                track_ids: vec![track_id(Slot::Local, tracks[0]); 3],
            }))
            .expect("create local playlist")
            .expect("local playlist creation changes the Library");
        assert_eq!(change.home, AcceptedHomeChange::Keep);
        assert!(change.download_coverage_changed);
        let id = created_playlist_id(change);
        model.source_mut(Slot::Local).local_playlist = Some(LocalPlaylist {
            id,
            entries: vec![
                Occurrence {
                    id: String::new(),
                    track: tracks[0],
                },
                Occurrence {
                    id: String::new(),
                    track: tracks[0],
                },
                Occurrence {
                    id: String::new(),
                    track: tracks[0],
                },
            ],
        });
        record_generated_occurrences(harness.loaded(Slot::Local), model.source_mut(Slot::Local));
        return;
    }

    let track = (0..TRACKS).find(|index| has_track(Slot::Local, model.source(Slot::Local), *index));
    let Some(track) = track else { return };
    let playlist = model
        .source(Slot::Local)
        .local_playlist
        .as_ref()
        .expect("created local Playlist")
        .id
        .clone();
    let accepted = harness
        .loaded(Slot::Local)
        .accept_playlist(PlaylistAcceptance::RufinOwned(PlaylistEdit::AddTracks {
            playlist_id: playlist,
            track_ids: vec![track_id(Slot::Local, track)],
        }))
        .expect("add local playlist occurrence")
        .expect("added local playlist occurrence");
    assert_eq!(accepted.home, AcceptedHomeChange::Keep);
    assert!(accepted.download_coverage_changed);
    model
        .source_mut(Slot::Local)
        .local_playlist
        .as_mut()
        .expect("local Playlist remains")
        .entries
        .push(Occurrence {
            id: String::new(),
            track,
        });
    record_generated_occurrences(harness.loaded(Slot::Local), model.source_mut(Slot::Local));
}

fn record_generated_occurrences(loaded: &Arc<Library>, source: &mut SourceModel) {
    let present = source.local_tracks;
    let playlist = source
        .local_playlist
        .as_mut()
        .expect("record occurrences for a local Playlist");
    let detail = loaded
        .playlist_detail(&playlist.id)
        .expect("read local Playlist")
        .expect("created local Playlist");
    let mut ids = HashSet::new();
    let expected = playlist
        .entries
        .iter_mut()
        .filter(|entry| present[entry.track])
        .collect::<Vec<_>>();
    assert_eq!(detail.entries.len(), expected.len());
    for (index, expected) in expected.into_iter().enumerate() {
        let actual = detail
            .entries
            .entry(index)
            .expect("read Playlist occurrence")
            .expect("created Playlist occurrence");
        assert_eq!(actual.track.id, track_id(Slot::Local, expected.track));
        assert!(!actual.occurrence_id.is_empty());
        assert!(ids.insert(actual.occurrence_id.clone()));
        expected.id = actual.occurrence_id;
    }
}

fn forget(harness: &mut Harness, model: &mut Model, slot: Slot) {
    harness
        .library()
        .remove_source_data(&slot.source_id())
        .expect("forget source data");
    harness.loaded[slot.index()] = None;
    let source = model.source_mut(slot);
    source.present = false;
    if slot == Slot::Local {
        source.favorites = [[false; TRACKS]; 3];
        source.local_playlist = None;
    }
}

fn verify(harness: &Harness, model: &Model, step: usize) {
    let resident = Slot::ALL.map(|slot| {
        harness.loaded[slot.index()]
            .as_ref()
            .map(|loaded| product_snapshot(loaded, slot, model.source(slot)))
    });
    let reopened = harness.reopened_product(model);
    for slot in Slot::ALL {
        let expected = expected_product(model, slot);
        assert_eq!(
            resident[slot.index()],
            expected,
            "resident witness step {step}, {slot:?}"
        );
        assert_eq!(
            reopened[slot.index()],
            expected,
            "reopened witness step {step}, {slot:?}"
        );
    }

    let connection = Connection::open(&harness.path).expect("open verifier SQLite connection");
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("run SQLite integrity check");
    assert_eq!(integrity, "ok", "SQLite integrity step {step}");
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .expect("prepare foreign-key check");
    assert!(
        statement
            .query([])
            .expect("run foreign-key check")
            .next()
            .expect("read foreign-key check")
            .is_none(),
        "SQLite foreign-key violation step {step}"
    );
}

fn expected_product(model: &Model, slot: Slot) -> Option<ProductSource> {
    let source = model.source(slot);
    source.present.then(|| {
        (
            (0..TRACKS)
                .filter(|index| has_track(slot, source, *index))
                .map(|index| {
                    (
                        track_id(slot, index).to_string(),
                        source.titles[index].clone(),
                        source.favorites[0][index],
                        source.favorites[1][index],
                        source.favorites[2][index],
                    )
                })
                .collect(),
            expected_playlists(slot, source),
            source.freshness.clone(),
        )
    })
}

fn expected_playlists(slot: Slot, source: &SourceModel) -> Vec<ProductPlaylist> {
    let mut playlists = Vec::new();
    if slot.is_remote() {
        playlists.push((
            remote_playlist_id(slot).to_string(),
            format!("{} Remote Playlist", slot.prefix()),
            source
                .remote_entries
                .iter()
                .enumerate()
                .map(|(position, track)| {
                    (
                        format!("{}:remote:{position}", slot.prefix()),
                        track_id(slot, *track).to_string(),
                    )
                })
                .collect(),
        ));
    }
    if let Some(playlist) = &source.local_playlist {
        playlists.push((
            playlist.id.to_string(),
            "Law Local Playlist".to_string(),
            playlist
                .entries
                .iter()
                .filter(|entry| has_track(slot, source, entry.track))
                .map(|entry| (entry.id.clone(), track_id(slot, entry.track).to_string()))
                .collect(),
        ));
    }
    playlists.sort_by(|left, right| left.0.cmp(&right.0));
    playlists
}

fn product_snapshot(loaded: &Arc<Library>, slot: Slot, source: &SourceModel) -> ProductSource {
    assert_eq!(loaded.source_id(), &slot.source_id());
    let count = (0..TRACKS)
        .filter(|&index| has_track(slot, source, index))
        .count();
    let counts = loaded.counts().expect("read loaded counts");
    assert_eq!(counts.tracks, count);
    assert_eq!(counts.albums, count);
    assert_eq!(loaded.artists(None).expect("read Artists").len(), count);
    assert_eq!(loaded.genres(None).expect("read Genres").len(), count);
    let tracks = (0..TRACKS)
        .filter(|index| has_track(slot, source, *index))
        .map(|index| {
            let track = loaded
                .track(&track_id(slot, index))
                .expect("read loaded Track")
                .expect("fixture Track survives");
            let album = loaded
                .album(track.album_id.as_ref().expect("fixture Album ID"))
                .expect("read derived Album")
                .expect("fixture Album survives");
            let artist = loaded
                .artist(track.primary_artist_id().expect("fixture Artist ID"))
                .expect("read derived Artist")
                .expect("fixture Artist survives");
            assert_forward_and_reverse(loaded, &track, slot, index);
            (
                track.id.to_string(),
                track.title.clone(),
                track.favorite,
                album.favorite,
                artist.favorite,
            )
        })
        .collect();
    let mut playlists = loaded
        .playlists()
        .expect("read loaded Playlists")
        .iter()
        .map(|summary| {
            let detail = loaded
                .playlist_detail(&summary.playlist.id)
                .expect("read Playlist detail")
                .expect("listed Playlist has detail");
            (
                summary.playlist.id.to_string(),
                summary.playlist.name.clone(),
                (0..detail.entries.len())
                    .map(|index| {
                        let entry = detail
                            .entries
                            .entry(index)
                            .expect("read Playlist occurrence")
                            .expect("listed Playlist occurrence");
                        (entry.occurrence_id, entry.track.id.to_string())
                    })
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    playlists.sort_by(|left, right| left.0.cmp(&right.0));
    (
        tracks,
        playlists,
        loaded
            .provider_freshness()
            .expect("read provider freshness"),
    )
}

fn assert_forward_and_reverse(loaded: &Arc<Library>, track: &Track, slot: Slot, index: usize) {
    assert_eq!(track.artist_credits(), [credit(slot, index)]);
    assert_eq!(track.album_artist_credits(), [credit(slot, index)]);
    assert_eq!(
        track.genre_names().collect::<Vec<_>>(),
        [format!("{} Genre {index}", slot.prefix())]
    );
    let expected = track.id.clone();
    assert_eq!(
        reverse_track_id(
            loaded
                .album_detail(track.album_id.as_ref().expect("fixture Album ID"), None)
                .expect("read Album reverse relationship")
                .expect("fixture Album detail")
                .tracks
        ),
        expected
    );
    assert_eq!(
        reverse_track_id(
            loaded
                .artist_track_detail(track.primary_artist_id().expect("fixture Artist ID"), None)
                .expect("read Artist reverse relationship")
                .expect("fixture Artist detail")
                .tracks
        ),
        track.id
    );
    assert_eq!(
        reverse_track_id(
            loaded
                .genre_detail(&genre_id(slot, index), None)
                .expect("read Genre reverse relationship")
                .expect("fixture Genre detail")
                .tracks
        ),
        track.id
    );
}

fn reverse_track_id(list: library::TrackList) -> TrackId {
    let tracks = list
        .materialize()
        .expect("materialize reverse relationship");
    assert_eq!(tracks.len(), 1);
    tracks[0].id.clone()
}

fn created_playlist_id(change: AcceptedLibraryChange) -> PlaylistId {
    let ids = change.playlists;
    assert_eq!(ids.len(), 1);
    ids.into_iter().next().expect("created local Playlist ID")
}

fn track(slot: Slot, source: &SourceModel, index: usize) -> Track {
    Track::new(TrackData {
        id: track_id(slot, index),
        album_id: Some(album_id(slot, index)),
        title: source.titles[index].clone(),
        artist: format!("{} Artist {index}", slot.prefix()),
        album: format!("{} Album {index}", slot.prefix()),
        album_artwork: None,
        year: 2024,
        release_date: Some("2024-01-01".to_string()),
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        duration_seconds: 180 + u32::try_from(index).expect("fixture index"),
        favorite: slot.is_remote() && source.favorites[0][index],
        disc_number: 1,
        track_number: u16::try_from(index + 1).expect("fixture track number"),
        image_ref: None,
        local_artwork: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        source_path: (slot == Slot::Local)
            .then(|| format!("/law/{}/track-{index}.flac", slot.prefix())),
        cue: None,
        source_format: Some("flac".to_string()),
        comment: None,
        skip_count: None,
        bpm: None,
        relations: TrackRelations {
            artists: vec![credit(slot, index)],
            album_artists: vec![credit(slot, index)],
            genres: vec![GenreCredit {
                id: genre_id(slot, index),
                name: format!("{} Genre {index}", slot.prefix()),
            }],
            moods: Vec::new(),
            music_folders: Vec::new(),
        },
    })
}

fn album(slot: Slot, source: &SourceModel, index: usize) -> Album {
    Album {
        id: album_id(slot, index),
        title: format!("{} Album {index}", slot.prefix()),
        artist: format!("{} Artist {index}", slot.prefix()),
        year: 2024,
        release_date: Some("2024-01-01".to_string()),
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        favorite: slot.is_remote() && source.favorites[1][index],
        color_seed: 0,
        image_ref: None,
        local_artwork: None,
        release_types: Vec::new(),
        is_compilation: None,
        musicbrainz_album_id: None,
        musicbrainz_release_group_id: None,
        relations: AlbumRelations {
            album_artists: vec![credit(slot, index)],
            artists: vec![credit(slot, index)],
            genres: vec![GenreCredit {
                id: genre_id(slot, index),
                name: format!("{} Genre {index}", slot.prefix()),
            }],
        },
    }
}

fn artist(slot: Slot, source: &SourceModel, index: usize) -> Artist {
    Artist {
        id: artist_id(slot, index),
        name: format!("{} Artist {index}", slot.prefix()),
        favorite: slot.is_remote() && source.favorites[2][index],
        last_played: None,
        play_count: None,
        user_rating: None,
        musicbrainz_artist_id: None,
        image_ref: None,
        local_artwork: None,
    }
}

fn genre(slot: Slot, index: usize) -> Genre {
    Genre {
        id: genre_id(slot, index),
        name: format!("{} Genre {index}", slot.prefix()),
        image_ref: None,
    }
}

fn remote_playlist(slot: Slot, source: &SourceModel) -> PlaylistSnapshot {
    PlaylistSnapshot {
        playlist: Playlist {
            id: remote_playlist_id(slot),
            name: format!("{} Remote Playlist", slot.prefix()),
            image_ref: None,
        },
        entries: source
            .remote_entries
            .iter()
            .enumerate()
            .map(|(position, track)| PlaylistEntry {
                occurrence_id: format!("{}:remote:{position}", slot.prefix()),
                track_id: track_id(slot, *track),
            })
            .collect(),
    }
}

fn track_id(slot: Slot, index: usize) -> TrackId {
    TrackId::new(format!("{}:track:shared-{index}", slot.prefix()))
}

fn album_id(slot: Slot, index: usize) -> AlbumId {
    AlbumId::new(format!("{}:album:shared-{index}", slot.prefix()))
}

fn artist_id(slot: Slot, index: usize) -> ArtistId {
    ArtistId::new(format!("{}:artist:shared-{index}", slot.prefix()))
}

fn genre_id(slot: Slot, index: usize) -> GenreId {
    GenreId::new(format!("{}:genre:shared-{index}", slot.prefix()))
}

fn remote_playlist_id(slot: Slot) -> PlaylistId {
    PlaylistId::new(format!("{}:playlist:shared", slot.prefix()))
}

fn credit(slot: Slot, index: usize) -> ArtistCredit {
    ArtistCredit {
        id: artist_id(slot, index),
        name: format!("{} Artist {index}", slot.prefix()),
        musicbrainz_artist_id: None,
    }
}

fn has_track(slot: Slot, source: &SourceModel, index: usize) -> bool {
    slot.is_remote() || source.local_tracks[index]
}

fn source_item(slot: Slot, index: usize) -> bool {
    slot.is_remote() || index == 0
}
