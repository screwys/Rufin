use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use library::{Database, SourceKey, TrackKey, TrackRow};
use playback::{ResolvedStream, StreamRequest};
use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, DATE, ETAG, IF_RANGE,
    LAST_MODIFIED, RANGE,
};
use serde::{Deserialize, Serialize};
use sources::{SourceError, SourceId, SourceResult};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use tracing::warn;

use crate::{DownloadOwner, ReleasedDownloadOwner, rebind_released_subject};

pub(super) const RECORD_VERSION: u32 = 4;
pub(super) const AUDIO_EXTENSION: &str = "audio";
pub(super) const RECORD_EXTENSION: &str = "json";
pub(super) const PART_EXTENSION: &str = "part";
const CHECKPOINT_EXTENSION: &str = "resume";
pub(super) const CUSTOM_STAGING_DIRECTORY: &str = ".rufin-partials";

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct DownloadRecord {
    pub(super) version: u32,
    pub(super) source_id: SourceId,
    pub(super) track_id: TrackKey,
    #[serde(default)]
    pub(super) owners: HashSet<DownloadOwner>,
    #[serde(default)]
    pub(super) custom_storage: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) relative_audio_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) completed_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ReleasedDownloadRecordV3 {
    version: u32,
    source_id: SourceId,
    track_id: String,
    #[serde(default)]
    owners: HashSet<ReleasedDownloadOwner>,
    #[serde(default)]
    audio_root: Option<PathBuf>,
    #[serde(default)]
    audio_path: Option<PathBuf>,
}

