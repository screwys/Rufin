use std::sync::{
    Arc, RwLock,
    atomic::{AtomicUsize, Ordering},
};

use library::{Database, ReadCancellation, Scan, ScanOutcome, SourceId};
use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::any};

use super::{FileAuthentication, FileCredentials, FileSourceSettings, RemoteSource};

#[test]
fn file_source_locations_preserve_server_authority_and_encoded_names() {
    for (kind, url) in [
        ("smb", "smb://nas.example:1445/Music/"),
        ("webdav", "https://nas.example:8443/Music/"),
    ] {
        let settings = FileSourceSettings {
            url: url.into(),
            alternate_urls: vec![],
            folders: vec![],
            username: String::new(),
            domain: String::new(),
            authentication: FileAuthentication::Anonymous,
            trust_invalid_certificate: false,
            certificate_pem: None,
            require_smb_encryption: false,
        };
        let configuration = settings
            .configuration(SourceId::new("files:test"), kind, "Files".into())
            .unwrap();
        let source = RemoteSource::open(&configuration, None).unwrap();
        let name = "日本語/track 100% # ?.flac";
        let location = source.location(name).unwrap();
        assert_eq!(source.relative(&location).unwrap(), name);
        assert!(
            source
                .relative(&location.replace("nas.example", "other.example"))
                .is_err()
        );
        assert!(
            source
                .relative(&location.replace("/Music/", "/Music-other/"))
                .is_err()
        );
        assert!(source.location("../outside.flac").is_err());
    }
}

#[tokio::test]
async fn dav_scan_attempts_remaining_files_after_an_unreadable_file() {
    let server = MockServer::start().await;
    let cue = Arc::new(AtomicUsize::new(0));
    let response_cue = Arc::clone(&cue);
    let audio = wave();
    Mock::given(any())
        .respond_with(move |request: &Request| match request.method.as_str() {
            "PROPFIND" => {
                let mut entries = String::new();
                if request.headers.get("depth").unwrap() == "1" {
                    for name in ["a.wav", "b.wav", "c.wav", "c.lrc"].into_iter().chain((response_cue.load(Ordering::Relaxed) == 1).then_some("album.cue")) {
                        entries.push_str(&format!("<d:response><d:href>/{name}</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>{}</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>", audio.len()));
                    }
                }
                ResponseTemplate::new(207).set_body_string(format!("<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>{entries}</d:multistatus>"))
            }
            "GET" if request.url.path() == "/album.cue" => ResponseTemplate::new(200).set_body_string("FILE \"a.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nFILE \"b.wav\" WAVE\nTRACK 02 AUDIO\nINDEX 01 00:00:00\n"),
            "GET" if request.url.path() == "/a.wav" => ResponseTemplate::new(403),
            "GET" if request.url.path() == "/c.lrc" => ResponseTemplate::new(200).set_body_string("[00:00.00]A lyric line\n"),
            "GET" => ResponseTemplate::new(200).set_body_bytes(audio.clone()),
            _ => ResponseTemplate::new(404),
        })
        .mount(&server)
        .await;
    let settings = FileSourceSettings {
        url: format!("{}/", server.uri()),
        alternate_urls: vec![],
        folders: vec![],
        username: String::new(),
        domain: String::new(),
        authentication: FileAuthentication::Anonymous,
        trust_invalid_certificate: false,
        certificate_pem: None,
        require_smb_encryption: false,
    };
    for with_cue in 0..2 {
        cue.store(with_cue, Ordering::Relaxed);
        let before = server.received_requests().await.unwrap().len();
        let config = settings
            .configuration(SourceId::new("webdav:partial"), "webdav", "DAV".into())
            .unwrap();
        let source = crate::Source::open(config, None, None).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .unwrap();
        let progress = std::sync::Mutex::new(Vec::new());
        let result = source
            .manual_refresh(
                &database,
                "DAV",
                &|value| progress.lock().unwrap().push(value),
                Arc::new(false.into()),
            )
            .await
            .unwrap();
        let ScanOutcome::Changed(publication) = result else {
            panic!("Accepted tracks must still be published: {result:?}");
        };
        let rows = database
            .mapping_track_page(publication.source, None, None, 10, &ReadCancellation::new())
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        let requests = server.received_requests().await.unwrap();
        for name in ["/a.wav", "/b.wav", "/c.wav", "/c.lrc"] {
            assert!(
                requests[before..]
                    .iter()
                    .any(|r| r.method == "GET" && r.url.path() == name)
            );
        }
        let progress = progress.lock().unwrap();
        assert_eq!(
            progress
                .iter()
                .filter(|p| p.stage == crate::SourceReadStage::Tracks)
                .map(|p| (p.completed, p.total))
                .collect::<Vec<_>>(),
            [(0, None), (1, None), (2, None)]
        );
        assert_eq!(
            progress.last().unwrap().stage,
            crate::SourceReadStage::Finalizing
        );
    }
}

