use crate::source::SourceOwner;
use localization::{msgid, tr};
use sources::{
    AlbumMetadata, AlbumMetadataEdit, AlbumMetadataValues, AlbumMetadataWritable, ArtistMetadata,
    ArtistMetadataEdit, ArtistMetadataValues, ArtistMetadataWritable, ArtworkEdit, MetadataEdit,
    MetadataFieldKind, SourceMetadataError, TrackMetadata, TrackMetadataEdit, TrackMetadataValues,
    TrackMetadataWritable,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "the metadata dialog owns one draft and keeps its concrete editor value inline"
)]
pub enum MetadataDraft {
    Track(TrackMetadata),
    Album(AlbumMetadata),
    Artist(ArtistMetadata),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MetadataField {
    Title,
    SortTitle,
    Artist,
    Album,
    AlbumArtist,
    TrackNumber,
    DiscNumber,
    Year,
    Genre,
    Comment,
    Bpm,
    MusicBrainzRecordingId,
    MusicBrainzReleaseTrackId,
    MusicBrainzAlbumId,
    MusicBrainzReleaseGroupId,
    MusicBrainzArtistId,
    Locked,
    Extra(usize),
}

pub struct MetadataFieldState {
    pub field: MetadataField,
    pub label: String,
    pub value: String,
    pub writable: bool,
    pub mixed: bool,
    pub kind: MetadataFieldKind,
}

pub struct MetadataEditor {
    pub draft: Arc<MetadataDraft>,
    values: HashMap<MetadataField, String>,
    locked: Option<bool>,
    touched: HashSet<MetadataField>,
    token: Option<String>,
    identified_originals: HashMap<MetadataField, String>,
    committed: Option<bool>,
}

impl MetadataEditor {
    pub fn artwork_search_fields(&self) -> (String, Option<String>) {
        let field = |field| self.values.get(&field).cloned().unwrap_or_default();
        let (artist, album) = match self.draft.as_ref() {
            MetadataDraft::Track(_) => (
                field(MetadataField::AlbumArtist),
                Some(field(MetadataField::Album)),
            ),
            MetadataDraft::Album(_) => (
                field(MetadataField::AlbumArtist),
                Some(field(MetadataField::Title)),
            ),
            MetadataDraft::Artist(_) => (field(MetadataField::Title), None),
        };
        let artist = if artist.trim().is_empty() {
            field(MetadataField::Artist)
        } else {
            artist
        };
        (artist, album)
    }

    pub fn artwork_query(&self, artist: String, album: String) -> super::ArtworkQuery {
        let artist = artist.trim().to_string();
        let album = album.trim().to_string();
        let (draft_artist, draft_album) = self.artwork_search_fields();
        let same_identity = artist == draft_artist.trim()
            && album == draft_album.as_deref().unwrap_or_default().trim();
        let value = |field| {
            self.values
                .get(&field)
                .map(|value| value.trim().to_string())
                .filter(|value| same_identity && !value.is_empty())
        };
        if matches!(self.draft.as_ref(), MetadataDraft::Artist(_)) {
            super::ArtworkQuery::Artist {
                name: artist,
                musicbrainz_id: value(MetadataField::MusicBrainzArtistId),
            }
        } else {
            super::ArtworkQuery::Album {
                artist,
                album,
                release_id: value(MetadataField::MusicBrainzAlbumId),
                release_group_id: value(MetadataField::MusicBrainzReleaseGroupId),
            }
        }
    }
    pub fn new(draft: MetadataDraft) -> Self {
        let draft = Arc::new(draft);
        let locked = match draft.as_ref() {
            MetadataDraft::Track(value) => value.values.locked,
            MetadataDraft::Album(value) => value.values.locked,
            MetadataDraft::Artist(value) => value.values.locked,
        };
        let mut editor = Self {
            draft,
            values: HashMap::new(),
            locked,
            touched: HashSet::new(),
            token: None,
            identified_originals: HashMap::new(),
            committed: None,
        };
        for field in editor.draft.field_keys() {
            let mut value = editor.draft.display_value(field);
            if field == MetadataField::Year
                && let Some(date) = editor.draft.date()
                && (!date.value.is_empty() || !editor.draft.rufin_filled(field))
            {
                value = date.value.clone();
            }
            editor.values.insert(field, value);
            if editor.draft.rufin_filled(field) && editor.writable(field) {
                editor
                    .identified_originals
                    .insert(field, editor.draft.source_value(field));
                editor.touched.insert(field);
            }
        }
        editor
    }

