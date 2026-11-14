use lofty::prelude::Accessor;
use lofty::tag::{ItemKey, Tag, TagType};

use super::super::lofty::MetadataWriter;
use crate::{
    MetadataEdit, MetadataField, MetadataFieldKind, SourceEntityKind, SourceMetadataError,
};

struct Field {
    key: &'static str,
    label: Option<&'static str>,
    item: ItemKey,
    kind: MetadataFieldKind,
    scopes: u8,
    /// The share of the item's values this field holds, for the release type tag: its one
    /// primary type, or its secondary types.
    part: Option<&'static [&'static str]>,
}

/// MusicBrainz's release types. A release type tag holds a primary one, then secondary ones.
const PRIMARY_RELEASE_TYPES: &[&str] = &["Album", "Single", "EP", "Broadcast", "Other"];
const SECONDARY_RELEASE_TYPES: &[&str] = &[
    "Compilation",
    "Soundtrack",
    "Spokenword",
    "Interview",
    "Audiobook",
    "Audio drama",
    "Live",
    "Remix",
    "DJ-mix",
    "Mixtape/Street",
    "Demo",
    "Field recording",
];

/// A flag is set, cleared or absent, and an absent one leaves the answer to other tags.
const FLAG: &[&str] = &["Yes", "No"];

/// The values of a release type tag this field holds, as written.
fn part_values(tag: &Tag, field: &Field) -> Vec<String> {
    let primary = field.part == Some(PRIMARY_RELEASE_TYPES);
    tag.get_strings(field.item)
        .flat_map(|value| value.split([';', '\0']))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| {
            PRIMARY_RELEASE_TYPES
                .iter()
                .any(|known| known.eq_ignore_ascii_case(value))
                == primary
        })
        .map(str::to_owned)
        .collect()
}

/// Whether two fields edit the same values of a tag.
fn same(a: &Field, b: &Field) -> bool {
    a.item == b.item && a.part == b.part
}

macro_rules! fields {
    ($($definition:tt),* $(,)?) => { const FIELDS: &[Field] = &[$(fields!(@field $definition)),*]; };
    (@field ($key:literal, $item:ident, $kind:ident, $scopes:literal)) => {
        Field { key: $key, label: None, item: ItemKey::$item,
            kind: MetadataFieldKind::$kind, scopes: $scopes, part: None }
    };
    (@field ($key:literal, $label:literal, $item:ident, $kind:ident, $scopes:literal)) => {
        Field { key: $key, label: Some($label), item: ItemKey::$item,
            kind: MetadataFieldKind::$kind, scopes: $scopes, part: None }
    };
    (@field ($key:literal, $label:literal, $item:ident, $kind:ident, $scopes:literal, $part:ident)) => {
        Field { key: $key, label: Some($label), item: ItemKey::$item,
            kind: MetadataFieldKind::$kind, scopes: $scopes, part: Some($part) }
    };
}