#[tokio::test]
async fn nextcloud_refresh_skips_unchanged_branches_and_keeps_selected_folder_identity() {
    const ROOT: &str = "/remote.php/dav/files/%E8%81%B4/";
    let server = MockServer::start().await;
    let generation = Arc::new(AtomicUsize::new(0));
    let state = Arc::clone(&generation);
    let audio = wave();
    Mock::given(any()).respond_with(move |request: &Request| {
        let version = state.load(Ordering::Relaxed);
        let path = request.url.path().strip_prefix(ROOT).unwrap();
        let changed_name = if version == 0 { "first.wav" } else { "renamed.wav" };
        let entry = |path: &str, directory: bool, etag: &str, id: &str| {
            let kind = if directory { "<d:collection/>" } else { "" };
            format!("<d:response><d:href>{ROOT}{path}</d:href><d:propstat><d:prop><d:resourcetype>{kind}</d:resourcetype><d:getcontentlength>{}</d:getcontentlength><d:getetag>&quot;{etag}&quot;</d:getetag><o:fileid>{id}</o:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>", audio.len()).replace("%E8%81%B4", "%e8%81%b4")
        };
        match request.method.as_str() {
            "PROPFIND" => {
                let etag = if path.starts_with("library/stable") { "stable".to_string() } else { version.to_string() };
                let mut entries = entry(path, true, &etag, path);
                if request.headers.get("depth").unwrap() == "1" {
                    entries += &match path.trim_end_matches('/') {
                        "library" => entry("library/stable/", true, "stable", "stable") + &entry("library/changing/", true, &etag, "changing"),
                        "library/stable" => entry("library/stable/second.wav", false, "audio", "2"),
                        "library/changing" => entry(&format!("library/changing/{changed_name}"), false, "audio", "1"),
                        _ => panic!("Traversal outside selected folders: {path}"),
                    };
                }
                ResponseTemplate::new(207).set_body_string(format!("<d:multistatus xmlns:d=\"DAV:\" xmlns:o=\"http://owncloud.org/ns\">{entries}</d:multistatus>"))
            }
            "GET" => {
                assert!(path.ends_with(".wav"));
                if let Some(range) = request.headers.get("range") {
                    let (start, end) = range.to_str().unwrap().strip_prefix("bytes=").unwrap().split_once('-').unwrap();
                    let start = start.parse::<usize>().unwrap();
                    let end = if end.is_empty() || end == "0" { audio.len() - 1 } else { end.parse::<usize>().unwrap().min(audio.len() - 1) };
                    ResponseTemplate::new(206).insert_header("Content-Range", format!("bytes {start}-{end}/{}", audio.len())).insert_header("ETag", "\"audio\"").set_body_bytes(audio[start..=end].to_vec())
                } else { ResponseTemplate::new(200).set_body_bytes(audio.clone()) }
            }
            _ => ResponseTemplate::new(404),
        }
    }).mount(&server).await;
    let settings = FileSourceSettings {
        url: format!("{}{ROOT}", server.uri()).replace("%E8%81%B4", "%e8%81%b4"),
        alternate_urls: vec![],
        folders: vec!["library".into()],
        username: String::new(),
        domain: String::new(),
        authentication: FileAuthentication::Anonymous,
        trust_invalid_certificate: false,
        certificate_pem: None,
        require_smb_encryption: false,
    };
    let config = settings
        .configuration(
            SourceId::new("webdav:nextcloud"),
            "webdav",
            "Nextcloud".into(),
        )
        .unwrap();
    let source = crate::Source::open(config, None, None).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("library.sqlite3"))
        .await
        .unwrap();
    let mut identities = Vec::new();
    for round in 0..3 {
        let before = server.received_requests().await.unwrap().len();
        if round == 2 {
            generation.store(1, Ordering::Relaxed);
        }
        let outcome = source
            .refresh_if_needed(
                &database,
                "Nextcloud",
                &|_| panic!("automatic inventory must not replace artwork progress"),
                Arc::new(false.into()),
            )
            .await
            .unwrap()
            .unwrap();
        let publication = match outcome {
            ScanOutcome::Changed(p) if round != 1 => p,
            ScanOutcome::Identical(p) if round == 1 => p,
            other => panic!("Unexpected refresh: {other:?}"),
        };
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests[before..].iter().all(|request| {
                request.method != "GET" || request.headers.contains_key("range")
            }),
            "Nextcloud metadata reads must not fall back to whole-file downloads"
        );
        if round == 1 {
            assert_eq!(
                requests.len() - before,
                1,
                "Warm check should only read the selected root marker"
            );
        } else if round == 2 {
            assert!(
                !requests[before..]
                    .iter()
                    .any(|r| r.url.path().contains("/stable/")),
                "Unchanged branch was traversed or downloaded"
            );
        }
        let tracks = database
            .mapping_track_page(publication.source, None, None, 10, &ReadCancellation::new())
            .await
            .unwrap();
        assert_eq!(tracks.len(), 2);
        let mut current = tracks
            .iter()
            .map(|track| track.media_uri.clone())
            .collect::<Vec<_>>();
        current.sort();
        if round == 0 {
            identities = current;
        } else {
            assert_eq!(current, identities);
        }
    }
}