    pub fn fields(&self) -> Vec<MetadataFieldState> {
        self.draft
            .field_keys()
            .into_iter()
            .map(|field| {
                let extra = match field {
                    MetadataField::Extra(index) => Some(&self.draft.extra()[index]),
                    MetadataField::Year => self.draft.date(),
                    _ => None,
                };
                MetadataFieldState {
                    field,
                    value: self.text(field).to_string(),
                    writable: self.writable(field),
                    label: extra.map(|value| tr(&value.label)).unwrap_or_else(|| {
                        if field == MetadataField::Comment
                            && !matches!(self.draft.as_ref(), MetadataDraft::Track(_))
                        {
                            tr(msgid("Overview"))
                        } else {
                            tr(field_title(field))
                        }
                    }),
                    mixed: extra
                        .map(|value| value.mixed)
                        .unwrap_or_else(|| self.draft.mixed(field)),
                    kind: extra
                        .map(|value| value.kind)
                        .unwrap_or_else(|| match field {
                            MetadataField::TrackNumber
                            | MetadataField::DiscNumber
                            | MetadataField::Year
                            | MetadataField::Bpm => MetadataFieldKind::Number,
                            _ => MetadataFieldKind::Text,
                        }),
                }
            })
            .collect()
    }

    pub fn text(&self, field: MetadataField) -> &str {
        self.values
            .get(&field)
            .expect("metadata field belongs to this draft")
    }

    pub fn has_field(&self, field: MetadataField) -> bool {
        self.values.contains_key(&field)
    }

    pub fn set_text(&mut self, field: MetadataField, value: String) {
        let current = self
            .values
            .get_mut(&field)
            .expect("metadata field belongs to this draft");
        if *current != value {
            *current = value;
            self.touched.insert(field);
        }
    }

    pub fn locked(&self) -> Option<bool> {
        self.locked
    }

    pub fn set_locked(&mut self, value: bool) {
        if self.locked != Some(value) {
            self.locked = Some(value);
            self.touched.insert(MetadataField::Locked);
        }
    }

    pub fn writable(&self, field: MetadataField) -> bool {
        if let MetadataField::Extra(index) = field {
            return self.draft.extra()[index].writable;
        }
        if field == MetadataField::Year
            && let Some(date) = self.draft.date()
        {
            return date.writable;
        }
        match self.draft.as_ref() {
            MetadataDraft::Track(value) => track_writable(&value.writable, field),
            MetadataDraft::Album(value) => album_writable(&value.writable, field),
            MetadataDraft::Artist(value) => artist_writable(&value.writable, field),
        }
    }

    pub fn is_identified(&self, field: MetadataField) -> bool {
        self.identified_originals.contains_key(&field)
    }

    pub fn undo_identified(&mut self, field: MetadataField) -> bool {
        let Some(original) = self.identified_originals.remove(&field) else {
            return false;
        };
        self.values.insert(field, original);
        self.touched.remove(&field);
        if self.identified_originals.is_empty() {
            self.token = None;
        }
        true
    }

    pub fn has_changes(&self) -> bool {
        !self.touched.is_empty() || self.token.is_some()
    }
    pub fn token(&self) -> Option<String> {
        self.token.clone()
    }

    pub fn values(&self) -> Result<MetadataValues, String> {
        match self.draft.as_ref() {
            MetadataDraft::Track(_) => Ok(MetadataValues::Track(track_values(self)?)),
            MetadataDraft::Album(_) => Ok(MetadataValues::Album(album_values(self)?)),
            MetadataDraft::Artist(_) => Ok(MetadataValues::Artist(artist_values(self)?)),
        }
    }

