use super::*;
use rufin_core::metadata::{MetadataBatch, MetadataEditor, MetadataItemId, MetadataLookupReview};
use sources::{ArtworkStorage, LyricsStorage, SourceMetadataError};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/metadata", post(open).patch(edit).delete(close))
        .route("/api/metadata/save", post(save))
        .route("/api/metadata/artwork", get(artwork).post(edit_artwork))
        .route("/api/metadata/lyrics", get(lyrics).post(edit_lyrics))
        .route(
            "/api/metadata/lookup",
            get(lookup).post(open_lookup).delete(close_lookup),
        )
        .route("/api/metadata/lookup/item", post(edit_lookup))
        .route("/api/metadata/lookup/accept", post(accept_lookup))
}

/// An open editor: core keeps its draft between requests, as GTK's dialog keeps it open.
struct Session {
    editor: Arc<MetadataEditor>,
    batch: Option<Arc<MetadataBatch>>,
    artwork_results: Vec<rufin_core::metadata::ArtworkResult>,
    lyrics_results: Vec<rufin_core::lyrics::LyricsSearchResult>,
    /// The online lookup the editor runs, as GTK's Lookup dialog does.
    review: Option<Arc<MetadataLookupReview>>,
}

/// The editors browsers have open; the oldest closes past a handful, as a closed tab never
/// says it closed.
static SESSIONS: std::sync::LazyLock<Mutex<Vec<(u64, Arc<tokio::sync::Mutex<Session>>)>>> =
    std::sync::LazyLock::new(Mutex::default);
static NEXT: AtomicU64 = AtomicU64::new(1);
const OPEN_EDITORS: usize = 8;

fn session(id: u64) -> Result<Arc<tokio::sync::Mutex<Session>>, Error> {
    SESSIONS
        .lock()
        .expect("metadata sessions")
        .iter()
        .find(|(open, _)| *open == id)
        .map(|(_, session)| session.clone())
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "This editor is closed"))
}

fn metadata_error(error: SourceMetadataError) -> Error {
    match error {
        SourceMetadataError::InvalidInput(message) => bad_request(message),
        SourceMetadataError::Unavailable => {
            bad_request(localization::tr("Metadata editing is no longer available"))
        }
        SourceMetadataError::LocalAccessRequired { source_path, .. } => bad_request(
            localization::tr("Rufin needs local access to {path}").replace("{path}", &source_path),
        ),
        error => bad_request(error.to_string()),
    }
}

fn snapshot(id: u64, session: &Session) -> Value {
    let snapshot = session.editor.snapshot();
    json!({
        "id": id,
        "kind": session.editor.draft.kind,
        "fields": snapshot.fields.iter().map(|field| json!({
            "key": field.field.key,
            "label": field.field.label,
            "kind": field.field.kind,
            "value": field.field.value,
            "writable": field.field.writable,
            "mixed": field.field.mixed,
            "options": field.field.options,
            "changed": field.identified,
            "original": field.original_value,
            "mixed_label": field.mixed_label,
        })).collect::<Vec<_>>(),
        "locked": snapshot.locked,
        "lock_writable": snapshot.lock_writable,
        "has_changes": snapshot.has_changes,
        "committed": snapshot.committed,
        "track_count": snapshot.track_count,
        "selected_count": snapshot.selected_count,
        "scope_summary": snapshot.scope_summary,
        "artwork_revision": snapshot.artwork_revision,
        "artwork_storage": snapshot.artwork_storage,
        "can_embed_artwork": snapshot.can_embed_artwork,
        "can_remove_artwork": snapshot.can_remove_artwork,
        "has_current_artwork": snapshot.has_current_artwork,
        "search_artist": snapshot.search_artist,
        "search_album": snapshot.search_album,
        "external_lookup_allowed": snapshot.external_lookup_allowed,
        "scope_title": session.batch.as_ref().and_then(|batch| batch.scope_title()),
    })
}

#[derive(Deserialize)]
struct Open {
    /// One track, album or artist: its kind and media URI.
    kind: Option<String>,
    uri: Option<String>,
    /// Or the selected rows of a page, or tracks by URI.
    page: Option<page::PageToken>,
    ranges: Option<Vec<std::ops::Range<usize>>>,
    #[serde(default)]
    uris: Vec<String>,
}