#[tokio::test]
async fn dav_inventory_reuses_tags_and_follows_native_file_identity() {
    for sync_supported in [false, true] {
        dav_inventory_round_trip(sync_supported).await;
    }
}

async fn dav_inventory_round_trip(sync_supported: bool) {
    let server = MockServer::start().await;
    let path = Arc::new(RwLock::new("first.wav".to_string()));
    let gets = Arc::new(AtomicUsize::new(0));
    let current_path = Arc::clone(&path);
    let reads = Arc::clone(&gets);
    let expired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let token_expired = Arc::clone(&expired);
    let audio = wave();
    Mock::given(any()).respond_with(move |request: &Request| {
        assert_eq!(request.headers.get("authorization").unwrap(), "Basic bGlzdGVuZXI6c2VjcmV0");
        let path = current_path.read().unwrap();
        match request.method.as_str() {
            "PROPFIND" => {
                let token = if sync_supported { format!("<d:sync-token>urn:rufin:{path}</d:sync-token>") } else { String::new() };
                let file = if request.headers.get("depth").unwrap() == "1" {
                    format!("<d:response><d:href>/music/{path}</d:href><d:propstat><d:prop><d:getcontentlength>{}</d:getcontentlength><d:getetag>&quot;unchanged&quot;</d:getetag><o:fileid>42</o:fileid><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>", audio.len())
                } else { String::new() };
                ResponseTemplate::new(207).set_body_string(format!("<d:multistatus xmlns:d=\"DAV:\" xmlns:o=\"http://owncloud.org/ns\"><d:response><d:href>/music/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype>{token}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>{file}</d:multistatus>"))
            }
            "REPORT" if sync_supported => {
                if token_expired.load(Ordering::Relaxed) { return ResponseTemplate::new(409); }
                let token = format!("urn:rufin:{path}");
                let changes = if String::from_utf8_lossy(&request.body).contains(&token) { String::new() }
                    else { "<d:response><d:href>/music/first.wav</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>".into() };
                ResponseTemplate::new(207).set_body_string(format!("<d:multistatus xmlns:d=\"DAV:\">{changes}<d:sync-token>{token}</d:sync-token></d:multistatus>"))
            }
            "GET" if request.url.path() == format!("/music/{path}") => {
                reads.fetch_add(1, Ordering::Relaxed);
                if let Some(range) = request.headers.get("range") {
                    let (start, end) = range.to_str().unwrap().strip_prefix("bytes=").unwrap().split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    let end = if end.is_empty() { audio.len() - 1 } else { end.parse::<usize>().unwrap().min(audio.len() - 1) };
                    ResponseTemplate::new(206).insert_header("Content-Range", format!("bytes {start}-{end}/{}", audio.len()))
                        .insert_header("ETag", "\"unchanged\"").set_body_bytes(audio[start..=end].to_vec())
                } else { ResponseTemplate::new(200).set_body_bytes(audio.clone()) }
            }
            _ => ResponseTemplate::new(404),
        }
    }).mount(&server).await;
    let settings = FileSourceSettings {
        url: format!("{}/music/", server.uri()),
        alternate_urls: vec![],
        folders: vec![],
        username: "listener".into(),
        domain: String::new(),
        authentication: FileAuthentication::Password,
        trust_invalid_certificate: false,
        certificate_pem: None,
        require_smb_encryption: false,
    };
    let configuration = settings
        .configuration(SourceId::new("webdav:test"), "webdav", "DAV".into())
        .unwrap();
    assert!(!configuration.provider_payload.contains("secret"));
    let credential = serde_json::to_string(&FileCredentials {
        secret: "secret".into(),
        headers: vec![],
    })
    .unwrap();
    let source = RemoteSource::open(&configuration, Some(credential)).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("library.sqlite3"))
        .await
        .unwrap();
    let mut first_uri = String::new();
    let mut reads_after_first = 0;
    for round in 0..4 {
        let before = server.received_requests().await.unwrap().len();
        if round == 3 {
            expired.store(true, Ordering::Relaxed);
        }
        if round == 2 {
            *path.write().unwrap() = "renamed.wav".into();
        }
        let mut scan = Scan::begin(
            &database,
            configuration.source_id.as_str(),
            "DAV",
            "dav",
            None,
        )
        .await
        .unwrap();
        source
            .stage_catalog(&database, &mut scan, &|_| {}, &|| false)
            .await
            .unwrap();
        let outcome = scan.finish().await.unwrap();
        if sync_supported && matches!(round, 1 | 3) {
            let requests = server.received_requests().await.unwrap();
            let current = &requests[before..];
            assert!(current.iter().any(|r| r.method.as_str() == "REPORT"));
            assert_eq!(
                current
                    .iter()
                    .filter(|r| r.headers.get("depth").is_some_and(|depth| depth == "1"))
                    .count(),
                usize::from(round == 3),
                "Unchanged sync reuses listings; expired tokens reacquire them"
            );
        }
        if round == 1 {
            assert!(
                matches!(outcome, ScanOutcome::Identical(_)),
                "unchanged inventory: {outcome:?}"
            );
            assert_eq!(
                gets.load(Ordering::Relaxed),
                reads_after_first,
                "unchanged files must not be downloaded or parsed again"
            );
        }
        let publication = match outcome {
            ScanOutcome::Changed(p) | ScanOutcome::Identical(p) => p,
            other => panic!("{other:?}"),
        };
        let tracks = database
            .mapping_track_page(publication.source, None, None, 10, &ReadCancellation::new())
            .await
            .unwrap();
        assert_eq!(tracks.len(), 1);
        if round == 0 {
            first_uri = tracks[0].media_uri.clone();
            reads_after_first = gets.load(Ordering::Relaxed);
        } else {
            assert_eq!(tracks[0].media_uri, first_uri);
        }
        assert_eq!(
            library::source_entity_parts(&first_uri).unwrap().0,
            configuration.source_id
        );
        if round == 2 {
            let observation = database
                .observed_media_file(&first_uri)
                .await
                .unwrap()
                .unwrap();
            assert!(observation.path.ends_with("/renamed.wav"));
            let stream = source.stream(&database, &first_uri).await.unwrap();
            let response = reqwest::get(stream.uri()).await.unwrap();
            assert_eq!(response.bytes().await.unwrap().as_ref(), wave());
        }
    }
}

