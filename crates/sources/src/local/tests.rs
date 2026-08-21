use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use library::{
    CandidateBatch, CandidateFinish, CandidateHeader, HomeFacts, Libraries, LocalFile,
    LocalFileState, SourceId, Track, TrackSort,
};
use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::id3::v2::Id3v2Tag;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};

use super::*;
use crate::source::{BatchEmitter, SourceReadProgress, SourceReadStage};

#[derive(Default)]
struct ScanFacts {
    batches: Vec<CandidateBatch>,
    progress: Vec<SourceReadProgress>,
}

impl ScanFacts {
    fn albums(&self) -> Vec<library::Album> {
        self.batches
            .iter()
            .filter_map(|batch| match batch {
                CandidateBatch::Albums(values) => Some(values.as_slice()),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect()
    }

    fn tracks(&self) -> Vec<Track> {
        self.batches
            .iter()
            .filter_map(|batch| match batch {
                CandidateBatch::Tracks(values) => Some(values.as_slice()),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect()
    }

    fn artists(&self) -> Vec<library::Artist> {
        self.batches
            .iter()
            .filter_map(|batch| match batch {
                CandidateBatch::Artists(values) => Some(values.as_slice()),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect()
    }

    fn files(&self) -> Vec<LocalFile> {
        self.batches
            .iter()
            .filter_map(|batch| match batch {
                CandidateBatch::LocalFiles(values) => Some(values.as_slice()),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect()
    }
}

fn complete_scan(source: &LocalSource) -> ScanFacts {
    let (batches, receiver) = async_channel::unbounded();
    let progress = std::sync::Mutex::new(Vec::new());
    let emitter = BatchEmitter::new(batches);
    source
        .read_facts(
            &emitter,
            &|value| progress.lock().expect("progress lock").push(value),
            &|| false,
        )
        .expect("complete Local scan");
    drop(emitter);
    ScanFacts {
        batches: std::iter::from_fn(|| receiver.try_recv().ok()).collect(),
        progress: progress.into_inner().expect("progress lock"),
    }
}

fn exact_replacement(
    source: &LocalSource,
    loaded: &library::Library,
    path: PathBuf,
    observed_at: i64,
) -> library::LocalComponentReplacement {
    source
        .prepare_change(
            loaded,
            crate::ObservedSourceChange::LocalPaths(BTreeSet::from([path])),
            observed_at,
            &|_| {},
            &|| false,
        )
        .expect("prepare exact Local change")
        .expect("changed Local file")
}

#[test]
fn complete_scan_reads_picard_album_classification() {
    let root = tempfile::tempdir().expect("Local root");
    write_tagged_release_wav(
        &root.path().join("Track.wav"),
        "Track",
        " Album ; Compilation; Live; album ",
        None,
    )
    .expect("write tagged WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");

    let albums = complete_scan(&source).albums();

    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].release_types, ["album", "compilation", "live"]);
    assert_eq!(albums[0].is_compilation, Some(true));
}

#[test]
fn album_classification_combines_track_tags_in_stable_order() {
    let root = tempfile::tempdir().expect("Local root");
    write_tagged_release_wav(
        &root.path().join("First.wav"),
        "First",
        "Single; Live",
        Some(false),
    )
    .expect("write first tagged WAV");
    write_tagged_release_wav(
        &root.path().join("Second.wav"),
        "Second",
        "EP; single",
        Some(true),
    )
    .expect("write second tagged WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");

    let albums = complete_scan(&source).albums();

    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].release_types, ["ep", "live", "single"]);
    assert_eq!(albums[0].is_compilation, Some(true));
}

#[test]
fn metadata_availability_uses_accepted_paths_and_registered_lofty_writers() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let wav = root_path.join("editable.wav");
    write_tagged_wav(&wav, "Editable", "Artist", "Album", 1).expect("write editable WAV");
    let source = LocalSource::from_roots(vec![root_path.clone()]).expect("open Local source");
    let track = complete_scan(&source)
        .tracks()
        .into_iter()
        .next()
        .expect("scanned WAV Track");

    let subject = library::MetadataSubject::track(track.clone());
    let draft = source
        .read_metadata(&subject)
        .expect("read Lofty-backed WAV metadata");
    assert!(draft.editing.includes(library::MetadataField::Title));
    assert!(source.metadata_entry_available(subject.item()));
    assert!(super::metadata::mapped_editing_available(
        &library::MetadataItem::Track(track.clone())
    ));

    for extension in ["mka", "wma"] {
        let path = root_path.join(format!("read-only.{extension}"));
        fs::write(&path, []).expect("write discoverer-backed format");
        let mut discoverer_track = track.clone();
        discoverer_track.make_mut().source_path = Some(path.to_string_lossy().into_owned());
        discoverer_track.make_mut().source_format = Some(extension.to_string());
        assert!(matches!(
            source.read_metadata(&library::MetadataSubject::track(discoverer_track.clone())),
            Err(library::MetadataError::Unavailable)
        ));
        assert!(
            !source
                .metadata_entry_available(&library::MetadataItem::Track(discoverer_track.clone()))
        );
        assert!(!super::metadata::mapped_editing_available(
            &library::MetadataItem::Track(discoverer_track)
        ));
    }

    let mut cue_track = track.clone();
    cue_track.make_mut().cue = Some(library::CueSegment {
        cue_path: root_path.join("album.cue").to_string_lossy().into_owned(),
        start_millis: 0,
        end_millis: 1_000,
    });
    assert!(matches!(
        source.read_metadata(&library::MetadataSubject::track(cue_track.clone())),
        Err(library::MetadataError::Unavailable)
    ));
    assert!(!source.metadata_entry_available(&library::MetadataItem::Track(cue_track.clone())));
    assert!(!super::metadata::mapped_editing_available(
        &library::MetadataItem::Track(cue_track)
    ));

    let outside = tempfile::tempdir().expect("outside directory");
    let outside_path = outside.path().join("outside.wav");
    write_tagged_wav(&outside_path, "Outside", "Artist", "Album", 1).expect("write outside WAV");
    let mut outside_track = track;
    outside_track.make_mut().source_path = Some(outside_path.to_string_lossy().into_owned());
    assert!(matches!(
        source.read_metadata(&library::MetadataSubject::track(outside_track.clone())),
        Err(library::MetadataError::Unavailable)
    ));
    assert!(!source.metadata_entry_available(&library::MetadataItem::Track(outside_track.clone())));

    let mut non_normal_track = outside_track.clone();
    let separator = std::path::MAIN_SEPARATOR;
    let mut non_normal_path = root_path.as_os_str().to_os_string();
    non_normal_path.push(format!(
        "{separator}nested{separator}..{separator}editable.wav"
    ));
    non_normal_track.make_mut().source_path = Some(non_normal_path.to_string_lossy().into_owned());
    assert!(!source.metadata_entry_available(&library::MetadataItem::Track(non_normal_track)));

    #[cfg(unix)]
    {
        let link = root_path.join("outside-link.wav");
        std::os::unix::fs::symlink(&outside_path, &link).expect("link outside WAV");
        outside_track.make_mut().source_path = Some(link.to_string_lossy().into_owned());
        assert!(matches!(
            source.read_metadata(&library::MetadataSubject::track(outside_track.clone())),
            Err(library::MetadataError::Unavailable)
        ));
        assert!(
            source.metadata_entry_available(&library::MetadataItem::Track(outside_track.clone()))
        );

        let link = root_path.join("inside-link.wav");
        std::os::unix::fs::symlink(&wav, &link).expect("link inside WAV");
        outside_track.make_mut().source_path = Some(link.to_string_lossy().into_owned());
        assert!(matches!(
            source.read_metadata(&library::MetadataSubject::track(outside_track.clone())),
            Err(library::MetadataError::Unavailable)
        ));
        assert!(source.metadata_entry_available(&library::MetadataItem::Track(outside_track)));
    }
}

#[test]
fn mapped_metadata_reads_only_the_exact_projected_file() {
    let root = tempfile::tempdir().expect("mapped metadata root");
    let actual_root = root.path().join("actual");
    fs::create_dir(&actual_root).expect("create actual mapped metadata root");
    #[cfg(unix)]
    let mapping_root = {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&actual_root, &alias).expect("link mapped metadata root");
        alias
    };
    #[cfg(not(unix))]
    let mapping_root = actual_root.clone();
    let path = actual_root.join("Artist/Track.wav");
    fs::create_dir_all(path.parent().expect("mapped Track parent"))
        .expect("create mapped Track parent");
    write_tagged_wav(&path, "Track", "Artist", "Album", 1).expect("write mapped WAV");
    let source = LocalSource::from_roots(vec![actual_root.clone()]).expect("open Local source");
    let mut track = complete_scan(&source)
        .tracks()
        .into_iter()
        .next()
        .expect("scanned mapped Track");
    track.make_mut().id = library::TrackId::new("navidrome:track:mapped");
    track.make_mut().source_path = Some("/music/Artist/Track.wav".to_string());
    let mut missing = track.clone();
    missing.make_mut().id = library::TrackId::new("navidrome:track:missing");
    missing.make_mut().source_path = Some("/music/Artist/Missing.wav".to_string());
    #[cfg(unix)]
    let _outside = {
        let outside = tempfile::tempdir().expect("outside mapped metadata root");
        let outside_path = outside.path().join("Outside.wav");
        write_tagged_wav(&outside_path, "Outside", "Artist", "Album", 1)
            .expect("write outside mapped WAV");
        std::os::unix::fs::symlink(&outside_path, actual_root.join("Artist/Missing.wav"))
            .expect("link outside mapped WAV");
        outside
    };

    let store = tempfile::tempdir().expect("mapped metadata Store");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new("navidrome:server:mapped"),
            input_digest: [12; 32],
        })
        .expect("begin mapped candidate");
    candidate
        .write(CandidateBatch::Tracks(vec![track, missing]))
        .expect("write mapped Tracks");
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept mapped library");
    let mapping = library::LocalAccessMapping {
        root_path: mapping_root,
        server_prefix: Some("/music".to_string()),
        local_prefix: None,
    };
    let (subject, targets) = accepted
        .library
        .metadata_subject_with_local_access(
            &library::MetadataItemId::Track(library::TrackId::new("navidrome:track:mapped")),
            Some(&mapping),
        )
        .expect("project mapped metadata")
        .expect("mapped Track");