    pub fn edit(&self, artwork: Option<ArtworkEdit>) -> Result<MetadataEdit, String> {
        let extra = self
            .draft
            .field_keys()
            .into_iter()
            .filter(|field| self.touched.contains(field))
            .filter_map(|field| {
                let key = match field {
                    MetadataField::Extra(index) => &self.draft.extra()[index].key,
                    MetadataField::Year => &self.draft.date()?.key,
                    _ => return None,
                };
                Some((key.clone(), self.text(field).trim().to_string()))
            })
            .collect();
        match self.draft.as_ref() {
            MetadataDraft::Track(_) => Ok(MetadataEdit::Track(TrackMetadataEdit {
                values: track_values(self)?,
                changed: track_changed(self),
                artwork,
                extra,
            })),
            MetadataDraft::Album(_) => Ok(MetadataEdit::Album(AlbumMetadataEdit {
                values: album_values(self)?,
                changed: album_changed(self),
                artwork,
                extra,
            })),
            MetadataDraft::Artist(_) => Ok(MetadataEdit::Artist(ArtistMetadataEdit {
                values: artist_values(self)?,
                changed: artist_changed(self),
                artwork,
                extra,
            })),
        }
    }

    pub fn apply_identified(&mut self, values: MetadataValues, token: Option<String>) {
        for field in self.draft.field_keys() {
            if matches!(field, MetadataField::Extra(_)) || !self.writable(field) {
                continue;
            }
            let value = match &values {
                MetadataValues::Track(values) => track_value(values, field),
                MetadataValues::Album(values) => album_value(values, field),
                MetadataValues::Artist(values) => artist_value(values, field),
            };
            let current = self.text(field);
            // Identify supplies a year. Keep a more precise date when that year already matches.
            if field == MetadataField::Year
                && self.draft.date().is_some()
                && current.split('-').next() == Some(value.as_str())
            {
                continue;
            }
            if current == value {
                continue;
            }
            let original = current.to_string();
            self.identified_originals.entry(field).or_insert(original);
            self.set_text(field, value);
        }
        self.token = token;
    }

    pub fn record_save_result(&mut self, result: &Result<(), SourceMetadataError>) {
        self.committed = match result {
            Ok(()) | Err(SourceMetadataError::SavedRefreshFailed(_)) => Some(true),
            Err(SourceMetadataError::PartiallySaved { .. }) => Some(false),
            Err(_) => None,
        };
    }

    /// A committed write cannot be submitted again from the same source revision.
    pub fn committed(&self) -> Option<bool> {
        self.committed
    }
}

impl MetadataDraft {
    fn field_keys(&self) -> Vec<MetadataField> {
        use MetadataField::*;
        let fields: &[MetadataField] = match self {
            Self::Track(_) => &[
                Title,
                SortTitle,
                Artist,
                Album,
                AlbumArtist,
                TrackNumber,
                DiscNumber,
                Year,
                Genre,
                Comment,
                Bpm,
                MusicBrainzRecordingId,
                MusicBrainzReleaseTrackId,
                MusicBrainzAlbumId,
                MusicBrainzReleaseGroupId,
                MusicBrainzArtistId,
            ],
            Self::Album(_) => &[
                Title,
                SortTitle,
                Artist,
                AlbumArtist,
                Year,
                Genre,
                Comment,
                MusicBrainzAlbumId,
                MusicBrainzReleaseGroupId,
            ],
            Self::Artist(_) => &[Title, SortTitle, Genre, Comment, MusicBrainzArtistId],
        };
        fields
            .iter()
            .copied()
            .chain(
                self.extra()
                    .iter()
                    .enumerate()
                    .filter(|(_, field)| field.key != "recording_date")
                    .map(|(index, _)| Extra(index)),
            )
            .collect()
    }

    fn display_value(&self, field: MetadataField) -> String {
        if let MetadataField::Extra(index) = field {
            return self.extra()[index].value.clone();
        }
        match self {
            Self::Track(value) => track_value(&value.values, field),
            Self::Album(value) => album_value(&value.values, field),
            Self::Artist(value) => artist_value(&value.values, field),
        }
    }

    fn mixed(&self, field: MetadataField) -> bool {
        use MetadataField::*;
        match self {
            Self::Track(_) => false,
            Self::Album(value) => match field {
                Title => value.mixed.title,
                SortTitle => value.mixed.sort_title,
                Artist => value.mixed.artist,
                AlbumArtist => value.mixed.album_artist,
                Year => value.mixed.year,
                Genre => value.mixed.genre,
                Comment => value.mixed.comment,
                MusicBrainzAlbumId => value.mixed.musicbrainz_album_id,
                MusicBrainzReleaseGroupId => value.mixed.musicbrainz_release_group_id,
                _ => false,
            },
            Self::Artist(value) => match field {
                Title => value.mixed.name,
                SortTitle => value.mixed.sort_name,
                Genre => value.mixed.genre,
                Comment => value.mixed.comment,
                MusicBrainzArtistId => value.mixed.musicbrainz_artist_id,
                _ => false,
            },
        }
    }
    pub fn extra(&self) -> &[sources::MetadataField] {
        match self {
            Self::Track(value) => &value.extra,
            Self::Album(value) => &value.extra,
            Self::Artist(value) => &value.extra,
        }
    }