#[derive(Clone)]
pub(super) struct DownloadPaths {
    pub(super) directory: PathBuf,
    pub(super) audio_root: Option<PathBuf>,
    pub(super) audio: PathBuf,
    pub(super) audio_part: PathBuf,
    pub(super) record: PathBuf,
    pub(super) record_part: PathBuf,
    pub(super) checkpoint: PathBuf,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TransferCheckpoint {
    representation: String,
    validator: String,
    length: u64,
}

fn resume_validator(headers: &reqwest::header::HeaderMap) -> Option<String> {
    if let Some(etag) = headers.get(ETAG) {
        let etag = etag.to_str().ok()?;
        return strong_etag(etag).then(|| etag.to_string());
    }
    let modified = headers.get(LAST_MODIFIED)?.to_str().ok()?;
    let date = headers.get(DATE)?.to_str().ok()?;
    let age = httpdate::parse_http_date(date)
        .ok()?
        .duration_since(httpdate::parse_http_date(modified).ok()?)
        .ok()?;
    (age >= Duration::from_secs(60)).then(|| modified.to_string())
}

#[derive(Default)]
pub(super) struct TransferClients {
    strict: tokio::sync::OnceCell<reqwest::Client>,
    insecure: tokio::sync::OnceCell<reqwest::Client>,
}

impl TransferClients {
    async fn download_cancellable(
        &self,
        stream: &ResolvedStream,
        representation: &str,
        paths: &DownloadPaths,
        cancellation: &mut oneshot::Receiver<()>,
    ) -> SourceResult<()> {
        let clients = if stream.trust_invalid_certificate() {
            &self.insecure
        } else {
            &self.strict
        };
        let trust_invalid_certificate = stream.trust_invalid_certificate();
        let client = clients
            .get_or_try_init(|| async move {
                reqwest::Client::builder()
                    .danger_accept_invalid_certs(trust_invalid_certificate)
                    .connect_timeout(Duration::from_secs(15))
                    .build()
                    .map_err(download_request_error)
            })
            .await?;
        let resume = read_checkpoint(paths, Some(representation)).await?;
        let response = send_request(client, stream.uri(), resume.as_ref(), cancellation).await?;
        let status = response.status();
        if status == reqwest::StatusCode::OK {
            return download_full(response, paths, representation, cancellation).await;
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT
            && let Some((checkpoint, offset)) = resume.as_ref()
            && valid_partial(&response, checkpoint, *offset)
        {
            return download_partial(response, paths, checkpoint, *offset, cancellation).await;
        }
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE
            && let Some((checkpoint, offset)) = resume.as_ref()
            && *offset == checkpoint.length
            && unsatisfied_total(&response) == Some(checkpoint.length)
        {
            return Ok(());
        }
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && resume.is_some() {
            discard_staging(paths).await?;
            let response = send_request(client, stream.uri(), None, cancellation).await?;
            if response.status() == reqwest::StatusCode::OK {
                return download_full(response, paths, representation, cancellation).await;
            }
            if !response.status().is_success() {
                return response_error(response.status(), paths).await;
            }
            return Err(SourceError::Other(
                "the download server returned an unsupported successful response".to_string(),
            ));
        }
        if !status.is_success() {
            return response_error(status, paths).await;
        }
        Err(SourceError::Other(
            "the download server returned an unsupported successful response".to_string(),
        ))
    }
}

fn representation_key(source_id: &SourceId, request: &StreamRequest, redacted_uri: &str) -> String {
    let value = serde_json::to_vec(&(source_id, request, redacted_uri))
        .expect("a download representation can be encoded");
    hash_id_bytes(&value)
}

async fn send_request(
    client: &reqwest::Client,
    uri: &str,
    resume: Option<&(TransferCheckpoint, u64)>,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<reqwest::Response> {
    let mut request = client.get(uri).header(ACCEPT_ENCODING, "identity");
    if let Some((checkpoint, offset)) = resume {
        request = request
            .header(RANGE, format!("bytes={offset}-"))
            .header(IF_RANGE, &checkpoint.validator);
    }
    tokio::select! {
        biased;
        result = request.send() => result.map_err(download_request_error),
        _ = &mut *cancellation => Err(SourceError::Cancelled),
    }
}

async fn download_full(
    response: reqwest::Response,
    paths: &DownloadPaths,
    representation: &str,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    if !identity_response(&response) {
        discard_staging(paths).await?;
        return Err(SourceError::Other(
            "the download response used an unsupported encoding".to_string(),
        ));
    }
    let expected = response_length(&response).filter(|length| *length > 0);
    let checkpoint = expected.and_then(|length| {
        resume_validator(response.headers()).map(|validator| TransferCheckpoint {
            representation: representation.to_string(),
            validator,
            length,
        })
    });
    discard_staging(paths).await?;
    let file = tokio::fs::File::create(&paths.audio_part)
        .await
        .map_err(|error| SourceError::Other(format!("could not create download: {error}")))?;
    if let Some(checkpoint) = checkpoint.as_ref() {
        write_checkpoint(paths, checkpoint).await?;
    }
    write_response(
        response,
        paths,
        file,
        expected,
        checkpoint.is_some(),
        0,
        cancellation,
    )
    .await?;
    Ok(())
}

async fn download_partial(
    response: reqwest::Response,
    paths: &DownloadPaths,
    checkpoint: &TransferCheckpoint,
    offset: u64,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    let file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&paths.audio_part)
        .await
        .map_err(|error| SourceError::Other(format!("could not resume download: {error}")))?;
    write_response(
        response,
        paths,
        file,
        Some(checkpoint.length - offset),
        true,
        offset,
        cancellation,
    )
    .await?;
    Ok(())
}

async fn write_response(
    response: reqwest::Response,
    paths: &DownloadPaths,
    mut file: tokio::fs::File,
    expected: Option<u64>,
    resumable: bool,
    starting_length: u64,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    let written = match stream_body(response, &mut file, expected, cancellation).await {
        Ok(written) => written,
        Err(error) => return finish_failed_body(paths, file, resumable, error).await,
    };
    if written == 0 || expected.is_some_and(|expected| written != expected) {
        let error = if expected.is_some() {
            SourceError::Network("the download ended before its declared length".to_string())
        } else {
            SourceError::Other("the download response was empty".to_string())
        };
        return finish_failed_body(
            paths,
            file,
            resumable && (starting_length > 0 || written > 0),
            error,
        )
        .await;
    }
    finish_file(file).await
}

async fn stream_body(
    mut response: reqwest::Response,
    file: &mut tokio::fs::File,
    maximum: Option<u64>,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<u64> {
    let mut bytes_written = 0u64;
    loop {
        let chunk = tokio::select! {
            biased;
            result = tokio::time::timeout(Duration::from_secs(60), response.chunk()) => result
                .map_err(|_| SourceError::Network("the download stalled".to_string()))?
                .map_err(download_request_error)?,
            _ = &mut *cancellation => return Err(SourceError::Cancelled),
        };
        let Some(chunk) = chunk else {
            break;
        };
        let next = bytes_written.saturating_add(chunk.len() as u64);
        if maximum.is_some_and(|maximum| next > maximum) {
            return Err(SourceError::Other(
                "the download response exceeded its declared length".to_string(),
            ));
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| SourceError::Other(format!("could not write download: {error}")))?;
        bytes_written = next;
    }
    Ok(bytes_written)
}

async fn finish_failed_body<T>(
    paths: &DownloadPaths,
    mut file: tokio::fs::File,
    resumable: bool,
    error: SourceError,
) -> SourceResult<T> {
    if resumable && file.flush().await.is_ok() && file.sync_all().await.is_ok() {
        return Err(error);
    }
    drop(file);
    discard_staging(paths).await?;
    Err(error)
}

async fn finish_file(mut file: tokio::fs::File) -> SourceResult<()> {
    file.flush()
        .await
        .map_err(|error| SourceError::Other(format!("could not finish download: {error}")))?;
    file.sync_all()
        .await
        .map_err(|error| SourceError::Other(format!("could not save download: {error}")))
}

async fn read_checkpoint(
    paths: &DownloadPaths,
    representation: Option<&str>,
) -> SourceResult<Option<(TransferCheckpoint, u64)>> {
    let encoded = match tokio::fs::read(&paths.checkpoint).await {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            discard_staging(paths).await?;
            return Ok(None);
        }
        Err(error) => {
            return Err(SourceError::Other(format!(
                "could not read download checkpoint: {error}"
            )));
        }
    };
    let checkpoint = serde_json::from_slice::<TransferCheckpoint>(&encoded).ok();
    let length = tokio::fs::metadata(&paths.audio_part)
        .await
        .ok()
        .map(|metadata| metadata.len());
    if checkpoint.as_ref().is_some_and(|checkpoint| {
        !checkpoint.representation.is_empty()
            && representation.is_none_or(|expected| checkpoint.representation == expected)
            && checkpoint.length > 0
            && (strong_etag(&checkpoint.validator)
                || httpdate::parse_http_date(&checkpoint.validator).is_ok())
    }) && let (Some(checkpoint), Some(length)) = (checkpoint, length)
        && length > 0
        && length <= checkpoint.length
    {
        return Ok(Some((checkpoint, length)));
    }
    discard_staging(paths).await?;
    Ok(None)
}

async fn write_checkpoint(
    paths: &DownloadPaths,
    checkpoint: &TransferCheckpoint,
) -> SourceResult<()> {
    let encoded = serde_json::to_vec(checkpoint)
        .map_err(|error| SourceError::Other(format!("could not encode checkpoint: {error}")))?;
    tokio::fs::write(&paths.checkpoint, encoded)
        .await
        .map_err(|error| SourceError::Other(format!("could not write checkpoint: {error}")))
}

pub(super) async fn discard_staging(paths: &DownloadPaths) -> SourceResult<()> {
    for path in [&paths.audio_part, &paths.checkpoint] {
        remove_file_if_present(path)
            .await
            .map_err(SourceError::Other)?;
    }
    Ok(())
}

pub(super) async fn cleanup_staging(
    root: &Path,
    source_id: &SourceId,
    directory: Option<&Path>,
    track_ids: &HashSet<TrackKey>,
) -> SourceResult<()> {
    let expected = track_ids
        .iter()
        .map(|track_id| staging_paths(root, source_id, track_id, directory))
        .flat_map(|paths| [paths.audio_part.clone(), paths.checkpoint.clone()])
        .collect::<HashSet<_>>();
    let custom = directory.map(|directory| {
        directory
            .join(CUSTOM_STAGING_DIRECTORY)
            .join(hash_id(source_id.as_str()))
    });
    for directory in [Some(source_directory(root, source_id)), custom.clone()]
        .into_iter()
        .flatten()
    {
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SourceError::Other(format!(
                    "could not inspect download staging at {}: {error}",
                    directory.display()
                )));
            }
        };
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            SourceError::Other(format!("could not inspect download staging: {error}"))
        })? {
            let path = entry.path();
            if managed_staging_file(&path) && !expected.contains(&path) {
                remove_file_if_present(&path)
                    .await
                    .map_err(SourceError::Other)?;
            }
        }
    }
    if let Some(source_staging) = custom {
        let _ = tokio::fs::remove_dir(source_staging).await;
    }
    if let Some(directory) = directory {
        let _ = tokio::fs::remove_dir(directory.join(CUSTOM_STAGING_DIRECTORY)).await;
    }
    Ok(())
}

