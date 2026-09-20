use std::io::{BufRead, Write};
use std::path::Path;

use futures_util::TryStreamExt;
use sqlx::Connection;

use crate::{
    Database, LibraryError, LibraryResult, PlaylistEntryWrite, PlaylistIdentity, PlaylistKey,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistPathMode {
    #[default]
    Automatic,
    Relative,
    Absolute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistFormat {
    M3u,
    Pls,
    Xspf,
}

impl PlaylistFormat {
    pub fn from_path(file: &Path) -> Option<Self> {
        match file.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "m3u" | "m3u8" => Some(Self::M3u),
            "pls" => Some(Self::Pls),
            "xspf" => Some(Self::Xspf),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct PlaylistFile {
    pub name: Option<String>,
    pub identity: Option<PlaylistIdentity>,
    pub entries: Vec<PlaylistFileEntry>,
    pub skipped: u64,
}

#[derive(Debug, Default)]
pub struct PlaylistFileEntry {
    pub locator: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_millis: Option<i64>,
    pub duration_present: bool,
    pub display: Option<String>,
    pub preserved: Option<PlaylistEntryWrite>,
}

fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::InvalidRequest(message.into())
}

fn duration(value: &str, multiplier: f64) -> Option<i64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| (value * multiplier) as i64)
}

fn display_title(entry: &mut PlaylistFileEntry, title: &str) {
    entry.display = Some(title.to_owned());
    if let Some((artist, title)) = title.split_once(" - ") {
        entry.artist = Some(artist.to_owned());
        entry.title = Some(title.to_owned());
    } else {
        entry.title = Some(title.to_owned());
    }
}

impl PlaylistFile {
    pub fn read(mut input: impl BufRead, file: &Path) -> LibraryResult<Self> {
        let format = PlaylistFormat::from_path(file)
            .ok_or_else(|| invalid("Unsupported playlist format"))?;
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        if bytes.contains(&0) {
            return Err(invalid("Playlist contains binary data"));
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(_) if format == PlaylistFormat::M3u => "",
            Err(error) => return Err(invalid(format!("Playlist is not UTF-8: {error}"))),
        };
        let text = text.trim_start_matches('\u{feff}');
        let mut playlist = Self::default();
        match format {
            PlaylistFormat::M3u => {
                let mut entry = PlaylistFileEntry::default();
                for line in bytes.split(|byte| *byte == b'\n') {
                    let line = match std::str::from_utf8(line) {
                        Ok(line) => line.trim_start_matches('\u{feff}').trim(),
                        Err(_) => {
                            playlist.skipped += 1;
                            entry = PlaylistFileEntry::default();
                            continue;
                        }
                    };
                    if line.is_empty() {
                        continue;
                    }
                    if let Some(value) = line.strip_prefix("#RUFIN-PLAYLIST:1:") {
                        playlist.identity = serde_json::from_str(value).ok();
                    } else if let Some(value) = line.strip_prefix("#PLAYLIST:") {
                        playlist.name = Some(value.to_owned());
                    } else if let Some(value) = line.strip_prefix("#RUFIN-MEDIA:1:") {
                        entry.preserved = serde_json::from_str(value).ok();
                    } else if let Some(value) = line.strip_prefix("#EXTINF:") {
                        if let Some((seconds, title)) = value.split_once(',') {
                            entry.duration_millis = duration(seconds, 1000.0);
                            entry.duration_present = true;
                            display_title(&mut entry, title);
                        }
                    } else if let Some(value) = line.strip_prefix("#EXTART:") {
                        entry.artist = Some(value.to_owned());
                        entry.display = None;
                    } else if let Some(value) = line.strip_prefix("#EXTALB:") {
                        entry.album = Some(value.to_owned());
                    } else if !line.starts_with('#') {
                        entry.locator = line.to_owned();
                        playlist.entries.push(std::mem::take(&mut entry));
                    }
                }
            }
            PlaylistFormat::Pls => {
                let mut entries = std::collections::BTreeMap::<u64, PlaylistFileEntry>::new();
                if !text
                    .lines()
                    .any(|line| line.trim().eq_ignore_ascii_case("[playlist]"))
                {
                    return Err(invalid("Not a PLS playlist"));
                }
                for line in text.lines() {
                    let Some((key, value)) = line.trim().split_once('=') else {
                        continue;
                    };
                    let key = key.to_ascii_lowercase();
                    if key == "playlistname" {
                        playlist.name = Some(value.to_owned());
                        continue;
                    }
                    if key == "x-rufin-playlist" {
                        playlist.identity = serde_json::from_str(value).ok();
                        continue;
                    }
                    let split = key.find(|c: char| c.is_ascii_digit()).unwrap_or(key.len());
                    let (field, index) = key.split_at(split);
                    let Ok(index) = index.parse::<u64>() else {
                        continue;
                    };
                    let entry = entries.entry(index).or_default();
                    match field {
                        "file" => entry.locator = value.to_owned(),
                        "title" => display_title(entry, value),
                        "length" => {
                            entry.duration_millis = duration(value, 1000.0);
                            entry.duration_present = true;
                        }
                        "x-rufin-media" => entry.preserved = serde_json::from_str(value).ok(),
                        _ => {}
                    }
                }
                playlist.entries = entries
                    .into_values()
                    .filter(|entry| !entry.locator.is_empty())
                    .collect();
            }
            PlaylistFormat::Xspf => {
                let document = roxmltree::Document::parse(text)
                    .map_err(|error| invalid(format!("Invalid XSPF: {error}")))?;
                let root = document.root_element();
                if root.tag_name().name() != "playlist"
                    || root.tag_name().namespace() != Some("http://xspf.org/ns/0/")
                {
                    return Err(invalid("Not an XSPF playlist"));
                }
                for child in root
                    .children()
                    .filter(|node| node.tag_name().namespace() == Some("http://xspf.org/ns/0/"))
                {
                    match child.tag_name().name() {
                        "title" => playlist.name = child.text().map(str::to_owned),
                        "meta"
                            if child.attribute("rel") == Some("https://rufin.app/playlist/1") =>
                        {
                            playlist.identity = child
                                .text()
                                .and_then(|value| serde_json::from_str(value).ok())
                        }
                        "trackList" => {
                            for track in child.children().filter(|node| {
                                node.has_tag_name(("http://xspf.org/ns/0/", "track"))
                            }) {
                                let mut entry = PlaylistFileEntry::default();
                                for field in track.children().filter(|node| {
                                    node.tag_name().namespace() == Some("http://xspf.org/ns/0/")
                                }) {
                                    let Some(value) = field.text() else { continue };
                                    match field.tag_name().name() {
                                        "location" if entry.locator.is_empty() => {
                                            entry.locator = xspf_location(field, file)
                                                .unwrap_or_else(|| value.to_owned())
                                        }
                                        "title" => entry.title = Some(value.to_owned()),
                                        "creator" => entry.artist = Some(value.to_owned()),
                                        "album" => entry.album = Some(value.to_owned()),
                                        "duration" => {
                                            entry.duration_millis = duration(value, 1.0);
                                            entry.duration_present = true;
                                        }
                                        "meta"
                                            if field.attribute("rel")
                                                == Some("https://rufin.app/media/1") =>
                                        {
                                            entry.preserved = serde_json::from_str(value).ok()
                                        }
                                        _ => {}
                                    }
                                }
                                if !entry.locator.is_empty() {
                                    playlist.entries.push(entry);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(playlist)
    }

    pub fn write(
        &self,
        file: &Path,
        mode: PlaylistPathMode,
        output: impl Write,
    ) -> LibraryResult<u64> {
        let mut writer = PlaylistWriter::new(
            output,
            file,
            mode,
            self.name.as_deref(),
            self.identity.as_ref(),
        )?;
        for (position, item) in self.entries.iter().enumerate() {
            let entry = item.to_entry(position as i64);
            writer.entry_with_locator(&entry, &item.locator)?;
        }
        writer.finish()
    }
}

impl PlaylistFileEntry {
    fn to_entry(&self, position: i64) -> PlaylistEntryWrite {
        let mut entry = self
            .preserved
            .clone()
            .unwrap_or_else(|| PlaylistEntryWrite {
                object_id: format!("playlist:{position}"),
                media_uri: self.locator.clone(),
                title: None,
                artist: None,
                album: None,
                album_display_artist: None,
                snapshot_at: 0,
                duration_millis: None,
                disc_number: None,
                track_number: None,
                year: None,
                release_date: None,
                source_format: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                position,
            });
        let unchanged_display = self
            .display
            .as_ref()
            .is_some_and(|value| *value == entry_display(&entry));
        if !unchanged_display {
            if let Some(value) = &self.title {
                entry.title = Some(value.clone());
                entry.artist = self.artist.clone();
            }
            if let Some(value) = &self.artist {
                entry.artist = Some(value.clone());
            }
        }
        if let Some(value) = &self.album {
            entry.album = Some(value.clone());
        }
        if self.duration_present {
            entry.duration_millis = self.duration_millis;
        }
        entry.position = position;
        entry
    }
}

#[derive(Debug)]
pub struct PlaylistImportReport {
    pub playlist: PlaylistKey,
    pub imported: u64,
    pub skipped: u64,
    pub changed: bool,
}

impl Database {
    pub async fn import_playlist_file(
        &self,
        input: impl BufRead,
        file: &Path,
        target: Option<PlaylistKey>,
        recognize: impl Fn(&str) -> Option<String>,
    ) -> LibraryResult<PlaylistImportReport> {
        let parsed = PlaylistFile::read(input, file)?;
        self.import_playlist_document(parsed, file, target, recognize)
            .await
    }

    pub async fn import_playlist_document(
        &self,
        parsed: PlaylistFile,
        file: &Path,
        target: Option<PlaylistKey>,
        recognize: impl Fn(&str) -> Option<String>,
    ) -> LibraryResult<PlaylistImportReport> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let report = import_playlist_on(&mut transaction, parsed, file, target, recognize).await?;
        transaction.commit().await?;
        Ok(report)
    }

    pub async fn export_playlist_file(
        &self,
        playlist: PlaylistKey,
        file: &Path,
        mode: PlaylistPathMode,
        output: impl Write,
    ) -> LibraryResult<u64> {
        let (_export, mut connection) = self.acquire_export().await?;
        export_playlist_on(&mut connection, playlist, file, mode, output).await
    }
}

pub(crate) async fn import_playlist_on(
    connection: &mut sqlx::SqliteConnection,
    parsed: PlaylistFile,
    file: &Path,
    target: Option<PlaylistKey>,
    recognize: impl Fn(&str) -> Option<String>,
) -> LibraryResult<PlaylistImportReport> {
    let object_id: String =
        sqlx::query_scalar("SELECT 'rufin:playlist:' || lower(hex(randomblob(16)))")
            .fetch_one(&mut *connection)
            .await?;
    let mut identity = PlaylistIdentity {
        source_id: None,
        object_id,
        name: Some(
            file.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        position: 0,
    };
    if let Some(value) = parsed.identity {
        if value.name.is_some() && !value.object_id.is_empty() && value.position >= 0 {
            let native = sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM main.playlists playlist JOIN main.source_ids source USING(source_key) WHERE source.object_id=?1 AND playlist.object_id=?2 AND playlist.name IS NULL) OR EXISTS(SELECT 1 FROM catalog.native_playlists playlist JOIN catalog.sources source USING(source_key) WHERE source.object_id=?1 AND playlist.object_id=?2)")
                .bind(&value.source_id).bind(&value.object_id).fetch_one(&mut *connection).await?;
            if !native {
                identity = value;
            }
        }
    }
    if let Some(name) = parsed.name {
        identity.name = Some(name);
    }
    let mut changed = true;
    let key = if let Some(key) = target {
        if key.raw() < 0 {
            return Err(invalid("Cannot replace a provider playlist from a file"));
        }
        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM main.playlists WHERE playlist_key=?1 AND name IS NOT NULL",
        )
        .bind(key)
        .fetch_optional(&mut *connection)
        .await?
        .ok_or_else(|| invalid("Playlist is not editable"))?;
        changed = Some(&name) != identity.name.as_ref();
        if changed {
            sqlx::query("UPDATE main.playlists SET name=?2, normalized_name=lower(?2), sort_text=lower(?2) WHERE playlist_key=?1")
                .bind(key).bind(&identity.name).execute(&mut *connection).await?;
        }
        key
    } else {
        crate::playlists::write_playlist_identity(connection, &identity).await?
    };
    let mut entries = Vec::with_capacity(parsed.entries.len());
    let mut skipped = parsed.skipped;
    for item in parsed.entries {
        let locator = recognize(&item.locator).unwrap_or_else(|| item.locator.clone());
        let Some(uri) = playlist_locator(&locator, file.parent().unwrap_or_else(|| Path::new(".")))
        else {
            skipped += 1;
            continue;
        };
        if confirmed_non_music(&uri) {
            skipped += 1;
            continue;
        }
        let mut entry = item.to_entry(entries.len() as i64);
        entry.media_uri = uri;
        if entry.object_id.is_empty() {
            entry.object_id = format!("playlist:{}", entries.len());
        }
        entries.push(entry);
    }
    let imported = entries.len() as u64;
    let mut entries_changed = true;
    if target.is_some() {
        let mut rows = sqlx::query_as::<_, PlaylistEntryWrite>("SELECT object_id,media_uri,title,artist,album,album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,position FROM main.playlist_entries WHERE playlist_key=?1 ORDER BY position")
            .bind(key).fetch(&mut *connection);
        entries_changed = false;
        for entry in &entries {
            let Some(mut stored) = rows.try_next().await? else {
                entries_changed = true;
                break;
            };
            // Private identifiers and snapshot age do not change playlist contents.
            stored.object_id.clone_from(&entry.object_id);
            stored.snapshot_at = entry.snapshot_at;
            if &stored != entry {
                entries_changed = true;
                break;
            }
        }
        if !entries_changed && rows.try_next().await?.is_some() {
            entries_changed = true;
        }
    }
    if entries_changed {
        changed = true;
        sqlx::query("DELETE FROM main.playlist_entries WHERE playlist_key=?1")
            .bind(key)
            .execute(&mut *connection)
            .await?;
        for mut entry in entries {
            if sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM main.playlist_entries WHERE playlist_key=?1 AND object_id=?2)").bind(key).bind(&entry.object_id).fetch_one(&mut *connection).await? {
            entry.object_id = sqlx::query_scalar("SELECT 'playlist:' || lower(hex(randomblob(16)))").fetch_one(&mut *connection).await?;
        }
            crate::playlists::write_playlist_entry(connection, key, &entry).await?;
        }
    }
    Ok(PlaylistImportReport {
        playlist: key,
        imported,
        skipped,
        changed,
    })
}

pub(crate) async fn export_playlist_on(
    connection: &mut sqlx::SqliteConnection,
    playlist: PlaylistKey,
    file: &Path,
    mode: PlaylistPathMode,
    output: impl Write,
) -> LibraryResult<u64> {
    let mut identity = sqlx::query_as::<_, PlaylistIdentity>(if playlist.raw() >= 0 {
        "SELECT source.object_id source_id,playlist.object_id,playlist.name,playlist.position FROM main.playlists playlist LEFT JOIN main.source_ids source USING(source_key) WHERE playlist.playlist_key=?1"
    } else {
        "SELECT source.object_id source_id,playlist.object_id,playlist.name,playlist.position FROM playlists playlist LEFT JOIN catalog.sources source USING(source_key) WHERE playlist.playlist_key=?1"
    }).bind(playlist).fetch_one(&mut *connection).await?;
    let name = identity.name.clone();
    if playlist.raw() < 0 {
        identity.name = None;
    }
    let mut writer = PlaylistWriter::new(output, file, mode, name.as_deref(), Some(&identity))?;
    let mut entries = sqlx::query_as::<_, PlaylistEntryWrite>("SELECT object_id,media_uri,title,artist,album,album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,position FROM playlist_entries WHERE playlist_key=?1 ORDER BY position")
        .bind(playlist).fetch(&mut *connection);
    while let Some(entry) = entries.try_next().await? {
        writer.entry(&entry)?;
    }
    writer.finish()
}

pub(crate) struct PlaylistWriter<'a, W: Write> {
    output: W,
    file: &'a Path,
    format: PlaylistFormat,
    mode: PlaylistPathMode,
    count: u64,
}

impl<'a, W: Write> PlaylistWriter<'a, W> {
    pub(crate) fn new(
        mut output: W,
        file: &'a Path,
        mode: PlaylistPathMode,
        name: Option<&str>,
        identity: Option<&PlaylistIdentity>,
    ) -> LibraryResult<Self> {
        let format = PlaylistFormat::from_path(file)
            .ok_or_else(|| invalid("Unsupported playlist format"))?;
        match format {
            PlaylistFormat::M3u => {
                writeln!(output, "#EXTM3U")?;
                if let Some(name) = name {
                    writeln!(output, "#PLAYLIST:{}", single_line(name))?;
                }
                if let Some(identity) = identity {
                    writeln!(
                        output,
                        "#RUFIN-PLAYLIST:1:{}",
                        serde_json::to_string(identity)?
                    )?;
                }
            }
            PlaylistFormat::Pls => {
                writeln!(output, "[playlist]")?;
                if let Some(name) = name {
                    writeln!(output, "PlaylistName={}", single_line(name))?;
                }
                if let Some(identity) = identity {
                    writeln!(
                        output,
                        "X-Rufin-Playlist={}",
                        serde_json::to_string(identity)?
                    )?;
                }
            }
            PlaylistFormat::Xspf => {
                writeln!(
                    output,
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<playlist version=\"1\" xmlns=\"http://xspf.org/ns/0/\">"
                )?;
                if let Some(name) = name {
                    writeln!(output, "<title>{}</title>", xml(name))?;
                }
                if let Some(identity) = identity {
                    writeln!(
                        output,
                        "<meta rel=\"https://rufin.app/playlist/1\">{}</meta>",
                        xml(&serde_json::to_string(identity)?)
                    )?;
                }
                writeln!(output, "<trackList>")?;
            }
        }
        Ok(Self {
            output,
            file,
            format,
            mode,
            count: 0,
        })
    }

    pub(crate) fn entry(&mut self, entry: &PlaylistEntryWrite) -> LibraryResult<()> {
        self.entry_with_locator(entry, &entry.media_uri)
    }

    fn entry_with_locator(
        &mut self,
        entry: &PlaylistEntryWrite,
        locator: &str,
    ) -> LibraryResult<()> {
        if locator.starts_with("local:")
            || playlist_locator(locator, self.file.parent().unwrap_or(Path::new("."))).is_none()
        {
            return Ok(());
        }
        let locator = export_locator(locator, self.file, self.mode);
        let display = entry_display(entry);
        let seconds = entry
            .duration_millis
            .map(|value| value as f64 / 1000.0)
            .unwrap_or(-1.0);
        match self.format {
            PlaylistFormat::M3u => {
                writeln!(
                    self.output,
                    "#RUFIN-MEDIA:1:{}",
                    serde_json::to_string(entry)?
                )?;
                writeln!(self.output, "#EXTINF:{seconds},{}", single_line(&display))?;
                writeln!(self.output, "{}", single_line(&locator))?;
            }
            PlaylistFormat::Pls => {
                let index = self.count + 1;
                writeln!(
                    self.output,
                    "File{index}={}\nTitle{index}={}\nLength{index}={seconds}\nX-Rufin-Media{index}={}",
                    single_line(&locator),
                    single_line(&display),
                    serde_json::to_string(entry)?
                )?;
            }
            PlaylistFormat::Xspf => {
                let locator = if url::Url::parse(&locator).is_ok() {
                    locator
                } else {
                    const URI_PATH: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
                        .add(b' ')
                        .add(b'%')
                        .add(b'#')
                        .add(b'?')
                        .add(b'"')
                        .add(b'<')
                        .add(b'>');
                    percent_encoding::utf8_percent_encode(&locator, URI_PATH).to_string()
                };
                writeln!(self.output, "<track><location>{}</location>", xml(&locator))?;
                for (field, value) in [
                    ("title", &entry.title),
                    ("creator", &entry.artist),
                    ("album", &entry.album),
                ] {
                    if let Some(value) = value {
                        writeln!(self.output, "<{field}>{}</{field}>", xml(value))?;
                    }
                }
                if let Some(value) = entry.duration_millis {
                    writeln!(self.output, "<duration>{value}</duration>")?;
                }
                writeln!(
                    self.output,
                    "<meta rel=\"https://rufin.app/media/1\">{}</meta></track>",
                    xml(&serde_json::to_string(entry)?)
                )?;
            }
        }
        self.count += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> LibraryResult<u64> {
        match self.format {
            PlaylistFormat::M3u => {}
            PlaylistFormat::Pls => {
                writeln!(self.output, "NumberOfEntries={}\nVersion=2", self.count)?
            }
            PlaylistFormat::Xspf => writeln!(self.output, "</trackList></playlist>")?,
        }
        Ok(self.count)
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

fn entry_display(entry: &PlaylistEntryWrite) -> String {
    match (&entry.artist, &entry.title) {
        (Some(artist), Some(title)) => format!("{artist} - {title}"),
        (_, title) => title.clone().unwrap_or_default(),
    }
}

fn xspf_location(node: roxmltree::Node<'_, '_>, file: &Path) -> Option<String> {
    let ancestors = node.ancestors().collect::<Vec<_>>();
    let mut base = file
        .to_str()
        .filter(|file| file.contains("://"))
        .and_then(|file| url::Url::parse(file).ok())
        .or_else(|| normalized_file_uri(file).and_then(|value| url::Url::parse(&value).ok()))?;
    for value in ancestors
        .iter()
        .rev()
        .filter_map(|node| node.attribute((roxmltree::NS_XML_URI, "base")))
    {
        base = base.join(value).ok()?;
    }
    base.join(node.text()?).ok().map(|uri| uri.to_string())
}

fn export_locator(uri: &str, file: &Path, mode: PlaylistPathMode) -> String {
    if mode != PlaylistPathMode::Absolute {
        if let Some(path) = crate::file_media_path(uri) {
            if let Some(relative) = relative_path(
                &playlist_host_path(&path),
                playlist_host_path(file).parent().unwrap_or(Path::new(".")),
            )
            .filter(|path| mode == PlaylistPathMode::Relative || !path.starts_with(".."))
            {
                let mut locator = relative
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                if !locator.contains(['\r', '\n']) {
                    if locator.starts_with('#')
                        || locator
                            .split('/')
                            .next()
                            .is_some_and(|part| part.contains(':'))
                    {
                        locator.insert_str(0, "./");
                    }
                    return locator;
                }
            }
        }
    }
    crate::file_media_path(uri)
        .and_then(|path| normalized_file_uri(&playlist_host_path(&path)))
        .unwrap_or_else(|| uri.to_owned())
}

fn relative_path(path: &Path, base: &Path) -> Option<std::path::PathBuf> {
    let path = std::path::absolute(path).ok()?;
    let base = std::path::absolute(base).ok()?;
    let target = path.components().collect::<Vec<_>>();
    let parent = base.components().collect::<Vec<_>>();
    if target.first() != parent.first() {
        return None;
    }
    let common = target
        .iter()
        .zip(&parent)
        .take_while(|(a, b)| a == b)
        .count();
    let mut relative = std::path::PathBuf::new();
    for _ in common..parent.len() {
        relative.push("..");
    }
    for component in &target[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn confirmed_non_music(uri: &str) -> bool {
    let Some(path) = crate::file_media_path(uri) else {
        return false;
    };
    // Missing files are retained. Existing file content is checked by the source's audio probe.
    path.is_dir()
}

fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

pub fn playlist_locator(value: &str, base: &Path) -> Option<String> {
    if let Some(unc) = value.strip_prefix("\\\\") {
        let (host, path) = unc.split_once('\\')?;
        let mut uri = url::Url::parse(&format!("file://{host}/")).ok()?;
        uri.set_path(&path.replace('\\', "/").replace('%', "%25"));
        return Some(uri.to_string());
    }
    // Drive letters are paths, not URI schemes, even when importing on Unix.
    if value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && value
            .as_bytes()
            .get(2)
            .is_some_and(|value| matches!(value, b'/' | b'\\'))
    {
        let mut uri = url::Url::parse("file:///").ok()?;
        uri.set_path(&value.replace('\\', "/").replace('%', "%25"));
        return Some(uri.to_string());
    }
    if Path::new(value).is_absolute() {
        return normalized_file_uri(Path::new(value));
    }
    if crate::source_entity_parts(value).is_some() {
        return Some(value.to_owned());
    }
    if let Some((_, backing, _, _)) = crate::cue_media_parts(value) {
        return crate::file_media_path(&backing).map(|_| value.to_owned());
    }
    if !value.contains("://") && !value.starts_with("file:") {
        let path = base.join(value);
        // Existing native names win when a Unix filename contains a backslash.
        return normalized_file_uri(&if value.contains('\\') && !path.exists() {
            base.join(value.replace('\\', "/"))
        } else {
            path
        });
    }
    if let Ok(uri) = url::Url::parse(value) {
        if !uri.username().is_empty()
            || uri.password().is_some()
            || uri.query_pairs().any(|(key, _)| {
                matches!(
                    key.to_ascii_lowercase().as_str(),
                    "api_key"
                        | "apikey"
                        | "access_token"
                        | "token"
                        | "password"
                        | "authorization"
                        | "x-emby-token"
                ) || (uri.path().contains("/rest/") && matches!(key.as_ref(), "p" | "t" | "s"))
            })
        {
            return None;
        }
        return crate::normalize_direct_media_uri(value).or_else(|| Some(uri.to_string()));
    }
    if value.contains("://") {
        return None;
    }
    let path = Path::new(value);
    normalized_file_uri(&base.join(path))
}

fn normalized_file_uri(path: &Path) -> Option<String> {
    // Url::parse removes dot segments that Url::from_file_path keeps verbatim.
    let absolute = std::path::absolute(path).ok()?;
    let uri = url::Url::from_file_path(absolute).ok()?;
    url::Url::parse(uri.as_str())
        .ok()
        .map(|uri| uri.to_string())
}

/// Return the host's path for a document-portal grant, without changing the path used for I/O.
pub fn playlist_host_path(path: &Path) -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = vec![0u8; 65536];
        for ancestor in path.ancestors() {
            if let Ok(length) = rustix::fs::getxattr(
                ancestor,
                "user.document-portal.host-path",
                bytes.as_mut_slice(),
            ) {
                let value = &bytes[..length];
                let value = value.strip_suffix(&[0]).unwrap_or(value);
                if !value.is_empty() {
                    let host = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(value));
                    if let Ok(suffix) = path.strip_prefix(ancestor) {
                        return host.join(suffix);
                    }
                }
            }
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn targeted_refresh_keeps_unchanged_occurrences_and_applies_file_edits() {
        let directory = tempfile::tempdir().unwrap();
        let database = crate::Database::open(directory.path().join("store.sqlite"))
            .await
            .unwrap();
        let file = directory.path().join("music.m3u8");
        let content = "#EXTM3U\n#PLAYLIST:Music\nmissing.flac\nother.flac\nmissing.flac\n";
        let first = database
            .import_playlist_file(std::io::Cursor::new(content), &file, None, |_| None)
            .await
            .unwrap();
        assert!(first.changed);
        let keys = database
            .playlist_entry_order(
                first.playlist,
                None,
                crate::PlaylistEntrySort::Position,
                false,
                "",
                &crate::ReadCancellation::new(),
            )
            .await
            .unwrap();
        let same = database
            .import_playlist_file(
                std::io::Cursor::new(content),
                &file,
                Some(first.playlist),
                |_| None,
            )
            .await
            .unwrap();
        assert!(!same.changed);
        assert_eq!(same.imported, 3);
        assert_eq!(
            database
                .playlist_entry_order(
                    first.playlist,
                    None,
                    crate::PlaylistEntrySort::Position,
                    false,
                    "",
                    &crate::ReadCancellation::new()
                )
                .await
                .unwrap(),
            keys
        );
        let renamed = content.replace("#PLAYLIST:Music", "#PLAYLIST:Renamed");
        let rename = database
            .import_playlist_file(
                std::io::Cursor::new(&renamed),
                &file,
                Some(first.playlist),
                |_| None,
            )
            .await
            .unwrap();
        assert!(rename.changed);
        assert_eq!(
            database
                .playlist_entry_order(
                    first.playlist,
                    None,
                    crate::PlaylistEntrySort::Position,
                    false,
                    "",
                    &crate::ReadCancellation::new()
                )
                .await
                .unwrap(),
            keys
        );
        let reordered = "#EXTM3U\n#PLAYLIST:Renamed\nmissing.flac\nmissing.flac\nother.flac\n";
        let reorder = database
            .import_playlist_file(
                std::io::Cursor::new(reordered),
                &file,
                Some(first.playlist),
                |_| None,
            )
            .await
            .unwrap();
        assert!(reorder.changed);
        assert_eq!(reorder.imported, 3);
        let mut output = Vec::new();
        database
            .export_playlist_file(
                first.playlist,
                &file,
                super::PlaylistPathMode::Relative,
                &mut output,
            )
            .await
            .unwrap();
        let mut roundtrip = super::PlaylistFile::read(std::io::Cursor::new(output), &file).unwrap();
        assert_eq!(
            roundtrip
                .entries
                .iter()
                .map(|entry| entry.locator.as_str())
                .collect::<Vec<_>>(),
            vec!["missing.flac", "missing.flac", "other.flac"]
        );
        let snapshot = roundtrip.entries[0].preserved.as_mut().unwrap();
        snapshot.object_id = "external-occurrence".into();
        snapshot.snapshot_at += 1;
        let private_only = database
            .import_playlist_document(roundtrip, &file, Some(first.playlist), |_| None)
            .await
            .unwrap();
        assert!(!private_only.changed);
        let unchanged = database
            .import_playlist_file(
                std::io::Cursor::new(reordered),
                &file,
                Some(first.playlist),
                |_| None,
            )
            .await
            .unwrap();
        assert!(!unchanged.changed);
        let empty = database
            .import_playlist_file(
                std::io::Cursor::new("#EXTM3U\n#PLAYLIST:Renamed\n"),
                &file,
                Some(first.playlist),
                |_| None,
            )
            .await
            .unwrap();
        assert!(empty.changed);
        assert_eq!(empty.imported, 0);
        database.close().await.unwrap();
    }
    use super::*;

    #[tokio::test]
    async fn malformed_m3u_lines_leave_valid_entries_and_clear_pending_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let file = directory.path().join("mix.m3u");
        let report = database
            .import_playlist_file(
                std::io::Cursor::new(
                    b"#EXTM3U\nfirst.flac\n#EXTINF:12,Wrong title\nbad\xff.flac\nlast.flac\n",
                ),
                &file,
                None,
                |_| None,
            )
            .await
            .unwrap();
        assert_eq!((report.imported, report.skipped), (2, 1));
        let mut output = Vec::new();
        database
            .export_playlist_file(
                report.playlist,
                &file,
                PlaylistPathMode::Automatic,
                &mut output,
            )
            .await
            .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\nfirst.flac\n"));
        assert!(text.contains("\nlast.flac\n"));
        assert!(!text.contains("Wrong title"));
    }

    #[tokio::test]
    async fn formats_preserve_order_duplicates_identities_and_relative_siblings() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let file = directory.path().join("lists/mix.m3u8");
        let song = directory.path().join("Artist/100% #1 & café.flac");
        let uri = url::Url::from_file_path(&song).unwrap().to_string();
        let provider = crate::source_entity_uri(&crate::SourceId::new("source"), "track", "one");
        let cue = crate::cue_media_uri("disc:1", &uri, 1000, 2000);
        let original = database.import_playlist_file(std::io::Cursor::new(format!(
            "#EXTM3U\n#PLAYLIST:Mix & café\n#EXTINF:12.5,Track\n#EXTART:Earth - Wind\n../Artist/100% #1 & café.flac\n../Artist/100% #1 & café.flac\n{provider}\n{cue}\nhttps://example.test/stream\n"
        )), &file, None, |_| None).await.unwrap();
        for extension in ["m3u8", "pls", "xspf"] {
            let export = file.with_extension(extension);
            let mut output = Vec::new();
            assert_eq!(
                database
                    .export_playlist_file(
                        original.playlist,
                        &export,
                        PlaylistPathMode::Relative,
                        &mut output
                    )
                    .await
                    .unwrap(),
                5
            );
            let text = String::from_utf8(output.clone()).unwrap();
            assert!(text.contains("../Artist/100"), "{text}");
            let parsed = PlaylistFile::read(std::io::Cursor::new(&output), &export).unwrap();
            assert_eq!(parsed.name.as_deref(), Some("Mix & café"));
            assert_eq!(parsed.entries[0].duration_millis, Some(12500));
            assert_eq!(
                parsed.entries[0].to_entry(0).title.as_deref(),
                Some("Track")
            );
            assert_eq!(
                parsed.entries[0].to_entry(0).artist.as_deref(),
                Some("Earth - Wind")
            );
            let imported = database
                .import_playlist_file(std::io::Cursor::new(output), &export, None, |_| None)
                .await
                .unwrap();
            assert_eq!(imported.playlist, original.playlist);
            assert_eq!(imported.imported, 5);
            let (_permit, mut connection) = database
                .acquire_general(&crate::ReadCancellation::new())
                .await
                .unwrap();
            let actual: Vec<String> = sqlx::query_scalar(
                "SELECT media_uri FROM playlist_entries WHERE playlist_key=?1 ORDER BY position",
            )
            .bind(imported.playlist)
            .fetch_all(&mut *connection)
            .await
            .unwrap();
            assert_eq!(
                actual,
                vec![
                    uri.clone(),
                    uri.clone(),
                    provider.clone(),
                    cue.clone(),
                    "https://example.test/stream".to_owned()
                ]
            );
        }
    }

    #[tokio::test]
    async fn standard_edits_override_snapshots_and_target_overrides_file_identity() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let file = directory.path().join("mix.m3u8");
        let first = database
            .import_playlist_file(
                std::io::Cursor::new(
                    "#EXTM3U\n#PLAYLIST:Old\n#EXTINF:12,Artist - Old track\nold.flac\n",
                ),
                &file,
                None,
                |_| None,
            )
            .await
            .unwrap();
        let second = database
            .import_playlist_file(
                std::io::Cursor::new("#EXTM3U\nother.flac\n"),
                &file,
                None,
                |_| None,
            )
            .await
            .unwrap();
        let mut output = Vec::new();
        database
            .export_playlist_file(
                first.playlist,
                &file,
                PlaylistPathMode::Automatic,
                &mut output,
            )
            .await
            .unwrap();
        let text = String::from_utf8(output)
            .unwrap()
            .replace("#PLAYLIST:Old", "#PLAYLIST:New")
            .replace("#EXTINF:12,Artist - Old track", "#EXTINF:30,New track")
            .replace("\nold.flac\n", "\nnew.flac\n");
        let updated = database
            .import_playlist_file(
                std::io::Cursor::new(text),
                &file,
                Some(second.playlist),
                |_| None,
            )
            .await
            .unwrap();
        assert_eq!(updated.playlist, second.playlist);
        let mut output = Vec::new();
        database
            .export_playlist_file(
                second.playlist,
                &file,
                PlaylistPathMode::Automatic,
                &mut output,
            )
            .await
            .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("#PLAYLIST:New\n"));
        assert!(text.contains("#EXTINF:30,New track\nnew.flac\n"));
        assert!(
            database
                .import_playlist_file(
                    std::io::Cursor::new("binary\0data"),
                    &file,
                    Some(second.playlist),
                    |_| None
                )
                .await
                .is_err()
        );
        assert_eq!(
            database
                .playlist_file_uri_page(second.playlist, -1)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn paths_keep_literal_characters_and_normalize_parent_segments() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("lists/mix.m3u8");
        let actual =
            playlist_locator("../Artist: Album/song.flac", file.parent().unwrap()).unwrap();
        assert_eq!(
            crate::file_media_path(&actual).unwrap(),
            directory.path().join("Artist: Album/song.flac")
        );
        assert_eq!(
            playlist_locator("C:\\Music\\100% #1.flac", directory.path()).unwrap(),
            "file:///C:/Music/100%25%20%231.flac"
        );
        assert_eq!(
            playlist_locator("track: title.flac", directory.path()).unwrap(),
            url::Url::from_file_path(directory.path().join("track: title.flac"))
                .unwrap()
                .to_string()
        );
        assert_eq!(
            crate::file_media_path(
                &playlist_locator(".\\Artist\\song.flac", directory.path()).unwrap()
            )
            .unwrap(),
            directory.path().join("Artist/song.flac")
        );
        assert_eq!(
            playlist_locator("\\\\server\\share\\song.flac", directory.path()).unwrap(),
            "file://server/share/song.flac"
        );
    }

    #[test]
    fn export_modes_distinguish_descendants_and_sibling_folders() {
        let directory = tempfile::tempdir().unwrap();
        let song = directory.path().join("Artist/song.flac");
        let uri = url::Url::from_file_path(&song).unwrap().to_string();
        let beside = directory.path().join("mix.m3u8");
        let sibling = directory.path().join("Playlists/mix.m3u8");
        assert_eq!(
            export_locator(&uri, &beside, PlaylistPathMode::Automatic),
            "Artist/song.flac"
        );
        assert_eq!(
            export_locator(&uri, &sibling, PlaylistPathMode::Automatic),
            uri
        );
        assert_eq!(
            export_locator(&uri, &sibling, PlaylistPathMode::Relative),
            "../Artist/song.flac"
        );
        assert_eq!(
            export_locator(&uri, &beside, PlaylistPathMode::Absolute),
            uri
        );
    }

    #[test]
    fn document_writer_keeps_locations_resolved_by_remote_source() {
        for extension in ["m3u8", "pls", "xspf"] {
            for locator in [
                "https://example.test/Music/Artist/song.flac",
                "../Artist/song.flac",
            ] {
                let file = std::path::PathBuf::from(format!("Playlists/mix.{extension}"));
                let playlist = PlaylistFile {
                    entries: vec![PlaylistFileEntry {
                        locator: locator.to_owned(),
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                let mut output = Vec::new();
                playlist
                    .write(&file, PlaylistPathMode::Automatic, &mut output)
                    .unwrap();
                let text = String::from_utf8(output).unwrap();
                assert!(text.contains(locator), "{text}");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn portal_grants_export_in_host_coordinates() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let music = directory.path().join("granted-music");
        let lists = directory.path().join("granted-lists");
        std::fs::create_dir(&music).unwrap();
        std::fs::create_dir(&lists).unwrap();
        let host = directory.path().join("host/Music");
        for (grant, original) in [(&music, host.clone()), (&lists, host.join("Playlists"))] {
            rustix::fs::setxattr(
                grant,
                "user.document-portal.host-path",
                original.as_os_str().as_bytes(),
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
        }
        let song = music.join("Artist/song.flac");
        let destination = lists.join("mix.m3u8");
        assert_eq!(
            playlist_host_path(&destination),
            host.join("Playlists/mix.m3u8")
        );
        let uri = url::Url::from_file_path(&song).unwrap().to_string();
        assert_eq!(
            export_locator(&uri, &destination, PlaylistPathMode::Relative),
            "../Artist/song.flac"
        );
        assert_eq!(
            export_locator(&uri, &destination, PlaylistPathMode::Absolute),
            url::Url::from_file_path(host.join("Artist/song.flac"))
                .unwrap()
                .to_string()
        );
    }

    #[test]
    fn xspf_resolves_xml_base_and_keeps_alternate_locations_as_one_track() {
        let text = r#"<playlist xmlns="http://xspf.org/ns/0/" version="1" xml:base="../Music/">
          <trackList><track xml:base="Artist/"><location>100%25%20%231.flac</location>
          <location>https://example.test/alternative</location><duration>1234</duration></track></trackList>
        </playlist>"#;
        let directory = tempfile::tempdir().unwrap();
        let parsed = PlaylistFile::read(
            std::io::Cursor::new(text),
            &directory.path().join("lists/mix.xspf"),
        )
        .unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(
            crate::file_media_path(&parsed.entries[0].locator).unwrap(),
            directory.path().join("Music/Artist/100% #1.flac")
        );
        let remote = PlaylistFile::read(
            std::io::Cursor::new(text),
            Path::new("https://example.test/lists/mix.xspf"),
        )
        .unwrap();
        assert_eq!(
            remote.entries[0].locator,
            "https://example.test/Music/Artist/100%25%20%231.flac"
        );
    }

    #[tokio::test]
    async fn smart_export_streams_large_direct_http_results_without_current() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let file = directory.path().join("large.m3u8");
        {
            let mut input = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
            for index in 0..10001 {
                writeln!(
                    input,
                    "#EXTINF:12,Song {index}\nhttps://example.test/{index}"
                )
                .unwrap();
            }
        }
        database
            .import_playlist_file(
                std::io::BufReader::new(std::fs::File::open(&file).unwrap()),
                &file,
                None,
                |_| None,
            )
            .await
            .unwrap();
        let key = database
            .create_smart_playlist("All", &crate::SmartPlaylistDefinition::default())
            .await
            .unwrap();
        let mut output = tempfile::tempfile().unwrap();
        assert_eq!(
            database
                .export_smart_playlist_file(
                    key,
                    None,
                    None,
                    0,
                    &file,
                    PlaylistPathMode::Automatic,
                    &mut output
                )
                .await
                .unwrap(),
            10001
        );
    }

    #[tokio::test]
    async fn native_export_imports_as_an_authored_copy_without_combining_entries() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        {
            let mut writer = database.writer().await.unwrap();
            let connection = writer.as_mut().unwrap();
            sqlx::raw_sql("INSERT INTO catalog.sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (5,'source','Source','source',zeroblob(32));
              INSERT INTO catalog.native_playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text) VALUES(8,5,'native','Native','native','native');
              INSERT INTO catalog.native_playlist_entries(playlist_key,object_id,media_uri,title,position) VALUES(8,'one','https://example.test/song','Song',0),(8,'two','https://example.test/song','Song',1);")
              .execute(connection).await.unwrap();
        }
        let file = directory.path().join("native.m3u8");
        let mut output = Vec::new();
        database
            .export_playlist_file(
                PlaylistKey::from_raw(-8),
                &file,
                PlaylistPathMode::Automatic,
                &mut output,
            )
            .await
            .unwrap();
        let copy = database
            .import_playlist_file(std::io::Cursor::new(output), &file, None, |_| None)
            .await
            .unwrap();
        assert_eq!(copy.imported, 2);
        assert_eq!(
            database
                .playlist_owner(copy.playlist, &crate::ReadCancellation::new())
                .await
                .unwrap(),
            Some((None, None))
        );
        let mut copied = Vec::new();
        assert_eq!(
            database
                .export_playlist_file(
                    copy.playlist,
                    &file,
                    PlaylistPathMode::Automatic,
                    &mut copied
                )
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            String::from_utf8(copied)
                .unwrap()
                .matches("\nhttps://example.test/song\n")
                .count(),
            2
        );
        let mut native = Vec::new();
        assert_eq!(
            database
                .export_playlist_file(
                    PlaylistKey::from_raw(-8),
                    &file,
                    PlaylistPathMode::Automatic,
                    &mut native
                )
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn local_paths_import_without_a_configured_source() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let path = directory.path().join("café %.flac");
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        let input = format!("#EXTM3U\ncafé %.flac\n{}\n{uri}\n", path.display());
        let report = database
            .import_playlist_file(
                std::io::Cursor::new(input),
                &directory.path().join("mix.m3u8"),
                None,
                |_| None,
            )
            .await
            .unwrap();
        assert_eq!(report.imported, 3);
        let entries = database
            .playlist_file_uri_page(report.playlist, -1)
            .await
            .unwrap();
        assert_eq!(entries.len(), 3);
        for (_, actual) in entries {
            assert_eq!(actual, uri);
            assert_eq!(crate::file_media_path(&actual).unwrap(), path);
        }
    }

    #[tokio::test]
    async fn ordinary_m3u_preserves_unicode_duplicates_missing_paths_and_exact_reimport() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let file = directory.path().join("mix.m3u8");
        let report = database.import_playlist_file(std::io::Cursor::new("#EXTM3U\n#PLAYLIST:Björk\n#EXTINF:12.5,Björk - Jóga\nmissing.flac\nmissing.flac\nhttps://example.test/song\nhttps://user:secret@example.test/song\nhttps:// bad\n"), &file, None, |_| None).await.unwrap();
        assert_eq!((report.imported, report.skipped), (3, 2));
        let mut output = Vec::new();
        assert_eq!(
            database
                .export_playlist_file(
                    report.playlist,
                    &file,
                    PlaylistPathMode::Automatic,
                    &mut output
                )
                .await
                .unwrap(),
            3
        );
        let text = String::from_utf8(output.clone()).unwrap();
        assert!(text.contains("#PLAYLIST:Björk"));
        assert!(text.contains("\nmissing.flac\n"));
        assert!(!text.contains("secret"));
        let again = database
            .import_playlist_file(std::io::Cursor::new(output), &file, None, |_| None)
            .await
            .unwrap();
        assert_eq!(again.playlist, report.playlist);
        assert_eq!(again.imported, 3);
    }
}
