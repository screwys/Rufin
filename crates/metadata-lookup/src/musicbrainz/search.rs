use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MetadataMatchKind {
    Recording,
    Release,
    ReleaseGroup,
    Artist,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataCandidate {
    pub id: String,
    pub kind: MetadataMatchKind,
    pub title: String,
    pub detail: String,
    pub from_fingerprint: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct MetadataLookup {
    kind: MetadataMatchKind,
    value: Value,
}

pub fn search_metadata(
    cache: &crate::LookupCache,
    kind: SourceEntityKind,
    values: &MetadataChanges,
    query: &str,
) -> Result<Option<Vec<MetadataCandidate>>, String> {
    let value = |key| {
        values
            .get(key)
            .map(String::as_str)
            .unwrap_or_default()
            .trim()
    };
    let artist = if value("album_artist").is_empty() {
        value("artist")
    } else {
        value("album_artist")
    };
    let album = if kind == SourceEntityKind::Album {
        value("title")
    } else {
        value("album")
    };
    let (endpoint, collection, match_kind) = match kind {
        SourceEntityKind::Artist => (MUSICBRAINZ_ARTIST_URL, "artists", MetadataMatchKind::Artist),
        SourceEntityKind::Album => (
            MUSICBRAINZ_RELEASE_SEARCH_URL,
            "releases",
            MetadataMatchKind::Release,
        ),
        SourceEntityKind::Track if !album.is_empty() => (
            MUSICBRAINZ_RELEASE_SEARCH_URL,
            "releases",
            MetadataMatchKind::Release,
        ),
        SourceEntityKind::Track => (
            MUSICBRAINZ_RECORDING_URL,
            "recordings",
            MetadataMatchKind::Recording,
        ),
    };
    let query = if query.trim().is_empty() {
        let mut fields = Vec::new();
        let name = if match_kind == MetadataMatchKind::Release {
            album
        } else {
            value("title")
        };
        if !name.is_empty() {
            let field = match match_kind {
                MetadataMatchKind::Artist => "artist",
                MetadataMatchKind::Recording => "recording",
                _ => "release",
            };
            fields.push(format!("{field}:\"{}\"", musicbrainz_phrase(name)));
        }
        if kind != SourceEntityKind::Artist && !artist.is_empty() {
            for artist in artist
                .split(';')
                .map(str::trim)
                .filter(|artist| !artist.is_empty())
            {
                fields.push(format!("artist:\"{}\"", musicbrainz_phrase(artist)));
            }
        }
        fields.join(" AND ")
    } else {
        query.trim().to_string()
    };
    if query.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let url = Url::parse_with_params(
        endpoint,
        [("query", query.as_str()), ("fmt", "json"), ("limit", "20")],
    )
    .map_err(|e| e.to_string())?;
    let key = crate::cache::cache_key("musicbrainz-search", &url.as_str());
    let Some(data) = cache.fetch(&key, || {
        fetch_musicbrainz_json(client()?, url, "MusicBrainz search")
    })?
    else {
        return Ok(None);
    };
    Ok(Some(
        data.get(collection)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entity| {
                let id = text(entity, "id")?;
                let title = text(
                    entity,
                    if kind == SourceEntityKind::Artist {
                        "name"
                    } else {
                        "title"
                    },
                )?;
                let mut detail = Vec::new();
                if let Some(artist) = artist_credit(entity) {
                    detail.push(artist);
                }
                for key in ["date", "country", "disambiguation"] {
                    if let Some(value) = text(entity, key) {
                        detail.push(value);
                    }
                }
                if let Some(media) = entity.get("media").and_then(Value::as_array) {
                    for medium in media {
                        if let Some(format) = text(medium, "format") {
                            detail.push(format);
                        }
                    }
                }
                Some(MetadataCandidate {
                    id,
                    from_fingerprint: false,
                    kind: match_kind,
                    title,
                    detail: detail.join(" · "),
                })
            })
            .collect(),
    ))
}

pub fn lookup_metadata(
    cache: &crate::LookupCache,
    candidate: &MetadataCandidate,
) -> Result<Option<MetadataLookup>, String> {
    let (root, inc) = match candidate.kind {
        MetadataMatchKind::Release => (
            MUSICBRAINZ_RELEASE_SEARCH_URL,
            "artist-credits+recordings+release-groups+genres+media+labels",
        ),
        MetadataMatchKind::ReleaseGroup => (
            MUSICBRAINZ_RELEASE_GROUP_SEARCH_URL,
            "artist-credits+genres",
        ),
        MetadataMatchKind::Recording => (MUSICBRAINZ_RECORDING_URL, "artist-credits+genres"),
        MetadataMatchKind::Artist => (MUSICBRAINZ_ARTIST_URL, "genres"),
    };
    let key = crate::cache::cache_key("musicbrainz-entity", &(root, &candidate.id, inc));
    cache.fetch(&key, || {
        let value = fetch_musicbrainz_entity(root, &candidate.id, inc, "MusicBrainz lookup")?
            .ok_or_else(|| "MusicBrainz did not return this result".to_string())?;
        Ok(MetadataLookup {
            kind: candidate.kind,
            value,
        })
    })
}

impl MetadataLookup {
    pub fn artists(&self) -> Vec<MetadataCandidate> {
        self.value
            .get("artist-credit")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|credit| {
                let artist = credit.get("artist")?;
                Some(MetadataCandidate {
                    id: text(artist, "id")?,
                    kind: MetadataMatchKind::Artist,
                    title: text(artist, "name")?,
                    detail: text(artist, "disambiguation").unwrap_or_default(),
                    from_fingerprint: true,
                })
            })
            .collect()
    }

    pub fn changes(
        &self,
        kind: SourceEntityKind,
        values: &MetadataChanges,
        recordings: &[String],
    ) -> Option<MetadataChanges> {
        let id = text(&self.value, "id")?;
        match self.kind {
            MetadataMatchKind::Artist => Some(observations([
                ("title", text(&self.value, "name")),
                ("sort_title", text(&self.value, "sort-name")),
                ("genre", genres(&self.value)),
                ("musicbrainz_artist_id", Some(id)),
            ])),
            MetadataMatchKind::Release if kind == SourceEntityKind::Track => {
                let track_id = match_release_track(&self.value, values, recordings)?;
                track_from_release(&self.value, &track_id)
            }
            MetadataMatchKind::Release => Some(album_from_release(&self.value, &id)),
            MetadataMatchKind::Recording => {
                let mut changes = observations([
                    ("title", text(&self.value, "title")),
                    ("artist", artist_credit(&self.value)),
                    ("genre", genres(&self.value)),
                    ("musicbrainz_recording_id", Some(id)),
                ]);
                add_credit_ids(&mut changes, &self.value, "musicbrainz_artist_id");
                Some(changes)
            }
            MetadataMatchKind::ReleaseGroup => {
                let mut changes = observations([
                    ("title", text(&self.value, "title")),
                    ("artist", artist_credit(&self.value)),
                    ("album_artist", artist_credit(&self.value)),
                    ("genre", genres(&self.value)),
                    ("musicbrainz_release_group_id", Some(id)),
                    (
                        "year",
                        year(&self.value, "first-release-date").map(|year| year.to_string()),
                    ),
                    (
                        "original_release_date",
                        text(&self.value, "first-release-date"),
                    ),
                ]);
                add_credit_ids(&mut changes, &self.value, "musicbrainz_album_artist_id");
                Some(changes)
            }
        }
    }
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn match_release_track(
    release: &Value,
    values: &MetadataChanges,
    recordings: &[String],
) -> Option<String> {
    let mut tracks = release
        .get("media")?
        .as_array()?
        .iter()
        .flat_map(|medium| {
            medium
                .get("tracks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |track| (medium, track))
        })
        .collect::<Vec<_>>();
    let value = |key| values.get(key).map(String::as_str).unwrap_or_default();
    let unique = |matches: Vec<(&Value, &Value)>| {
        if matches.len() == 1 {
            text(matches[0].1, "id")
        } else {
            None
        }
    };
    if !recordings.is_empty() {
        tracks.retain(|(_, track)| {
            track
                .pointer("/recording/id")
                .and_then(Value::as_str)
                .is_some_and(|id| recordings.iter().any(|recording| recording == id))
        });
        if let Some(id) = unique(tracks.clone()) {
            return Some(id);
        }
        if tracks.is_empty() {
            return None;
        }
    }
    for key in ["musicbrainz_release_track_id", "musicbrainz_recording_id"] {
        if !value(key).is_empty() {
            let found = tracks
                .iter()
                .copied()
                .filter(|(_, track)| {
                    let id = if key == "musicbrainz_release_track_id" {
                        track.get("id")
                    } else {
                        track.pointer("/recording/id")
                    };
                    id.and_then(Value::as_str) == Some(value(key))
                })
                .collect();
            if let Some(id) = unique(found) {
                return Some(id);
            }
        }
    }
    // Release positions distinguish repeated titles and recordings on the same album.
    if let Ok(number) = value("track_number").parse::<u64>() {
        let disc = value("disc_number").parse::<u64>().ok();
        let found = tracks
            .iter()
            .copied()
            .filter(|(medium, track)| {
                track.get("position").and_then(Value::as_u64) == Some(number)
                    && disc.is_none_or(|disc| {
                        medium.get("position").and_then(Value::as_u64) == Some(disc)
                    })
            })
            .collect();
        if let Some(id) = unique(found) {
            return Some(id);
        }
    }
    let title = normalized(value("title"));
    if title.is_empty() {
        return None;
    }
    unique(
        tracks
            .into_iter()
            .filter(|(_, track)| {
                text(track, "title")
                    .or_else(|| track.get("recording").and_then(|r| text(r, "title")))
                    .is_some_and(|name| normalized(&name) == title)
            })
            .collect(),
    )
}

pub(super) fn add_credit_ids(changes: &mut MetadataChanges, entity: &Value, key: &str) {
    let ids = entity
        .get("artist-credit")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|credit| credit.pointer("/artist/id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        changes.insert(key.into(), ids.join("; "));
    }
}