    let draft = read_mapped_metadata(&subject, &targets).expect("read exact mapped metadata");

    assert_eq!(draft.values.title, "Track");
    assert!(
        accepted
            .library
            .local_access_files()
            .expect("read Local access facts")
            .is_empty(),
        "an exact mapped read must not need a whole-folder scan"
    );
    let (missing, targets) = accepted
        .library
        .metadata_subject_with_local_access(
            &library::MetadataItemId::Track(library::TrackId::new("navidrome:track:missing")),
            Some(&mapping),
        )
        .expect("project missing metadata")
        .expect("missing Track");
    assert!(matches!(
        read_mapped_metadata(&missing, &targets),
        Err(library::MetadataError::Unavailable)
    ));
}

#[test]
fn metadata_availability_does_not_probe_an_unavailable_accepted_root() {
    let parent = tempfile::tempdir().expect("temporary parent");
    let root = parent.path().join("Music");
    fs::create_dir(&root).expect("create Local root");
    let path = root.join("Track.wav");
    write_tagged_wav(&path, "Track", "Artist", "Album", 1).expect("write tagged WAV");
    let source = LocalSource::from_roots(vec![root.clone()]).expect("open Local source");
    let track = complete_scan(&source)
        .tracks()
        .into_iter()
        .next()
        .expect("scanned Track");
    let subject = library::MetadataSubject::track(track);

    assert!(source.metadata_entry_available(subject.item()));
    fs::rename(&root, parent.path().join("Unavailable")).expect("make Local root unavailable");

    assert!(source.metadata_entry_available(subject.item()));
    assert!(matches!(
        source.read_metadata(&subject),
        Err(library::MetadataError::Unavailable)
    ));
}

#[test]
fn aggregate_metadata_read_requires_every_backing_track() {
    let root = tempfile::tempdir().expect("Local root");
    let first = root.path().join("First.wav");
    let second = root.path().join("Second.wav");
    write_tagged_wav_fields(&first, "First", "Artist", "Album", 1, 1).expect("write first WAV");
    write_tagged_wav_fields(&second, "Second", "Artist", "Album", 1, 2).expect("write second WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let mut facts = complete_scan(&source);
    let album_id = facts.albums().into_iter().next().expect("scanned Album").id;
    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [9; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches.clone() {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");
    let album = accepted
        .library
        .album(&album_id)
        .expect("read accepted Album")
        .expect("accepted Album");
    let subject = library::MetadataSubject::aggregate(
        library::MetadataItem::Album((*album).clone()),
        accepted.library.album_track_selection(&album_id, None),
    );
    assert!(source.metadata_entry_available(subject.item()));
    source
        .read_metadata(&subject)
        .expect("read metadata from every supported backing Track");

    let unsupported = root.path().join("Second.wma");
    fs::write(&unsupported, []).expect("write unsupported backing file");
    for batch in &mut facts.batches {
        if let CandidateBatch::Tracks(tracks) = batch {
            tracks[1].make_mut().source_path = Some(unsupported.to_string_lossy().into_owned());
        }
    }
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new("local:unsupported"),
            input_digest: [10; 32],
        })
        .expect("begin unsupported Local candidate");
    for batch in facts.batches {
        candidate
            .write(batch)
            .expect("write unsupported Local facts");
    }
    let unsupported = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 2,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept unsupported Local library");
    let album = unsupported
        .library
        .album(&album_id)
        .expect("read unsupported Album")
        .expect("unsupported Album");
    let subject = library::MetadataSubject::aggregate(
        library::MetadataItem::Album((*album).clone()),
        unsupported.library.album_track_selection(&album_id, None),
    );
    assert!(source.metadata_entry_available(subject.item()));
    assert!(matches!(
        source.read_metadata(&subject),
        Err(library::MetadataError::Unavailable)
    ));
}

#[test]
fn metadata_write_preserves_unrelated_tags_and_artwork() {
    let root = tempfile::tempdir().expect("Local root");
    let path = root.path().join("Tagged.wav");
    write_complete_tagged_wav(&path).expect("write tagged WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let track = complete_scan(&source)
        .tracks()
        .into_iter()
        .next()
        .expect("scanned Track");
    let subject = library::MetadataSubject::track(track.clone());
    let draft = source.read_metadata(&subject).expect("read metadata draft");
    source
        .write_metadata(
            &subject,
            &library::MetadataEdit {
                item_id: library::MetadataItemId::Track(track.id.clone()),
                revision: draft.revision,
                application: None,
                changes: vec![
                    library::MetadataChange::Title("Updated title".to_string()),
                    library::MetadataChange::SortTitle(Some("Title, Updated".to_string())),
                    library::MetadataChange::Comment(Some("Updated comment".to_string())),
                    library::MetadataChange::Year(Some(2026)),
                    library::MetadataChange::MusicBrainzRecordingId(Some(
                        "01234567-89ab-cdef-0123-456789abcdef".to_string(),
                    )),
                    library::MetadataChange::MusicBrainzReleaseGroupId(Some(
                        "11234567-89ab-cdef-0123-456789abcdef".to_string(),
                    )),
                ],
            },
        )
        .expect("write metadata");

    let writer = super::lofty_metadata::MetadataWriter::for_path(&path).expect("WAV writer");
    let tagged = super::lofty_metadata::read_lofty_for_edit(&path, writer.file_type())
        .expect("read written WAV")
        .expect("matching WAV contents");
    let tag = tagged.primary_tag().expect("written primary tag");
    assert_eq!(tag.title().as_deref(), Some("Updated title"));
    assert_eq!(
        tag.get_string(ItemKey::TrackTitleSortOrder),
        Some("Title, Updated")
    );
    assert_eq!(tag.comment().as_deref(), Some("Updated comment"));
    assert_eq!(tag.date().map(|date| date.year), Some(2026));
    assert_eq!(
        tag.get_string(ItemKey::MusicBrainzRecordingId),
        Some("01234567-89ab-cdef-0123-456789abcdef")
    );
    assert_eq!(
        tag.get_string(ItemKey::MusicBrainzReleaseId),
        Some("release-id")
    );
    assert_eq!(
        tag.get_string(ItemKey::MusicBrainzReleaseGroupId),
        Some("11234567-89ab-cdef-0123-456789abcdef")
    );
    assert_eq!(tag.pictures().len(), 1);
    assert_eq!(tag.pictures()[0].data(), TEST_PNG);
}

#[cfg(unix)]
#[test]
fn opening_metadata_does_not_require_write_permission() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("Local root");
    let path = root.path().join("Read only.wav");
    write_tagged_wav(&path, "Read only", "Artist", "Album", 1).expect("write tagged WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let track = complete_scan(&source)
        .tracks()
        .into_iter()
        .next()
        .expect("scanned Track");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444))
        .expect("make metadata file read only");

    let read = source.read_metadata(&library::MetadataSubject::track(track));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
        .expect("restore metadata file permissions");
    read.expect("read metadata without probing write access");
}

