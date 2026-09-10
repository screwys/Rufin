//! Persistent discovery subprocess. Only URIs and extracted facts cross this boundary.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::{Deserialize, Serialize};

use super::{
    Discoverer, Metadata, ensure_gstreamer_initialized, gst, image_from_info, metadata_from_info,
};
use crate::{ImageBytes, SourceError};

#[derive(Serialize, Deserialize)]
struct Request<'a> {
    uri: &'a str,
    picture_index: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Reply {
    Metadata(Option<Box<Metadata>>),
    Image {
        length: usize,
        content_type: Option<String>,
        #[serde(skip)]
        bytes: Vec<u8>,
    },
    Error(String),
    NotFound,
}

pub(super) struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Client {
    pub(super) fn start(timeout_seconds: u64) -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        #[cfg(not(test))]
        command
            .arg("--discovery-worker")
            .arg(timeout_seconds.to_string());
        #[cfg(test)]
        command
            .args([
                "--exact",
                "file::discovery::process::tests::worker_process",
                "--nocapture",
            ])
            .env("RUFIN_DISCOVERY_TEST_TIMEOUT", timeout_seconds.to_string());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let output = BufReader::new(child.stdout.take().unwrap());
        let input = child.stdin.take().unwrap();
        let client = Self {
            child,
            input,
            output,
        };
        #[cfg(test)]
        let mut client = client;
        #[cfg(test)]
        {
            // Libtest writes its introduction before entering the worker test.
            let mut line = String::new();
            loop {
                line.clear();
                if client.output.read_line(&mut line)? == 0 {
                    return Err(io::Error::other("Discovery test worker did not start"));
                }
                if line.trim() == "discovery worker ready" {
                    break;
                }
            }
        }
        Ok(client)
    }

    pub(super) fn request(&mut self, uri: &str, picture_index: Option<u32>) -> io::Result<Reply> {
        let input = &mut self.input;
        serde_json::to_writer(&mut *input, &Request { uri, picture_index })?;
        input.write_all(b"\n")?;
        input.flush()?;
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Discovery worker exited before returning a result",
            ));
        }
        let mut reply = serde_json::from_str(&line)?;
        if let Reply::Image { length, bytes, .. } = &mut reply {
            bytes.resize(*length, 0);
            self.output.read_exact(bytes)?;
        }
        Ok(reply)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run Rufin's private discovery mode before starting the application runtime.
pub fn run_worker(timeout_seconds: u64) -> io::Result<()> {
    ensure_gstreamer_initialized().map_err(io::Error::other)?;
    let discoverer =
        Discoverer::new(gst::ClockTime::from_seconds(timeout_seconds)).map_err(io::Error::other)?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let request: Request<'_> = serde_json::from_str(&line)?;
        let info = discoverer.discover_uri(request.uri).ok();
        let response = match (info, request.picture_index) {
            (Some(info), Some(index)) => match image_from_info(&info, index) {
                Ok(ImageBytes {
                    bytes,
                    content_type,
                }) => Reply::Image {
                    length: bytes.len(),
                    content_type,
                    bytes,
                },
                Err(SourceError::NotFound) => Reply::NotFound,
                Err(error) => Reply::Error(error.to_string()),
            },
            (Some(info), None) => Reply::Metadata(metadata_from_info(&info).map(Box::new)),
            (None, _) => Reply::Metadata(None),
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        if let Reply::Image { bytes, .. } = response {
            output.write_all(&bytes)?;
        }
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use lofty::config::WriteOptions;
    use lofty::prelude::{Accessor, TagExt};
    use lofty::tag::{Tag, TagType};

    use super::super::{Reader, read_image_input};
    use crate::file::local::media::read_media;
    use crate::file::media::{MediaRead, Worker};

    #[test]
    #[allow(clippy::exit)] // The subprocess must not append libtest output to its protocol.
    fn worker_process() {
        let Ok(timeout) = std::env::var("RUFIN_DISCOVERY_TEST_TIMEOUT") else {
            return;
        };
        writeln!(std::io::stdout().lock(), "\ndiscovery worker ready").unwrap();
        let result = super::run_worker(timeout.parse().unwrap());
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }

    #[test]
    fn discovery_survives_worker_exit_and_preserves_metadata_and_artwork() {
        use base64::Engine;

        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("音楽 with spaces.bin");
        let mut frame = vec![0; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        fs::write(&audio, frame.repeat(40)).unwrap();
        let picture = base64::engine::general_purpose::STANDARD.decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII="
        ).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("A title".into());
        tag.set_artist("An artist".into());
        tag.push_picture(
            lofty::picture::Picture::unchecked(picture.clone())
                .mime_type(lofty::picture::MimeType::Png)
                .pic_type(lofty::picture::PictureType::CoverFront)
                .build(),
        );
        tag.save_to_path(&audio, WriteOptions::new()).unwrap();
        let uri = url::Url::from_file_path(&audio).unwrap();
        let mut reader = Reader::default();
        let mut file = fs::File::open(&audio).unwrap();
        let metadata = reader.read_input(&mut file, uri.as_str()).unwrap().unwrap();
        assert_eq!(metadata.title.as_deref(), Some("A title"));
        assert_eq!(metadata.artist.as_deref(), Some("An artist"));
        let image = read_image_input(
            &mut reader,
            &mut file,
            uri.as_str(),
            metadata.artwork_index.unwrap(),
        )
        .unwrap();
        assert_eq!(image.bytes, picture);
        assert_eq!(image.content_type.as_deref(), Some("image/png"));

        let child = &mut reader.process.as_mut().unwrap().child;
        child.kill().unwrap();
        child.wait().unwrap();
        // Leading junk makes the shared media reader require discovery.
        let mut bytes = vec![0; 32];
        bytes.extend(fs::read(&audio).unwrap());
        fs::write(&audio, bytes).unwrap();
        let mut worker = Worker { discovery: reader };
        assert!(matches!(
            read_media(&mut worker, audio.clone(), None),
            MediaRead::Unreadable
        ));
        assert!(matches!(
            read_media(&mut worker, audio.clone(), None),
            MediaRead::Accepted(_)
        ));

        let text = directory.path().join("license.txt");
        fs::write(&text, b"Creative Commons\nThis is a license, not audio.\n").unwrap();
        // Old GStreamer crashes; fixed versions return a normal rejection.
        assert!(matches!(
            read_media(&mut worker, text, None),
            MediaRead::Rejected | MediaRead::Unreadable
        ));
        assert!(matches!(
            read_media(&mut worker, audio, None),
            MediaRead::Accepted(_)
        ));
    }

    #[tokio::test]
    async fn discovery_reads_the_existing_network_stream_uri() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let mut frame = vec![0; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        let bytes = frame.repeat(40);
        Mock::given(method("GET"))
            .and(path("/stream"))
            .and(query_param("token", "private-stream"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
            .mount(&server)
            .await;
        let uri = format!("{}/stream?token=private-stream", server.uri());
        tokio::task::spawn_blocking(move || {
            Reader::network().read_input(&mut std::io::Cursor::new(bytes), &uri)
        })
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    }
}
