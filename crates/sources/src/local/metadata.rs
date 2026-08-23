use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use library::{
    Artist, MetadataChange, MetadataDraft, MetadataEdit, MetadataEditing, MetadataError,
    MetadataField, MetadataItem, MetadataItemId, MetadataScope, MetadataSubject, MetadataValues,
    Track,
};
use lofty::config::WriteOptions;
use lofty::file::{TaggedFile, TaggedFileExt};
use lofty::id3::v2::Id3v2Tag;
use lofty::picture::Picture;
use lofty::prelude::*;
use lofty::tag::items::popularimeter::{Popularimeter, StarRating};
use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};

use super::lofty_metadata::{MetadataWriter, bpm_key, read_lofty_for_edit};

pub(super) struct MetadataTarget {
    pub(super) path: PathBuf,
    pub(super) writer: MetadataWriter,
}

struct SubjectTarget {
    target: MetadataTarget,
    track: Track,
}

const ALBUM_FIELDS: &[(MetadataField, ItemKey)] = &[
    (MetadataField::Title, ItemKey::AlbumTitle),
    (MetadataField::SortTitle, ItemKey::AlbumTitleSortOrder),
    (MetadataField::AlbumArtist, ItemKey::AlbumArtist),
    (MetadataField::Year, ItemKey::RecordingDate),
    (MetadataField::Genre, ItemKey::Genre),
    (
        MetadataField::MusicBrainzAlbumId,
        ItemKey::MusicBrainzReleaseId,
    ),
    (
        MetadataField::MusicBrainzReleaseGroupId,
        ItemKey::MusicBrainzReleaseGroupId,
    ),
];

const METADATA_REPLACEMENT_AVAILABLE: bool = cfg!(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows"
));

pub(super) fn mapped_editing_available(item: &MetadataItem) -> bool {
    if !METADATA_REPLACEMENT_AVAILABLE {
        return false;
    }
    match item {
        MetadataItem::Track(track) => {
            if track.cue.is_some() {
                return false;
            }
            track
                .source_format
                .as_deref()
                .and_then(MetadataWriter::for_source_format)
                .and_then(MetadataWriter::metadata_editing)
                .is_some()
        }
        MetadataItem::Album(_) | MetadataItem::Artist(_) => true,
    }
}

pub(super) fn entry_editing_available(roots: &[PathBuf], item: &MetadataItem) -> bool {
    if !METADATA_REPLACEMENT_AVAILABLE {
        return false;
    }
    match item {
        MetadataItem::Track(track) => accepted_target(roots, track)
            .and_then(|target| target.writer.metadata_editing())
            .is_some(),
        MetadataItem::Album(_) | MetadataItem::Artist(_) => true,
    }
}

#[cfg(test)]
pub(super) fn target(roots: &[PathBuf], track: &Track) -> Option<MetadataTarget> {
    let roots = canonical_roots(roots.iter().map(PathBuf::as_path))?;
    target_in_roots(&roots, track)
}

pub(super) fn read_subject(
    roots: &[PathBuf],
    subject: &MetadataSubject,
) -> Result<MetadataDraft, MetadataError> {
    ensure_metadata_replacement_available()?;
    let targets = native_subject_targets(roots, subject)?;
    read_subject_targets(subject.item(), &targets)
}

pub(super) fn read_mapped_subject(
    subject: &MetadataSubject,
    access: &[library::LocalAccessTarget],
) -> Result<MetadataDraft, MetadataError> {
    ensure_metadata_replacement_available()?;
    let targets = mapped_subject_targets(subject, access)?;
    read_subject_targets(subject.item(), &targets)
}

fn native_subject_targets(
    roots: &[PathBuf],
    subject: &MetadataSubject,
) -> Result<Vec<SubjectTarget>, MetadataError> {
    normalize_subject_targets(native_subject_target_candidates(roots, subject)?)
}

fn native_subject_target_candidates(
    roots: &[PathBuf],
    subject: &MetadataSubject,
) -> Result<Vec<SubjectTarget>, MetadataError> {
    let tracks = subject_tracks(subject)?;
    let roots =
        canonical_roots(roots.iter().map(PathBuf::as_path)).ok_or(MetadataError::Unavailable)?;
    let mut targets = Vec::with_capacity(tracks.len());
    for track in tracks {
        let target = target_in_roots(&roots, &track).ok_or(MetadataError::Unavailable)?;
        targets.push(SubjectTarget { target, track });
    }
    Ok(targets)
}

fn mapped_subject_targets(
    subject: &MetadataSubject,
    access: &[library::LocalAccessTarget],
) -> Result<Vec<SubjectTarget>, MetadataError> {
    normalize_subject_targets(mapped_subject_target_candidates(subject, access)?)
}

fn mapped_subject_target_candidates(
    subject: &MetadataSubject,
    access: &[library::LocalAccessTarget],
) -> Result<Vec<SubjectTarget>, MetadataError> {
    let tracks = subject_tracks(subject)?;
    if tracks.len() != access.len() {
        return Err(MetadataError::Unavailable);
    }
    let roots = canonical_roots(access.iter().map(library::LocalAccessTarget::root_path))
        .ok_or(MetadataError::Unavailable)?;
    let mut targets = Vec::with_capacity(tracks.len());
    for (track, access) in tracks.into_iter().zip(access) {
        let target =
            mapped_target_in_roots(&roots, access, &track).ok_or(MetadataError::Unavailable)?;
        targets.push(SubjectTarget { target, track });
    }
    Ok(targets)
}

fn subject_tracks(subject: &MetadataSubject) -> Result<Vec<Track>, MetadataError> {
    match subject.item() {
        MetadataItem::Track(track) => Ok(vec![track.clone()]),
        MetadataItem::Album(_) | MetadataItem::Artist(_) => subject
            .tracks()
            .cloned()
            .ok_or(MetadataError::Unavailable)?
            .prepare()
            .and_then(|tracks| tracks.materialize_owned())
            .map_err(|error| MetadataError::Write(error.to_string())),
    }
}

fn normalize_subject_targets(
    mut targets: Vec<SubjectTarget>,
) -> Result<Vec<SubjectTarget>, MetadataError> {
    if targets.is_empty() {
        return Err(MetadataError::Unavailable);
    }
    targets.sort_by(|left, right| left.target.path.cmp(&right.target.path));
    if targets
        .windows(2)
        .any(|pair| pair[0].target.path == pair[1].target.path)
    {
        return Err(MetadataError::Write(
            "More than one selected track resolves to the same metadata file.".to_string(),
        ));
    }
    Ok(targets)
}

fn read_subject_targets(
    item: &MetadataItem,
    targets: &[SubjectTarget],
) -> Result<MetadataDraft, MetadataError> {
    match item {
        MetadataItem::Track(track) => read(&targets[0].target, track),
        MetadataItem::Album(album) => read_album(album, targets),
        MetadataItem::Artist(artist) => read_artist(artist, targets),
    }
}