fn managed_staging_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".audio.part") || name.ends_with(".audio.part.resume"))
}

fn managed_record_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some(RECORD_EXTENSION)
        && path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|stem| {
                stem.len() == 64
                    && stem
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
}

fn response_length(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn strong_etag(value: &str) -> bool {
    !value.starts_with("W/") && value.starts_with('"') && value.ends_with('"') && value.len() >= 2
}

fn valid_partial(
    response: &reqwest::Response,
    checkpoint: &TransferCheckpoint,
    offset: u64,
) -> bool {
    if !identity_response(response) {
        return false;
    }
    let Some((start, end, total)) = satisfied_range(response) else {
        return false;
    };
    let expected_suffix = checkpoint.length.saturating_sub(offset);
    start == offset
        && end.checked_add(1) == Some(total)
        && total == checkpoint.length
        && response_length(response).is_none_or(|length| length == expected_suffix)
}

fn identity_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.eq_ignore_ascii_case("identity"))
}

fn satisfied_range(response: &reqwest::Response) -> Option<(u64, u64, u64)> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn unsatisfied_total(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes */")?
        .parse()
        .ok()
}

fn status_error(status: u16) -> SourceError {
    match status {
        401 | 403 => SourceError::Auth("the download was not authorized".to_string()),
        404 => SourceError::NotFound,
        status => SourceError::Server {
            status,
            message: "the download request failed".to_string(),
        },
    }
}

async fn response_error<T>(status: reqwest::StatusCode, paths: &DownloadPaths) -> SourceResult<T> {
    if status == reqwest::StatusCode::NOT_FOUND
        || (status.is_client_error() && !matches!(status.as_u16(), 401 | 403 | 429))
    {
        discard_staging(paths).await?;
    }
    Err(status_error(status.as_u16()))
}

fn download_request_error(error: reqwest::Error) -> SourceError {
    if error.is_timeout() {
        SourceError::Network("the download timed out".to_string())
    } else if error.is_connect() {
        SourceError::Network("could not connect for the download".to_string())
    } else if error
        .to_string()
        .to_ascii_lowercase()
        .contains("certificate")
    {
        SourceError::Tls("the download certificate was rejected".to_string())
    } else {
        SourceError::Network("the download was interrupted".to_string())
    }
}

pub(super) async fn run_transfer(
    source_id: &SourceId,
    request: &StreamRequest,
    stream: &ResolvedStream,
    paths: &DownloadPaths,
    transfers: &TransferClients,
    mut cancellation: oneshot::Receiver<()>,
) -> SourceResult<()> {
    for directory in [
        Some(paths.directory.as_path()),
        paths.audio.parent(),
        paths.audio_part.parent(),
    ]
    .into_iter()
    .flatten()
    {
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(|error| {
                SourceError::Other(format!("could not create a download directory: {error}"))
            })?;
    }
    remove_file_if_present(&paths.record_part)
        .await
        .map_err(SourceError::Other)?;
    let representation = representation_key(source_id, request, stream.redacted_uri());
    transfers
        .download_cancellable(stream, &representation, paths, &mut cancellation)
        .await
}

