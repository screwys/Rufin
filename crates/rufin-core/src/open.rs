//! Open host media through the ordinary playback queue.
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use gio::prelude::*;
use library::{PlaylistFile, PlaylistFormat, QueueProvenance, TrackReference};

pub async fn arguments(
    queue: Arc<crate::playback::PlaybackOwner>,
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
    queue: Arc<crate::playback::PlaybackOwner>,
    database: &library::Database,
    local: Option<&sources::SourceConfiguration>,
    files: Vec<gio::File>,
) -> Result<(), String> {
    let local = local.cloned();
    let database = database.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
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
            .map(|file| library::PlaylistFileEntry {
                locator: file.uri().to_string(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let mut playlists = vec![PlaylistFrame {
            base: None,
            identity: String::new(),
            entries: entries.into_iter(),
        }];
        let mut items = Vec::new();
        let mut opened = false;
        let mut failure = None;
        let flush = |items: &mut Vec<TrackReference>, opened: &mut bool| -> Result<(), String> {
            if items.is_empty() {
                return Ok(());
            }
            let input = library::QueueLoad::items(
                std::mem::take(items)
                    .into_iter()
                    .map(|item| (item, QueueProvenance::Manual))
                    .collect(),
            );
            let entries = runtime
                .block_on(database.queue_entries(input, None))
                .map_err(|error| error.to_string())?
                .entries;
            queue.play_selection(crate::playback::PlayRequest::from_input(
                library::QueueLoad::Captured(entries.into()),
                if *opened {
                    crate::playback::QueuePlacement::End
                } else {
                    crate::playback::QueuePlacement::Replace { anchor_index: 0 }
                },
            ));
            *opened = true;
            Ok(())
        };
        while let Some(frame) = playlists.last_mut() {
            let Some(entry) = frame.entries.next() else {
                playlists.pop();
                continue;
            };
            let base = frame.base.clone();
            let result = (|| -> Result<(), String> {
                let location = match base.as_ref() {
                    None => entry.locator.clone(),
                    Some(base) if base.path().is_some() => {
                        let path = base.path().expect("local playlist path");
                        let Some(location) = library::playlist_locator(
                            &entry.locator,
                            path.parent().unwrap_or(Path::new(".")),
                        ) else {
                            return Ok(());
                        };
                        location
                    }
                    Some(base)
                        if base.uri_scheme().as_deref() == Some("content")
                            && url::Url::parse(&entry.locator).is_err() =>
                    {
                        sources::resolve_document_relative(
                            &base.uri(),
                            &entry.locator,
                            &document_roots,
                        )
                        .map_err(|error| error.to_string())?
                        .uri
                    }
                    Some(base) => {
                        let Some(location) = url::Url::parse(&base.uri())
                            .and_then(|base| base.join(&entry.locator))
                            .ok()
                        else {
                            return Ok(());
                        };
                        location.into()
                    }
                };
                let file = canonical_local_file(local.as_ref(), gio::File::for_uri(&location));
                let identity = file_identity(&file);
                if playlists.iter().any(|frame| frame.identity == identity) {
                    tracing::warn!("skipping circular playlist reference");
                    return Ok(());
                }
                let mut item = entry.to_entry(0).track;
                let known_media = item.media_uri == file.uri()
                    && item
                        .metadata
                        .source_format
                        .as_deref()
                        .is_some_and(|format| !format.trim().is_empty());
                item.media_uri = file.uri().to_string();
                if item.metadata.title.is_empty() {
                    item.metadata.title = display_name(&file);
                }
                // Ready entries must reach the queue before another remote or nested read.
                if !known_media && (file.path().is_none() || extension_format(&file).is_some()) {
                    flush(&mut items, &mut opened)?;
                }
                if let Some((base, playlist)) = if known_media {
                    None
                } else {
                    read_playlist(&client, &file)?
                } {
                    let identity = file_identity(&base);
                    if playlists.iter().any(|frame| frame.identity == identity) {
                        tracing::warn!("skipping circular playlist reference");
                    } else {
                        playlists.push(PlaylistFrame {
                            base: Some(base),
                            identity,
                            entries: playlist.entries.into_iter(),
                        });
                    }
                    return Ok(());
                }
                let metadata = if file.uri_scheme().as_deref() == Some("content") {
                    runtime
                        .block_on(sources::read_document_track_reference(&file.uri()))
                        .ok()
                        .flatten()
                } else {
                    file.path()
                        .and_then(|path| sources::read_local_track_reference(&path))
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
                items.push(item);
                if !opened || items.len() == 128 {
                    flush(&mut items, &mut opened)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                tracing::warn!(%error, "could not open media item");
                failure.get_or_insert(error);
            }
        }
        flush(&mut items, &mut opened)?;
        if opened {
            Ok(())
        } else {
            Err(failure.unwrap_or_else(|| localization::tr("No playable entries were found")))
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

struct PlaylistFrame {
    base: Option<gio::File>,
    identity: String,
    entries: std::vec::IntoIter<library::PlaylistFileEntry>,
}

fn file_identity(file: &gio::File) -> String {
    file.path()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| gio::File::for_path(path).uri().to_string())
        .unwrap_or_else(|| file.uri().to_string())
}

fn read_playlist(
    client: &reqwest::blocking::Client,
    file: &gio::File,
) -> Result<Option<(gio::File, PlaylistFile)>, String> {
    let uri = file.uri();
    let (base, mime, mut input): (_, _, Box<dyn Read>) =
        if matches!(file.uri_scheme().as_deref(), Some("http" | "https")) {
            if extension_format(file).is_none()
                && let Ok(response) = client.head(uri.as_str()).send()
                && response.status().is_success()
                && media_mime(response_mime(&response))
            {
                return Ok(None);
            }
            let response = client
                .get(uri.as_str())
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|error| error.without_url().to_string())?;
            let mime = response_mime(&response).to_owned();
            if media_mime(&mime) {
                return Ok(None);
            }
            (
                gio::File::for_uri(response.url().as_str()),
                mime,
                Box::new(response),
            )
        } else if file.uri_scheme().as_deref() == Some("content") {
            let mime = sources::stat_document(&uri)
                .map(|entry| entry.mime_type)
                .unwrap_or_default();
            if media_mime(&mime) {
                return Ok(None);
            }
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
        .map_err(|error| error.to_string())?;
    let Some(format) = playlist_format(&bytes, &mime, &base) else {
        return Ok(None);
    };
    input
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if is_hls(&bytes) {
        return Ok(None);
    }
    let playlist =
        PlaylistFile::read_as(Cursor::new(bytes), Path::new(base.uri().as_str()), format)
            .map_err(|error| error.to_string())?;
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
    mime_format(guessed.as_deref().unwrap_or(mime)).or_else(|| extension_format(file))
}

fn response_mime(response: &reqwest::blocking::Response) -> &str {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

fn media_mime(mime: &str) -> bool {
    let category = mime.split('/').next().unwrap_or_default().trim();
    (category.eq_ignore_ascii_case("audio") || category.eq_ignore_ascii_case("video"))
        && mime_format(mime).is_none()
}

fn mime_format(mime: &str) -> Option<PlaylistFormat> {
    match mime
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
        _ => None,
    }
}

fn extension_format(file: &gio::File) -> Option<PlaylistFormat> {
    if matches!(file.uri_scheme().as_deref(), Some("http" | "https")) {
        url::Url::parse(&file.uri())
            .ok()
            .and_then(|url| PlaylistFormat::from_path(Path::new(url.path())))
    } else {
        PlaylistFormat::from_path(Path::new(&display_name(file)))
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
        .unwrap_or_else(|| {
            url::Url::parse(&file.uri())
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default()
        })
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