    pub fn date(&self) -> Option<&sources::MetadataField> {
        self.extra()
            .iter()
            .find(|field| field.key == "recording_date")
    }

    pub fn artwork(&self) -> &sources::ArtworkEditing {
        match self {
            Self::Track(value) => &value.artwork,
            Self::Album(value) => &value.artwork,
            Self::Artist(value) => &value.artwork,
        }
    }

    pub fn source_search(&self) -> bool {
        match self {
            Self::Track(value) => value.source_search,
            Self::Album(value) => value.source_search,
            Self::Artist(value) => value.source_search,
        }
    }
    pub fn revision(&self) -> Option<String> {
        match self {
            Self::Track(value) => value.revision.clone(),
            Self::Album(value) => value.revision.clone(),
            Self::Artist(value) => value.revision.clone(),
        }
    }
    pub fn track_count(&self) -> usize {
        match self {
            Self::Track(_) => 1,
            Self::Album(value) => value.track_count,
            Self::Artist(value) => value.track_count,
        }
    }

    pub fn source_value(&self, field: MetadataField) -> String {
        if let MetadataField::Extra(index) = field {
            return self.extra()[index].value.clone();
        }
        if field == MetadataField::Year {
            if let Some(date) = self.date() {
                return date.value.clone();
            }
        }
        match self {
            Self::Track(value) => track_value(&value.source_values, field),
            Self::Album(value) => album_value(&value.source_values, field),
            Self::Artist(value) => artist_value(&value.source_values, field),
        }
    }

    pub fn rufin_filled(&self, field: MetadataField) -> bool {
        if matches!(field, MetadataField::Extra(_)) {
            return false;
        }
        match self {
            Self::Track(value) => track_writable(&value.rufin_filled, field),
            Self::Album(value) => album_writable(&value.rufin_filled, field),
            Self::Artist(value) => artist_writable(&value.rufin_filled, field),
        }
    }
}

pub enum MetadataValues {
    Track(TrackMetadataValues),
    Album(AlbumMetadataValues),
    Artist(ArtistMetadataValues),
}

impl MetadataValues {
    pub fn identify(self, owner: &SourceOwner, media_uri: String) -> MetadataIdentifyReceiver {
        match self {
            Self::Track(values) => MetadataIdentifyReceiver::Track(super::identify_track_metadata(
                owner, media_uri, values,
            )),
            Self::Album(values) => MetadataIdentifyReceiver::Album(super::identify_album_metadata(
                owner, media_uri, values,
            )),
            Self::Artist(values) => MetadataIdentifyReceiver::Artist(
                super::identify_artist_metadata(owner, media_uri, values),
            ),
        }
    }
    pub fn title(&self) -> &str {
        match self {
            Self::Track(values) => &values.title,
            Self::Album(values) => &values.title,
            Self::Artist(values) => &values.name,
        }
    }

