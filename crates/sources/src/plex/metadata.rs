use super::*;
use crate::remote_json::{field, id, items};
use crate::{
    MetadataDraft, MetadataEdit, MetadataField, SourceEntityKind, SourceMetadataError as Error,
};
fn error(error: SourceError) -> Error {
    Error::Write(error.to_string())
}
fn text(item: &Value, key: &str) -> Option<String> {
    item[key].as_str().map(str::to_string)
}
fn tags(item: &Value, key: &str) -> Option<String> {
    let names: Vec<_> = items(&item[key])
        .iter()
        .filter_map(|genre| genre["tag"].as_str())
        .collect();
    (!names.is_empty()).then(|| names.join("; "))
}
fn revision(item: &Value) -> String {
    id(&item["updatedAt"]).unwrap_or_default()
}

const EXTRA_FIELDS: &[(&str, &str, &str, crate::MetadataFieldKind, u8)] = &[
    (
        "mood[0].tag",
        "Mood",
        "Mood",
        crate::MetadataFieldKind::List,
        7,
    ),
    (
        "collection[0].tag",
        "Collections",
        "Collection",
        crate::MetadataFieldKind::List,
        7,
    ),
    (
        "label[0].tag",
        "Labels",
        "Label",
        crate::MetadataFieldKind::List,
        7,
    ),
    (
        "originallyAvailableAt",
        "Release date",
        "originallyAvailableAt",
        crate::MetadataFieldKind::Date,
        2,
    ),
    (
        "studio",
        "Studio",
        "studio",
        crate::MetadataFieldKind::Text,
        2,
    ),
    (
        "style[0].tag",
        "Style",
        "Style",
        crate::MetadataFieldKind::List,
        6,
    ),
    (
        "country[0].tag",
        "Country",
        "Country",
        crate::MetadataFieldKind::List,
        4,
    ),
    (
        "similar[0].tag",
        "Similar artists",
        "Similar",
        crate::MetadataFieldKind::List,
        4,
    ),
];

fn extra_fields(item: &Value, writable: bool, scope: u8) -> Vec<crate::MetadataField> {
    EXTRA_FIELDS
        .iter()
        .filter(|(_, _, _, _, scopes)| scopes & scope != 0)
        .map(|&(key, label, json, kind, _)| crate::MetadataField {
            key: key.into(),
            label: label.into(),
            kind,
            value: if kind == crate::MetadataFieldKind::List {
                tags(item, json)
            } else {
                text(item, json)
            }
            .unwrap_or_default(),
            writable,
            mixed: false,
            suggestion: None,
        })
        .collect()
}

// Each ordinary field has one native read key and, when supported, one write key.
const FIELDS: &[(&str, &str, &str, u8, u8)] = &[
    ("title", "title", "title", 7, 7),
    ("sort_title", "titleSort", "titleSort", 7, 7),
    ("artist", "originalTitle", "originalTitle", 3, 1),
    ("album", "parentTitle", "", 1, 0),
    ("album_artist", "grandparentTitle", "", 3, 0),
    ("track_number", "index", "index", 1, 1),
    ("disc_number", "parentIndex", "parentIndex", 1, 1),
    ("year", "year", "year", 3, 2),
    ("genre", "Genre", "genre[0].tag", 7, 7),
    ("comment", "summary", "summary", 7, 7),
    ("bpm", "", "", 1, 0),
    ("locked", "", "", 7, 0),
    ("musicbrainz_recording_id", "", "", 1, 0),
    ("musicbrainz_release_track_id", "", "", 1, 0),
    ("musicbrainz_album_id", "", "", 3, 0),
    ("musicbrainz_release_group_id", "", "", 3, 0),
    ("musicbrainz_artist_id", "", "", 5, 0),
];

fn scope(kind: SourceEntityKind) -> u8 {
    match kind {
        SourceEntityKind::Track => 1,
        SourceEntityKind::Album => 2,
        SourceEntityKind::Artist => 4,
    }
}

