//! Open host media through the ordinary playback queue.
use std::io::{Cursor, Read};
use std::path::Path;

use gio::prelude::*;
use library::{PlaylistFile, PlaylistFormat, QueueInput, QueueItem, QueueProvenance};

pub async fn arguments(
    queue: &playback::QueueHandle,
    database: &library::Database,
    local: Option<&sources::SourceConfiguration>,
    arguments: Vec<std::ffi::OsString>,
) -> Result<(), String> {
    files(
        queue,
        database,
        local,
        arguments
            .into_iter()
            .map(gio::File::for_commandline_arg)
            .collect(),
    )
    .await
}

pub async fn files(
    queue: &playback::QueueHandle,
    database: &library::Database,
    local: Option<&sources::SourceConfiguration>,
    files: Vec<gio::File>,
) -> Result<(), String> {
    let local = local.cloned();
    let database = database.clone();
    let runtime = tokio::runtime::Handle::current();
    let items = tokio::task::spawn_blocking(move || {
        let document_roots = match local
            .as_ref()
            .and_then(|configuration| configuration.editable().ok())
        {
            Some(sources::EditableSource::Local { document_roots, .. }) => document_roots,
            _ => Vec::new(),
        };
        let client = reqwest::blocking::Client::new();
        let entries = files
            .into_iter()
            .map(|file| {
                let file = canonical_local_file(local.as_ref(), file);
                let item = QueueItem::direct(file.uri(), display_name(&file), "", "", 0);
                (file, item)
            })
            .collect::<Vec<_>>();
        // Each frame owns the remaining entries of one open playlist. Removing
        // the frame allows the same playlist to appear again elsewhere.
        let mut playlists = vec![(String::new(), entries.into_iter())];
        let mut items = Vec::new();
        while let Some((_, entries)) = playlists.last_mut() {
            let Some((file, item)) = entries.next() else {
                playlists.pop();
                continue;
            };
            if let Some((base, playlist)) = read_playlist(&client, &file)? {
                let identity = base
                    .path()
                    .and_then(|path| path.canonicalize().ok())
                    .map(|path| gio::File::for_path(path).uri().to_string())
                    .unwrap_or_else(|| base.uri().to_string());
                if playlists.iter().any(|(ancestor, _)| ancestor == &identity) {
                    tracing::warn!(uri = %base.uri(), "skipping circular playlist reference");
                    continue;
                }
                let entries = playlist
                    .entries
                    .into_iter()
                    .filter_map(|entry| {
                        let location = if let Some(path) = base.path() {
                            library::playlist_locator(
                                &entry.locator,
                                path.parent().unwrap_or(Path::new(".")),
                            )
                        } else if base.uri_scheme().as_deref() == Some("content")
                            && url::Url::parse(&entry.locator).is_err()
                        {
                            match sources::resolve_document_relative(
                                &base.uri(),
                                &entry.locator,
                                &document_roots,
                            ) {
                                Ok(entry) => Some(entry.uri),
                                Err(error) => return Some(Err(error.to_string())),
                            }
                        } else {
                            url::Url::parse(&base.uri())
                                .and_then(|base| base.join(&entry.locator))
                                .ok()
                                .map(String::from)
                        }?;
                        let file = gio::File::for_uri(&location);
                        let file = canonical_local_file(local.as_ref(), file);
                        let item = QueueItem::direct(
                            file.uri(),
                            entry.title.unwrap_or_else(|| display_name(&file)),
                            entry.artist.unwrap_or_default(),
                            entry.album.unwrap_or_default(),
                            entry.duration_millis.unwrap_or_default(),
                        );
                        Some(Ok((file, item)))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                playlists.push((identity, entries.into_iter()));
            } else {
                let metadata = if file.uri_scheme().as_deref() == Some("content") {
                    runtime
                        .block_on(sources::read_document_queue_item(&file.uri()))
                        .ok()
                        .flatten()
                } else {
                    file.path()
                        .and_then(|path| sources::read_local_queue_item(&path))
                };
                let mut item = metadata.unwrap_or(item);
                if file.uri_scheme().as_deref() == Some("content") {
                    let uri = file.uri().to_string();
                    let identity =
                        sources::document_identity(&uri).map_err(|error| error.to_string())?;
                    item.media_uri = library::document_media_uri(&identity);
                    runtime
                        .block_on(database.upsert_local_locator(&library::LocalLocatorWrite {
                            source_id: None,
                            media_uri: item.media_uri.clone(),
                            origin: "import".into(),
                            path: uri.clone(),
                            root: uri.clone(),
                            relative_path: display_name(&file),
                            access_uri: uri,
                        }))
                        .map_err(|error| error.to_string())?;
                }
                items.push((item, QueueProvenance::Manual));
            }
        }
        Ok::<_, String>(items)
    })
    .await
    .map_err(|error| error.to_string())??;
    if items.is_empty() {
        return Err(localization::tr("No playable entries were found"));
    }
    queue.play(playback::PlayRequest::ordered(
        QueueInput::Items(items),
        0,
        playback::QueuePlacement::Now,
        false,
    ));
    Ok(())
}

fn read_playlist(
    client: &reqwest::blocking::Client,
    file: &gio::File,
) -> Result<Option<(gio::File, PlaylistFile)>, String> {
    let uri = file.uri();
    let (base, mime, mut input): (_, _, Box<dyn Read>) =
        if matches!(file.uri_scheme().as_deref(), Some("http" | "https")) {
            let response = client
                .get(uri.as_str())
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|error| format!("{uri}: {error}"))?;
            let mime = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            (
                gio::File::for_uri(response.url().as_str()),
                mime,
                Box::new(response),
            )
        } else if file.uri_scheme().as_deref() == Some("content") {
            let mime = sources::stat_document(&uri)
                .map(|entry| entry.mime_type)
                .unwrap_or_default();
            // Sniff without making a seekable copy of a cloud-backed audio file.
            let Ok(prefix) = sources::read_document_prefix(&uri, 8192) else {
                return Ok(None);
            };
            if playlist_format(&prefix, &mime, file).is_none() {
                return Ok(None);
            }
            match sources::open_document_input(&uri) {
                Ok(input) => (file.clone(), mime, Box::new(input)),
                Err(_) => return Ok(None),
            }
        } else {
            match file.read(gio::Cancellable::NONE) {
                Ok(input) => (file.clone(), String::new(), Box::new(input.into_read())),
                // Other media protocols and unavailable files are handled by playback.
                Err(_) => return Ok(None),
            }
        };
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(8192)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{uri}: {error}"))?;
    let Some(format) = playlist_format(&bytes, &mime, &base) else {
        return Ok(None);
    };
    input
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{uri}: {error}"))?;
    if is_hls(&bytes) {
        return Ok(None);
    }
    let playlist =
        PlaylistFile::read_as(Cursor::new(bytes), Path::new(base.uri().as_str()), format)
            .map_err(|error| format!("{uri}: {error}"))?;
    Ok(Some((base, playlist)))
}

fn playlist_format(bytes: &[u8], mime: &str, file: &gio::File) -> Option<PlaylistFormat> {
    if bytes.contains(&0) || is_hls(bytes) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with("#EXTM3U") || text.starts_with("#EXTINF:") {
        return Some(PlaylistFormat::M3u);
    }
    if text
        .get(..10)
        .is_some_and(|header| header.eq_ignore_ascii_case("[playlist]"))
    {
        return Some(PlaylistFormat::Pls);
    }
    if text.starts_with('<') {
        return text
            .contains("http://xspf.org/ns/0/")
            .then_some(PlaylistFormat::Xspf);
    }
    let guessed = if mime.is_empty() {
        let (content_type, _) = gio::content_type_guess(file.basename(), Some(bytes));
        gio::content_type_get_mime_type(&content_type)
    } else {
        None
    };
    match guessed
        .as_deref()
        .unwrap_or(mime)
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/mpegurl"
        | "audio/x-mpegurl"
        | "application/mpegurl"
        | "application/x-mpegurl"
        | "application/vnd.apple.mpegurl"
        | "application/m3u"
        | "audio/m3u"
        | "audio/x-m3u"
        | "audio/x-mp3-playlist" => Some(PlaylistFormat::M3u),
        "audio/x-scpls" | "audio/scpls" | "application/pls" => Some(PlaylistFormat::Pls),
        "application/xspf+xml" | "application/x-xspf+xml" => Some(PlaylistFormat::Xspf),
        _ => PlaylistFormat::from_path(Path::new(&display_name(file))),
    }
}

fn is_hls(bytes: &[u8]) -> bool {
    bytes
        .split(|byte| *byte == b'\n')
        .any(|line| line.trim_ascii_start().starts_with(b"#EXT-X-"))
}

fn display_name(file: &gio::File) -> String {
    if file.uri_scheme().as_deref() == Some("content")
        && let Ok(entry) = sources::stat_document(&file.uri())
    {
        return entry.name;
    }
    file.basename()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.uri().to_string())
}

fn canonical_local_file(
    local: Option<&sources::SourceConfiguration>,
    file: gio::File,
) -> gio::File {
    if let Some(path) = file.path()
        && let Some((_, access)) = local.and_then(|local| local.local_file_location(&path))
        && (access.is_file() || !path.is_file())
    {
        gio::File::for_path(access)
    } else {
        file
    }
}