    pub fn has_exact_musicbrainz_identity(&self) -> bool {
        match self {
            Self::Track(values) => {
                values
                    .musicbrainz_recording_id
                    .as_deref()
                    .is_some_and(usable_identity)
                    || values
                        .musicbrainz_release_track_id
                        .as_deref()
                        .is_some_and(usable_identity)
            }
            Self::Album(values) => {
                values
                    .musicbrainz_album_id
                    .as_deref()
                    .is_some_and(usable_identity)
                    || values
                        .musicbrainz_release_group_id
                        .as_deref()
                        .is_some_and(usable_identity)
            }
            Self::Artist(values) => values
                .musicbrainz_artist_id
                .as_deref()
                .is_some_and(usable_identity),
        }
    }
}

fn usable_identity(value: &str) -> bool {
    !value.trim().is_empty()
}

pub fn identification_available(
    source_search: bool,
    external_lookup_allowed: bool,
    values: &MetadataValues,
) -> bool {
    source_search && !values.title().trim().is_empty()
        || external_lookup_allowed && values.has_exact_musicbrainz_identity()
}

pub enum MetadataIdentifyReceiver {
    Track(async_channel::Receiver<Result<Option<(TrackMetadataValues, Option<String>)>, String>>),
    Album(async_channel::Receiver<Result<Option<(AlbumMetadataValues, Option<String>)>, String>>),
    Artist(async_channel::Receiver<Result<Option<(ArtistMetadataValues, Option<String>)>, String>>),
}

pub struct MetadataIdentification {
    pub values: MetadataValues,
    pub token: Option<String>,
}

impl MetadataIdentifyReceiver {
    pub async fn recv(self) -> Result<Option<MetadataIdentification>, String> {
        let unavailable = || tr(msgid("Metadata editing is no longer available"));
        match self {
            Self::Track(receiver) => {
                receiver
                    .recv()
                    .await
                    .map_err(|_| unavailable())?
                    .map(|value| {
                        value.map(|(values, token)| MetadataIdentification {
                            values: MetadataValues::Track(values),
                            token,
                        })
                    })
            }
            Self::Album(receiver) => {
                receiver
                    .recv()
                    .await
                    .map_err(|_| unavailable())?
                    .map(|value| {
                        value.map(|(values, token)| MetadataIdentification {
                            values: MetadataValues::Album(values),
                            token,
                        })
                    })
            }
            Self::Artist(receiver) => {
                receiver
                    .recv()
                    .await
                    .map_err(|_| unavailable())?
                    .map(|value| {
                        value.map(|(values, token)| MetadataIdentification {
                            values: MetadataValues::Artist(values),
                            token,
                        })
                    })
            }
        }
    }
}
fn track_values(editor: &MetadataEditor) -> Result<TrackMetadataValues, String> {
    let MetadataDraft::Track(draft) = editor.draft.as_ref() else {
        unreachable!()
    };
    let mut values = draft.source_values.clone();
    apply_text(editor, MetadataField::Title, &mut values.title)?;
    apply_optional(editor, MetadataField::SortTitle, &mut values.sort_title);
    apply_optional(editor, MetadataField::Artist, &mut values.artist);
    apply_optional(editor, MetadataField::Album, &mut values.album);
    apply_optional(editor, MetadataField::AlbumArtist, &mut values.album_artist);
    apply_number(editor, MetadataField::TrackNumber, &mut values.track_number)?;
    apply_number(editor, MetadataField::DiscNumber, &mut values.disc_number)?;
    apply_number(editor, MetadataField::Year, &mut values.year)?;
    apply_optional(editor, MetadataField::Genre, &mut values.genre);
    apply_optional(editor, MetadataField::Comment, &mut values.comment);
    apply_number(editor, MetadataField::Bpm, &mut values.bpm)?;
    apply_optional(
        editor,
        MetadataField::MusicBrainzRecordingId,
        &mut values.musicbrainz_recording_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzReleaseTrackId,
        &mut values.musicbrainz_release_track_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzAlbumId,
        &mut values.musicbrainz_album_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzReleaseGroupId,
        &mut values.musicbrainz_release_group_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzArtistId,
        &mut values.musicbrainz_artist_id,
    );
    if editor.touched.contains(&MetadataField::Locked) {
        values.locked = editor.locked;
    }
    Ok(values)
}
fn album_values(editor: &MetadataEditor) -> Result<AlbumMetadataValues, String> {
    let MetadataDraft::Album(draft) = editor.draft.as_ref() else {
        unreachable!()
    };
    let mut values = draft.source_values.clone();
    apply_text(editor, MetadataField::Title, &mut values.title)?;
    apply_optional(editor, MetadataField::SortTitle, &mut values.sort_title);
    apply_optional(editor, MetadataField::Artist, &mut values.artist);
    apply_optional(editor, MetadataField::AlbumArtist, &mut values.album_artist);
    apply_number(editor, MetadataField::Year, &mut values.year)?;
    apply_optional(editor, MetadataField::Genre, &mut values.genre);
    apply_optional(editor, MetadataField::Comment, &mut values.comment);
    apply_optional(
        editor,
        MetadataField::MusicBrainzAlbumId,
        &mut values.musicbrainz_album_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzReleaseGroupId,
        &mut values.musicbrainz_release_group_id,
    );
    if editor.touched.contains(&MetadataField::Locked) {
        values.locked = editor.locked;
    }
    Ok(values)
}
fn artist_values(editor: &MetadataEditor) -> Result<ArtistMetadataValues, String> {
    let MetadataDraft::Artist(draft) = editor.draft.as_ref() else {
        unreachable!()
    };
    let mut values = draft.source_values.clone();
    apply_text(editor, MetadataField::Title, &mut values.name)?;
    apply_optional(editor, MetadataField::SortTitle, &mut values.sort_name);
    apply_optional(editor, MetadataField::Genre, &mut values.genre);
    apply_optional(editor, MetadataField::Comment, &mut values.comment);
    apply_optional(
        editor,
        MetadataField::MusicBrainzArtistId,
        &mut values.musicbrainz_artist_id,
    );
    if editor.touched.contains(&MetadataField::Locked) {
        values.locked = editor.locked;
    }
    Ok(values)
}

fn apply_text(
    editor: &MetadataEditor,
    field: MetadataField,
    target: &mut String,
) -> Result<(), String> {
    if !editor.touched.contains(&field) {
        return Ok(());
    }
    let value = editor.text(field).trim().to_string();
    if value.is_empty() {
        return Err(tr(msgid("Add a title")));
    }
    *target = value;
    Ok(())
}
fn apply_optional(editor: &MetadataEditor, field: MetadataField, target: &mut Option<String>) {
    if editor.touched.contains(&field) {
        let value = editor.text(field).trim().to_string();
        *target = (!value.is_empty()).then_some(value);
    }
}
fn apply_number(
    editor: &MetadataEditor,
    field: MetadataField,
    target: &mut Option<u16>,
) -> Result<(), String> {
    if !editor.touched.contains(&field) {
        return Ok(());
    }
    let value = editor.text(field).trim().to_string();
    if field == MetadataField::Year && editor.draft.date().is_some() {
        *target = value.split('-').next().and_then(|year| year.parse().ok());
        return Ok(());
    }
    *target = if value.is_empty() {
        None
    } else {
        Some(
            value
                .parse()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| tr(msgid("Use a number above zero")))?,
        )
    };
    Ok(())
}