/// Opens the editor an "Edit metadata" entry opens: a collection on a file source edits its
/// tracks together, as several selected tracks do.
async fn open(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let input: Open = body(request).await?;
    let source = products.source.clone();
    let (editor, batch) = match (input.kind.as_deref(), input.uri) {
        (Some(kind), Some(uri)) => {
            let item = match kind {
                "track" => MetadataItemId::Track(uri),
                "album" => MetadataItemId::Album(uri),
                "artist" => MetadataItemId::Artist(uri),
                _ => return Err(bad_request("Unknown item")),
            };
            let editor = MetadataEditor::open(source, item)
                .await
                .map_err(metadata_error)?;
            match editor.track_batch() {
                Some(batch) => (
                    batch
                        .clone()
                        .editor(Vec::new())
                        .await
                        .map_err(metadata_error)?,
                    Some(batch),
                ),
                None => (editor, None),
            }
        }
        _ => {
            let selection = match input.page {
                Some(page) => page::selection(&products, page, input.ranges)
                    .await?
                    .ok_or_else(|| bad_request("Nothing to edit here"))?,
                None => library::MemberSelection::Uris(input.uris.into()),
            };
            let batch = MetadataBatch::open(source, Arc::new(selection))
                .await
                .map_err(metadata_error)?;
            (
                batch
                    .clone()
                    .editor(Vec::new())
                    .await
                    .map_err(metadata_error)?,
                Some(batch),
            )
        }
    };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let session = Session {
        editor,
        batch,
        artwork_results: Vec::new(),
        lyrics_results: Vec::new(),
        review: None,
    };
    let value = snapshot(id, &session);
    let mut sessions = SESSIONS.lock().expect("metadata sessions");
    if sessions.len() >= OPEN_EDITORS {
        sessions.remove(0);
    }
    sessions.push((id, Arc::new(tokio::sync::Mutex::new(session))));
    Ok(json_response(StatusCode::OK, value))
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Edit {
    Set { key: String, value: String },
    Reset { key: String },
    Lock { locked: bool },
}

async fn edit(
    Query(parameters): Query<HashMap<String, String>>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let edit: Edit = body(request).await?;
    let session = session(id)?;
    let session = session.lock().await;
    match edit {
        Edit::Set { key, value } => session.editor.set_field(key, value),
        Edit::Reset { key } => session.editor.reset_field(key).await,
        Edit::Lock { locked } => session.editor.set_locked(locked),
    }
    .map_err(metadata_error)?;
    Ok(json_response(StatusCode::OK, snapshot(id, &session)))
}

async fn close(Query(parameters): Query<HashMap<String, String>>) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let closed = {
        let mut sessions = SESSIONS.lock().expect("metadata sessions");
        let index = sessions.iter().position(|(open, _)| *open == id);
        index.map(|index| sessions.remove(index).1)
    };
    // A lookup still running stops with its editor.
    if let Some(review) = match closed {
        Some(session) => session.lock().await.review.take(),
        None => None,
    } {
        review.cancel();
    }
    Ok(json_response(StatusCode::OK, json!({"closed": true})))
}

/// Saves the draft; without `confirmed`, says first how many tracks it would write, as GTK asks
/// before writing tracks.
async fn save(
    Query(parameters): Query<HashMap<String, String>>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        #[serde(default)]
        confirmed: bool,
        lyrics: Option<(String, LyricsStorage)>,
    }
    let id = number(&parameters, "id", 0)? as u64;
    let input: Input = body(request).await?;
    let session = session(id)?;
    let mut session = session.lock().await;
    if let Some((text, storage)) = input.lyrics {
        session.editor.set_lyrics(text, storage);
    }
    if !input.confirmed {
        let summary = session
            .editor
            .prepare_save()
            .await
            .map_err(metadata_error)?;
        if summary.tracks > 0 {
            return Ok(json_response(
                StatusCode::OK,
                json!({"confirm": summary.message, "tracks": summary.tracks}),
            ));
        }
    }
    let saved = session.editor.clone().save().await;
    match saved {
        Ok(editor) => {
            session.editor = editor;
            let done = !session
                .batch
                .as_ref()
                .is_some_and(|batch| batch.has_changes());
            let mut value = snapshot(id, &session);
            value["done"] = json!(done);
            Ok(json_response(StatusCode::OK, value))
        }
        Err(error) => {
            let mut value = snapshot(id, &session);
            value["error"] = json!(metadata_error_text(&error));
            Ok(json_response(StatusCode::OK, value))
        }
    }
}

fn metadata_error_text(error: &SourceMetadataError) -> String {
    match error {
        SourceMetadataError::InvalidInput(message) => message.clone(),
        error => error.to_string(),
    }
}