fn canonical_roots<'a>(roots: impl IntoIterator<Item = &'a Path>) -> Option<Vec<PathBuf>> {
    let mut canonical = roots
        .into_iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .collect::<Vec<_>>();
    canonical.sort();
    canonical.dedup();
    (!canonical.is_empty()).then_some(canonical)
}

fn target_in_roots(roots: &[PathBuf], track: &Track) -> Option<MetadataTarget> {
    if track.cue.is_some() {
        return None;
    }
    let source_path = Path::new(track.source_path.as_deref()?);
    target_path(roots, source_path)
}

fn accepted_target(roots: &[PathBuf], track: &Track) -> Option<MetadataTarget> {
    if track.cue.is_some() {
        return None;
    }
    let path = PathBuf::from(track.source_path.as_deref()?);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || !roots.iter().any(|root| path.starts_with(root))
    {
        return None;
    }
    let writer = track
        .source_format
        .as_deref()
        .and_then(MetadataWriter::for_source_format)?;
    Some(MetadataTarget { path, writer })
}

fn mapped_target_in_roots(
    roots: &[PathBuf],
    access: &library::LocalAccessTarget,
    track: &Track,
) -> Option<MetadataTarget> {
    if track.cue.is_some() {
        return None;
    }
    let root = fs::canonicalize(access.root_path()).ok()?;
    let path = access.path().strip_prefix(access.root_path()).map_or_else(
        |_| access.path().to_path_buf(),
        |relative| root.join(relative),
    );
    target_path(roots, &path)
}

fn target_path(roots: &[PathBuf], source_path: &Path) -> Option<MetadataTarget> {
    let path = fs::canonicalize(source_path).ok()?;
    if cfg!(not(target_os = "windows")) && source_path != path {
        return None;
    }
    if !roots.iter().any(|root| path.starts_with(root)) {
        return None;
    }
    let writer = MetadataWriter::for_path(&path)?;
    Some(MetadataTarget { path, writer })
}

pub(super) fn read(target: &MetadataTarget, track: &Track) -> Result<MetadataDraft, MetadataError> {
    let tagged = read_tagged(&target.path, target.writer)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    Ok(MetadataDraft {
        item_id: MetadataItemId::Track(track.id.clone()),
        editing: target
            .writer
            .metadata_editing()
            .ok_or(MetadataError::Unavailable)?,
        source_search: false,
        revision: Some(file_revision(&target.path)?),
        values: values(tag, track),
        scope: library::MetadataScope::Item,
        mixed_fields: Default::default(),
    })
}