fn track_changed(editor: &MetadataEditor) -> TrackMetadataWritable {
    let touched = &editor.touched;
    let mut changed = TrackMetadataWritable::default();
    for field in touched
        .iter()
        .copied()
        .filter(|field| editor.writable(*field))
    {
        match field {
            MetadataField::Title => changed.title = true,
            MetadataField::SortTitle => changed.sort_title = true,
            MetadataField::Artist => changed.artist = true,
            MetadataField::Album => changed.album = true,
            MetadataField::AlbumArtist => changed.album_artist = true,
            MetadataField::TrackNumber => changed.track_number = true,
            MetadataField::DiscNumber => changed.disc_number = true,
            MetadataField::Year => changed.year = editor.draft.date().is_none(),
            MetadataField::Genre => changed.genre = true,
            MetadataField::Comment => changed.comment = true,
            MetadataField::Bpm => changed.bpm = true,
            MetadataField::MusicBrainzRecordingId => changed.musicbrainz_recording_id = true,
            MetadataField::MusicBrainzReleaseTrackId => changed.musicbrainz_release_track_id = true,
            MetadataField::MusicBrainzAlbumId => changed.musicbrainz_album_id = true,
            MetadataField::MusicBrainzReleaseGroupId => changed.musicbrainz_release_group_id = true,
            MetadataField::MusicBrainzArtistId => changed.musicbrainz_artist_id = true,
            MetadataField::Locked => changed.locked = true,
            MetadataField::Extra(_) => {}
        }
    }
    changed
}
fn album_changed(editor: &MetadataEditor) -> AlbumMetadataWritable {
    let touched = &editor.touched;
    let mut changed = AlbumMetadataWritable::default();
    for field in touched
        .iter()
        .copied()
        .filter(|field| editor.writable(*field))
    {
        match field {
            MetadataField::Title => changed.title = true,
            MetadataField::SortTitle => changed.sort_title = true,
            MetadataField::Artist => changed.artist = true,
            MetadataField::AlbumArtist => changed.album_artist = true,
            MetadataField::Year => changed.year = editor.draft.date().is_none(),
            MetadataField::Genre => changed.genre = true,
            MetadataField::Comment => changed.comment = true,
            MetadataField::MusicBrainzAlbumId => changed.musicbrainz_album_id = true,
            MetadataField::MusicBrainzReleaseGroupId => changed.musicbrainz_release_group_id = true,
            MetadataField::Locked => changed.locked = true,
            _ => {}
        }
    }
    changed
}
fn artist_changed(editor: &MetadataEditor) -> ArtistMetadataWritable {
    let touched = &editor.touched;
    let mut changed = ArtistMetadataWritable::default();
    for field in touched
        .iter()
        .copied()
        .filter(|field| editor.writable(*field))
    {
        match field {
            MetadataField::Title => changed.name = true,
            MetadataField::SortTitle => changed.sort_name = true,
            MetadataField::Genre => changed.genre = true,
            MetadataField::Comment => changed.comment = true,
            MetadataField::MusicBrainzArtistId => changed.musicbrainz_artist_id = true,
            MetadataField::Locked => changed.locked = true,
            _ => {}
        }
    }
    changed
}