pub(super) async fn add_owner_to_existing_download(
    root: &Path,
    source_id: &SourceId,
    track_id: &TrackKey,
    owner: &DownloadOwner,
    custom_directory: Option<&Path>,
) -> Result<bool, String> {
    let metadata_paths = download_paths(root, source_id, track_id);
    if !metadata_paths.record.is_file() {
        return Ok(false);
    }
    let bytes = tokio::fs::read(&metadata_paths.record)
        .await
        .map_err(|error| format!("could not read the download record: {error}"))?;
    let mut record = serde_json::from_slice::<DownloadRecord>(&bytes)
        .map_err(|error| format!("could not decode the download record: {error}"))?;
    if record.source_id != *source_id || record.track_id != *track_id {
        return Ok(false);
    }
    let paths = match record_download_paths(root, source_id, &record, custom_directory) {
        Ok(paths) => paths,
        Err(error) => {
            warn!(%error, path = %metadata_paths.record.display(), "ignored an unsafe download record");
            quarantine_record(&metadata_paths.record);
            return Ok(false);
        }
    };
    if !paths.audio.is_file() {
        return Ok(false);
    }
    if record.owners.is_empty() {
        record.owners.insert(DownloadOwner::Retained);
    }
    if record.owners.insert(owner.clone()) {
        write_record(&paths, &record).await?;
    }
    Ok(true)
}

pub(super) async fn write_record(
    paths: &DownloadPaths,
    record: &DownloadRecord,
) -> Result<(), String> {
    let encoded = serde_json::to_vec(record)
        .map_err(|error| format!("could not encode the download record: {error}"))?;
    tokio::fs::write(&paths.record_part, encoded)
        .await
        .map_err(|error| format!("could not save the download record: {error}"))?;
    tokio::fs::rename(&paths.record_part, &paths.record)
        .await
        .map_err(|error| format!("could not finish the download record: {error}"))
}

pub(super) async fn finalize_download(
    paths: &DownloadPaths,
    source_id: SourceId,
    track_id: TrackKey,
    owner: DownloadOwner,
) -> Result<(), String> {
    tokio::fs::rename(&paths.audio_part, &paths.audio)
        .await
        .map_err(|error| format!("could not save the downloaded track: {error}"))?;
    let completed_size = tokio::fs::metadata(&paths.audio)
        .await
        .map_err(|error| format!("could not inspect the downloaded track: {error}"))?
        .len();
    let storage_root = paths.audio_root.as_deref().unwrap_or(&paths.directory);
    let relative_audio_path = paths
        .audio
        .strip_prefix(storage_root)
        .map_err(|_| "the downloaded track is outside its managed storage".to_string())?
        .to_path_buf();
    if !normal_relative_path(&relative_audio_path) {
        return Err("the downloaded track has an invalid managed path".to_string());
    }
    let record = DownloadRecord {
        version: RECORD_VERSION,
        source_id,
        track_id,
        owners: HashSet::from([owner]),
        custom_storage: storage_root != paths.directory,
        relative_audio_path: Some(relative_audio_path),
        completed_size: Some(completed_size),
    };
    write_record(paths, &record).await?;
    let _ = remove_file_if_present(&paths.checkpoint).await;
    Ok(())
}

pub(super) fn load_download_records(
    root: &Path,
    source_id: &SourceId,
    custom_directory: Option<&Path>,
) -> Result<HashMap<TrackKey, DownloadRecord>, String> {
    load_download_state(root, source_id, custom_directory).map(|(_, records)| records)
}

fn load_download_state(
    root: &Path,
    source_id: &SourceId,
    custom_directory: Option<&Path>,
) -> Result<
    (
        HashMap<TrackKey, DownloadPaths>,
        HashMap<TrackKey, DownloadRecord>,
    ),
    String,
> {
    match std::fs::metadata(root) {
        Ok(metadata) if !metadata.is_dir() => {
            return Err(format!(
                "could not read downloads at {}: not a directory",
                root.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not read downloads at {}: {error}",
                root.display()
            ));
        }
        Ok(_) => {}
    }
    let directory = source_directory(root, source_id);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((HashMap::new(), HashMap::new()));
        }
        Err(error) => {
            return Err(format!(
                "could not read downloads at {}: {error}",
                directory.display()
            ));
        }
    };
    let mut files = HashMap::new();
    let mut records = HashMap::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read a download entry: {error}"))?;
        let path = entry.path();
        if managed_staging_file(&path) {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) == Some(PART_EXTENSION)
            && managed_record_file(&path.with_extension(""))
        {
            let _ = std::fs::remove_file(path);
            continue;
        }
        if !managed_record_file(&path) {
            continue;
        }
        let mut record = match std::fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<DownloadRecord>(&bytes).map_err(|error| error.to_string())
            }) {
            Ok(record) if record.version <= RECORD_VERSION && record.source_id == *source_id => {
                record
            }
            Ok(_) | Err(_) => {
                warn!(path = %path.display(), "ignored an invalid download record");
                quarantine_record(&path);
                continue;
            }
        };
        if record.owners.is_empty() {
            record.owners.insert(DownloadOwner::Retained);
        }
        let expected = match record_download_paths(root, source_id, &record, custom_directory) {
            Ok(paths) => paths,
            Err(error) => {
                warn!(%error, path = %path.display(), "ignored an unsafe download record");
                quarantine_record(&path);
                continue;
            }
        };
        if path != expected.record {
            warn!(path = %path.display(), "ignored a misplaced download record");
            quarantine_record(&path);
            continue;
        }
        if expected.audio.is_file()
            && record.completed_size.is_none_or(|expected_size| {
                std::fs::metadata(&expected.audio)
                    .is_ok_and(|metadata| metadata.len() == expected_size)
            })
        {
            let _ = std::fs::remove_file(&expected.audio_part);
            let _ = std::fs::remove_file(&expected.checkpoint);
            files.insert(record.track_id, expected);
            records.insert(record.track_id.clone(), record);
        } else {
            warn!(path = %path.display(), "ignored a missing or size-mismatched download");
            quarantine_record(&path);
        }
    }
    Ok((files, records))
}