/// The artwork the draft shows: the chosen image, else the item's own.
async fn artwork(
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let session = session(id)?;
    let session = session.lock().await;
    let image = match session.editor.artwork() {
        Some(image) => Some(image),
        None if !session.editor.artwork_changed() => session
            .editor
            .load_current_artwork()
            .await
            .map_err(metadata_error)?,
        None => None,
    };
    let image = image.ok_or_else(|| error(StatusCode::NOT_FOUND, "No artwork"))?;
    Ok(Response::builder()
        .header(
            "content-type",
            image
                .content_type
                .clone()
                .unwrap_or_else(|| "image/jpeg".into()),
        )
        .header("cache-control", "no-store")
        .body(Body::from(image.bytes.clone()))
        .map_err(internal)?)
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum ArtworkEdit {
    /// An uploaded image, base64 encoded.
    Upload {
        data: String,
    },
    Remove,
    Current,
    Storage {
        storage: ArtworkStorage,
    },
    Search {
        artist: String,
        album: String,
    },
    Choose {
        index: usize,
    },
}

async fn edit_artwork(
    Query(parameters): Query<HashMap<String, String>>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let edit: ArtworkEdit = body(request).await?;
    let session = session(id)?;
    let mut session = session.lock().await;
    let mut results = None;
    match edit {
        ArtworkEdit::Upload { data } => {
            let bytes =
                <_ as base64::Engine>::decode(&base64::engine::general_purpose::STANDARD, data)
                    .map_err(bad_request)?;
            let image = session
                .editor
                .prepare_artwork(bytes)
                .await
                .map_err(metadata_error)?;
            session.editor.select_artwork(image);
        }
        ArtworkEdit::Remove => session.editor.remove_artwork(),
        ArtworkEdit::Current => session.editor.use_current_artwork(),
        ArtworkEdit::Storage { storage } => session.editor.set_artwork_storage(storage),
        ArtworkEdit::Search { artist, album } => {
            session.artwork_results = session
                .editor
                .search_artwork(artist, album)
                .await
                .map_err(metadata_error)?;
            results = Some(
                session
                    .artwork_results
                    .iter()
                    .map(|result| json!({"title": result.title, "detail": result.detail, "thumbnail": result.thumbnail_url}))
                    .collect::<Vec<_>>(),
            );
        }
        ArtworkEdit::Choose { index } => {
            let url = session
                .artwork_results
                .get(index)
                .ok_or_else(|| bad_request("Search again"))?
                .image_url
                .clone();
            let image = session
                .editor
                .download_artwork(url)
                .await
                .map_err(metadata_error)?;
            session.editor.select_artwork(image);
        }
    }
    let mut value = snapshot(id, &session);
    value["results"] = json!(results);
    Ok(json_response(StatusCode::OK, value))
}

async fn lyrics(
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let session = session(id)?;
    let session = session.lock().await;
    let settings = session
        .editor
        .lyrics_settings()
        .await
        .map_err(metadata_error)?;
    Ok(json_response(
        StatusCode::OK,
        json!({
            "storages": settings.storages,
            "storage": settings.storage,
            "can_search": settings.can_search,
            "artist": settings.artist,
            "title": settings.title,
            "text": settings.text,
        }),
    ))
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum LyricsEdit {
    Search { artist: String, title: String },
    Choose { index: usize },
}

async fn edit_lyrics(
    Query(parameters): Query<HashMap<String, String>>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let edit: LyricsEdit = body(request).await?;
    let session = session(id)?;
    let mut session = session.lock().await;
    Ok(json_response(
        StatusCode::OK,
        match edit {
            LyricsEdit::Search { artist, title } => {
                session.lyrics_results = session
                    .editor
                    .search_lyrics(artist, title)
                    .await
                    .map_err(metadata_error)?;
                json!({"results": session.lyrics_results.iter().map(|result| json!({
                    "title": result.track_name,
                    "artist": result.artist_name,
                    "album": result.album_name,
                    "duration": result.duration_seconds,
                    "provider": format!("{:?}", result.provider),
                })).collect::<Vec<_>>()})
            }
            LyricsEdit::Choose { index } => {
                let result = session
                    .lyrics_results
                    .get(index)
                    .cloned()
                    .ok_or_else(|| bad_request("Search again"))?;
                json!({"text": session.editor.resolve_lyrics(result).await.map_err(metadata_error)?})
            }
        },
    ))
}

fn lookup_review(session: &Session) -> Result<Arc<MetadataLookupReview>, Error> {
    session
        .review
        .clone()
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "The lookup is closed"))
}