// Scope bits are track, album and artist. Artist identity remains in its role-aware editor.
fields![
    ("title", TrackTitle, Text, 1),
    ("title", AlbumTitle, Text, 2),
    ("sort_title", TrackTitleSortOrder, Text, 1),
    ("sort_title", AlbumTitleSortOrder, Text, 2),
    ("artist", TrackArtist, Text, 3),
    ("album", AlbumTitle, Text, 1),
    ("album_artist", AlbumArtist, Text, 3),
    ("track_number", TrackNumber, Number, 1),
    ("disc_number", DiscNumber, Number, 1),
    ("genre", Genre, Text, 7),
    ("comment", Comment, Text, 7),
    ("bpm", IntegerBpm, Number, 1),
    ("musicbrainz_recording_id", MusicBrainzRecordingId, Text, 1),
    ("musicbrainz_release_track_id", MusicBrainzTrackId, Text, 1),
    ("musicbrainz_album_id", MusicBrainzReleaseId, Text, 3),
    (
        "musicbrainz_release_group_id",
        MusicBrainzReleaseGroupId,
        Text,
        3
    ),
    ("musicbrainz_artist_id", MusicBrainzArtistId, Text, 1),
    ("recording_date", "Date", RecordingDate, Date, 3),
    ("release_date", "Release date", ReleaseDate, Date, 3),
    (
        "original_release_date",
        "Original release date",
        OriginalReleaseDate,
        Date,
        3
    ),
    ("composer", "Composer", Composer, List, 3),
    ("composer_sort", "Composer sort", ComposerSortOrder, Text, 3),
    ("performer", "Performer", Performer, List, 3),
    ("artist_sort", "Artist sort", TrackArtistSortOrder, Text, 3),
    (
        "album_artist_sort",
        "Album artist sort",
        AlbumArtistSortOrder,
        Text,
        3
    ),
    ("album_sort", "Album sort", AlbumTitleSortOrder, Text, 1),
    ("disc_subtitle", "Disc subtitle", SetSubtitle, Text, 1),
    ("grouping", "Grouping", ContentGroup, Text, 3),
    ("track_total", "Total tracks", TrackTotal, Number, 3),
    ("disc_total", "Total discs", DiscTotal, Number, 3),
    ("label", "Record label", Label, List, 3),
    ("compilation", "Compilation", FlagCompilation, Boolean, 3),
    (
        "release_type",
        "Release type",
        MusicBrainzReleaseType,
        Text,
        3,
        PRIMARY_RELEASE_TYPES
    ),
    (
        "secondary_release_types",
        "Secondary release types",
        MusicBrainzReleaseType,
        List,
        3,
        SECONDARY_RELEASE_TYPES
    ),
    (
        "musicbrainz_album_artist_id",
        "MusicBrainz album artist ID",
        MusicBrainzReleaseArtistId,
        List,
        3
    ),
    ("mood", "Mood", Mood, List, 7),
    ("key", "Musical key", InitialKey, Text, 3),
    ("copyright", "Copyright", CopyrightMessage, Text, 7),
];

/// The item a field edits in `tag_type`: grouping's is the one the scan reads.
fn item(field: &Field, tag_type: TagType) -> ItemKey {
    if field.item == ItemKey::ContentGroup {
        super::super::lofty::grouping_key(tag_type)
    } else {
        field.item
    }
}

fn writable(writer: MetadataWriter, field: &Field) -> bool {
    let tag_type = writer.file_type().primary_tag_type();
    if field.key == "bpm" {
        super::super::lofty::bpm_key(tag_type).is_some()
    } else {
        let item = item(field, tag_type);
        writer.metadata_key_is_writable(item)
            && (field.label.is_none() || canonical(tag_type, item))
    }
}

fn canonical(tag_type: TagType, key: ItemKey) -> bool {
    matches!(
        key,
        ItemKey::TrackNumber | ItemKey::DiscNumber | ItemKey::TrackTotal | ItemKey::DiscTotal
    ) || key
        .map_key(tag_type)
        .and_then(|key| ItemKey::from_key(tag_type, key))
        .is_none_or(|canonical| canonical == key)
}