impl PlexSource {
    pub async fn write_artwork(
        &self,
        object: &str,
        edit: Option<&crate::ArtworkEdit>,
    ) -> Result<(), Error> {
        let Some(edit) = edit else { return Ok(()) };
        if edit.storage != crate::ArtworkStorage::Server || !self.config.owned {
            return Err(Error::Unavailable);
        }
        self.change_artwork(object, &edit.change)
            .await
            .map_err(error)
    }

    pub async fn change_artwork(
        &self,
        object: &str,
        change: &crate::ArtworkChange,
    ) -> super::SourceResult<()> {
        let (method, endpoint) = match change {
            crate::ArtworkChange::Replace(_) => (Method::POST, "posters"),
            crate::ArtworkChange::Remove => (Method::DELETE, "thumb"),
        };
        let path = format!(
            "/library/metadata/{}/{endpoint}",
            crate::policy::raw_item_id(object)
        );
        let mut request = self.request(method, &path, &[]).await?;
        if let crate::ArtworkChange::Replace(image) = change {
            request = request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    image
                        .content_type
                        .as_deref()
                        .unwrap_or("application/octet-stream"),
                )
                .body(image.bytes.clone());
        }
        crate::remote_http::unit(request, HTTP).await
    }
    pub async fn read_track_metadata(
        &self,
        track: library::TrackView,
    ) -> Result<MetadataDraft, Error> {
        self.read_metadata(
            track.object_id.as_deref().ok_or(Error::Unavailable)?,
            SourceEntityKind::Track,
            &track.metadata.title,
            1,
        )
        .await
    }
    pub async fn read_album_metadata(
        &self,
        album: library::AlbumView,
    ) -> Result<MetadataDraft, Error> {
        self.read_metadata(
            &album.object_id,
            SourceEntityKind::Album,
            &album.metadata.title,
            album.summary.track_count.max(0) as usize,
        )
        .await
    }
    pub async fn read_artist_metadata(
        &self,
        artist: library::ArtistView,
    ) -> Result<MetadataDraft, Error> {
        self.read_metadata(
            &artist.object_id,
            SourceEntityKind::Artist,
            &artist.metadata.name,
            artist.summary.track_count.max(0) as usize,
        )
        .await
    }
    async fn read_metadata(
        &self,
        object: &str,
        kind: SourceEntityKind,
        title: &str,
        count: usize,
    ) -> Result<MetadataDraft, Error> {
        let item = self.metadata(object).await.map_err(error)?;
        let scope = scope(kind);
        let mut fields = FIELDS
            .iter()
            .filter(|(_, _, _, scopes, _)| scopes & scope != 0)
            .map(|&(key, native, _, _, writable)| {
                let value = match key {
                    "title" => text(&item, native).or_else(|| Some(title.to_owned())),
                    "artist" if kind == SourceEntityKind::Track => {
                        text(&item, native).or_else(|| text(&item, "grandparentTitle"))
                    }
                    "artist" => None,
                    "album_artist" if kind == SourceEntityKind::Album => text(&item, "parentTitle"),
                    "track_number" | "disc_number" | "year" => {
                        let value: Option<u16> = field(&item, native);
                        let value = if key == "year" && kind == SourceEntityKind::Track {
                            value
                                .filter(|v| *v > 0)
                                .or_else(|| field::<u16>(&item, "parentYear").filter(|v| *v > 0))
                        } else {
                            value
                        };
                        value.map(|v| v.to_string())
                    }
                    "genre" => tags(&item, native),
                    _ => text(&item, native),
                }
                .unwrap_or_default();
                MetadataField::new(key, value, self.config.owned && writable & scope != 0)
            })
            .collect::<Vec<_>>();
        fields.extend(extra_fields(&item, self.config.owned, scope));
        let mut draft = MetadataDraft::new(kind, fields);
        draft.revision = Some(revision(&item));
        draft.track_count = count;
        Ok(draft)
    }
    async fn write_fields(
        &self,
        object: &str,
        expected: &str,
        kind: &str,
        fields: Vec<(&str, String)>,
    ) -> Result<(), Error> {
        if !self.config.owned {
            return Err(Error::Unavailable);
        }
        let item = self.metadata(object).await.map_err(error)?;
        if revision(&item) != expected {
            return Err(Error::Conflict);
        }
        if fields.is_empty() {
            return Ok(());
        }
        let section = id(&item["librarySectionID"]).ok_or(Error::Unavailable)?;
        let mut params = vec![
            (
                "id".to_string(),
                crate::policy::raw_item_id(object).to_string(),
            ),
            ("type".to_string(), kind.to_string()),
        ];
        if kind == "9" && fields.iter().any(|(key, _)| *key == "title") {
            if let Some(artist) = id(&item["parentRatingKey"]) {
                params.push(("artist.id.value".into(), artist));
            }
        }
        for (key, value) in fields {
            if let Some(name) = key.strip_suffix("[0].tag") {
                let json_key = EXTRA_FIELDS
                    .iter()
                    .find(|(field, _, _, _, _)| *field == key)
                    .map_or("Genre", |(_, _, json, _, _)| *json);
                params.push((format!("{name}.locked"), "1".into()));
                params.push((
                    format!("{name}[].tag.tag-"),
                    tags(&item, json_key).unwrap_or_default().replace("; ", ","),
                ));
                for (index, tag) in value
                    .split(';')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .enumerate()
                {
                    params.push((format!("{name}[{index}].tag.tag"), tag.into()));
                }
            } else {
                params.push((format!("{key}.value"), value));
                params.push((format!("{key}.locked"), "1".into()));
            }
        }
        let borrowed: Vec<_> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        self.unit(
            Method::PUT,
            &format!("/library/sections/{section}/all"),
            &borrowed,
        )
        .await
        .map_err(error)
    }
    pub async fn write_metadata(
        &self,
        object: &str,
        expected: &str,
        edit: &MetadataEdit,
    ) -> Result<(), Error> {
        let scope = scope(edit.kind);
        let fields = edit
            .changes
            .iter()
            .filter_map(|(key, value)| {
                FIELDS
                    .iter()
                    .find(|(field, _, _, _, writable)| *field == key && writable & scope != 0)
                    .map(|(_, _, native, _, _)| *native)
                    .or_else(|| {
                        EXTRA_FIELDS
                            .iter()
                            .find(|(field, _, _, _, scopes)| *field == key && scopes & scope != 0)
                            .map(|(field, _, _, _, _)| *field)
                    })
                    .map(|native| (native, value.clone()))
            })
            .collect();
        let kind = match edit.kind {
            SourceEntityKind::Track => "10",
            SourceEntityKind::Album => "9",
            SourceEntityKind::Artist => "8",
        };
        self.write_fields(object, expected, kind, fields).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn album_title_edit_keeps_artist_identity_and_locks_only_changed_fields() {
        let server = MockServer::start().await;
        let source = super::super::catalog::tests::source(&server);
        Mock::given(method("GET")).and(path("/library/metadata/album")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"MediaContainer":{"Metadata":[{"ratingKey":"album","type":"album","librarySectionID":"section","updatedAt":5,"parentRatingKey":"artist","title":"Before","year":[],"Genre":[{"tag":"Rock"}]}]}}))).expect(1).mount(&server).await;
        Mock::given(method("PUT"))
            .and(path("/library/sections/section/all"))
            .and(query_param("type", "9"))
            .and(query_param("id", "album"))
            .and(query_param("title.value", "After"))
            .and(query_param("title.locked", "1"))
            .and(query_param("artist.id.value", "artist"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        source
            .write_metadata(
                "plex:album:album",
                "5",
                &crate::MetadataEdit {
                    kind: SourceEntityKind::Album,
                    artwork: None,
                    changes: [("title".into(), "After".into())].into(),
                },
            )
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert!(
            !requests[1]
                .url
                .query_pairs()
                .any(|(key, _)| key.starts_with("year") || key.starts_with("genre"))
        );
    }
}