fn track_writable(value: &TrackMetadataWritable, field: MetadataField) -> bool {
    match field {
        MetadataField::Title => value.title,
        MetadataField::SortTitle => value.sort_title,
        MetadataField::Artist => value.artist,
        MetadataField::Album => value.album,
        MetadataField::AlbumArtist => value.album_artist,
        MetadataField::TrackNumber => value.track_number,
        MetadataField::DiscNumber => value.disc_number,
        MetadataField::Year => value.year,
        MetadataField::Genre => value.genre,
        MetadataField::Comment => value.comment,
        MetadataField::Bpm => value.bpm,
        MetadataField::MusicBrainzRecordingId => value.musicbrainz_recording_id,
        MetadataField::MusicBrainzReleaseTrackId => value.musicbrainz_release_track_id,
        MetadataField::MusicBrainzAlbumId => value.musicbrainz_album_id,
        MetadataField::MusicBrainzReleaseGroupId => value.musicbrainz_release_group_id,
        MetadataField::MusicBrainzArtistId => value.musicbrainz_artist_id,
        MetadataField::Locked => value.locked,
        MetadataField::Extra(_) => false,
    }
}
fn album_writable(value: &AlbumMetadataWritable, field: MetadataField) -> bool {
    match field {
        MetadataField::Title => value.title,
        MetadataField::SortTitle => value.sort_title,
        MetadataField::Artist => value.artist,
        MetadataField::AlbumArtist => value.album_artist,
        MetadataField::Year => value.year,
        MetadataField::Genre => value.genre,
        MetadataField::Comment => value.comment,
        MetadataField::MusicBrainzAlbumId => value.musicbrainz_album_id,
        MetadataField::MusicBrainzReleaseGroupId => value.musicbrainz_release_group_id,
        MetadataField::Locked => value.locked,
        _ => false,
    }
}
fn artist_writable(value: &ArtistMetadataWritable, field: MetadataField) -> bool {
    match field {
        MetadataField::Title => value.name,
        MetadataField::SortTitle => value.sort_name,
        MetadataField::Genre => value.genre,
        MetadataField::Comment => value.comment,
        MetadataField::MusicBrainzArtistId => value.musicbrainz_artist_id,
        MetadataField::Locked => value.locked,
        _ => false,
    }
}

fn track_value(values: &TrackMetadataValues, field: MetadataField) -> String {
    match field {
        MetadataField::Title => values.title.clone(),
        MetadataField::SortTitle => values.sort_title.clone().unwrap_or_default(),
        MetadataField::Artist => values.artist.clone().unwrap_or_default(),
        MetadataField::Album => values.album.clone().unwrap_or_default(),
        MetadataField::AlbumArtist => values.album_artist.clone().unwrap_or_default(),
        MetadataField::TrackNumber => values
            .track_number
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::DiscNumber => values
            .disc_number
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::Year => values
            .year
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::Genre => values.genre.clone().unwrap_or_default(),
        MetadataField::Comment => values.comment.clone().unwrap_or_default(),
        MetadataField::Bpm => values
            .bpm
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::MusicBrainzRecordingId => {
            values.musicbrainz_recording_id.clone().unwrap_or_default()
        }
        MetadataField::MusicBrainzReleaseTrackId => values
            .musicbrainz_release_track_id
            .clone()
            .unwrap_or_default(),
        MetadataField::MusicBrainzAlbumId => {
            values.musicbrainz_album_id.clone().unwrap_or_default()
        }
        MetadataField::MusicBrainzReleaseGroupId => values
            .musicbrainz_release_group_id
            .clone()
            .unwrap_or_default(),
        MetadataField::MusicBrainzArtistId => {
            values.musicbrainz_artist_id.clone().unwrap_or_default()
        }
        MetadataField::Locked | MetadataField::Extra(_) => String::new(),
    }
}