pub(super) fn read(
    tag: Option<&Tag>,
    writer: Option<MetadataWriter>,
    scope: u8,
) -> Vec<MetadataField> {
    FIELDS
        .iter()
        .filter(|field| field.scopes & scope != 0)
        .filter_map(|field| {
            if field.label.is_some()
                && tag.is_some_and(|tag| !canonical(tag.tag_type(), item(field, tag.tag_type())))
            {
                return None;
            }
            let writable = writer.is_some_and(|writer| writable(writer, field));
            let mut value = match field.key {
                "track_number" => tag.and_then(Tag::track).map(|value| value.to_string()),
                "disc_number" => tag.and_then(Tag::disk).map(|value| value.to_string()),
                "bpm" => tag
                    .and_then(|tag| {
                        tag.get_string(ItemKey::IntegerBpm)
                            .or_else(|| tag.get_string(ItemKey::Bpm))
                    })
                    .map(str::to_string),
                "artist"
                    if tag.is_some_and(|tag| tag.get_string(ItemKey::TrackArtists).is_some()) =>
                {
                    Some(
                        tag.into_iter()
                            .flat_map(|tag| tag.get_strings(ItemKey::TrackArtists))
                            .collect::<Vec<_>>()
                            .join("; "),
                    )
                }
                _ if field.part.is_some() => tag.map(|tag| part_values(tag, field).join("; ")),
                _ => Some(
                    tag.into_iter()
                        .flat_map(|tag| tag.get_strings(item(field, tag.tag_type())))
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            }
            .unwrap_or_default();
            let flag = field.kind == MetadataFieldKind::Boolean;
            if flag && !value.is_empty() {
                value = FLAG[usize::from(value != "1")].into();
            }
            (field.label.is_none() || writable || !value.is_empty()).then(|| {
                let mut metadata =
                    MetadataField::new(field.key, value.trim().to_string(), writable);
                if let Some(label) = field.label {
                    metadata.label = label.into();
                }
                metadata.kind = field.kind;
                metadata.with_options(if flag {
                    FLAG
                } else {
                    field.part.unwrap_or_default()
                })
            })
        })
        .collect()
}

pub(super) fn apply(
    tag: &mut Tag,
    writer: MetadataWriter,
    edit: &MetadataEdit,
) -> Result<(), SourceMetadataError> {
    let scope = match edit.kind {
        SourceEntityKind::Track => 1,
        SourceEntityKind::Album => 2,
        SourceEntityKind::Artist => 4,
    };
    for (key, value) in &edit.changes {
        if scope == 4
            && matches!(
                key.as_str(),
                "title" | "sort_title" | "musicbrainz_artist_id"
            )
        {
            continue;
        }
        let field = FIELDS
            .iter()
            .find(|field| field.key == key && field.scopes & scope != 0)
            .filter(|field| writable(writer, field))
            .ok_or(SourceMetadataError::Unavailable)?;
        let value = value.trim();
        let label = field
            .label
            .map(str::to_string)
            .unwrap_or_else(|| MetadataField::new(key, "", false).label);
        if key == "title" && value.is_empty() {
            return Err(SourceMetadataError::Write("Add a title".into()));
        }
        if field.kind == MetadataFieldKind::Number && !value.is_empty() {
            let number = value
                .parse::<u32>()
                .map_err(|_| SourceMetadataError::Write(format!("Invalid {}: {value}", label)))?;
            if matches!(key.as_str(), "track_number" | "disc_number" | "bpm")
                && (number == 0 || number > u32::from(u16::MAX))
            {
                return Err(SourceMetadataError::Write(format!(
                    "Invalid {label}: {value}"
                )));
            }
            if writer.file_type().primary_tag_type() == TagType::Mp4Ilst
                && number > u32::from(u16::MAX)
            {
                return Err(SourceMetadataError::Write(format!(
                    "{} exceeds the MP4 limit: {value}",
                    label
                )));
            }
        }
        let item = if key == "bpm" {
            tag.remove_key(ItemKey::Bpm);
            tag.remove_key(ItemKey::IntegerBpm);
            super::super::lofty::bpm_key(writer.file_type().primary_tag_type())
                .ok_or(SourceMetadataError::Unavailable)?
        } else {
            item(field, writer.file_type().primary_tag_type())
        };
        let list = |tag: &mut Tag, item| {
            for value in value
                .split(';')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                // ID3 credits use TIPL instead of ordinary key mappings.
                tag.push_unchecked(lofty::tag::TagItem::new(
                    item,
                    lofty::tag::ItemValue::Text(value.into()),
                ));
            }
        };
        // Artist reads prefer the credit list, so an edit rewrites it where the file has one.
        if key == "artist" && tag.get_string(ItemKey::TrackArtists).is_some() {
            tag.remove_key(ItemKey::TrackArtists);
            list(tag, ItemKey::TrackArtists);
        }
        if field.part.is_some() {
            // The tag keeps the other field's values: the primary type first, as Picard
            // writes them.
            let other = FIELDS
                .iter()
                .find(|other| other.item == item && other.part.is_some() && !same(other, field))
                .map(|other| part_values(tag, other))
                .unwrap_or_default();
            let own = value
                .split(';')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let (primary, secondary) = if field.part == Some(PRIMARY_RELEASE_TYPES) {
                (own, other)
            } else {
                (other, own)
            };
            tag.remove_key(item);
            for value in primary.into_iter().chain(secondary) {
                tag.push_unchecked(lofty::tag::TagItem::new(
                    item,
                    lofty::tag::ItemValue::Text(value.to_lowercase()),
                ));
            }
            continue;
        }
        tag.remove_key(item);
        if !value.is_empty() {
            match field.kind {
                MetadataFieldKind::Boolean => {
                    let value = match value {
                        "Yes" => "1",
                        "No" => "0",
                        _ => {
                            return Err(SourceMetadataError::Write(format!(
                                "Invalid {}: {value}",
                                label
                            )));
                        }
                    };
                    tag.insert_text(item, value.into());
                }
                MetadataFieldKind::List => list(tag, item),
                _ => {
                    if field.label.is_none() {
                        tag.insert_unchecked(lofty::tag::TagItem::new(
                            item,
                            lofty::tag::ItemValue::Text(value.into()),
                        ));
                    } else {
                        tag.insert_text(item, value.into());
                    }
                }
            }
        }
    }
    Ok(())
}