fn wave() -> Vec<u8> {
    let frames = 8000_u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8000_u32.to_le_bytes());
    bytes.extend_from_slice(&16000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 2).to_le_bytes());
    for frame in 0..frames {
        bytes.extend_from_slice(&((frame % 100) as i16 * 100).to_le_bytes());
    }
    bytes
}

#[tokio::test]
async fn dav_cue_inventory_preserves_segments_and_refreshes_backing_audio() {
    let server = MockServer::start().await;
    // Initial, unchanged, renamed CUE, changed audio, removed CUE.
    let round = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let state = Arc::clone(&round);
    let gets = Arc::clone(&reads);
    Mock::given(any()).respond_with(move |request: &Request| {
        let round = state.load(Ordering::Relaxed);
        let cue_name = if round >= 2 { "renamed.cue" } else { "album.cue" };
        let revision = if round >= 3 { "audio-2" } else { "audio-1" };
        let cue = b"TITLE \"CUE album\"\nFILE \"audio.wav\" WAVE\nTRACK 01 AUDIO\nTITLE \"First\"\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nTITLE \"Second\"\nINDEX 01 00:00:37\n";
        let audio = wave();
        match request.method.as_str() {
            "PROPFIND" => {
                let mut files = String::new();
                if request.headers.get("depth").unwrap() == "1" {
                    for (name, id, revision, size) in [("audio.wav", "audio-id", revision, audio.len()), (cue_name, "cue-id", "cue-1", cue.len())] {
                        if round == 4 && name == cue_name { continue; }
                        files.push_str(&format!("<d:response><d:href>/music/{name}</d:href><d:propstat><d:prop><d:getcontentlength>{size}</d:getcontentlength><d:getetag>&quot;{revision}&quot;</d:getetag><o:fileid>{id}</o:fileid><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"));
                    }
                }
                ResponseTemplate::new(207).set_body_string(format!("<d:multistatus xmlns:d=\"DAV:\" xmlns:o=\"http://owncloud.org/ns\"><d:response><d:href>/music/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>{files}</d:multistatus>"))
            }
            "GET" if request.url.path() == format!("/music/{cue_name}") => {
                gets.fetch_add(1, Ordering::Relaxed);
                ResponseTemplate::new(200).set_body_bytes(cue.to_vec())
            }
            "GET" if request.url.path() == "/music/audio.wav" => {
                gets.fetch_add(1, Ordering::Relaxed);
                if let Some(range) = request.headers.get("range") {
                    let (start, end) = range.to_str().unwrap().strip_prefix("bytes=").unwrap().split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    let end = if end.is_empty() { audio.len() - 1 } else { end.parse::<usize>().unwrap().min(audio.len() - 1) };
                    ResponseTemplate::new(206).insert_header("Content-Range", format!("bytes {start}-{end}/{}", audio.len()))
                        .insert_header("ETag", format!("\"{revision}\"")).set_body_bytes(audio[start..=end].to_vec())
                } else { ResponseTemplate::new(200).set_body_bytes(audio) }
            }
            _ => ResponseTemplate::new(404),
        }
    }).mount(&server).await;
    let settings = FileSourceSettings {
        url: format!("{}/music/", server.uri()),
        alternate_urls: vec![],
        folders: vec![],
        username: String::new(),
        domain: String::new(),
        authentication: FileAuthentication::Anonymous,
        trust_invalid_certificate: false,
        certificate_pem: None,
        require_smb_encryption: false,
    };
    let configuration = settings
        .configuration(SourceId::new("webdav:cue-test"), "webdav", "DAV".into())
        .unwrap();
    let source = RemoteSource::open(&configuration, None).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let database = Database::open(folder.path().join("library.sqlite3"))
        .await
        .unwrap();
    let mut original_uris = Vec::new();
    let mut previous_reads = 0;
    for pass in 0..5 {
        round.store(pass, Ordering::Relaxed);
        let mut scan = Scan::begin(
            &database,
            configuration.source_id.as_str(),
            "DAV",
            "dav",
            None,
        )
        .await
        .unwrap();
        source
            .stage_catalog(&database, &mut scan, &|_| {}, &|| false)
            .await
            .unwrap();
        let outcome = scan.finish().await.unwrap();
        if pass == 1 {
            assert!(matches!(outcome, ScanOutcome::Identical(_)), "{outcome:?}");
            assert_eq!(
                reads.load(Ordering::Relaxed),
                previous_reads,
                "warm CUE refresh must not read audio or CUE bytes"
            );
        }
        if pass == 3 {
            assert!(
                reads.load(Ordering::Relaxed) > previous_reads,
                "changed backing audio must be re-read"
            );
        }
        previous_reads = reads.load(Ordering::Relaxed);
        let publication = match outcome {
            ScanOutcome::Changed(p) | ScanOutcome::Identical(p) => p,
            other => panic!("{other:?}"),
        };
        let rows = database
            .mapping_track_page(publication.source, None, None, 10, &ReadCancellation::new())
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            if pass == 4 { 1 } else { 2 },
            "backing audio appears only after its CUE is removed"
        );
        let mut uris = rows
            .iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        uris.sort();
        if pass == 0 {
            original_uris = uris;
        } else if pass < 4 {
            assert_eq!(
                uris, original_uris,
                "CUE rename and audio changes preserve segment identity"
            );
        }
        if pass == 2 {
            let mut windows = Vec::new();
            for row in rows {
                let file = database
                    .observed_media_file(&row.media_uri)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(file.path.ends_with("/audio.wav"));
                let stream = source.stream(&database, &row.media_uri).await.unwrap();
                windows.push(stream.window().unwrap().clone());
            }
            windows.sort_by_key(|window| window.start_millis);
            assert_eq!(windows[0].start_millis, 0);
            assert_eq!(windows[0].end_millis, 493);
            assert_eq!(windows[1].start_millis, 493);
            assert_eq!(windows[1].end_millis, 1000);
        }
    }
}