#[test]
fn metadata_conflict_keeps_the_concurrent_file_change() {
    let root = tempfile::tempdir().expect("Local root");
    let path = root.path().join("Conflict.wav");
    write_tagged_wav(&path, "Before", "Artist", "Album", 1).expect("write tagged WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let track = complete_scan(&source)
        .tracks()
        .into_iter()
        .next()
        .expect("scanned Track");
    let target = super::metadata::target(source.roots(), &track).expect("writable metadata target");
    let draft = super::metadata::read(&target, &track).expect("read metadata draft");
    let edit = library::MetadataEdit {
        item_id: library::MetadataItemId::Track(track.id.clone()),
        revision: draft.revision,
        application: None,
        changes: vec![library::MetadataChange::Title("Editor title".to_string())],
    };
    let concurrent = fs::read(&path)
        .expect("read original")
        .into_iter()
        .chain([1, 2, 3, 4])
        .collect::<Vec<_>>();

    let error = super::metadata::write_with_test_hook(&target, &track, &edit, |_| {
        fs::write(&path, &concurrent)
            .map_err(|error| library::MetadataError::Write(error.to_string()))
    })
    .expect_err("concurrent change must conflict");

    assert_eq!(error, library::MetadataError::Conflict);
    assert_eq!(fs::read(&path).expect("read conflicted file"), concurrent);
}

#[test]
fn metadata_failure_before_replace_leaves_the_original_unchanged() {
    let root = tempfile::tempdir().expect("Local root");
    let path = root.path().join("Failure.wav");
    write_tagged_wav(&path, "Before", "Artist", "Album", 1).expect("write tagged WAV");
    let original = fs::read(&path).expect("read original");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let track = complete_scan(&source)
        .tracks()
        .into_iter()
        .next()
        .expect("scanned Track");
    let target = super::metadata::target(source.roots(), &track).expect("writable metadata target");
    let draft = super::metadata::read(&target, &track).expect("read metadata draft");
    let edit = library::MetadataEdit {
        item_id: library::MetadataItemId::Track(track.id.clone()),
        revision: draft.revision,
        application: None,
        changes: vec![library::MetadataChange::Title("Editor title".to_string())],
    };

    let error = super::metadata::write_with_test_hook(&target, &track, &edit, |_| {
        Err(library::MetadataError::Write(
            "injected failure".to_string(),
        ))
    })
    .expect_err("injected failure");

    assert_eq!(
        error,
        library::MetadataError::Write("injected failure".to_string())
    );
    assert_eq!(
        fs::read(&path).expect("read original after failure"),
        original
    );
}

#[test]
fn album_metadata_edit_prepares_every_track_and_preserves_track_tags() {
    let root = tempfile::tempdir().expect("Local root");
    let first = root.path().join("First.wav");
    let second = root.path().join("Second.wav");
    write_tagged_wav_fields(&first, "First track", "Artist", "Album", 1, 1)
        .expect("write first WAV");
    write_tagged_wav_fields(&second, "Second track", "Artist", "Album", 1, 2)
        .expect("write second WAV");
    set_album_test_tags(&first, "Artist", 2001, "Electronic").expect("tag first WAV");
    set_album_test_tags(&second, "Artist", 2002, "Ambient").expect("tag second WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let facts = complete_scan(&source);
    let album = facts.albums().into_iter().next().expect("scanned Album");
    let tracks = facts.tracks();
    let item = library::MetadataItem::Album(album.clone());
    let draft = super::metadata::read_aggregate_with_tracks(source.roots(), &item, tracks.clone())
        .expect("read aggregate album metadata");
    assert_eq!(draft.scope, library::MetadataScope::Tracks(2));
    assert!(draft.mixed_fields.contains(&library::MetadataField::Year));
    assert!(draft.mixed_fields.contains(&library::MetadataField::Genre));

    super::metadata::write_aggregate_with_test_hook(
        source.roots(),
        &item,
        tracks,
        &library::MetadataEdit {
            item_id: library::MetadataItemId::Album(album.id),
            revision: draft.revision,
            application: None,
            changes: vec![
                library::MetadataChange::Title("Renamed album".to_string()),
                library::MetadataChange::Year(Some(2024)),
                library::MetadataChange::Genre(Some("Jazz".to_string())),
            ],
        },
        |_| Ok(()),
    )
    .expect("write aggregate album metadata");

    for (path, title) in [(&first, "First track"), (&second, "Second track")] {
        let tagged = Probe::open(path)
            .expect("open written WAV")
            .read()
            .expect("read written WAV");
        let tag = tagged.primary_tag().expect("written tag");
        assert_eq!(tag.title().as_deref(), Some(title));
        assert_eq!(tag.album().as_deref(), Some("Renamed album"));
        assert_eq!(tag.date().map(|date| date.year), Some(2024));
        assert_eq!(tag.genre().as_deref(), Some("Jazz"));
    }
}

#[test]
fn artist_metadata_edit_replaces_only_the_selected_credit() {
    let root = tempfile::tempdir().expect("Local root");
    let first = root.path().join("First.wav");
    let second = root.path().join("Second.wav");
    for (path, title, track) in [(&first, "First", 1), (&second, "Second", 2)] {
        write_tagged_wav_fields(path, title, "Lead; Guest", "Album", 1, track)
            .expect("write multi-artist WAV");
        set_album_test_tags(path, "Lead", 2020, "Rock").expect("tag album artist");
    }
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let facts = complete_scan(&source);
    let artist = facts
        .artists()
        .into_iter()
        .find(|artist| artist.name == "Lead")
        .expect("Lead artist");
    let tracks = facts.tracks();
    let item = library::MetadataItem::Artist(artist.clone());
    let draft = super::metadata::read_aggregate_with_tracks(source.roots(), &item, tracks.clone())
        .expect("read aggregate artist metadata");
    assert!(draft.editing.includes(library::MetadataField::Title));
    assert!(
        !draft
            .editing
            .includes(library::MetadataField::MusicBrainzArtistId)
    );

    super::metadata::write_aggregate_with_test_hook(
        source.roots(),
        &item,
        tracks,
        &library::MetadataEdit {
            item_id: library::MetadataItemId::Artist(artist.id),
            revision: draft.revision,
            application: None,
            changes: vec![library::MetadataChange::Title("Renamed".to_string())],
        },
        |_| Ok(()),
    )
    .expect("write aggregate artist metadata");

    for path in [&first, &second] {
        let tagged = Probe::open(path)
            .expect("open written WAV")
            .read()
            .expect("read written WAV");
        let tag = tagged.primary_tag().expect("written tag");
        assert_eq!(tag.artist().as_deref(), Some("Renamed; Guest"));
        assert_eq!(tag.get_string(ItemKey::AlbumArtist), Some("Renamed"));
    }
}

#[test]
fn aggregate_commit_failure_restores_every_original_file() {
    let root = tempfile::tempdir().expect("Local root");
    let first = root.path().join("First.wav");
    let second = root.path().join("Second.wav");
    write_tagged_wav_fields(&first, "First", "Artist", "Album", 1, 1).expect("write first WAV");
    write_tagged_wav_fields(&second, "Second", "Artist", "Album", 1, 2).expect("write second WAV");
    let originals = [
        fs::read(&first).expect("read first original"),
        fs::read(&second).expect("read second original"),
    ];
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let facts = complete_scan(&source);
    let album = facts.albums().into_iter().next().expect("scanned Album");
    let tracks = facts.tracks();
    let item = library::MetadataItem::Album(album.clone());
    let draft = super::metadata::read_aggregate_with_tracks(source.roots(), &item, tracks.clone())
        .expect("read aggregate album metadata");

    let error = super::metadata::write_aggregate_with_test_hook(
        source.roots(),
        &item,
        tracks,
        &library::MetadataEdit {
            item_id: library::MetadataItemId::Album(album.id),
            revision: draft.revision,
            application: None,
            changes: vec![library::MetadataChange::Title("Renamed".to_string())],
        },
        |index| {
            (index == 0).then_some(()).ok_or_else(|| {
                library::MetadataError::Write("injected second-file failure".to_string())
            })
        },
    )
    .expect_err("second exchange must fail");

    assert_eq!(
        error,
        library::MetadataError::Write("injected second-file failure".to_string())
    );
    assert_eq!(fs::read(&first).expect("read restored first"), originals[0]);
    assert_eq!(
        fs::read(&second).expect("read untouched second"),
        originals[1]
    );
}

#[test]
fn complete_scan_maps_lofty_metadata_before_background_embedded_artwork() {
    let root = tempfile::tempdir().expect("Local root");
    let path = root.path().join("Tagged.wav");
    write_complete_tagged_wav(&path).expect("write complete tagged WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");

    let facts = complete_scan(&source);
    let tracks = facts.tracks();
    let [track] = tracks.as_slice() else {
        panic!("expected one mapped Track");
    };

    assert_eq!(track.title, "Track Title");
    assert_eq!(track.artist, "Track Artist One; Track Artist Two");
    assert_eq!(track.album, "Album Title");
    assert_eq!(track.year, 2024);
    assert_eq!(track.duration_seconds, 1);
    assert_eq!(track.disc_number, 2);
    assert_eq!(track.track_number, 7);
    assert_eq!(track.comment.as_deref(), Some("Track comment"));
    assert_eq!(track.bpm, Some(123));
    assert_eq!(
        track.musicbrainz_recording_id.as_deref(),
        Some("recording-id")
    );
    assert_eq!(
        track
            .relations
            .artists
            .iter()
            .map(|artist| (
                artist.name.as_str(),
                artist.musicbrainz_artist_id.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            ("Track Artist One", Some("artist-one-id")),
            ("Track Artist Two", Some("artist-two-id")),
        ]
    );
    assert_eq!(
        track
            .relations
            .album_artists
            .iter()
            .map(|artist| (
                artist.name.as_str(),
                artist.musicbrainz_artist_id.as_deref()
            ))
            .collect::<Vec<_>>(),
        [("Album Artist", Some("album-artist-id"))]
    );
    assert_eq!(
        track.genre_names().collect::<Vec<_>>(),
        ["Electronic", "Ambient"]
    );
    assert_eq!(track.mood_names().collect::<Vec<_>>(), ["Focused", "Calm"]);

    assert_eq!(track.local_artwork, None);

    let albums = facts.albums();
    let [album] = albums.as_slice() else {
        panic!("expected one mapped Album");
    };
    assert_eq!(album.artist, "Album Artist");
    assert_eq!(album.release_types, ["album", "live"]);
    assert_eq!(album.is_compilation, Some(false));
    assert_eq!(album.musicbrainz_album_id.as_deref(), Some("release-id"));
    assert_eq!(
        album.musicbrainz_release_group_id.as_deref(),
        Some("release-group-id")
    );
}

#[test]
fn local_access_reuses_unchanged_files_and_drops_deleted_files() {
    let root = tempfile::tempdir().expect("local access root");
    let path = root.path().join("No Tags.unknown");
    write_silent_wav(&path, 1).expect("write extensionless-capable WAV");

    let first =
        read_local_access(root.path(), &[], &|_| {}, &|| false).expect("read local access files");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].title, "No Tags");
    assert_eq!(first[0].artist, "Unknown Artist");

    let mut accepted = first;
    accepted[0].title = "Accepted without rereading tags".to_string();
    let unchanged = read_local_access(root.path(), &accepted, &|_| {}, &|| false)
        .expect("reuse unchanged local access file");
    assert_eq!(unchanged, accepted);

    write_silent_wav(&path, 2).expect("change local access file");
    let changed = read_local_access(root.path(), &unchanged, &|_| {}, &|| false)
        .expect("reread changed local access file");
    assert_eq!(changed[0].title, "No Tags");

    fs::remove_file(path).expect("remove local access file");
    let deleted = read_local_access(root.path(), &changed, &|_| {}, &|| false)
        .expect("rescan after deletion");
    assert!(deleted.is_empty());
}

#[test]
fn local_access_reads_only_match_fields_from_audio_files() {
    let root = tempfile::tempdir().expect("local access root");
    let path = root.path().join("Tagged.wav");
    write_tagged_wav_fields(&path, "Title", "Artist", "Album", 2, 7).expect("write tagged WAV");
    fs::write(root.path().join("Album.cue"), b"FILE \"Tagged.wav\" WAVE")
        .expect("write sibling CUE");
    fs::write(root.path().join("cover.png"), [1_u8, 2, 3, 4]).expect("write sibling image");
    let progress = std::sync::Mutex::new(Vec::new());

    let files = read_local_access(
        root.path(),
        &[],
        &|value| progress.lock().expect("progress lock").push(value),
        &|| false,
    )
    .expect("read local access fields");

    assert_eq!(files.len(), 1);
    let file = &files[0];
    assert_eq!(file.title, "Title");
    assert_eq!(file.artist, "Artist");
    assert_eq!(file.album, "Album");
    assert_eq!(file.disc_number, 2);
    assert_eq!(file.track_number, 7);
    assert_eq!(file.duration_seconds, 1);
    let progress = progress.into_inner().expect("progress lock");
    assert!(progress.iter().any(|progress| {
        progress.stage == SourceReadStage::Files
            && progress.completed == 1
            && progress.total == Some(1)
    }));
    assert!(progress.iter().any(|progress| {
        progress.stage == SourceReadStage::Tracks
            && progress.completed == 1
            && progress.total == Some(1)
    }));
}

#[test]
fn local_access_honors_cancellation_before_reading_tags() {
    let root = tempfile::tempdir().expect("local access root");
    write_silent_wav(&root.path().join("Track.wav"), 1).expect("write WAV");

    let error = read_local_access(root.path(), &[], &|_| {}, &|| true)
        .expect_err("cancel local access read");

    assert!(matches!(error, crate::SourceError::Cancelled));
}

#[test]
fn complete_scan_streams_roots_and_rejects_invalid_media() {
    let first = tempfile::tempdir().expect("first Local root");
    let nested = first.path().join("nested");
    let second = tempfile::tempdir().expect("second Local root");
    fs::create_dir_all(&nested).expect("create nested root");
    write_silent_wav(&nested.join("Good.wav"), 1).expect("write WAV");
    fs::write(second.path().join("Not Audio.mp3"), []).expect("write invalid candidate");
    let source = LocalSource::from_roots(vec![
        first.path().to_path_buf(),
        nested,
        second.path().to_path_buf(),
    ])
    .expect("open Local source");

    let facts = complete_scan(&source);
    let mut tracks = facts.tracks();
    tracks.sort_by(|left, right| left.title.cmp(&right.title));

    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Good");
    let files = facts.files();
    assert_eq!(
        files
            .iter()
            .filter(|file| file.state == LocalFileState::Rejected)
            .count(),
        1
    );
    assert!(
        files
            .iter()
            .all(|file| file.state != LocalFileState::Unreadable)
    );
    assert!(facts.batches.iter().all(|batch| batch.len() <= 1_024));
    assert!(facts.progress.iter().any(|progress| {
        progress.stage == SourceReadStage::Finalizing
            && progress.completed == 1
            && progress.total == Some(1)
    }));
}

#[test]
fn complete_scan_discovers_content_without_an_audio_suffix() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let audio = root_path.join("Extensionless Track");
    let non_media = root_path.join("notes.pdf");
    let playlist = root_path.join("queue.m3u8");
    write_silent_wav(&audio, 1).expect("write extensionless WAV");
    fs::write(&non_media, b"not media").expect("write non-media candidate");
    fs::write(&playlist, audio.to_string_lossy().as_bytes()).expect("write ignored playlist");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");

    let facts = complete_scan(&source);
    let tracks = facts.tracks();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Extensionless Track");
    assert_eq!(
        tracks[0].source_path.as_deref(),
        Some(audio.to_string_lossy().as_ref())
    );
    assert_eq!(tracks[0].source_format.as_deref(), Some("wav"));
    assert!(source.metadata_entry_available(&library::MetadataItem::Track(tracks[0].clone())));
    let files = facts.files();
    assert!(files.iter().any(|file| {
        file.path == non_media.to_string_lossy()
            && file.kind == library::LocalFileKind::Media
            && file.state == LocalFileState::Rejected
    }));
    assert!(
        files
            .iter()
            .all(|file| file.path != playlist.to_string_lossy())
    );
}