pub(super) fn project(edit: &MetadataEdit) -> crate::MetadataChanges {
    let scope = match edit.kind {
        SourceEntityKind::Track => 1,
        SourceEntityKind::Album => 2,
        SourceEntityKind::Artist => 4,
    };
    edit.changes
        .iter()
        .filter_map(|(key, value)| {
            let field = FIELDS
                .iter()
                .find(|field| field.key == key && field.scopes & scope != 0)?;
            let track = FIELDS
                .iter()
                .find(|track| same(track, field) && track.scopes & 1 != 0)?;
            Some((track.key.to_string(), value.clone()))
        })
        .collect()
}

pub(super) fn scope_fields(
    kind: SourceEntityKind,
    track: &crate::MetadataDraft,
) -> Vec<MetadataField> {
    let scope = if kind == SourceEntityKind::Album {
        2
    } else {
        4
    };
    FIELDS
        .iter()
        .filter(|field| field.scopes & scope != 0)
        .filter_map(|field| {
            let canonical = FIELDS
                .iter()
                .find(|track| same(track, field) && track.scopes & 1 != 0)?;
            let original = track
                .fields
                .iter()
                .find(|track| track.key == canonical.key)?;
            let mut metadata =
                MetadataField::new(field.key, original.value.clone(), original.writable);
            metadata.kind = field.kind;
            metadata.options.clone_from(&original.options);
            if let Some(label) = field.label {
                metadata.label = label.into();
            }
            Some(metadata)
        })
        .collect()
}

pub(super) fn scope_values(
    kind: SourceEntityKind,
    values: &crate::MetadataChanges,
) -> crate::MetadataChanges {
    let scope = match kind {
        SourceEntityKind::Track => 1,
        SourceEntityKind::Album => 2,
        SourceEntityKind::Artist => 4,
    };
    FIELDS
        .iter()
        .filter(|field| field.scopes & scope != 0)
        .filter_map(|field| {
            let track = FIELDS
                .iter()
                .find(|track| same(track, field) && track.scopes & 1 != 0)?;
            Some((field.key.to_string(), values.get(track.key)?.clone()))
        })
        .collect()
}