#[tokio::test]
async fn dav_recovers_an_unavailable_endpoint_and_keeps_namespace_on_address_edit() {
    let primary = MockServer::start().await;
    let alternate = MockServer::start().await;
    let unavailable = Arc::new(AtomicUsize::new(0));
    for (server, state) in [
        (&primary, Arc::clone(&unavailable)),
        (&alternate, Arc::new(AtomicUsize::new(0))),
    ] {
        Mock::given(any()).respond_with(move |request: &Request| {
            assert_eq!(request.headers.get("x-access").unwrap(), "retained-secret");
            if state.load(Ordering::Relaxed) != 0 { return ResponseTemplate::new(503); }
            ResponseTemplate::new(207).set_body_string("<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/music/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>")
        }).mount(server).await;
    }
    let mut settings = FileSourceSettings {
        url: format!("{}/music/", primary.uri()),
        alternate_urls: vec![format!("{}/music/", alternate.uri())],
        folders: vec![],
        username: String::new(),
        domain: String::new(),
        authentication: FileAuthentication::Anonymous,
        trust_invalid_certificate: false,
        certificate_pem: None,
        require_smb_encryption: false,
    };
    let credentials = FileCredentials {
        secret: "saved-password".into(),
        headers: vec![("X-Access".into(), "retained-secret".into())],
    };
    let configuration = settings
        .configuration(SourceId::new("dav:addresses"), "webdav", "Music".into())
        .unwrap();
    let credential = Some(serde_json::to_string(&credentials).unwrap());
    let source = RemoteSource::open(&configuration, credential.clone()).unwrap();
    let original = source.location("a song #.wav").unwrap();
    let first = source.input().await.unwrap();
    unavailable.store(1, Ordering::Relaxed);
    assert!(source.stat(&first, "").await.is_err());
    let recovered = source.input().await.unwrap();
    let super::input::FileInput::WebDav(client) = recovered.input() else {
        panic!("wrong protocol");
    };
    assert_eq!(client.root().as_str(), settings.alternate_urls[0]);
    assert_eq!(source.location("a song #.wav").unwrap(), original);
    settings.url = settings.alternate_urls.remove(0);
    let edited = super::edit(
        configuration,
        credential.clone(),
        "Away".into(),
        settings,
        super::FileCredentialsEdit::default(),
    )
    .await
    .unwrap();
    let crate::SourceEditResult::Connected(edited) = edited else {
        panic!("address edit did not reconnect");
    };
    let (configuration, _, saved) = edited.into_parts();
    assert_eq!(saved, credential);
    assert!(!configuration.provider_payload.contains("retained-secret"));
    let reopened = RemoteSource::open(&configuration, saved).unwrap();
    assert_eq!(reopened.location("a song #.wav").unwrap(), original);
    assert_eq!(reopened.relative(&original).unwrap(), "a song #.wav");
}