async fn migrate_released_download_records(
    root: &Path,
    database: &Database,
    source_key: SourceKey,
    source_id: &SourceId,
    custom_directory: Option<&Path>,
) -> Result<(), String> {
    let directory = source_directory(root, source_id);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not read downloads at {}: {error}",
                directory.display()
            ));
        }
    };
    let cancellation = library::ReadCancellation::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read a download entry: {error}"))?;
        let path = entry.path();
        if !managed_record_file(&path) {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(%error, path = %path.display(), "could not read a released download record");
                continue;
            }
        };
        if serde_json::from_slice::<DownloadRecord>(&bytes).is_ok() {
            continue;
        }
        let released = match serde_json::from_slice::<ReleasedDownloadRecordV3>(&bytes) {
            Ok(record) if record.version == 3 && record.source_id == *source_id => record,
            _ => continue,
        };
        let Some(track_id) = database
            .track_key_by_object(source_key, &released.track_id, &cancellation)
            .await
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let (custom_storage, relative_audio_path) = match released_audio_location(
            root,
            source_id,
            &released,
            custom_directory,
        ) {
            Ok(location) => location,
            Err(error) => {
                warn!(%error, path = %path.display(), "quarantined an unsafe released download record");
                quarantine_record(&path);
                continue;
            }
        };
        let storage_root = if custom_storage {
            custom_directory.expect("custom released storage was authorized")
        } else {
            directory.as_path()
        };
        let audio = authorize_existing_audio(storage_root, &relative_audio_path)?;
        let completed_size = std::fs::metadata(&audio)
            .map_err(|error| format!("could not inspect {}: {error}", audio.display()))?
            .len();
        let mut owners = HashSet::new();
        for owner in released.owners {
            match owner {
                ReleasedDownloadOwner::Retained => {
                    owners.insert(DownloadOwner::Retained);
                }
                ReleasedDownloadOwner::Subject(subject) => {
                    if let Some(subject) =
                        rebind_released_subject(database, source_key, subject, &cancellation)
                            .await?
                    {
                        owners.insert(DownloadOwner::Subject(subject));
                    }
                }
            }
        }
        if owners.is_empty() {
            owners.insert(DownloadOwner::Retained);
        }
        let record = DownloadRecord {
            version: RECORD_VERSION,
            source_id: source_id.clone(),
            track_id,
            owners,
            custom_storage,
            relative_audio_path: Some(relative_audio_path),
            completed_size: Some(completed_size),
        };
        let paths = record_download_paths(root, source_id, &record, custom_directory)?;
        if let Some(parent) = paths.record.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| error.to_string())?;
        }
        write_record(&paths, &record).await?;
        if path != paths.record {
            std::fs::remove_file(&path).map_err(|error| {
                format!("could not finish released download migration: {error}")
            })?;
        }
    }
    Ok(())
}

fn released_audio_location(
    root: &Path,
    source_id: &SourceId,
    record: &ReleasedDownloadRecordV3,
    custom_directory: Option<&Path>,
) -> Result<(bool, PathBuf), String> {
    let internal = source_directory(root, source_id);
    let Some(stored_audio) = record.audio_path.as_deref() else {
        return Ok((
            false,
            PathBuf::from(format!("{}.{}", hash_id(&record.track_id), AUDIO_EXTENSION)),
        ));
    };
    if stored_audio.is_absolute() {
        for (custom, approved) in [(false, Some(internal.as_path())), (true, custom_directory)] {
            let Some(approved) = approved else { continue };
            if let Ok(relative) = stored_audio.strip_prefix(approved)
                && normal_relative_path(relative)
                && authorize_existing_audio(approved, relative).is_ok()
            {
                return Ok((custom, relative.to_path_buf()));
            }
        }
        return Err("the released download path is outside configured storage".to_string());
    }
    if !normal_relative_path(stored_audio) {
        return Err("the released download path is not a normal relative path".to_string());
    }
    let custom = match record.audio_root.as_deref() {
        None => false,
        Some(stored_root) if same_approved_root(stored_root, &internal) => false,
        Some(stored_root)
            if custom_directory
                .is_some_and(|approved| same_approved_root(stored_root, approved)) =>
        {
            true
        }
        Some(_) => return Err("the released download root is not currently configured".to_string()),
    };
    let approved = if custom {
        custom_directory.expect("custom released storage was selected")
    } else {
        internal.as_path()
    };
    authorize_existing_audio(approved, stored_audio)?;
    Ok((custom, stored_audio.to_path_buf()))
}

