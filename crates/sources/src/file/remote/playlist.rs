use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

use library::{Database, PlaylistImportReport};
use url::Url;

use super::RemoteSource;
use crate::{SourceError, SourceResult};

impl RemoteSource {
    pub(crate) async fn import_playlist_file(
        &self,
        database: &Database,
        path: &str,
    ) -> SourceResult<PlaylistImportReport> {
        let location = self.location(path)?;
        let copy = self.working_copy(path, &location).await.map_err(error)?;
        let prepared = tempfile::NamedTempFile::new().map_err(error)?;
        let mut input = BufReader::new(std::fs::File::open(&copy.file).map_err(error)?);
        let mut output = BufWriter::new(prepared.reopen().map_err(error)?);
        let mut line = String::new();
        while playlist_line(&mut input, &mut line)? {
            let value = line.trim_start_matches('\u{feff}').trim();
            if !value.is_empty()
                && !value.starts_with('#')
                && let Some(location) = self.playlist_location(path, value)
            {
                let uri = database
                    .media_uri_for_file_path(self.source_id.as_str(), &location)
                    .await?
                    .unwrap_or_else(|| {
                        library::source_entity_uri(
                            &self.source_id,
                            "track",
                            &format!("file:{:016x}", crate::policy::stable_hash(&location)),
                        )
                    });
                writeln!(output, "{uri}").map_err(error)?;
            } else {
                output.write_all(line.as_bytes()).map_err(error)?;
            }
        }
        output.flush().map_err(error)?;
        database
            .import_playlist_m3u(
                BufReader::new(prepared.reopen().map_err(error)?),
                Path::new(path),
                |_| None,
            )
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn save_playlist_file(
        &self,
        database: &Database,
        path: &str,
        file: tempfile::TempPath,
    ) -> SourceResult<()> {
        self.location(path)?;
        let prepared = tempfile::NamedTempFile::new().map_err(error)?;
        let mut input = BufReader::new(std::fs::File::open(&file).map_err(error)?);
        let mut output = BufWriter::new(prepared.reopen().map_err(error)?);
        let mut line = String::new();
        while playlist_line(&mut input, &mut line)? {
            let uri = line.trim();
            if library::source_entity_parts(uri)
                .is_some_and(|(source, kind, _)| source == self.source_id && kind == "track")
                && let Some(observation) = database.observed_media_file(uri).await?
                && observation.cue_start_millis.is_none()
            {
                let file = self.relative(&observation.path)?;
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                let parents = parent
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>();
                let parts = file.split('/').collect::<Vec<_>>();
                let common = parents
                    .iter()
                    .zip(&parts)
                    .take_while(|(a, b)| a == b)
                    .count();
                let mut locator = "../".repeat(parents.len() - common) + &parts[common..].join("/");
                if locator.contains(['\r', '\n', ':']) {
                    locator = self.location(&file)?;
                }
                if locator.starts_with('#') {
                    locator.insert_str(0, "./");
                }
                writeln!(output, "{locator}").map_err(error)?;
            } else {
                output.write_all(line.as_bytes()).map_err(error)?;
            }
        }
        output.flush().map_err(error)?;
        drop(output);
        self.save_contents(path.into(), prepared.into_temp_path())
            .await
            .map_err(error)
    }

    fn playlist_location(&self, playlist: &str, value: &str) -> Option<String> {
        if let Ok(uri) = Url::parse(value) {
            if !uri.username().is_empty()
                || uri.password().is_some()
                || uri.query().is_some()
                || uri.fragment().is_some()
            {
                return None;
            }
            for address in std::iter::once(&self.namespace_url)
                .chain(std::iter::once(&self.settings.url))
                .chain(&self.settings.alternate_urls)
            {
                let base = super::collection_url(address).ok()?;
                if uri.scheme() == base.scheme()
                    && uri.host() == base.host()
                    && uri.port() == base.port()
                    && let Some(relative) = uri.path().strip_prefix(base.path())
                {
                    return self
                        .location(
                            &percent_encoding::percent_decode_str(relative)
                                .decode_utf8()
                                .ok()?,
                        )
                        .ok();
                }
            }
            None
        } else {
            self.location(&super::referenced_path(playlist, value).ok()?)
                .ok()
        }
    }
}

fn playlist_line(input: &mut impl BufRead, line: &mut String) -> SourceResult<bool> {
    line.clear();
    let length = input.take(1024 * 1024).read_line(line).map_err(error)?;
    if length == 1024 * 1024 {
        return Err(SourceError::InvalidRequest("Playlist line exceeds 1 MiB"));
    }
    Ok(length != 0)
}

fn error(error: impl std::fmt::Display) -> SourceError {
    SourceError::Other(error.to_string())
}