#[test]
fn unchanged_rescan_returns_no_component_plan() {
    let root = tempfile::tempdir().expect("Local root");
    write_silent_wav(&root.path().join("Track.wav"), 1).expect("write WAV");
    let source =
        LocalSource::from_roots(vec![root.path().to_path_buf()]).expect("open Local source");
    let facts = complete_scan(&source);
    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let source_id = SourceId::new(LOCAL_LIBRARY_SOURCE_ID);
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id,
            input_digest: [1; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");

    let track_progress = Mutex::new(Vec::new());
    assert!(
        source
            .prepare_change(
                &accepted.library,
                crate::ObservedSourceChange::LocalRescan,
                2,
                &|progress| {
                    if progress.stage == SourceReadStage::Tracks {
                        track_progress
                            .lock()
                            .expect("Track progress lock")
                            .push(progress);
                    }
                },
                &|| false,
            )
            .expect("prepare Local source change")
            .is_none()
    );
    assert!(
        track_progress
            .into_inner()
            .expect("Track progress lock")
            .is_empty()
    );
}

#[test]
fn unchanged_file_identity_keeps_an_accepted_unreadable_file() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let path = root_path.join("Recovered.wav");
    write_silent_wav(&path, 1).expect("write WAV");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");
    let mut facts = complete_scan(&source);
    for batch in &mut facts.batches {
        if let CandidateBatch::LocalFiles(files) = batch {
            for file in files {
                if file.path == path.to_string_lossy() {
                    file.state = LocalFileState::Unreadable;
                }
            }
        }
    }

    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [6; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");

    assert!(
        source
            .prepare_change(
                &accepted.library,
                crate::ObservedSourceChange::LocalPaths(BTreeSet::from([path.clone()])),
                2,
                &|_| {},
                &|| false,
            )
            .expect("prepare unchanged unreadable file")
            .is_none()
    );
}

#[test]
fn exact_reread_failure_keeps_the_accepted_path_backed_track() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let path = root_path.join("Temporarily Unreadable.wav");
    write_silent_wav(&path, 1).expect("write WAV");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");
    let facts = complete_scan(&source);
    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [7; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");
    let track_id = accepted
        .library
        .track_list(None, TrackSort::Title, false)
        .expect("read accepted Tracks")
        .materialize()
        .expect("materialize accepted Tracks")
        .first()
        .expect("accepted Track")
        .id
        .clone();

    write_silent_wav(&path, 2).expect("change WAV");
    let check = scan::check_exact(source.roots(), BTreeSet::from([path.clone()]), &|| false)
        .expect("check changed file");
    fs::remove_file(&path).expect("make the checked file temporarily unreadable");
    let accepted_files = accepted
        .library
        .local_file_baseline(check.file_seeds())
        .expect("read changed file baseline");
    let change = scan::confirm_change(check, accepted_files, &|_| {}, &|| false)
        .expect("confirm changed file")
        .expect("changed Local file");
    let baseline = accepted
        .library
        .local_component_baseline(change.component_seeds())
        .expect("read changed component");
    let replacement =
        scan::complete_change(change, baseline, 2, &|| false).expect("complete unreadable file");

    assert!(replacement.removed_track_ids.is_empty());
    assert!(replacement.files.iter().any(|file| {
        file.path == path.to_string_lossy() && file.state == LocalFileState::Unreadable
    }));
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept unreadable observation");
    assert!(
        accepted
            .library
            .track(&track_id)
            .expect("read retained Track")
            .is_some()
    );
}

#[test]
fn exact_change_to_rejected_media_removes_the_old_track() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let path = root_path.join("Track.wav");
    write_silent_wav(&path, 1).expect("write WAV");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");
    let facts = complete_scan(&source);
    let track_id = facts.tracks()[0].id.clone();
    let store = tempfile::tempdir().expect("Store directory");
    let libraries = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = libraries
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [8; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");

    fs::write(&path, b"not media").expect("replace WAV with invalid media");
    let replacement = source
        .prepare_change(
            &accepted.library,
            crate::ObservedSourceChange::LocalPaths(BTreeSet::from([path.clone()])),
            2,
            &|_| {},
            &|| false,
        )
        .expect("prepare invalid-media change")
        .expect("changed Local component");

    assert_eq!(
        replacement.removed_track_ids,
        std::slice::from_ref(&track_id)
    );
    assert!(replacement.files.iter().any(|file| {
        file.path == path.to_string_lossy() && file.state == LocalFileState::Rejected
    }));
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept rejected media");
    assert!(
        accepted
            .library
            .track(&track_id)
            .expect("read removed Track")
            .is_none()
    );
}

#[test]
fn exact_file_change_does_not_parse_or_replace_flat_folder_siblings() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let paths = (0..128)
        .map(|index| root_path.join(format!("Track {index:03}.wav")))
        .collect::<Vec<_>>();
    for path in &paths {
        write_silent_wav(path, 1).expect("write WAV");
    }
    let changed_path = paths[64].clone();
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");
    let facts = complete_scan(&source);
    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [2; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");

    let unchanged_progress = Mutex::new(Vec::new());
    assert!(
        source
            .prepare_change(
                &accepted.library,
                crate::ObservedSourceChange::LocalPaths(BTreeSet::from([changed_path.clone()])),
                2,
                &|progress| {
                    if progress.stage == SourceReadStage::Tracks {
                        unchanged_progress
                            .lock()
                            .expect("unchanged progress lock")
                            .push(progress);
                    }
                },
                &|| false,
            )
            .expect("prepare unchanged file")
            .is_none()
    );
    assert!(
        unchanged_progress
            .into_inner()
            .expect("unchanged progress lock")
            .is_empty()
    );

    write_silent_wav(&changed_path, 2).expect("edit WAV");
    let changed_progress = Mutex::new(Vec::new());
    let replacement = source
        .prepare_change(
            &accepted.library,
            crate::ObservedSourceChange::LocalPaths(BTreeSet::from([changed_path])),
            2,
            &|progress| {
                if progress.stage == SourceReadStage::Tracks {
                    changed_progress
                        .lock()
                        .expect("changed progress lock")
                        .push(progress);
                }
            },
            &|| false,
        )
        .expect("prepare changed file")
        .expect("changed Local file");
    assert_eq!(replacement.tracks.len(), 1);
    assert_eq!(
        changed_progress
            .into_inner()
            .expect("changed progress lock")
            .into_iter()
            .filter_map(|progress| progress.total)
            .max(),
        Some(1)
    );
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept exact changed file");

    write_silent_wav(&paths[65], 2).expect("edit a second WAV");
    let rescan_progress = Mutex::new(Vec::new());
    let replacement = source
        .prepare_change(
            &accepted.library,
            crate::ObservedSourceChange::LocalRescan,
            3,
            &|progress| {
                if progress.stage == SourceReadStage::Tracks {
                    rescan_progress
                        .lock()
                        .expect("rescan progress lock")
                        .push(progress);
                }
            },
            &|| false,
        )
        .expect("prepare changed rescan")
        .expect("changed rescan");
    assert_eq!(replacement.tracks.len(), 1);
    assert_eq!(
        rescan_progress
            .into_inner()
            .expect("rescan progress lock")
            .into_iter()
            .filter_map(|progress| progress.total)
            .max(),
        Some(1)
    );
}

#[test]
fn valid_cue_projects_ordered_segments_and_suppresses_the_raw_track() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let audio = root_path.join("album.wav");
    let cue = root_path.join("album.cue");
    let cover = root_path.join("cover.png");
    write_silent_wav(&audio, 8).expect("write WAV");
    fs::write(&cover, [1_u8, 2, 3, 4]).expect("write CUE Album cover");
    let cue_text = r#"
PERFORMER "Cue Artist"
TITLE "Cue Album"
FILE "album.wav" WAVE
  TRACK 01 AUDIO
    TITLE "First"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Second"
    INDEX 01 00:04:00
"#;
    fs::write(&cue, cue_text).expect("write CUE");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");

    let facts = complete_scan(&source);
    let mut tracks = facts.tracks();
    tracks.sort_by_key(|track| track.track_number);

    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].title, "First");
    assert_eq!(tracks[1].title, "Second");
    assert_eq!(tracks[0].duration_seconds, 4);
    assert_eq!(tracks[1].duration_seconds, 4);
    assert_eq!(tracks[0].id, media::cue_track_id(&cue, 1));
    assert_eq!(tracks[1].id, media::cue_track_id(&cue, 2));
    assert!(tracks.iter().all(|track| {
        track.source_path.as_deref() == Some(audio.to_string_lossy().as_ref())
            && track
                .cue
                .as_ref()
                .is_some_and(|segment| segment.cue_path == cue.to_string_lossy().as_ref())
    }));
    let album = facts
        .batches
        .iter()
        .find_map(|batch| match batch {
            CandidateBatch::Albums(albums) => albums.first(),
            _ => None,
        })
        .expect("CUE Album");
    assert_eq!(
        album
            .local_artwork
            .as_ref()
            .expect("CUE Album artwork")
            .path(),
        cover.to_string_lossy().as_ref()
    );
    let cue_file = facts
        .files()
        .into_iter()
        .find(|file| file.path == cue.to_string_lossy())
        .expect("accepted CUE observation");
    assert_eq!(cue_file.state, LocalFileState::Accepted);
    assert_eq!(
        cue_file.dependencies,
        vec![audio.to_string_lossy().into_owned()]
    );

    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [4; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local CUE library");

    write_silent_wav(&audio, 10).expect("change CUE backing audio");
    let replacement = exact_replacement(&source, &accepted.library, audio.clone(), 2);
    assert_eq!(replacement.tracks.len(), 2);
    assert_eq!(
        replacement.albums[0]
            .local_artwork
            .as_ref()
            .expect("retained CUE Album artwork")
            .path(),
        cover.to_string_lossy().as_ref()
    );
    assert!(
        replacement
            .tracks
            .iter()
            .any(|track| track.title == "Second" && track.duration_seconds == 6)
    );
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept changed CUE backing");

    fs::remove_file(&cue).expect("remove CUE");
    let replacement = exact_replacement(&source, &accepted.library, cue.clone(), 3);
    assert_eq!(replacement.tracks.len(), 1);
    assert_eq!(replacement.removed_track_ids.len(), 2);
    assert!(replacement.tracks[0].cue.is_none());
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept removed CUE");

    fs::write(&cue, cue_text).expect("restore CUE");
    let replacement = exact_replacement(&source, &accepted.library, cue, 4);
    assert_eq!(replacement.tracks.len(), 2);
    assert_eq!(replacement.removed_track_ids.len(), 1);
    assert!(replacement.tracks.iter().all(|track| track.cue.is_some()));
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept restored CUE");
    let final_tracks = accepted
        .library
        .track_list(None, library::TrackSort::Title, false)
        .expect("read final CUE Tracks")
        .materialize()
        .expect("materialize final CUE Tracks");
    assert_eq!(final_tracks.len(), 2);
    assert!(final_tracks.iter().all(|track| track.cue.is_some()));
}