fn album_value(values: &AlbumMetadataValues, field: MetadataField) -> String {
    match field {
        MetadataField::Title => values.title.clone(),
        MetadataField::SortTitle => values.sort_title.clone().unwrap_or_default(),
        MetadataField::Artist => values.artist.clone().unwrap_or_default(),
        MetadataField::AlbumArtist => values.album_artist.clone().unwrap_or_default(),
        MetadataField::Year => values
            .year
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::Genre => values.genre.clone().unwrap_or_default(),
        MetadataField::Comment => values.comment.clone().unwrap_or_default(),
        MetadataField::MusicBrainzAlbumId => {
            values.musicbrainz_album_id.clone().unwrap_or_default()
        }
        MetadataField::MusicBrainzReleaseGroupId => values
            .musicbrainz_release_group_id
            .clone()
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn artist_value(values: &ArtistMetadataValues, field: MetadataField) -> String {
    match field {
        MetadataField::Title => values.name.clone(),
        MetadataField::SortTitle => values.sort_name.clone().unwrap_or_default(),
        MetadataField::Genre => values.genre.clone().unwrap_or_default(),
        MetadataField::Comment => values.comment.clone().unwrap_or_default(),
        MetadataField::MusicBrainzArtistId => {
            values.musicbrainz_artist_id.clone().unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn field_title(field: MetadataField) -> &'static str {
    match field {
        MetadataField::Title => msgid("Title"),
        MetadataField::SortTitle => msgid("Sort title"),
        MetadataField::Artist => msgid("Artists"),
        MetadataField::Album => msgid("Album"),
        MetadataField::AlbumArtist => msgid("Album artists"),
        MetadataField::TrackNumber => msgid("Track number"),
        MetadataField::DiscNumber => msgid("Disc number"),
        MetadataField::Year => msgid("Year"),
        MetadataField::Genre => msgid("Genres"),
        MetadataField::Comment => msgid("Comment"),
        MetadataField::Bpm => msgid("BPM"),
        MetadataField::MusicBrainzRecordingId => msgid("MusicBrainz recording ID"),
        MetadataField::MusicBrainzReleaseTrackId => msgid("MusicBrainz release track ID"),
        MetadataField::MusicBrainzAlbumId => msgid("MusicBrainz release ID"),
        MetadataField::MusicBrainzReleaseGroupId => msgid("MusicBrainz release group ID"),
        MetadataField::MusicBrainzArtistId => msgid("MusicBrainz artist ID"),
        MetadataField::Locked => msgid("Lock metadata"),
        MetadataField::Extra(_) => unreachable!("extra fields carry their own label"),
    }
}

pub enum MetadataReceiver {
    Track(async_channel::Receiver<Result<TrackMetadata, SourceMetadataError>>),
    Album(async_channel::Receiver<Result<AlbumMetadata, SourceMetadataError>>),
    Artist(async_channel::Receiver<Result<ArtistMetadata, SourceMetadataError>>),
}

impl MetadataReceiver {
    pub async fn recv(self) -> Result<MetadataDraft, SourceMetadataError> {
        match self {
            Self::Track(receiver) => receiver
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map(MetadataDraft::Track),
            Self::Album(receiver) => receiver
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map(MetadataDraft::Album),
            Self::Artist(receiver) => receiver
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map(MetadataDraft::Artist),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_mode_keeps_provider_source_search_available() {
        let values = MetadataValues::Track(TrackMetadataValues {
            title: "Track".to_string(),
            ..TrackMetadataValues::default()
        });
        assert!(identification_available(true, false, &values));
        assert!(!identification_available(false, false, &values));
    }
}