fn same_approved_root(left: &Path, right: &Path) -> bool {
    left == right
        || std::fs::canonicalize(left)
            .ok()
            .zip(std::fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

pub(super) async fn attach_downloaded_files(
    root: &Path,
    database: &Database,
    source_key: SourceKey,
    source_id: &SourceId,
    custom_directory: Option<&Path>,
) -> Result<Vec<DownloadPaths>, String> {
    migrate_released_download_records(root, database, source_key, source_id, custom_directory)
        .await?;
    let (files, records) = load_download_state(root, source_id, custom_directory)?;
    let cancellation = library::ReadCancellation::new();
    let keys = files.keys().copied().collect::<Vec<_>>();
    let mut current = HashSet::new();
    for page in keys.chunks(256) {
        for track in database
            .track_rows(source_key, page, &cancellation)
            .await
            .map_err(|error| error.to_string())?
        {
            let Some(paths) = files.get(&track.track_key) else {
                continue;
            };
            let (storage_root, relative_path) = local_access_projection(paths)?;
            let metadata = std::fs::metadata(&paths.audio).map_err(|error| error.to_string())?;
            database
                .upsert_local_access(
                    source_key,
                    &library::LocalAccessWrite {
                        track_object_id: Some(track.object_id.clone()),
                        origin: library::LocalAccessOrigin::Download,
                        path: paths.audio.to_string_lossy().into_owned(),
                        root: storage_root.to_string_lossy().into_owned(),
                        relative_path: relative_path.to_string_lossy().into_owned(),
                        size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                        mtime_ns: metadata
                            .modified()
                            .ok()
                            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |value| {
                                i64::try_from(value.as_nanos()).unwrap_or(i64::MAX)
                            }),
                        device_id: None,
                        inode: None,
                        parser_version: RECORD_VERSION as i64,
                        title: track.title.clone(),
                        album: track.display_album.clone(),
                        artist: track.display_artist.clone(),
                        disc_number: track.disc_number,
                        track_number: track.track_number,
                        duration_millis: track.duration_millis,
                        media_uri: format!("file://{}", paths.audio.to_string_lossy()),
                        loudness_analysis_key: track.loudness_analysis_key,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            current.insert(track.track_key);
        }
    }
    Ok(keys
        .into_iter()
        .filter(|key| !current.contains(key))
        .map(|key| {
            records
                .get(&key)
                .and_then(|record| {
                    record_download_paths(root, source_id, record, custom_directory).ok()
                })
                .unwrap_or_else(|| download_paths(root, source_id, &key))
        })
        .collect())
}

pub(super) async fn remove_download_files(paths: &DownloadPaths) -> Result<bool, String> {
    let mut present = false;
    for path in [
        &paths.audio,
        &paths.audio_part,
        &paths.record,
        &paths.record_part,
        &paths.checkpoint,
    ] {
        match tokio::fs::remove_file(path).await {
            Ok(()) => present = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not remove {}: {error}", path.display())),
        }
    }
    remove_empty_audio_directories(paths).await;
    Ok(present)
}

async fn remove_empty_audio_directories(paths: &DownloadPaths) {
    let Some(root) = paths.audio_root.as_ref() else {
        return;
    };
    let Some(album) = paths.audio.parent() else {
        return;
    };
    let Some(artist) = album.parent() else {
        return;
    };
    for directory in [album, artist] {
        if directory == root {
            break;
        }
        if tokio::fs::remove_dir(directory).await.is_err() {
            break;
        }
    }
}

pub(super) async fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not replace {}: {error}", path.display())),
    }
}

pub(super) fn source_directory(root: &Path, source_id: &SourceId) -> PathBuf {
    root.join(hash_id(source_id.as_str()))
}

pub(super) fn download_paths(
    root: &Path,
    source_id: &SourceId,
    track_id: &TrackKey,
) -> DownloadPaths {
    let directory = source_directory(root, source_id);
    let stem = hash_id(&track_id.raw().to_string());
    let audio = directory.join(format!("{stem}.{AUDIO_EXTENSION}"));
    let audio_part = part_path(&audio);
    let checkpoint = checkpoint_path(&audio_part);
    DownloadPaths {
        audio_root: None,
        audio,
        audio_part,
        record: directory.join(format!("{stem}.{RECORD_EXTENSION}")),
        record_part: directory.join(format!("{stem}.{RECORD_EXTENSION}.{PART_EXTENSION}")),
        checkpoint,
        directory,
    }
}

pub(super) fn staging_paths(
    root: &Path,
    source_id: &SourceId,
    track_id: &TrackKey,
    directory: Option<&Path>,
) -> DownloadPaths {
    let mut paths = download_paths(root, source_id, track_id);
    let Some(directory) = directory else {
        return paths;
    };
    let staging = directory
        .join(CUSTOM_STAGING_DIRECTORY)
        .join(hash_id(source_id.as_str()));
    paths.audio_part = staging.join(format!(
        "{}.{}.{}",
        hash_id(&track_id.raw().to_string()),
        AUDIO_EXTENSION,
        PART_EXTENSION
    ));
    paths.checkpoint = checkpoint_path(&paths.audio_part);
    paths
}

pub(super) fn released_staging_paths(
    root: &Path,
    source_id: &SourceId,
    track_object_id: &str,
    directory: Option<&Path>,
) -> (PathBuf, PathBuf) {
    let staging = directory
        .map(|directory| {
            directory
                .join(CUSTOM_STAGING_DIRECTORY)
                .join(hash_id(source_id.as_str()))
        })
        .unwrap_or_else(|| source_directory(root, source_id));
    let audio_part = staging.join(format!(
        "{}.{}.{}",
        hash_id(track_object_id),
        AUDIO_EXTENSION,
        PART_EXTENSION
    ));
    let checkpoint = checkpoint_path(&audio_part);
    (audio_part, checkpoint)
}

pub(super) fn new_download_paths(
    root: &Path,
    source_id: &SourceId,
    track: &TrackRow,
    directory: Option<&Path>,
    transcoded_extension: Option<&str>,
) -> DownloadPaths {
    let mut paths = staging_paths(root, source_id, &track.track_key, directory);
    let audio_root = directory
        .map(Path::to_path_buf)
        .unwrap_or_else(|| source_directory(root, source_id));
    let artist = safe_path_component(&track.display_artist, "Unknown Artist");
    let album = safe_path_component(&track.display_album, "Unknown Album");
    let title = safe_path_component(&track.title, "Untitled");
    let id = source_track_hash(source_id, &track.track_key);
    let short_id = id.chars().take(12).collect::<String>();
    let extension = download_extension(track, transcoded_extension);
    let file_name = format!(
        "{:02}-{:02} {title} [{}].{extension}",
        track.disc_number, track.track_number, short_id
    );
    let audio = audio_root.join(artist).join(album).join(file_name);
    paths.audio_root = Some(audio_root);
    paths.audio = audio;
    paths
}

pub(super) fn record_download_paths(
    root: &Path,
    source_id: &SourceId,
    record: &DownloadRecord,
    custom_directory: Option<&Path>,
) -> Result<DownloadPaths, String> {
    let internal_root = source_directory(root, source_id);
    let audio_root = if record.custom_storage {
        custom_directory.ok_or_else(|| {
            "the download record requires a custom storage location that is not configured"
                .to_string()
        })?
    } else {
        internal_root.as_path()
    };
    let relative = record
        .relative_audio_path
        .as_deref()
        .ok_or_else(|| "the download record has no relative audio path".to_string())?;
    let audio = authorize_existing_audio(audio_root, relative)?;
    let mut paths = staging_paths(
        root,
        source_id,
        &record.track_id,
        record.custom_storage.then_some(audio_root),
    );
    paths.audio_root = Some(audio_root.to_path_buf());
    paths.audio = audio;
    Ok(paths)
}

pub(super) fn local_access_projection(paths: &DownloadPaths) -> Result<(&Path, PathBuf), String> {
    let storage_root = paths.audio_root.as_deref().unwrap_or(&paths.directory);
    let relative = paths
        .audio
        .strip_prefix(storage_root)
        .map_err(|_| "the downloaded track is outside its authorized storage".to_string())?;
    if !normal_relative_path(relative) {
        return Err("the downloaded track has an invalid relative storage path".to_string());
    }
    Ok((storage_root, relative.to_path_buf()))
}

fn part_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{PART_EXTENSION}"));
    value.into()
}