fn read_album(
    album: &library::Album,
    targets: &[SubjectTarget],
) -> Result<MetadataDraft, MetadataError> {
    let editing = album_editing(targets).ok_or(MetadataError::Unavailable)?;
    let mut observed = Vec::with_capacity(targets.len());
    for target in targets {
        let tagged = read_tagged(&target.target.path, target.target.writer)?;
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        observed.push(album_values(tag, &target.track));
    }
    let values = MetadataValues {
        title: album.title.clone(),
        sort_title: common_optional(&observed, |values| values.sort_title.clone()),
        artist: clean(&album.artist),
        album_artist: {
            let names = album
                .relations
                .album_artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            clean(&names).or_else(|| clean(&album.artist))
        },
        year: (album.year > 0).then_some(album.year),
        genre: {
            let genres = album
                .relations
                .genres
                .iter()
                .map(|genre| genre.name.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            clean(&genres)
        },
        musicbrainz_album_id: album.musicbrainz_album_id.clone(),
        musicbrainz_release_group_id: album.musicbrainz_release_group_id.clone(),
        ..MetadataValues::default()
    };
    let mixed_fields = editing
        .fields()
        .iter()
        .copied()
        .filter(|field| field_is_mixed(*field, &values, &observed))
        .collect();
    Ok(MetadataDraft {
        item_id: MetadataItemId::Album(album.id.clone()),
        editing,
        source_search: false,
        revision: Some(subject_revision(targets)?),
        values,
        scope: MetadataScope::Tracks(targets.len()),
        mixed_fields,
    })
}

fn read_artist(artist: &Artist, targets: &[SubjectTarget]) -> Result<MetadataDraft, MetadataError> {
    let editing = artist_editing(artist, targets).ok_or(MetadataError::Unavailable)?;
    let mut observations = Vec::new();
    for target in targets {
        let tagged = read_tagged(&target.target.path, target.target.writer)?;
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        observations.extend(artist_observations(tag, &target.track, artist)?);
    }
    if observations.is_empty() {
        return Err(MetadataError::Unavailable);
    }
    let values = MetadataValues {
        title: artist.name.clone(),
        musicbrainz_artist_id: artist.musicbrainz_artist_id.clone(),
        ..MetadataValues::default()
    };
    let mixed_fields = editing
        .fields()
        .iter()
        .copied()
        .filter(|field| field_is_mixed(*field, &values, &observations))
        .collect();
    Ok(MetadataDraft {
        item_id: MetadataItemId::Artist(artist.id.clone()),
        editing,
        source_search: false,
        revision: Some(subject_revision(targets)?),
        values,
        scope: MetadataScope::Tracks(targets.len()),
        mixed_fields,
    })
}

fn album_editing(targets: &[SubjectTarget]) -> Option<MetadataEditing> {
    let fields = ALBUM_FIELDS
        .iter()
        .filter_map(|(field, key)| {
            targets
                .iter()
                .all(|target| target.target.writer.metadata_key_is_writable(*key))
                .then_some(*field)
        })
        .collect::<Vec<_>>();
    (!fields.is_empty()).then(|| MetadataEditing::new(fields))
}

fn artist_editing(artist: &Artist, targets: &[SubjectTarget]) -> Option<MetadataEditing> {
    let mut title = true;
    let mut musicbrainz_id = true;
    let mut found = false;
    for target in targets {
        let occurrences = artist_occurrences(&target.track, artist);
        if occurrences.is_empty() {
            return None;
        }
        found = true;
        for occurrence in occurrences {
            let (title_key, id_key) = match occurrence.scope {
                ArtistScope::Track => (ItemKey::TrackArtist, ItemKey::MusicBrainzArtistId),
                ArtistScope::Album => (ItemKey::AlbumArtist, ItemKey::MusicBrainzReleaseArtistId),
            };
            title &= target.target.writer.metadata_key_is_writable(title_key);
            musicbrainz_id &= occurrence.credit_count == 1
                && target.target.writer.metadata_key_is_writable(id_key);
        }
    }
    let mut fields = Vec::new();
    if found && title {
        fields.push(MetadataField::Title);
    }
    if found && musicbrainz_id {
        fields.push(MetadataField::MusicBrainzArtistId);
    }
    (!fields.is_empty()).then(|| MetadataEditing::new(fields))
}

fn album_values(tag: Option<&Tag>, track: &Track) -> MetadataValues {
    MetadataValues {
        title: tag
            .and_then(|tag| text(tag.album()))
            .unwrap_or_else(|| track.album.clone()),
        sort_title: tag
            .and_then(|tag| tag.get_string(ItemKey::AlbumTitleSortOrder))
            .and_then(clean),
        album_artist: tag
            .and_then(|tag| tag.get_string(ItemKey::AlbumArtist))
            .and_then(clean)
            .or_else(|| {
                let names = track
                    .album_artist_credits()
                    .iter()
                    .map(|artist| artist.name.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                clean(&names)
            }),
        year: tag
            .and_then(Tag::date)
            .map(|date| date.year)
            .filter(|year| *year > 0),
        genre: tag.and_then(|tag| text(tag.genre())),
        musicbrainz_album_id: tag
            .and_then(|tag| tag.get_string(ItemKey::MusicBrainzReleaseId))
            .and_then(clean),
        musicbrainz_release_group_id: tag
            .and_then(|tag| tag.get_string(ItemKey::MusicBrainzReleaseGroupId))
            .and_then(clean),
        ..MetadataValues::default()
    }
}

fn common_optional(
    values: &[MetadataValues],
    get: impl Fn(&MetadataValues) -> Option<String>,
) -> Option<String> {
    let first = values.first().and_then(&get);
    values
        .iter()
        .skip(1)
        .all(|value| get(value) == first)
        .then_some(first)
        .flatten()
}

fn field_is_mixed(
    field: MetadataField,
    canonical: &MetadataValues,
    observed: &[MetadataValues],
) -> bool {
    let Some(first) = observed.first() else {
        return false;
    };
    !metadata_field_matches(field, canonical, first)
        || observed
            .iter()
            .skip(1)
            .any(|value| !metadata_field_matches(field, first, value))
}

fn metadata_field_matches(
    field: MetadataField,
    left: &MetadataValues,
    right: &MetadataValues,
) -> bool {
    match field {
        MetadataField::Title => left.title == right.title,
        MetadataField::SortTitle => left.sort_title == right.sort_title,
        MetadataField::AlbumArtist => left.album_artist == right.album_artist,
        MetadataField::Year => left.year == right.year,
        MetadataField::Genre => left.genre == right.genre,
        MetadataField::MusicBrainzAlbumId => {
            left.musicbrainz_album_id == right.musicbrainz_album_id
        }
        MetadataField::MusicBrainzReleaseGroupId => {
            left.musicbrainz_release_group_id == right.musicbrainz_release_group_id
        }
        MetadataField::MusicBrainzArtistId => {
            left.musicbrainz_artist_id == right.musicbrainz_artist_id
        }
        _ => true,
    }
}

#[derive(Clone, Copy)]
enum ArtistScope {
    Track,
    Album,
}

struct ArtistOccurrence {
    scope: ArtistScope,
    index: usize,
    credit_count: usize,
}

fn artist_occurrences(track: &Track, artist: &Artist) -> Vec<ArtistOccurrence> {
    let mut occurrences = Vec::new();
    occurrences.extend(
        track
            .artist_credits()
            .iter()
            .enumerate()
            .filter(|(_, credit)| credit.id == artist.id)
            .map(|(index, _)| ArtistOccurrence {
                scope: ArtistScope::Track,
                index,
                credit_count: track.artist_credits().len(),
            }),
    );
    occurrences.extend(
        track
            .album_artist_credits()
            .iter()
            .enumerate()
            .filter(|(_, credit)| credit.id == artist.id)
            .map(|(index, _)| ArtistOccurrence {
                scope: ArtistScope::Album,
                index,
                credit_count: track.album_artist_credits().len(),
            }),
    );
    occurrences
}

fn artist_observations(
    tag: Option<&Tag>,
    track: &Track,
    artist: &Artist,
) -> Result<Vec<MetadataValues>, MetadataError> {
    let track_credits = current_artist_values(tag, track, ArtistScope::Track);
    let album_credits = current_artist_values(tag, track, ArtistScope::Album);
    artist_occurrences(track, artist)
        .into_iter()
        .map(|occurrence| {
            let credits = match occurrence.scope {
                ArtistScope::Track => &track_credits,
                ArtistScope::Album => &album_credits,
            };
            credits
                .get(occurrence.index)
                .cloned()
                .ok_or(MetadataError::Conflict)
        })
        .collect()
}

fn current_artist_values(
    tag: Option<&Tag>,
    track: &Track,
    scope: ArtistScope,
) -> Vec<MetadataValues> {
    let (fallback, name_key, id_key) = match scope {
        ArtistScope::Track => (
            track.artist.clone(),
            ItemKey::TrackArtists,
            ItemKey::MusicBrainzArtistId,
        ),
        ArtistScope::Album => (
            {
                let names = track
                    .album_artist_credits()
                    .iter()
                    .map(|artist| artist.name.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                clean(&names).unwrap_or_else(|| track.artist.clone())
            },
            ItemKey::AlbumArtist,
            ItemKey::MusicBrainzReleaseArtistId,
        ),
    };
    let names = match scope {
        ArtistScope::Track => {
            let tagged = tag
                .map(|tag| {
                    tag.get_strings(name_key)
                        .flat_map(super::media::split_names)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if tagged.is_empty() {
                super::media::split_names(
                    tag.and_then(|tag| text(tag.artist()))
                        .as_deref()
                        .unwrap_or(&fallback),
                )
            } else {
                tagged
            }
        }
        ArtistScope::Album => super::media::split_names(
            tag.and_then(|tag| tag.get_string(name_key))
                .unwrap_or(&fallback),
        ),
    };
    let ids = tag
        .map(|tag| {
            tag.get_items(id_key)
                .filter_map(|item| item.value().text())
                .flat_map(super::media::split_names)
                .filter(|value| library::is_musicbrainz_id(value))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let ids = (ids.len() == names.len())
        .then(|| ids.into_iter().map(Some).collect::<Vec<_>>())
        .unwrap_or_else(|| names.iter().map(|_| None).collect());
    names
        .into_iter()
        .zip(ids)
        .map(|(name, musicbrainz_artist_id)| MetadataValues {
            title: name,
            musicbrainz_artist_id,
            ..MetadataValues::default()
        })
        .collect()
}

fn subject_revision(targets: &[SubjectTarget]) -> Result<String, MetadataError> {
    let mut revision = String::from("v1");
    for target in targets {
        let path = target.target.path.to_string_lossy();
        let file = file_revision(&target.target.path)?;
        revision.push_str(&format!(":{}:{path}:{}:{file}", path.len(), file.len()));
    }
    Ok(revision)
}

pub(super) fn write_subject(
    roots: &[PathBuf],
    subject: &MetadataSubject,
    edit: &MetadataEdit,
) -> Result<BTreeSet<PathBuf>, MetadataError> {
    let targets = native_subject_targets(roots, subject)?;
    write_subject_targets(subject.item(), targets, edit)
}

pub(super) fn write_rating(
    roots: &[PathBuf],
    track: &Track,
    rating: Option<u8>,
) -> Result<bool, MetadataError> {
    let Some(target) = accepted_target(roots, track) else {
        return Ok(false);
    };
    if !target
        .writer
        .metadata_key_is_writable(ItemKey::Popularimeter)
    {
        return Ok(false);
    }
    let prepared = prepare_file(
        &target,
        |tag| {
            replace_tag_rating(tag, rating);
            Ok(HashSet::from([ItemKey::Popularimeter]))
        },
        |expected, actual| {
            if rating_values(expected) == actual.map(rating_values).unwrap_or_default() {
                Ok(())
            } else {
                Err(MetadataError::Write(
                    "The rating did not survive the metadata update.".to_string(),
                ))
            }
        },
    )?;
    commit_batch(vec![prepared], |_| Ok(()))?;
    Ok(true)
}

fn replace_tag_rating(tag: &mut Tag, rating: Option<u8>) {
    let mut ratings = tag.ratings().collect::<Vec<_>>();
    tag.remove_key(ItemKey::Popularimeter);
    if let Some(stars) = rating
        .map(|rating| library::rating_to_whole_star(Some(rating)))
        .and_then(star_rating)
    {
        if let Some(current) = ratings.first_mut() {
            current.rating = stars;
        } else {
            ratings.push(Popularimeter::custom("Rufin", stars, 0));
        }
    } else if !ratings.is_empty() {
        ratings.remove(0);
    }
    for rating in ratings {
        tag.push_unchecked(TagItem::new(
            ItemKey::Popularimeter,
            ItemValue::Text(rating.to_string()),
        ));
    }
}

fn rating_values(tag: &Tag) -> Vec<String> {
    tag.ratings().map(|rating| rating.to_string()).collect()
}

fn star_rating(value: u8) -> Option<StarRating> {
    match value {
        1 => Some(StarRating::One),
        2 => Some(StarRating::Two),
        3 => Some(StarRating::Three),
        4 => Some(StarRating::Four),
        5 => Some(StarRating::Five),
        _ => None,
    }
}

pub(super) fn write_mapped_subject(
    subject: &MetadataSubject,
    access: &[library::LocalAccessTarget],
    edit: &MetadataEdit,
) -> Result<BTreeSet<PathBuf>, MetadataError> {
    let targets = mapped_subject_targets(subject, access)?;
    write_subject_targets(subject.item(), targets, edit)
}

fn write_subject_targets(
    item: &MetadataItem,
    targets: Vec<SubjectTarget>,
    edit: &MetadataEdit,
) -> Result<BTreeSet<PathBuf>, MetadataError> {
    ensure_metadata_replacement_available()?;
    write_batch(item, targets, edit, |_| Ok(()))
}

fn ensure_metadata_replacement_available() -> Result<(), MetadataError> {
    if METADATA_REPLACEMENT_AVAILABLE {
        Ok(())
    } else {
        Err(MetadataError::Unavailable)
    }
}

struct PreparedBatchFile {
    target: MetadataTarget,
    temp: tempfile::TempPath,
    expected_revision: String,
    #[cfg(target_os = "windows")]
    backup: WindowsBackupPath,
}

impl PreparedBatchFile {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn replace_target(&self) -> std::io::Result<()> {
        atomic_exchange(self.temp.as_ref(), &self.target.path)
    }

    #[cfg(target_os = "windows")]
    fn replace_target(&self) -> std::io::Result<()> {
        windows_replace_with_backup(self.temp.as_ref(), &self.target.path, &self.backup.path)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn replace_target(&self) -> std::io::Result<()> {
        Err(unsupported_metadata_replacement())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn original_path(&self) -> &Path {
        self.temp.as_ref()
    }

    #[cfg(target_os = "windows")]
    fn original_path(&self) -> &Path {
        &self.backup.path
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn original_path(&self) -> &Path {
        self.temp.as_ref()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn restore_target(&self) -> std::io::Result<()> {
        atomic_exchange(self.temp.as_ref(), &self.target.path)
    }

    #[cfg(target_os = "windows")]
    fn restore_target(&self) -> std::io::Result<()> {
        windows_replace_with_backup(&self.backup.path, &self.target.path, self.temp.as_ref())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn restore_target(&self) -> std::io::Result<()> {
        Err(unsupported_metadata_replacement())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn preserve_original(self) -> std::io::Result<PathBuf> {
        self.temp.keep().map_err(Into::into)
    }

    #[cfg(target_os = "windows")]
    fn preserve_original(self) -> std::io::Result<PathBuf> {
        Ok(self.backup.keep())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn preserve_original(self) -> std::io::Result<PathBuf> {
        Err(unsupported_metadata_replacement())
    }
}

#[cfg(target_os = "windows")]
struct WindowsBackupPath {
    path: PathBuf,
    keep: bool,
}

#[cfg(target_os = "windows")]
impl WindowsBackupPath {
    fn reserve(parent: &Path) -> std::io::Result<Self> {
        let file = tempfile::Builder::new()
            .prefix(".rufin-metadata-backup-")
            .tempfile_in(parent)?;
        let path = file.path().to_path_buf();
        file.close()?;
        Ok(Self { path, keep: false })
    }

    fn keep(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsBackupPath {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn write_batch(
    item: &MetadataItem,
    targets: Vec<SubjectTarget>,
    edit: &MetadataEdit,
    before_exchange: impl FnMut(usize) -> Result<(), MetadataError>,
) -> Result<BTreeSet<PathBuf>, MetadataError> {
    if edit.item_id != item.id() {
        return Err(MetadataError::Unavailable);
    }
    let editing = match item {
        MetadataItem::Track(_) => targets[0].target.writer.metadata_editing(),
        MetadataItem::Album(_) => album_editing(&targets),
        MetadataItem::Artist(artist) => artist_editing(artist, &targets),
    }
    .ok_or(MetadataError::Unavailable)?;
    edit.validate(&editing)?;
    let expected_revision = edit.revision.as_deref().ok_or(MetadataError::Conflict)?;
    if edit_revision(item, &targets)? != expected_revision {
        return Err(MetadataError::Conflict);
    }
    if edit.changes.is_empty() {
        return Ok(targets
            .into_iter()
            .map(|target| target.target.path)
            .collect());
    }

    let mut prepared = Vec::with_capacity(targets.len());
    for target in &targets {
        prepared.push(prepare_batch_file(item, target, &edit.changes)?);
    }
    if edit_revision(item, &targets)? != expected_revision {
        return Err(MetadataError::Conflict);
    }
    commit_batch(prepared, before_exchange)
}

fn edit_revision(item: &MetadataItem, targets: &[SubjectTarget]) -> Result<String, MetadataError> {
    match item {
        MetadataItem::Track(_) => file_revision(&targets[0].target.path),
        MetadataItem::Album(_) | MetadataItem::Artist(_) => subject_revision(targets),
    }
}

fn prepare_batch_file(
    item: &MetadataItem,
    subject: &SubjectTarget,
    changes: &[MetadataChange],
) -> Result<PreparedBatchFile, MetadataError> {
    let target = &subject.target;
    prepare_file(
        target,
        |tag| {
            let mut changed = HashSet::new();
            match item {
                MetadataItem::Track(_) => {
                    for change in changes {
                        apply_change(tag, change, &mut changed);
                    }
                }
                MetadataItem::Album(_) => apply_album_changes(tag, changes, &mut changed),
                MetadataItem::Artist(artist) => {
                    apply_artist_changes(tag, &subject.track, artist, changes, &mut changed)?
                }
            }
            Ok(changed)
        },
        |_, verified_tag| {
            verify_value_changes(
                &verification_values(item, verified_tag, &subject.track)?,
                changes,
            )
        },
    )
}

fn prepare_file(
    target: &MetadataTarget,
    mutate: impl FnOnce(&mut Tag) -> Result<HashSet<ItemKey>, MetadataError>,
    verify: impl FnOnce(&Tag, Option<&Tag>) -> Result<(), MetadataError>,
) -> Result<PreparedBatchFile, MetadataError> {
    let expected_revision = file_revision(&target.path)?;
    let parent = target.path.parent().ok_or_else(|| {
        MetadataError::Write("The metadata file has no parent folder.".to_string())
    })?;
    let temp = tempfile::Builder::new()
        .prefix(".rufin-metadata-")
        .tempfile_in(parent)
        .map_err(|error| write_error("create a temporary metadata file", error))?;
    fs::copy(&target.path, temp.path())
        .map_err(|error| write_error("copy the original metadata file", error))?;

    let tagged = read_tagged(temp.path(), target.writer)?;
    let mut tag = writable_tag(&tagged);
    let mut preserved = PreservedMetadata::new(&tag);
    preserved.allow_changes(mutate(&mut tag)?);
    save_tag(&tag, temp.path())?;

    let verified = read_tagged(temp.path(), target.writer)?;
    let verified_tag = verified.primary_tag().or_else(|| verified.first_tag());
    if !preserved.matches(verified_tag) {
        return Err(MetadataError::Write(
            "Unrelated tags or artwork changed while preparing the metadata update.".to_string(),
        ));
    }
    verify(&tag, verified_tag)?;
    temp.as_file()
        .sync_all()
        .map_err(|error| write_error("sync the updated metadata", error))?;
    if file_revision(&target.path)? != expected_revision {
        return Err(MetadataError::Conflict);
    }
    Ok(PreparedBatchFile {
        target: MetadataTarget {
            path: target.path.clone(),
            writer: target.writer,
        },
        temp: temp.into_temp_path(),
        expected_revision,
        #[cfg(target_os = "windows")]
        backup: WindowsBackupPath::reserve(parent)
            .map_err(|error| write_error("reserve a metadata recovery file", error))?,
    })
}

fn commit_batch(
    prepared: Vec<PreparedBatchFile>,
    mut before_exchange: impl FnMut(usize) -> Result<(), MetadataError>,
) -> Result<BTreeSet<PathBuf>, MetadataError> {
    let paths = prepared
        .iter()
        .map(|file| file.target.path.clone())
        .collect::<BTreeSet<_>>();
    let mut committed = 0;
    for (index, file) in prepared.iter().enumerate() {
        if let Err(error) = before_exchange(index) {
            return rollback_batch(prepared, committed, error);
        }
        if let Err(error) = file.replace_target() {
            return rollback_batch(
                prepared,
                committed,
                write_error("replace an original metadata file", error),
            );
        }
        committed += 1;
        match file_revision(file.original_path()) {
            Ok(revision) if revision == file.expected_revision => {}
            Ok(_) => return rollback_batch(prepared, committed, MetadataError::Conflict),
            Err(error) => return rollback_batch(prepared, committed, error),
        }
    }
    let parents = prepared
        .iter()
        .filter_map(|file| file.target.path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    for parent in parents {
        if let Err(error) = sync_parent(&parent) {
            return rollback_batch(prepared, committed, error);
        }
    }
    Ok(paths)
}

fn rollback_batch(
    prepared: Vec<PreparedBatchFile>,
    committed: usize,
    original_error: MetadataError,
) -> Result<BTreeSet<PathBuf>, MetadataError> {
    let mut recovery_errors = Vec::new();
    for (index, file) in prepared.into_iter().enumerate().rev() {
        if index >= committed {
            continue;
        }
        if let Err(error) = file.restore_target() {
            let recovery_path = file.original_path().to_path_buf();
            let target_path = file.target.path.clone();
            let kept = file.preserve_original();
            recovery_errors.push(match kept {
                Ok(_) => format!(
                    "could not restore {}; the previous file is preserved at {}: {error}",
                    target_path.display(),
                    recovery_path.display()
                ),
                Err(keep_error) => format!(
                    "could not restore {} or preserve its recovery file: {error}; {keep_error}",
                    target_path.display()
                ),
            });
            continue;
        }
        if let Some(parent) = file.target.path.parent()
            && let Err(error) = sync_parent(parent)
        {
            recovery_errors.push(format!(
                "restored {}, but could not sync its folder: {error}",
                file.target.path.display()
            ));
        }
    }
    if recovery_errors.is_empty() {
        Err(original_error)
    } else {
        Err(MetadataError::Write(format!(
            "{original_error} Recovery also failed: {}",
            recovery_errors.join("; ")
        )))
    }
}

fn apply_album_changes(tag: &mut Tag, changes: &[MetadataChange], changed: &mut HashSet<ItemKey>) {
    for change in changes {
        match change {
            MetadataChange::Title(value) => {
                changed.insert(ItemKey::AlbumTitle);
                set_text(tag, ItemKey::AlbumTitle, Some(value));
            }
            MetadataChange::SortTitle(value) => {
                changed.insert(ItemKey::AlbumTitleSortOrder);
                set_text(tag, ItemKey::AlbumTitleSortOrder, value.as_deref());
            }
            _ => apply_change(tag, change, changed),
        }
    }
}

fn apply_artist_changes(
    tag: &mut Tag,
    track: &Track,
    artist: &Artist,
    changes: &[MetadataChange],
    changed: &mut HashSet<ItemKey>,
) -> Result<(), MetadataError> {
    let occurrences = artist_occurrences(track, artist);
    if occurrences.is_empty() {
        return Err(MetadataError::Conflict);
    }
    let mut track_values = current_artist_values(Some(tag), track, ArtistScope::Track);
    let mut album_values = current_artist_values(Some(tag), track, ArtistScope::Album);
    let title = changes.iter().find_map(|change| match change {
        MetadataChange::Title(value) => Some(value.as_str()),
        _ => None,
    });
    let musicbrainz_id = changes.iter().find_map(|change| match change {
        MetadataChange::MusicBrainzArtistId(value) => Some(value.as_deref()),
        _ => None,
    });
    let mut changed_track_title = false;
    let mut changed_album_title = false;
    let mut changed_track_id = false;
    let mut changed_album_id = false;
    for occurrence in occurrences {
        let values = match occurrence.scope {
            ArtistScope::Track => &mut track_values,
            ArtistScope::Album => &mut album_values,
        };
        let value = values
            .get_mut(occurrence.index)
            .ok_or(MetadataError::Conflict)?;
        if let Some(title) = title {
            value.title = title.trim().to_string();
            match occurrence.scope {
                ArtistScope::Track => changed_track_title = true,
                ArtistScope::Album => changed_album_title = true,
            }
        }
        if let Some(musicbrainz_id) = musicbrainz_id {
            if occurrence.credit_count != 1 {
                return Err(MetadataError::Unavailable);
            }
            value.musicbrainz_artist_id = musicbrainz_id.map(ToString::to_string);
            match occurrence.scope {
                ArtistScope::Track => changed_track_id = true,
                ArtistScope::Album => changed_album_id = true,
            }
        }
    }
    if changed_track_title {
        changed.insert(ItemKey::TrackArtist);
        changed.insert(ItemKey::TrackArtists);
        let names = track_values
            .iter()
            .map(|value| value.title.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        set_text(tag, ItemKey::TrackArtist, Some(&names));
        tag.remove_key(ItemKey::TrackArtists);
    }
    if changed_album_title {
        changed.insert(ItemKey::AlbumArtist);
        let names = album_values
            .iter()
            .map(|value| value.title.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        set_text(tag, ItemKey::AlbumArtist, Some(&names));
    }
    if changed_track_id {
        changed.insert(ItemKey::MusicBrainzArtistId);
        set_text(
            tag,
            ItemKey::MusicBrainzArtistId,
            track_values[0].musicbrainz_artist_id.as_deref(),
        );
    }
    if changed_album_id {
        changed.insert(ItemKey::MusicBrainzReleaseArtistId);
        set_text(
            tag,
            ItemKey::MusicBrainzReleaseArtistId,
            album_values[0].musicbrainz_artist_id.as_deref(),
        );
    }
    Ok(())
}

fn verify_value_changes(
    values: &[MetadataValues],
    changes: &[MetadataChange],
) -> Result<(), MetadataError> {
    if let Some(change) = changes
        .iter()
        .find(|change| values.iter().any(|values| !change.matches(values)))
    {
        return Err(MetadataError::Write(format!(
            "The updated {} could not be verified.",
            field_name(change.field())
        )));
    }
    Ok(())
}

fn verification_values(
    item: &MetadataItem,
    tag: Option<&Tag>,
    track: &Track,
) -> Result<Vec<MetadataValues>, MetadataError> {
    match item {
        MetadataItem::Track(_) => Ok(vec![values(tag, track)]),
        MetadataItem::Album(_) => Ok(vec![album_values(tag, track)]),
        MetadataItem::Artist(artist) => artist_observations(tag, track, artist),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn atomic_exchange(first: &Path, second: &Path) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        first,
        rustix::fs::CWD,
        second,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(Into::into)
}

#[cfg(target_os = "windows")]
fn windows_replace_with_backup(
    replacement: &Path,
    target: &Path,
    backup: &Path,
) -> std::io::Result<()> {
    let replacement = windows_path(replacement)?;
    let target = windows_path(target)?;
    let backup = windows_path(backup)?;
    winsafe::ReplaceFile(
        target,
        replacement,
        Some(backup),
        winsafe::co::REPLACEFILE::default(),
    )
    .map_err(std::io::Error::other)
}

#[cfg(target_os = "windows")]
fn windows_path(path: &Path) -> std::io::Result<&str> {
    path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "metadata replacement requires a Unicode Windows path",
        )
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn unsupported_metadata_replacement() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic metadata replacement is unavailable on this platform",
    )
}

fn read_tagged(path: &Path, writer: MetadataWriter) -> Result<TaggedFile, MetadataError> {
    read_lofty_for_edit(path, writer.file_type())
        .map_err(|error| write_error("read the file metadata", error))?
        .ok_or_else(|| {
            MetadataError::Write(
                "The file contents no longer match the selected metadata writer.".to_string(),
            )
        })
}

fn writable_tag(tagged: &TaggedFile) -> Tag {
    if let Some(tag) = tagged.primary_tag() {
        return tag.clone();
    }
    let primary = tagged.file_type().primary_tag_type();
    let Some(mut tag) = tagged.first_tag().cloned() else {
        return Tag::new(primary);
    };
    tag.re_map(primary);
    tag
}

fn save_tag(tag: &Tag, path: &Path) -> Result<(), MetadataError> {
    let options = WriteOptions::new().remove_others(false);
    if tag.tag_type() != TagType::Id3v2 {
        return tag
            .save_to_path(path, options)
            .map_err(|error| write_error("write the updated metadata", error));
    }

    let mut extended_text = HashMap::<String, Vec<String>>::new();
    for item in tag.items() {
        let Some(key) = item.key().map_key(TagType::Id3v2) else {
            continue;
        };
        if key.len() == 4 {
            continue;
        }
        if let ItemValue::Text(value) = item.value() {
            extended_text
                .entry(key.to_string())
                .or_default()
                .push(value.clone());
        }
    }
    let mut id3v2 = Id3v2Tag::from(tag.clone());
    for (description, values) in extended_text {
        id3v2.insert_user_text(description, values.join("\0"));
    }
    id3v2
        .save_to_path(path, options)
        .map_err(|error| write_error("write the updated metadata", error))
}

struct PreservedMetadata {
    changed: HashSet<ItemKey>,
    items: HashMap<TagItem, usize>,
    pictures: HashMap<Picture, usize>,
}

impl PreservedMetadata {
    fn new(tag: &Tag) -> Self {
        Self {
            changed: HashSet::new(),
            items: counts(tag.items().cloned()),
            pictures: counts(tag.pictures().iter().cloned()),
        }
    }

    fn allow_changes(&mut self, changed: HashSet<ItemKey>) {
        self.items.retain(|item, _| !changed.contains(&item.key()));
        self.changed = changed;
    }

    fn matches(&self, tag: Option<&Tag>) -> bool {
        let Some(tag) = tag else {
            return self.items.is_empty() && self.pictures.is_empty();
        };
        self.items
            == counts(
                tag.items()
                    .filter(|item| !self.changed.contains(&item.key()))
                    .cloned(),
            )
            && self.pictures == counts(tag.pictures().iter().cloned())
    }
}

fn counts<T: Eq + std::hash::Hash>(values: impl IntoIterator<Item = T>) -> HashMap<T, usize> {
    let mut counts = HashMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts
}

fn values(tag: Option<&Tag>, track: &Track) -> MetadataValues {
    MetadataValues {
        title: tag
            .and_then(|tag| text(tag.title()))
            .unwrap_or_else(|| track.title.clone()),
        sort_title: tag
            .and_then(|tag| tag.get_string(ItemKey::TrackTitleSortOrder))
            .and_then(clean),
        artist: tag
            .and_then(|tag| text(tag.artist()))
            .or_else(|| Some(track.artist.clone())),
        album: tag
            .and_then(|tag| text(tag.album()))
            .or_else(|| Some(track.album.clone())),
        album_artist: tag
            .and_then(|tag| tag.get_string(ItemKey::AlbumArtist))
            .and_then(clean),
        track_number: tag.and_then(|tag| positive_u16(tag.track())),
        disc_number: tag.and_then(|tag| positive_u16(tag.disk())),
        year: tag
            .and_then(Tag::date)
            .map(|date| date.year)
            .filter(|year| *year > 0),
        genre: tag.and_then(|tag| text(tag.genre())),
        comment: tag.and_then(|tag| text(tag.comment())),
        bpm: tag
            .and_then(|tag| {
                tag.get_string(ItemKey::IntegerBpm)
                    .or_else(|| tag.get_string(ItemKey::Bpm))
            })
            .and_then(|value| value.trim().parse::<f64>().ok())
            .map(f64::round)
            .filter(|value| (1.0..=f64::from(u16::MAX)).contains(value))
            .map(|value| value as u16),
        musicbrainz_recording_id: tag
            .and_then(|tag| tag.get_string(ItemKey::MusicBrainzRecordingId))
            .and_then(clean)
            .or_else(|| track.musicbrainz_recording_id.clone()),
        musicbrainz_release_track_id: tag
            .and_then(|tag| tag.get_string(ItemKey::MusicBrainzTrackId))
            .and_then(clean)
            .or_else(|| track.musicbrainz_release_track_id.clone()),
        musicbrainz_album_id: tag
            .and_then(|tag| tag.get_string(ItemKey::MusicBrainzReleaseId))
            .and_then(clean)
            .or_else(|| {
                track
                    .album_artwork_facts()
                    .and_then(|album| album.musicbrainz_album_id.clone())
            }),
        musicbrainz_release_group_id: tag
            .and_then(|tag| tag.get_string(ItemKey::MusicBrainzReleaseGroupId))
            .and_then(clean)
            .or_else(|| {
                track
                    .album_artwork_facts()
                    .and_then(|album| album.musicbrainz_release_group_id.clone())
            }),
        ..MetadataValues::default()
    }
}

fn text(value: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    value.as_deref().and_then(clean)
}

fn clean(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn positive_u16(value: Option<u32>) -> Option<u16> {
    value
        .filter(|value| *value > 0)
        .map(|value| value.min(u32::from(u16::MAX)) as u16)
}

fn apply_change(tag: &mut Tag, change: &MetadataChange, changed: &mut HashSet<ItemKey>) {
    match change {
        MetadataChange::Title(value) => {
            changed.insert(ItemKey::TrackTitle);
            tag.set_title(value.trim().to_string());
        }
        MetadataChange::SortTitle(value) => {
            changed.insert(ItemKey::TrackTitleSortOrder);
            set_text(tag, ItemKey::TrackTitleSortOrder, value.as_deref())
        }
        MetadataChange::Artist(value) => {
            changed.insert(ItemKey::TrackArtist);
            set_text(tag, ItemKey::TrackArtist, value.as_deref());
        }
        MetadataChange::Album(value) => {
            changed.insert(ItemKey::AlbumTitle);
            set_text(tag, ItemKey::AlbumTitle, value.as_deref());
        }
        MetadataChange::AlbumArtist(value) => {
            changed.insert(ItemKey::AlbumArtist);
            set_text(tag, ItemKey::AlbumArtist, value.as_deref());
        }
        MetadataChange::TrackNumber(value) => {
            changed.insert(ItemKey::TrackNumber);
            match value {
                Some(value) => tag.set_track(u32::from(*value)),
                None => tag.remove_track(),
            }
        }
        MetadataChange::DiscNumber(value) => {
            changed.insert(ItemKey::DiscNumber);
            match value {
                Some(value) => tag.set_disk(u32::from(*value)),
                None => tag.remove_disk(),
            }
        }
        MetadataChange::Year(value) => {
            changed.insert(ItemKey::Year);
            changed.insert(ItemKey::RecordingDate);
            match value {
                Some(value) => {
                    let mut date = tag.date().unwrap_or_default();
                    date.year = *value;
                    tag.set_date(date);
                }
                None => tag.remove_date(),
            }
        }
        MetadataChange::Genre(value) => {
            changed.insert(ItemKey::Genre);
            set_text(tag, ItemKey::Genre, value.as_deref());
        }
        MetadataChange::Comment(value) => {
            changed.insert(ItemKey::Comment);
            set_text(tag, ItemKey::Comment, value.as_deref());
        }
        MetadataChange::Bpm(value) => {
            changed.insert(ItemKey::Bpm);
            changed.insert(ItemKey::IntegerBpm);
            tag.remove_key(ItemKey::Bpm);
            tag.remove_key(ItemKey::IntegerBpm);
            match value {
                Some(value) => {
                    if let Some(key) = bpm_key(tag.tag_type()) {
                        tag.insert_text(key, value.to_string());
                    }
                }
                None => {}
            }
        }
        MetadataChange::MusicBrainzRecordingId(value) => {
            changed.insert(ItemKey::MusicBrainzRecordingId);
            set_text(tag, ItemKey::MusicBrainzRecordingId, value.as_deref())
        }
        MetadataChange::MusicBrainzReleaseTrackId(value) => {
            changed.insert(ItemKey::MusicBrainzTrackId);
            set_text(tag, ItemKey::MusicBrainzTrackId, value.as_deref())
        }
        MetadataChange::MusicBrainzAlbumId(value) => {
            changed.insert(ItemKey::MusicBrainzReleaseId);
            set_text(tag, ItemKey::MusicBrainzReleaseId, value.as_deref())
        }
        MetadataChange::MusicBrainzReleaseGroupId(value) => {
            changed.insert(ItemKey::MusicBrainzReleaseGroupId);
            set_text(tag, ItemKey::MusicBrainzReleaseGroupId, value.as_deref())
        }
        MetadataChange::LockData(_) | MetadataChange::MusicBrainzArtistId(_) => {}
        MetadataChange::Lyrics(value) => {
            changed.insert(ItemKey::UnsyncLyrics);
            set_text(tag, ItemKey::UnsyncLyrics, value.as_deref())
        }
    }
}

fn set_text(tag: &mut Tag, key: ItemKey, value: Option<&str>) {
    match value.and_then(clean) {
        Some(value) => {
            if !tag.insert_text(key, value.clone())
                && tag.tag_type() == TagType::Id3v2
                && key == ItemKey::MusicBrainzRecordingId
            {
                tag.insert_unchecked(TagItem::new(key, ItemValue::Text(value)));
            }
        }
        None => {
            tag.remove_key(key);
        }
    }
}

fn field_name(field: library::MetadataField) -> &'static str {
    match field {
        library::MetadataField::Title => "title",
        library::MetadataField::SortTitle => "sort title",
        library::MetadataField::Artist => "artist",
        library::MetadataField::Album => "album",
        library::MetadataField::AlbumArtist => "album artist",
        library::MetadataField::TrackNumber => "track number",
        library::MetadataField::DiscNumber => "disc number",
        library::MetadataField::Year => "year",
        library::MetadataField::Genre => "genre",
        library::MetadataField::Comment => "comment",
        library::MetadataField::Bpm => "BPM",
        library::MetadataField::MusicBrainzRecordingId => "MusicBrainz recording ID",
        library::MetadataField::MusicBrainzReleaseTrackId => "MusicBrainz release track ID",
        library::MetadataField::MusicBrainzAlbumId => "MusicBrainz release ID",
        library::MetadataField::MusicBrainzReleaseGroupId => "MusicBrainz release group ID",
        library::MetadataField::MusicBrainzArtistId => "MusicBrainz artist ID",
        library::MetadataField::LockData => "metadata field",
        library::MetadataField::Lyrics => "lyrics",
    }
}

fn file_revision(path: &Path) -> Result<String, MetadataError> {
    let metadata =
        fs::metadata(path).map_err(|error| write_error("inspect the metadata file", error))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    Ok(format!(
        "{}:{}:{}:{}:{}",
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos(),
        metadata_device(&metadata),
        metadata_inode(&metadata)
    ))
}

#[cfg(unix)]
fn metadata_inode(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn metadata_inode(_metadata: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn metadata_device(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(not(unix))]
fn metadata_device(_metadata: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), MetadataError> {
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| write_error("sync the metadata folder", error))
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), MetadataError> {
    Ok(())
}

fn write_error(action: &str, error: impl std::fmt::Display) -> MetadataError {
    MetadataError::Write(format!("Could not {action}: {error}"))
}

#[cfg(test)]
fn write_track_with_hook(
    target: &MetadataTarget,
    track: &Track,
    edit: &MetadataEdit,
    before_exchange: impl FnMut(usize) -> Result<(), MetadataError>,
) -> Result<PathBuf, MetadataError> {
    let item = MetadataItem::Track(track.clone());
    let targets = vec![SubjectTarget {
        target: MetadataTarget {
            path: target.path.clone(),
            writer: target.writer,
        },
        track: track.clone(),
    }];
    write_batch(&item, targets, edit, before_exchange)?
        .into_iter()
        .next()
        .ok_or(MetadataError::Unavailable)
}

#[cfg(test)]
pub(super) fn write_with_test_hook(
    target: &MetadataTarget,
    track: &Track,
    edit: &MetadataEdit,
    before_exchange: impl FnMut(usize) -> Result<(), MetadataError>,
) -> Result<PathBuf, MetadataError> {
    write_track_with_hook(target, track, edit, before_exchange)
}

#[cfg(test)]
pub(super) fn read_aggregate_with_tracks(
    roots: &[PathBuf],
    item: &MetadataItem,
    tracks: Vec<Track>,
) -> Result<MetadataDraft, MetadataError> {
    let targets = test_subject_targets(roots, tracks)?;
    read_subject_targets(item, &targets)
}

#[cfg(test)]
pub(super) fn write_aggregate_with_test_hook(
    roots: &[PathBuf],
    item: &MetadataItem,
    tracks: Vec<Track>,
    edit: &MetadataEdit,
    before_exchange: impl FnMut(usize) -> Result<(), MetadataError>,
) -> Result<BTreeSet<PathBuf>, MetadataError> {
    let targets = test_subject_targets(roots, tracks)?;
    write_batch(item, targets, edit, before_exchange)
}

#[cfg(test)]
fn test_subject_targets(
    roots: &[PathBuf],
    tracks: Vec<Track>,
) -> Result<Vec<SubjectTarget>, MetadataError> {
    let mut targets = Vec::with_capacity(tracks.len());
    for track in tracks {
        let target = target(roots, &track).ok_or(MetadataError::Unavailable)?;
        targets.push(SubjectTarget { target, track });
    }
    normalize_subject_targets(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_uses_the_key_supported_by_each_tag_format() {
        for (tag_type, expected) in [
            (TagType::Id3v2, ItemKey::IntegerBpm),
            (TagType::VorbisComments, ItemKey::Bpm),
        ] {
            let mut tag = Tag::new(tag_type);
            let mut changed = HashSet::new();

            apply_change(&mut tag, &MetadataChange::Bpm(Some(123)), &mut changed);

            assert_eq!(tag.get_string(expected), Some("123"));
            assert!(changed.contains(&ItemKey::Bpm));
            assert!(changed.contains(&ItemKey::IntegerBpm));
        }
    }

    #[test]
    fn rating_replaces_only_the_rating_read_by_rufin() {
        let mut tag = Tag::new(TagType::Id3v2);
        for rating in [
            Popularimeter::musicbee(StarRating::Two, 7),
            Popularimeter::picard(StarRating::Four, 11),
        ] {
            tag.push_unchecked(TagItem::new(
                ItemKey::Popularimeter,
                ItemValue::Text(rating.to_string()),
            ));
        }

        replace_tag_rating(&mut tag, Some(9));

        let ratings = tag.ratings().collect::<Vec<_>>();
        assert_eq!(ratings[0].rating(), StarRating::Five);
        assert_eq!(ratings[0].play_counter, 7);
        assert_eq!(ratings[1].rating(), StarRating::Four);
        assert_eq!(ratings[1].play_counter, 11);
    }
}