/// Starts the lookup: AcoustID fingerprints and MusicBrainz searches for every track the
/// editor edits, run in the background while the browser follows its progress.
async fn open_lookup(
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let session = session(id)?;
    let review = {
        let mut session = session.lock().await;
        if let Some(previous) = session.review.take() {
            previous.cancel();
        }
        let review = MetadataLookupReview::open(session.editor.clone())
            .await
            .map_err(metadata_error)?;
        session.review = Some(review.clone());
        review
    };
    tokio::spawn(review.clone().run());
    lookup_json(&review, 0, 0)
        .await
        .map(|value| json_response(StatusCode::OK, value))
}

async fn lookup_json(
    review: &MetadataLookupReview,
    index: u64,
    offset: u64,
) -> Result<Value, Error> {
    let progress = review.progress();
    let tracks = review.tracks(offset, 200).await.map_err(metadata_error)?;
    let item = review.item(index).await.map_err(metadata_error)?;
    Ok(json!({
        "progress": {
            "completed": progress.completed,
            "cached": progress.cached,
            "total": progress.total,
            "finished": progress.finished,
            "failure": progress.failure,
            "active_track": progress.active_track,
            "stage": progress.stage,
        },
        "scope_title": review.scope_title(),
        "tracks": tracks.iter().map(|track| json!({
            "index": track.index,
            "title": track.title,
            "modified": track.modified,
            "error": track.error,
            "status": track.scan_status,
        })).collect::<Vec<_>>(),
        "item": {
            "index": index,
            "title": item.title,
            "ready": item.ready,
            "candidates": item.candidates,
            "selected": item.selected,
            "fields": item.fields.iter().map(|field| json!({
                "label": field.label,
                "original": field.original,
                "suggested": field.suggested,
                "changed": field.changed,
            })).collect::<Vec<_>>(),
            "failure": item.failure,
        },
    }))
}

/// The lookup's progress, its tracks and track `index`'s matches; with `wait`, after its next
/// change or ten seconds.
async fn lookup(
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let index = number(&parameters, "index", 0)? as u64;
    let offset = number(&parameters, "offset", 0)? as u64;
    let review = lookup_review(&*session(id)?.lock().await)?;
    if parameters.contains_key("wait") && !review.progress().finished {
        let _ = tokio::time::timeout(Duration::from_secs(10), review.next_update()).await;
    }
    lookup_json(&review, index, offset)
        .await
        .map(|value| json_response(StatusCode::OK, value))
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum LookupEdit {
    /// A match, or none to keep the track's own values.
    Choose {
        candidate: Option<rufin_core::metadata::MetadataCandidate>,
    },
    Search {
        query: String,
    },
    Refresh {
        query: String,
    },
}

async fn edit_lookup(
    Query(parameters): Query<HashMap<String, String>>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    let index = number(&parameters, "index", 0)? as u64;
    let edit: LookupEdit = body(request).await?;
    let review = lookup_review(&*session(id)?.lock().await)?;
    match edit {
        LookupEdit::Choose { candidate } => review.choose(index, candidate).await,
        LookupEdit::Search { query } => review.search(index, query).await,
        LookupEdit::Refresh { query } => review.refresh(index, query).await,
    }
    .map_err(metadata_error)?;
    lookup_json(&review, index, 0)
        .await
        .map(|value| json_response(StatusCode::OK, value))
}

/// Takes the chosen matches into the editor's draft, which Save then writes.
async fn accept_lookup(
    Query(parameters): Query<HashMap<String, String>>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        fill_missing: bool,
    }
    let id = number(&parameters, "id", 0)? as u64;
    let input: Input = body(request).await?;
    let session = session(id)?;
    let mut session = session.lock().await;
    let review = lookup_review(&session)?;
    review
        .accept(input.fill_missing)
        .await
        .map_err(metadata_error)?;
    session.review = None;
    if session.batch.is_some() {
        session.editor = session.editor.reload().await.map_err(metadata_error)?;
    }
    Ok(json_response(StatusCode::OK, snapshot(id, &session)))
}

async fn close_lookup(
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let id = number(&parameters, "id", 0)? as u64;
    if let Some(review) = session(id)?.lock().await.review.take() {
        review.cancel();
    }
    Ok(json_response(StatusCode::OK, json!({"closed": true})))
}