fn checkpoint_path(audio_part: &Path) -> PathBuf {
    let mut value = audio_part.as_os_str().to_os_string();
    value.push(format!(".{CHECKPOINT_EXTENSION}"));
    value.into()
}

fn normal_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn authorize_existing_audio(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if !normal_relative_path(relative) {
        return Err("the download record path is not a normal relative path".to_string());
    }
    let audio = root.join(relative);
    let root = std::fs::canonicalize(root)
        .map_err(|error| format!("could not authorize the download storage: {error}"))?;
    let parent = audio
        .parent()
        .ok_or_else(|| "the download record path has no parent".to_string())?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|error| format!("could not authorize the download parent: {error}"))?;
    if !parent.starts_with(&root) {
        return Err("the download record parent escapes its configured storage".to_string());
    }
    let metadata = std::fs::symlink_metadata(&audio)
        .map_err(|error| format!("could not inspect the downloaded track: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("the download record does not name a managed regular file".to_string());
    }
    let canonical_audio = std::fs::canonicalize(&audio)
        .map_err(|error| format!("could not authorize the downloaded track: {error}"))?;
    if !canonical_audio.starts_with(&root) {
        return Err("the downloaded track escapes its configured storage".to_string());
    }
    Ok(audio)
}

fn quarantine_record(path: &Path) {
    let quarantine = path.with_extension(format!("{RECORD_EXTENSION}.quarantine"));
    if !quarantine.exists()
        && let Err(error) = std::fs::rename(path, &quarantine)
    {
        warn!(%error, path = %path.display(), "could not quarantine a download record");
    }
}

fn safe_path_component(value: &str, fallback: &str) -> String {
    let sanitized = value
        .trim()
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .take(80)
        .collect::<String>();
    let sanitized = sanitized.trim_matches([' ', '.']);
    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized.to_string()
    }
}