#[test]
fn arbitrary_part_directories_share_one_album_and_parent_cover() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let album = root_path.join("Artist").join("Album");
    let first_part = album.join("first-half");
    let second_part = album.join("blue-section");
    fs::create_dir_all(&first_part).expect("create first part");
    fs::create_dir_all(&second_part).expect("create second part");
    let first = first_part.join("One.wav");
    let second = second_part.join("Two.wav");
    write_tagged_wav(&first, "One", "Artist", "Album", 1).expect("write first Track");
    write_tagged_wav(&second, "Two", "Artist", "Album", 2).expect("write second Track");
    let cover = album.join("cover.png");
    fs::write(&cover, [1_u8, 2, 3, 4]).expect("write cover");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");

    let facts = complete_scan(&source);
    let tracks = facts.tracks();
    let album_ids = tracks
        .iter()
        .filter_map(|track| track.album_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let albums = facts
        .batches
        .iter()
        .filter_map(|batch| match batch {
            CandidateBatch::Albums(values) => Some(values.as_slice()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();

    assert_eq!(tracks.len(), 2);
    assert_eq!(album_ids.len(), 1);
    let album_id = album_ids.into_iter().next().expect("shared Album ID");
    assert_eq!(albums.len(), 1);
    let artwork = albums[0]
        .local_artwork
        .as_ref()
        .expect("shared Album artwork");
    assert_eq!(artwork.path(), cover.to_string_lossy().as_ref());
    assert_eq!(
        source
            .image_bytes(artwork)
            .expect("read shared artwork")
            .bytes,
        [1, 2, 3, 4]
    );

    let store = tempfile::tempdir().expect("Store directory");
    let store_path = store.path().join("library.db");
    let library = Libraries::open(&store_path).expect("open Library");
    let source_id = SourceId::new(LOCAL_LIBRARY_SOURCE_ID);
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: source_id.clone(),
            input_digest: [3; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");

    write_tagged_wav(&first, "One Retagged", "Artist", "Album", 1).expect("retag first Track");
    let replacement = source
        .prepare_change(
            &accepted.library,
            crate::ObservedSourceChange::LocalPaths(BTreeSet::from([first.clone()])),
            2,
            &|_| {},
            &|| false,
        )
        .expect("prepare retagged Track")
        .expect("retagged Local Track");
    assert_eq!(replacement.tracks.len(), 1);
    assert_eq!(
        replacement.albums[0]
            .local_artwork
            .as_ref()
            .expect("retained parent artwork")
            .path(),
        cover.to_string_lossy().as_ref()
    );
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept retagged Track");
    let detail = accepted
        .library
        .album_detail(&album_id, None)
        .expect("read shared Album")
        .expect("shared Album");
    assert_eq!(detail.tracks.len(), 2);
    let detail_tracks = detail.tracks.materialize().expect("read Album Tracks");
    assert!(
        detail_tracks
            .iter()
            .any(|track| track.title == "One Retagged")
    );
    assert!(detail_tracks.iter().any(|track| track.title == "Two"));

    fs::remove_dir_all(&second_part).expect("remove second part directory");
    let replacement = exact_replacement(&source, &accepted.library, second_part.clone(), 3);
    assert_eq!(replacement.removed_track_ids.len(), 1);
    assert_eq!(
        replacement.albums[0]
            .local_artwork
            .as_ref()
            .expect("parent artwork after directory removal")
            .path(),
        cover.to_string_lossy().as_ref()
    );
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept removed part directory");
    let detail = accepted
        .library
        .album_detail(&album_id, None)
        .expect("read Album after directory removal")
        .expect("Album after directory removal");
    assert_eq!(detail.tracks.len(), 1);

    fs::create_dir(&second_part).expect("restore second part directory");
    write_tagged_wav(&second, "Two", "Artist", "Album", 2).expect("restore second Track");
    let replacement = exact_replacement(&source, &accepted.library, second_part, 4);
    assert_eq!(replacement.tracks.len(), 1);
    assert_eq!(
        replacement.albums[0]
            .local_artwork
            .as_ref()
            .expect("parent artwork after directory addition")
            .path(),
        cover.to_string_lossy().as_ref()
    );
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept restored part directory");

    fs::remove_file(&cover).expect("remove parent cover");
    let replacement = exact_replacement(&source, &accepted.library, cover.clone(), 5);
    assert!(replacement.tracks.is_empty());
    assert_eq!(replacement.albums.len(), 1);
    assert!(replacement.albums[0].local_artwork.is_none());
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept removed parent cover");

    fs::write(&cover, [5_u8, 6, 7, 8]).expect("restore parent cover");
    let replacement = exact_replacement(&source, &accepted.library, cover.clone(), 6);
    assert!(replacement.tracks.is_empty());
    assert_eq!(
        replacement.albums[0]
            .local_artwork
            .as_ref()
            .expect("restored parent artwork")
            .path(),
        cover.to_string_lossy().as_ref()
    );
    accepted
        .library
        .accept_local_component(replacement)
        .expect("accept restored parent cover");

    drop(accepted);
    drop(library);
    let reopened_library = Libraries::open(store_path).expect("reopen Library");
    let reopened = reopened_library
        .load_source(&source_id)
        .expect("load Local source")
        .expect("Local source");
    assert_eq!(
        reopened
            .album_detail(&album_id, None)
            .expect("read reopened Album")
            .expect("reopened Album")
            .tracks
            .len(),
        2
    );

    write_tagged_wav(&first, "One After Reopen", "Artist", "Album", 1)
        .expect("retag Track after reopen");
    let replacement = exact_replacement(&source, &reopened, first, 7);
    assert_eq!(
        replacement.albums[0]
            .local_artwork
            .as_ref()
            .expect("parent artwork after reopen")
            .path(),
        cover.to_string_lossy().as_ref()
    );
    reopened
        .accept_local_component(replacement)
        .expect("accept Track edit after reopen");
    let detail = reopened
        .album_detail(&album_id, None)
        .expect("read Album after reopen edit")
        .expect("Album after reopen edit");
    assert!(
        detail
            .tracks
            .materialize()
            .expect("read Tracks after reopen edit")
            .iter()
            .any(|track| track.title == "One After Reopen")
    );
}

#[test]
fn new_cross_directory_album_uses_an_existing_parent_cover() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let album = root_path.join("Artist").join("Future Album");
    let first_part = album.join("arbitrary-a");
    let second_part = album.join("arbitrary-b");
    fs::create_dir_all(&first_part).expect("create first part");
    fs::create_dir_all(&second_part).expect("create second part");
    let cover = album.join("cover.png");
    fs::write(&cover, [1_u8, 2, 3, 4]).expect("write future cover");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");
    let facts = complete_scan(&source);
    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [5; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept empty future Album");

    let first = first_part.join("One.wav");
    let second = second_part.join("Two.wav");
    write_tagged_wav(&first, "One", "Artist", "Future Album", 1).expect("write first Track");
    write_tagged_wav(&second, "Two", "Artist", "Future Album", 2).expect("write second Track");
    let replacement = source
        .prepare_change(
            &accepted.library,
            crate::ObservedSourceChange::LocalPaths(BTreeSet::from([first, second])),
            2,
            &|_| {},
            &|| false,
        )
        .expect("prepare new Album")
        .expect("new Local Album");

    assert_eq!(replacement.tracks.len(), 2);
    assert_eq!(replacement.albums.len(), 1);
    assert_eq!(
        replacement.albums[0]
            .local_artwork
            .as_ref()
            .expect("new Album parent artwork")
            .path(),
        cover.to_string_lossy().as_ref()
    );
}

#[test]
fn artist_directory_image_does_not_become_album_art_after_an_exact_edit() {
    let root = tempfile::tempdir().expect("Local root");
    let root_path = fs::canonicalize(root.path()).expect("canonical Local root");
    let artist = root_path.join("Artist");
    let first_album = artist.join("First Album").join("part");
    let second_album = artist.join("Second Album").join("part");
    fs::create_dir_all(&first_album).expect("create first Album");
    fs::create_dir_all(&second_album).expect("create second Album");
    let first = first_album.join("One.wav");
    let second = second_album.join("Two.wav");
    write_tagged_wav(&first, "One", "Artist", "First Album", 1).expect("write first Track");
    write_tagged_wav(&second, "Two", "Artist", "Second Album", 1).expect("write second Track");
    fs::write(artist.join("folder.jpg"), [1_u8, 2, 3, 4]).expect("write artist image");
    let source = LocalSource::from_roots(vec![root_path]).expect("open Local source");
    let facts = complete_scan(&source);
    assert!(facts.batches.iter().all(|batch| match batch {
        CandidateBatch::Albums(albums) => albums.iter().all(|album| album.local_artwork.is_none()),
        _ => true,
    }));

    let store = tempfile::tempdir().expect("Store directory");
    let library = Libraries::open(store.path().join("library.db")).expect("open Library");
    let mut candidate = library
        .begin_source_candidate(CandidateHeader {
            source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
            input_digest: [7; 32],
        })
        .expect("begin Local candidate");
    for batch in facts.batches {
        candidate.write(batch).expect("write Local facts");
    }
    let accepted = candidate
        .finish(
            CandidateFinish {
                freshness: None,
                home: HomeFacts::RufinDefined,
                accepted_at: 1,
            },
            None,
        )
        .and_then(library::PreparedSourceCandidate::accept)
        .expect("accept Local library");

    write_tagged_wav(&first, "One Retagged", "Artist", "First Album", 1)
        .expect("retag first Track");
    let replacement = exact_replacement(&source, &accepted.library, first, 2);

    assert_eq!(replacement.albums.len(), 1);
    assert!(replacement.albums[0].local_artwork.is_none());
}

#[test]
fn source_input_rejects_missing_or_relative_roots_without_forgetting_saved_roots() {
    let relative = LocalSource::from_roots(vec![PathBuf::from("music")])
        .expect_err("relative setup root is not accepted");
    assert!(matches!(relative, crate::SourceError::Other(_)));

    let directory = tempfile::tempdir().expect("temporary missing root parent");
    let missing_root = directory.path().join("rufin-music");
    let missing = SourceConfiguration {
        source_id: SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
        kind: LOCAL_SOURCE_ID.to_string(),
        name: "Local".to_string(),
        provider_payload: LocalSourceConfig {
            roots: vec![missing_root.clone()],
        }
        .into_payload()
        .to_string(),
    };
    let opened = LocalSource::from_configuration(&missing).expect("open saved Local source");
    assert_eq!(opened.roots(), [missing_root]);
    let (batches, _receiver) = async_channel::unbounded();
    let emitter = BatchEmitter::new(batches);
    assert!(opened.read_facts(&emitter, &|_| {}, &|| false).is_err());
}

fn write_tagged_wav(
    path: &Path,
    title: &str,
    artist: &str,
    album: &str,
    disc: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    write_silent_wav(path, 1)?;
    let mut tagged = Probe::open(path)?.read()?;
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.to_string());
    tag.set_artist(artist.to_string());
    tag.set_album(album.to_string());
    tag.set_disk(disc);
    tagged.insert_tag(tag);
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn write_tagged_wav_fields(
    path: &Path,
    title: &str,
    artist: &str,
    album: &str,
    disc: u32,
    track: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    write_silent_wav(path, 1)?;
    let mut tagged = Probe::open(path)?.read()?;
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.to_string());
    tag.set_artist(artist.to_string());
    tag.set_album(album.to_string());
    tag.set_disk(disc);
    tag.set_track(track);
    tagged.insert_tag(tag);
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn set_album_test_tags(
    path: &Path,
    album_artist: &str,
    year: u16,
    genre: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tagged = Probe::open(path)?.read()?;
    let tag = tagged.primary_tag_mut().ok_or("missing primary tag")?;
    tag.insert_text(ItemKey::AlbumArtist, album_artist.to_string());
    let mut date = tag.date().unwrap_or_default();
    date.year = year;
    tag.set_date(date);
    tag.set_genre(genre.to_string());
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn write_tagged_release_wav(
    path: &Path,
    title: &str,
    release_types: &str,
    is_compilation: Option<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    write_silent_wav(path, 1)?;
    let mut tagged = Probe::open(path)?.read()?;
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title(title.to_string());
    tag.set_artist("Artist".to_string());
    tag.set_album("Album".to_string());
    if !tag.insert_text(ItemKey::MusicBrainzReleaseType, release_types.to_string()) {
        return Err("ID3 does not support the MusicBrainz release type tag".into());
    }
    if let Some(is_compilation) = is_compilation
        && !tag.insert_text(
            ItemKey::FlagCompilation,
            u8::from(is_compilation).to_string(),
        )
    {
        return Err("ID3 does not support the compilation flag".into());
    }
    tagged.insert_tag(tag);
    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

const TEST_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn write_complete_tagged_wav(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    write_silent_wav(path, 1)?;
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Track Title".to_string());
    tag.set_artist("Track Artist One; Track Artist Two".to_string());
    tag.set_album("Album Title".to_string());
    tag.set_track(7);
    tag.set_disk(2);
    for (key, value) in [
        (ItemKey::AlbumArtist, "Album Artist"),
        (ItemKey::RecordingDate, "2024-03-14"),
        (ItemKey::Genre, "Electronic; Ambient"),
        (ItemKey::Mood, "Focused; Calm"),
        (ItemKey::Comment, "Track comment"),
        (ItemKey::IntegerBpm, "123"),
        (ItemKey::MusicBrainzArtistId, "artist-one-id; artist-two-id"),
        (ItemKey::MusicBrainzReleaseArtistId, "album-artist-id"),
        (ItemKey::MusicBrainzReleaseType, "Album; Live"),
        (ItemKey::FlagCompilation, "0"),
    ] {
        if !tag.insert_text(key, value.to_string()) {
            return Err(format!("ID3 does not support {key:?}").into());
        }
    }
    tag.insert_unchecked(TagItem::new(
        ItemKey::MusicBrainzRecordingId,
        ItemValue::Text("recording-id".to_string()),
    ));
    tag.push_picture(
        Picture::unchecked(TEST_PNG.to_vec())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build(),
    );
    let mut tag = Id3v2Tag::from(tag);
    tag.insert_user_text("MusicBrainz Album Id".to_string(), "release-id".to_string());
    tag.insert_user_text(
        "MusicBrainz Release Group Id".to_string(),
        "release-group-id".to_string(),
    );
    tag.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn write_silent_wav(path: &Path, seconds: u32) -> std::io::Result<()> {
    let sample_rate = 8_000_u32;
    let bits_per_sample = 16_u16;
    let channels = 1_u16;
    let sample_count = sample_rate.saturating_mul(seconds);
    let data_len = sample_count * u32::from(channels) * u32::from(bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample / 8);
    let block_align = channels * (bits_per_sample / 8);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(bytes.len() + data_len as usize, 0);
    fs::write(path, bytes)
}
