//! Bounded synchronous reads for the existing tag parser, driven from its blocking worker.

use std::io::{self, Read, Seek, SeekFrom};

use futures_util::TryStreamExt;
use playback::ResolvedStream;
use reqwest::{
    Client, StatusCode,
    header::{self, HeaderValue},
};
use tokio::io::AsyncReadExt;

use crate::{SourceError, SourceResult};

const READ_BYTES: u64 = 64 * 1024;

pub(crate) struct FileReader {
    input: Input,
    _stream: ResolvedStream,
    failed: bool,
}

enum Input {
    File(std::fs::File),
    Http(HttpFile),
}

struct HttpFile {
    runtime: tokio::runtime::Handle,
    client: Client,
    uri: String,
    length: u64,
    position: u64,
    buffer: Vec<u8>,
    buffer_start: u64,
    validator: Option<HeaderValue>,
}

impl FileReader {
    pub async fn open(stream: ResolvedStream) -> SourceResult<Self> {
        let uri = url::Url::parse(stream.uri()).map_err(|e| SourceError::Other(e.to_string()))?;
        let input = if uri.scheme() == "file" {
            let path = uri
                .to_file_path()
                .map_err(|_| SourceError::InvalidRequest("Invalid temporary media file URI"))?;
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|e| SourceError::Other(e.to_string()))?;
            Input::File(file.into_std().await)
        } else {
            let client = Client::builder()
                .no_proxy()
                .read_timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| SourceError::Network(e.to_string()))?;
            let (buffer, length, validator) =
                read_chunk(&client, stream.uri(), 0, READ_BYTES - 1, None)
                    .await
                    .map_err(|e| SourceError::Network(e.to_string()))?;
            Input::Http(HttpFile {
                runtime: tokio::runtime::Handle::current(),
                client,
                uri: stream.uri().to_owned(),
                length,
                position: 0,
                buffer,
                buffer_start: 0,
                validator,
            })
        };
        Ok(Self {
            input,
            _stream: stream,
            failed: false,
        })
    }

    pub fn failed(&self) -> bool {
        self.failed
    }
}

impl Read for FileReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let result = self.read_input(output);
        self.failed |= result.is_err();
        result
    }
}

impl FileReader {
    fn read_input(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let input = match &mut self.input {
            Input::File(file) => return file.read(output),
            Input::Http(input) => input,
        };
        if output.is_empty() || input.position >= input.length {
            return Ok(0);
        }
        if input.position < input.buffer_start
            || input.position >= input.buffer_start + input.buffer.len() as u64
        {
            let end = input.position.saturating_add(READ_BYTES).min(input.length) - 1;
            let (buffer, length, _) = input.runtime.block_on(read_chunk(
                &input.client,
                &input.uri,
                input.position,
                end,
                input.validator.as_ref(),
            ))?;
            if length != input.length {
                return Err(io::Error::other(
                    "Remote file length changed while reading metadata",
                ));
            }
            input.buffer = buffer;
            input.buffer_start = input.position;
        }
        let start = (input.position - input.buffer_start) as usize;
        let count = output.len().min(input.buffer.len() - start);
        output[..count].copy_from_slice(&input.buffer[start..start + count]);
        input.position += count as u64;
        Ok(count)
    }
}

impl Seek for FileReader {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        match &mut self.input {
            Input::File(file) => file.seek(seek),
            Input::Http(input) => {
                let position = match seek {
                    SeekFrom::Start(position) => Some(position),
                    SeekFrom::End(offset) => input.length.checked_add_signed(offset),
                    SeekFrom::Current(offset) => input.position.checked_add_signed(offset),
                }
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file seek"))?;
                input.position = position;
                Ok(position)
            }
        }
    }
}

async fn read_chunk(
    client: &Client,
    uri: &str,
    start: u64,
    end: u64,
    validator: Option<&HeaderValue>,
) -> io::Result<(Vec<u8>, u64, Option<HeaderValue>)> {
    let mut request = client
        .get(uri)
        .header(header::RANGE, format!("bytes={start}-{end}"))
        .header(header::ACCEPT_ENCODING, "identity");
    if let Some(validator) = validator {
        request = request.header(header::IF_RANGE, validator);
    }
    let response = request
        .send()
        .await
        .map_err(|e| io::Error::other(e.without_url()))?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(io::Error::other(
            "Remote file changed or no longer supports range reads",
        ));
    }
    let (actual_start, actual_end, length) = response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|h| h.to_str().ok())
        .and_then(content_range)
        .ok_or_else(|| io::Error::other("Invalid remote file range response"))?;
    if actual_start != start || actual_end != end.min(length - 1) {
        return Err(io::Error::other(
            "Remote file returned a different byte range",
        ));
    }
    let validator = response
        .headers()
        .get(header::ETAG)
        .filter(|v| !v.as_bytes().starts_with(b"W/"))
        .or_else(|| response.headers().get(header::LAST_MODIFIED))
        .cloned();
    let expected = actual_end - start + 1;
    let mut reader =
        tokio_util::io::StreamReader::new(response.bytes_stream().map_err(io::Error::other))
            .take(expected + 1);
    let mut bytes = Vec::with_capacity(expected as usize);
    reader.read_to_end(&mut bytes).await?;
    if bytes.len() as u64 != expected {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Remote file range length did not match",
        ));
    }
    Ok((bytes, length, validator))
}

pub(crate) fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start <= end && end < total).then_some((start, end, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    #[tokio::test]
    async fn tag_reader_seeks_across_large_files_with_bounded_range_requests() {
        let server = MockServer::start().await;
        let content: Vec<u8> = (0..2_000_000).map(|n| (n % 251) as u8).collect();
        let response_content = content.clone();
        Mock::given(method("GET"))
            .respond_with(move |request: &wiremock::Request| {
                let range = request.headers[header::RANGE]
                    .to_str()
                    .unwrap()
                    .strip_prefix("bytes=")
                    .unwrap();
                let (start, end) = range.split_once('-').unwrap();
                let start = start.parse::<usize>().unwrap();
                let end = end
                    .parse::<usize>()
                    .unwrap()
                    .min(response_content.len() - 1);
                assert!(end - start < READ_BYTES as usize);
                ResponseTemplate::new(206)
                    .insert_header(
                        "Content-Range",
                        format!("bytes {start}-{end}/{}", response_content.len()),
                    )
                    .insert_header("ETag", "\"v1\"")
                    .set_body_bytes(response_content[start..=end].to_vec())
            })
            .mount(&server)
            .await;
        let mut reader = FileReader::open(ResolvedStream::new(server.uri()))
            .await
            .unwrap();
        tokio::task::spawn_blocking(move || {
            let mut expected = io::Cursor::new(content);
            for seek in [
                SeekFrom::Start(0),
                SeekFrom::End(-16),
                SeekFrom::Start(700_001),
                SeekFrom::Current(-20),
                SeekFrom::End(10),
            ] {
                assert_eq!(reader.seek(seek).unwrap(), expected.seek(seek).unwrap());
                let mut actual = [0; 16];
                let mut wanted = [0; 16];
                assert_eq!(
                    reader.read(&mut actual).unwrap(),
                    Read::read(&mut expected, &mut wanted).unwrap()
                );
                assert_eq!(actual, wanted);
            }
            assert_eq!(
                reader.seek(SeekFrom::End(-2_000_001)).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        })
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 4);
        assert!(
            requests
                .iter()
                .skip(1)
                .all(|r| r.headers[header::IF_RANGE] == "\"v1\"")
        );
    }
}