fn download_extension(track: &TrackRow, transcoded_extension: Option<&str>) -> String {
    let extension = transcoded_extension
        .or(track.source_format.as_deref())
        .unwrap_or(AUDIO_EXTENSION)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    if extension.is_empty() {
        AUDIO_EXTENSION.to_string()
    } else {
        extension
    }
}

fn hash_id(value: &str) -> String {
    hash_id_bytes(value.as_bytes())
}

fn source_track_hash(source_id: &SourceId, track_id: &TrackKey) -> String {
    let value =
        serde_json::to_vec(&(source_id, track_id)).expect("a download identity can be encoded");
    hash_id_bytes(&value)
}

pub(super) fn hash_id_bytes(value: &[u8]) -> String {
    blake3::hash(value).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(relative_audio_path: PathBuf, custom_storage: bool, size: u64) -> DownloadRecord {
        DownloadRecord {
            version: RECORD_VERSION,
            source_id: SourceId::new("source"),
            track_id: TrackKey::from_raw(7),
            owners: HashSet::new(),
            custom_storage,
            relative_audio_path: Some(relative_audio_path),
            completed_size: Some(size),
        }
    }

    #[test]
    fn records_never_authorize_absolute_or_parent_paths() {
        let directory = tempfile::tempdir().expect("temporary download directory");
        let source = SourceId::new("source");
        let absolute = record(directory.path().join("outside.audio"), false, 1);
        assert!(record_download_paths(directory.path(), &source, &absolute, None).is_err());

        let traversal = record(PathBuf::from("Artist/../outside.audio"), false, 1);
        assert!(record_download_paths(directory.path(), &source, &traversal, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn custom_record_rejects_a_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary download directory");
        let custom = directory.path().join("custom");
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(&custom).expect("create custom root");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        std::fs::write(outside.join("track.audio"), b"external").expect("write external audio");
        symlink(&outside, custom.join("Artist")).expect("create escaping symlink");

        let record = record(PathBuf::from("Artist/track.audio"), true, 8);
        assert!(
            record_download_paths(
                directory.path(),
                &SourceId::new("source"),
                &record,
                Some(&custom),
            )
            .is_err()
        );
        assert!(outside.join("track.audio").is_file());
    }

    #[test]
    fn configured_custom_record_resolves_a_relative_object_path() {
        let directory = tempfile::tempdir().expect("temporary download directory");
        let custom = directory.path().join("custom");
        let relative = PathBuf::from("Artist/Album/track.audio");
        let audio = custom.join(&relative);
        std::fs::create_dir_all(audio.parent().expect("audio parent"))
            .expect("create audio parent");
        std::fs::write(&audio, b"audio").expect("write audio");

        let paths = record_download_paths(
            directory.path(),
            &SourceId::new("source"),
            &record(relative, true, 5),
            Some(&custom),
        )
        .expect("authorize configured custom audio");
        assert_eq!(paths.audio, audio);
        let (storage_root, projected) =
            local_access_projection(&paths).expect("project custom Local access");
        assert_eq!(storage_root, custom);
        assert_eq!(projected, PathBuf::from("Artist/Album/track.audio"));
        assert_eq!(storage_root.join(projected), paths.audio);
    }

    #[test]
    fn internal_nested_path_reconstructs_from_local_access_projection() {
        let directory = tempfile::tempdir().expect("temporary download directory");
        let source = SourceId::new("source");
        let source_root = source_directory(directory.path(), &source);
        let relative = PathBuf::from("Artist/Album/track.audio");
        let audio = source_root.join(&relative);
        std::fs::create_dir_all(audio.parent().expect("audio parent"))
            .expect("create audio parent");
        std::fs::write(&audio, b"audio").expect("write audio");
        let paths = record_download_paths(
            directory.path(),
            &source,
            &record(relative.clone(), false, 5),
            None,
        )
        .expect("authorize internal audio");

        let (storage_root, projected) =
            local_access_projection(&paths).expect("project internal Local access");
        assert_eq!(storage_root, source_root);
        assert_eq!(projected, relative);
        assert_eq!(storage_root.join(projected), paths.audio);
    }

    #[test]
    fn size_mismatch_quarantines_only_the_record() {
        let directory = tempfile::tempdir().expect("temporary download directory");
        let source = SourceId::new("source");
        let source_root = source_directory(directory.path(), &source);
        let relative = PathBuf::from("Artist/Album/track.audio");
        let audio = source_root.join(&relative);
        std::fs::create_dir_all(audio.parent().expect("audio parent"))
            .expect("create audio parent");
        std::fs::write(&audio, b"audio").expect("write audio");
        let record = record(relative, false, 99);
        let record_path = download_paths(directory.path(), &source, &record.track_id).record;
        std::fs::write(
            &record_path,
            serde_json::to_vec(&record).expect("encode record"),
        )
        .expect("write record");

        let (files, records) =
            load_download_state(directory.path(), &source, None).expect("load downloads");
        assert!(files.is_empty());
        assert!(records.is_empty());
        assert!(audio.is_file());
        assert!(
            record_path
                .with_extension(format!("{RECORD_EXTENSION}.quarantine"))
                .is_file()
        );
    }
}
