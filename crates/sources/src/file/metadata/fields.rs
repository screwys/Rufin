use lofty::tag::{ItemKey, Tag, TagType};

use super::super::lofty::MetadataWriter;
use crate::{MetadataChanges, MetadataField, MetadataFieldKind, SourceMetadataError};

struct Field {
    key: &'static str,
    label: &'static str,
    item: ItemKey,
    kind: MetadataFieldKind,
    scopes: u8,
}

macro_rules! fields {
    ($(($key:literal, $label:literal, $item:ident, $kind:ident, $scopes:literal)),* $(,)?) => {
        const FIELDS: &[Field] = &[$(Field { key: $key, label: $label, item: ItemKey::$item,
            kind: MetadataFieldKind::$kind, scopes: $scopes }),*];
    };
}

// Scope bits are track, album and artist. Artist identity remains in its role-aware editor.
fields![
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
    ("grouping", "Grouping", ContentGroup, Text, 3),
    ("track_total", "Total tracks", TrackTotal, Number, 3),
    ("disc_total", "Total discs", DiscTotal, Number, 3),
    ("label", "Record label", Label, List, 3),
    ("compilation", "Compilation", FlagCompilation, Boolean, 3),
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

fn writable(writer: MetadataWriter, field: &Field) -> bool {
    writer.metadata_key_is_writable(field.item)
        && canonical(writer.file_type().primary_tag_type(), field.item)
}

fn canonical(tag_type: TagType, key: ItemKey) -> bool {
    matches!(key, ItemKey::TrackTotal | ItemKey::DiscTotal)
        || key
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
            if tag.is_some_and(|tag| !canonical(tag.tag_type(), field.item)) {
                return None;
            }
            let writable = writer.is_some_and(|writer| writable(writer, field));
            let mut value = tag
                .into_iter()
                .flat_map(|tag| tag.get_strings(field.item))
                .collect::<Vec<_>>()
                .join("; ");
            if field.kind == MetadataFieldKind::Boolean && !value.is_empty() {
                value = (value == "1").to_string();
            }
            (writable || !value.is_empty()).then(|| MetadataField {
                key: field.key.into(),
                label: field.label.into(),
                kind: field.kind,
                value,
                writable,
                mixed: false,
            })
        })
        .collect()
}

pub(super) fn merge(current: &mut Vec<MetadataField>, next: Vec<MetadataField>) {
    for field in current.iter_mut() {
        if let Some(next) = next.iter().find(|next| next.key == field.key) {
            field.writable &= next.writable;
            field.mixed |= field.value != next.value;
        } else {
            field.writable = false;
            field.mixed |= !field.value.is_empty();
        }
    }
    for mut field in next {
        if !current.iter().any(|existing| existing.key == field.key) {
            field.mixed = !field.value.is_empty();
            field.writable = false;
            current.push(field);
        }
    }
}

pub(super) fn apply(
    tag: &mut Tag,
    writer: MetadataWriter,
    changes: &MetadataChanges,
) -> Result<(), SourceMetadataError> {
    for (key, value) in changes {
        let field = FIELDS
            .iter()
            .find(|field| field.key == key)
            .filter(|field| writable(writer, field))
            .ok_or(SourceMetadataError::Unavailable)?;
        let value = value.trim();
        if field.kind == MetadataFieldKind::Number && !value.is_empty() {
            let number = value.parse::<u32>().map_err(|_| {
                SourceMetadataError::Write(format!("Invalid {}: {value}", field.label))
            })?;
            if writer.file_type().primary_tag_type() == TagType::Mp4Ilst
                && number > u32::from(u16::MAX)
            {
                return Err(SourceMetadataError::Write(format!(
                    "{} exceeds the MP4 limit: {value}",
                    field.label
                )));
            }
        }
        tag.remove_key(field.item);
        if !value.is_empty() {
            match field.kind {
                MetadataFieldKind::Boolean => {
                    let value = match value {
                        "true" => "1",
                        "false" => "0",
                        _ => {
                            return Err(SourceMetadataError::Write(format!(
                                "Invalid {}: {value}",
                                field.label
                            )));
                        }
                    };
                    tag.insert_text(field.item, value.into());
                }
                MetadataFieldKind::List => {
                    for value in value
                        .split(';')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        // ID3 credits use TIPL instead of ordinary key mappings.
                        tag.push_unchecked(lofty::tag::TagItem::new(
                            field.item,
                            lofty::tag::ItemValue::Text(value.into()),
                        ));
                    }
                }
                _ => {
                    tag.insert_text(field.item, value.into());
                }
            }
        }
    }
    Ok(())
}
