use super::*;
use crate::{app, diagnostics::Diagnostics, paths::Paths, runtime::RuntimeInputs};
use playback::{BackendCommand, BackendError, BackendEvent, PlaybackBackend};

fn diagnostics() -> Arc<Diagnostics> {
    static DIAGNOSTICS: std::sync::OnceLock<(
        Arc<Diagnostics>,
        Mutex<crate::diagnostics::StderrGuard>,
        tempfile::TempDir,
    )> = std::sync::OnceLock::new();
    DIAGNOSTICS
        .get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            let (diagnostics, stderr) = Diagnostics::install(directory.path().to_owned());
            (diagnostics, Mutex::new(stderr), directory)
        })
        .0
        .clone()
}

#[test]
fn saved_settings_enable_only_remembered_legacy_profiles() {
    assert!(!ConnectSettings::default().enabled);
    for (saved, expected) in [
        (r#"{}"#, false),
        (r#"{"name":"Work Laptop"}"#, false),
        (r#"{"profile":null}"#, false),
        (r#"{"profile":"shared-profile"}"#, true),
        (r#"{"profile":"shared-profile","enabled":false}"#, false),
        (r#"{"enabled":true}"#, true),
    ] {
        let config = ConnectSettings::from_device_file(saved.as_bytes()).unwrap();
        assert_eq!(config.enabled, expected, "{saved}");
    }
}

struct IdleBackend;

#[test]
fn cue_backing_file_reuse_and_removal() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let inputs = device(root.path(), "cue-files", diagnostics(), Arc::new(secrets::MemorySecretStore::new())).await;
            let owner = &inputs.products.connect;
            for (case, mapped, original_exists, encoding) in [
                ("existing", true, true, DownloadQuality::Original),
                ("empty", true, false, DownloadQuality::Original),
                ("private", false, false, DownloadQuality::Original),
                ("smaller", true, false, DownloadQuality::Transcoded(library::AudioEncoding::mp3())),
            ] {
                let folder = root.path().join(case);
                std::fs::create_dir_all(&folder).unwrap();
                let original = folder.join("album.flac");
                if original_exists { std::fs::write(&original, b"whole audio").unwrap(); }
                owner.status.send_modify(|status| {
                    status.settings.encoding = encoding;
                    status.settings.folders.clear();
                    if mapped { status.settings.folders.insert(case.into(), downloads::DownloadDirectory::Native(folder.clone())); }
                });
                let remote = url::Url::from_file_path(root.path().join(format!("remote-{case}.flac"))).unwrap().to_string();
                let mut uris = Vec::new();
                for index in 0..3 {
                    let uri = library::cue_media_uri(&format!("{case}:{index}"), &remote, index * 1000, (index + 1) * 1000);
                    owner.database.connect_apply(&[ConnectRecord {
                        kind: "track".into(), key: uri.clone(), value: Some(serde_json::json!({
                            "object_id":format!("{case}:{index}"), "media_uri":uri, "source_id":case,
                            "title":format!("Track {index}"), "normalized_search":"track", "display_album":"Album", "display_artist":"Artist", "sort_text":"track", "duration_millis":1000,
                            "source_favorite":false, "disc_number":1, "track_number":index+1, "relative_path":"album.flac", "source_format":"flac", "revision":"v1"
                        }))
                    }]).await.unwrap();
                    uris.push(uri);
                }
                let mut stored = Vec::new();
                for (index, uri) in uris.iter().enumerate() {
                    if index == 1 {
                        assert!(downloads::ConnectDownload::reuse(owner.as_ref(), uri).await.unwrap(), "reuse {case}");
                    } else {
                        let part = folder.join(format!("input-{index}.part"));
                        std::fs::write(&part, b"whole audio").unwrap();
                        let destination = downloads::ConnectDownload::destination(owner.as_ref(), uri).await.unwrap()
                            .unwrap_or_else(|| folder.join("private.audio"));
                        downloads::ConnectDownload::finish(owner.as_ref(), library::ConnectMediaFile {
                            media_uri: uri.clone(), encoding:library::media_encoding_key(encoding.encoding()), revision:"\"v1\"".into(),
                            path:url::Url::from_file_path(&part).unwrap().into(), managed:true, hash:None,
                        }, &destination).await.unwrap();
                    }
                    let file = owner.database.connect_media_file(uri, &library::media_encoding_key(encoding.encoding())).await.unwrap().unwrap();
                    assert_eq!(file.managed, !original_exists);
                    stored.push(file.path);
                    let stream = crate::playback::prepare_stream(&owner.database, playback::StreamRequest::original(uri), None, None, |_| async { panic!("CUE uses its backing file") }).await.unwrap();
                    assert_eq!(stream.window().unwrap().start_millis, index as u64 * 1000);
                    assert_eq!(stream.window().unwrap().end_millis, (index as u64 + 1) * 1000);
                }
                assert!(stored.iter().all(|path| path == &stored[0]), "shared backing for {case}: {stored:?}");
                let file = library::file_media_path(&stored[0]).unwrap();
                assert_eq!(downloads::ConnectDownload::remove(owner.as_ref(), &uris[0]).await.unwrap(), 0);
                assert!(file.is_file());
                assert_eq!(downloads::ConnectDownload::remove(owner.as_ref(), &uris[1]).await.unwrap(), 0);
                assert!(file.is_file());
                assert_eq!(downloads::ConnectDownload::remove(owner.as_ref(), &uris[2]).await.unwrap(), usize::from(!original_exists));
                assert_eq!(file.is_file(), original_exists);
            }
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.clone();
            tokio::task::spawn_blocking(move || playback.shutdown()).await.unwrap();
        }))
    }).unwrap();
}

#[test]
fn cue_concurrent_completion_and_revision_changes() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let inputs = device(root.path(), "cue-revisions", diagnostics(), Arc::new(secrets::MemorySecretStore::new())).await;
            let owner = &inputs.products.connect;
            for original_exists in [false, true] {
                let folder = root.path().join(format!("mapped-{original_exists}"));
                std::fs::create_dir_all(&folder).unwrap();
                let original = folder.join("album.flac");
                if original_exists { std::fs::write(&original, b"old audio").unwrap(); }
                owner.status.send_modify(|status| { status.settings.folders.insert("cue".into(), downloads::DownloadDirectory::Native(folder.clone())); });
                let remote = url::Url::from_file_path(folder.join("remote.flac")).unwrap().to_string();
                let mut uris = Vec::new();
                for index in 0..3 {
                    let uri = library::cue_media_uri(&format!("{original_exists}:{index}"), &remote, index*1000, (index+1)*1000);
                    owner.database.connect_apply(&[ConnectRecord {
                        kind:"track".into(), key:uri.clone(), value:Some(serde_json::json!({
                            "object_id":format!("{original_exists}:{index}"), "media_uri":uri, "source_id":"cue",
                            "title":"Track", "normalized_search":"track", "display_album":"Album", "display_artist":"Artist", "sort_text":"track", "duration_millis":1000,
                            "source_favorite":false, "disc_number":1, "track_number":index+1, "relative_path":"album.flac", "source_format":"flac", "revision":"v1"
                        }))
                    }]).await.unwrap();
                    uris.push(uri);
                }
                let mut completions = Vec::new();
                for (index, uri) in uris[..2].iter().enumerate() {
                    let part = folder.join(format!("old-{index}.part"));
                    std::fs::write(&part, b"old audio").unwrap();
                    completions.push(library::ConnectMediaFile { media_uri:uri.clone(), encoding:"original".into(), revision:"\"v1\"".into(), path:url::Url::from_file_path(part).unwrap().into(), managed:true, hash:None });
                }
                let second = completions.pop().unwrap();
                let first = completions.pop().unwrap();
                let (first, second) = tokio::join!(
                    downloads::ConnectDownload::finish(owner.as_ref(), first, &original),
                    downloads::ConnectDownload::finish(owner.as_ref(), second, &original)
                );
                first.unwrap(); second.unwrap();
                let a = owner.database.connect_media_file(&uris[0], "original").await.unwrap().unwrap();
                let b = owner.database.connect_media_file(&uris[1], "original").await.unwrap().unwrap();
                assert_eq!(a.path, b.path);
                let mut new_paths = Vec::new();
                for uri in &uris {
                    let mut reference = owner.database.connect_track_reference(uri).await.unwrap().unwrap();
                    reference["revision"] = "v2".into();
                    owner.database.connect_apply(&[ConnectRecord {kind:"track".into(),key:uri.clone(),value:Some(reference)}]).await.unwrap();
                    let part = folder.join("new.part");
                    std::fs::write(&part, b"new audio").unwrap();
                    downloads::ConnectDownload::finish(owner.as_ref(), library::ConnectMediaFile {
                        media_uri:uri.clone(),encoding:"original".into(),revision:"\"v2\"".into(),path:url::Url::from_file_path(part).unwrap().into(),managed:true,hash:None,
                    }, &original).await.unwrap();
                    let file = owner.database.connect_media_file(uri,"original").await.unwrap().unwrap();
                    assert_eq!(std::fs::read(library::file_media_path(&file.path).unwrap()).unwrap(),b"new audio");
                    new_paths.push(file.path);
                    if new_paths.len() == 1 || original_exists { assert_eq!(std::fs::read(&original).unwrap(), b"old audio"); }
                }
                assert!(new_paths.iter().all(|path| path==&new_paths[0]));
                assert_eq!(original.is_file(), original_exists);
            }
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.clone();
            tokio::task::spawn_blocking(move || playback.shutdown()).await.unwrap();
        }))
    }).unwrap();
}

#[test]
fn cue_direct_continuation_transfers_one_file_without_enrollment() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let host = device(
                root.path(),
                "direct-host",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let guest = device(
                root.path(),
                "direct-guest",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let a = &host.products.connect;
            let b = &guest.products.connect;
            execute(a, Action::Create).await.unwrap();
            pair(a, b).await;
            let source = root.path().join("whole.flac");
            std::fs::write(&source, b"complete backing audio").unwrap();
            let remote = url::Url::from_file_path(&source).unwrap().to_string();
            let mut files = Vec::new();
            for index in 0..2 {
                let uri = library::cue_media_uri(
                    &format!("direct:{index}"),
                    &remote,
                    index * 1000,
                    (index + 1) * 1000,
                );
                let mut item =
                    library::TrackReference::direct(&uri, "Cue track", "Artist", "Album", 1000);
                item.metadata.source_format = Some("flac".into());
                host.products
                    .playback
                    .play_selection(crate::playback::PlayRequest::one(
                        item.clone(),
                        library::QueuePlacement::Now,
                    ));
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let queue = host.products.playback.clone();
                        let saved = tokio::task::spawn_blocking(move || queue.snapshot())
                            .await
                            .unwrap()
                            .unwrap();
                        if saved.entries.first().map(|entry| entry.track_key)
                            == a.database
                                .catalog_identity(
                                    library::CatalogKind::Track,
                                    &uri,
                                    &library::ReadCancellation::new(),
                                )
                                .await
                                .map(|identity| {
                                    identity
                                        .map(|identity| library::TrackKey::from_raw(identity.key))
                                })
                                .unwrap()
                            && !saved.entries.is_empty()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                b.fetch_direct_continuation(
                    &a.status().identity.unwrap(),
                    &item.media_uri,
                    &item.metadata,
                    0,
                )
                .await
                .unwrap();
                assert!(
                    b.database
                        .connect_track_reference(&uri)
                        .await
                        .unwrap()
                        .is_none()
                );
                let file = b
                    .database
                    .connect_media_file(&uri, "original")
                    .await
                    .unwrap()
                    .unwrap();
                let path = library::file_media_path(&file.path).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), b"complete backing audio");
                let stream = crate::playback::prepare_stream(
                    &b.database,
                    playback::StreamRequest::original(&uri),
                    None,
                    None,
                    |_| async { panic!("downloaded file") },
                )
                .await
                .unwrap();
                assert_eq!(stream.window().unwrap().start_millis, index as u64 * 1000);
                assert_eq!(
                    stream.window().unwrap().end_millis,
                    (index as u64 + 1) * 1000
                );
                files.push((uri, file.path));
            }
            assert_eq!(files[0].1, files[1].1);
            assert_eq!(
                downloads::ConnectDownload::remove(b.as_ref(), &files[0].0)
                    .await
                    .unwrap(),
                0
            );
            assert!(library::file_media_path(&files[1].1).unwrap().is_file());
            assert_eq!(
                downloads::ConnectDownload::remove(b.as_ref(), &files[1].0)
                    .await
                    .unwrap(),
                1
            );
            assert!(source.is_file());
            for inputs in [guest, host] {
                inputs.products.connect.leave_profile().await.unwrap();
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
        }))
    })
    .unwrap();
}

impl PlaybackBackend for IdleBackend {
    fn send(&mut self, _: BackendCommand) -> Result<(), BackendError> {
        Ok(())
    }
    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

#[test]
fn relay_changes_preserve_the_open_profile_and_device_identity() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let inputs = device(
                root.path(),
                "relay",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let owner = &inputs.products.connect;
            execute(owner, Action::Create).await.unwrap();
            let session = owner.active().await.unwrap();
            let identity = owner.status().identity;
            for relay in [Some("http://127.0.0.1:9/".to_owned()), None] {
                let previous = owner.connected_network().await.unwrap();
                execute(
                    owner,
                    Action::Network {
                        nearby: false,
                        relay: relay.clone(),
                        public_relay: false,
                    },
                )
                .await
                .unwrap();
                let current = owner.connected_network().await.unwrap();
                assert!(!Arc::ptr_eq(&previous, &current));
                assert!(
                    current
                        .matches_configuration(false, relay.as_deref(), false)
                        .unwrap()
                );
                assert!(Arc::ptr_eq(&session, &owner.active().await.unwrap()));
                assert_eq!(owner.status().identity, identity);
            }
            owner.leave_profile().await.unwrap();
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.clone();
            tokio::task::spawn_blocking(move || playback.shutdown())
                .await
                .unwrap();
        }))
    })
    .unwrap();
}

#[test]
fn connection_test_requires_the_peer_to_still_share_the_profile() {
    let root = tempfile::tempdir().unwrap();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let host = device(
                root.path(),
                "host",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let guest = device(
                root.path(),
                "guest",
                diagnostics(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let a = &host.products.connect;
            let b = &guest.products.connect;
            execute(a, Action::Create).await.unwrap();
            pair(a, b).await;
            let peer = b.status().identity.unwrap();
            execute(a, Action::TestConnection { peer: peer.clone() })
                .await
                .unwrap();
            assert!(
                a.status()
                    .devices
                    .iter()
                    .any(|device| device.id == peer && device.reachable)
            );
            let image_key = format!("originals/native/{:032x}/{:032x}/original.png", 1, 2);
            let image_query = artwork::CacheImageRequest {
                family: format!("native/{:032x}/{:032x}", 1, 2),
                size: artwork::ImageSize::Original,
                fallback: false,
            };
            let image = image::RgbaImage::from_fn(3072, 2048, |x, y| {
                let value = (x + y * 3072 + 1).wrapping_mul(0x9e3779b9);
                let value = (value ^ (value >> 16)).wrapping_mul(0x85ebca6b);
                let value = value ^ (value >> 13);
                image::Rgba([value as u8, (value >> 8) as u8, (value >> 16) as u8, 255])
            });
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            let encoded = encoded.into_inner();
            assert!(encoded.len() > 16 * 1024 * 1024);
            let image_file = root.path().join("published.png");
            std::fs::write(&image_file, &encoded).unwrap();
            a.source
                .shared
                .artwork
                .import_missing_cache_file(&image_key, &image_file)
                .unwrap();
            let host_id = a.status().identity.unwrap();
            execute(
                b,
                Action::TestConnection {
                    peer: host_id.clone(),
                },
            )
            .await
            .unwrap();
            let (received_key, received) = b.fetch_cached_artwork(image_query).await.unwrap();
            assert_eq!(received_key, image_key);
            assert_eq!(std::fs::read(&received).unwrap(), encoded);
            assert!(
                b.source
                    .shared
                    .artwork
                    .import_missing_cache_file(&image_key, &received)
                    .unwrap()
            );
            assert!(
                !b.source
                    .shared
                    .artwork
                    .import_missing_cache_file(&image_key, &image_file)
                    .unwrap()
            );
            for (size, requested, fallback) in [(800, 700, false), (512, 0, true)] {
                let family = format!("native/{:032x}/{:032x}", 3, size);
                let key = format!("ready/{family}/{size}.png");
                a.source
                    .shared
                    .artwork
                    .import_missing_cache_file(&key, &image_file)
                    .unwrap();
                let (selected, file) = b
                    .fetch_cached_artwork(artwork::CacheImageRequest {
                        family,
                        size: if requested == 0 {
                            artwork::ImageSize::Original
                        } else {
                            artwork::ImageSize::Thumbnail(requested)
                        },
                        fallback,
                    })
                    .await
                    .unwrap();
                assert_eq!(selected, key);
                assert_eq!(std::fs::read(file).unwrap(), encoded);
            }
            let audio = root.path().join("transferred.flac");
            std::fs::write(&audio, b"audio").unwrap();
            std::fs::write(audio.with_extension("lrc"), "[00:00.00]Source only").unwrap();
            let uri = url::Url::from_file_path(&audio).unwrap().to_string();
            a.database
                .remember_media_uris(std::slice::from_ref(&uri))
                .await
                .unwrap();
            b.database
                .remember_media_uris(std::slice::from_ref(&uri))
                .await
                .unwrap();
            assert!(
                b.fetch_cached_lyrics(LyricsCacheQuery {
                    media_uri: uri.clone(),
                    authority: "source".into(),
                    role: "original".into(),
                    language: String::new(),
                    script: String::new(),
                    input: [0; 32],
                })
                .await
                .is_none(),
                "cache reads do not acquire the available sidecar"
            );
            b.cache_transfer_assets(Some(&host_id), &uri).await;
            let cached = b
                .database
                .lyrics_cache_for_role(
                    &uri,
                    "original",
                    "",
                    "",
                    [0; 32],
                    "source",
                    &library::ReadCancellation::new(),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(lyrics::LyricsBundle::from_cache_payload(&cached.lyrics).is_some());
            assert!(cached.lyrics.contains("Source only"));
            let cached_uri = "https://example.invalid/shared-lyrics.flac";
            for (authority, origin) in [
                ("source", lyrics::LyricsOrigin::Native),
                (
                    "external",
                    lyrics::LyricsOrigin::External(lyrics::ExternalLyricsProvider::Lrclib),
                ),
            ] {
                let mut bundle =
                    lyrics::lyrics_from_edited_text("[00:00.00]Shared lyrics").unwrap();
                bundle.origin = origin;
                let payload = bundle.cache_payload().unwrap();
                a.database
                    .write_lyrics_cache(
                        cached_uri, authority, "original", "", "", [7; 32], &payload, 42,
                    )
                    .await
                    .unwrap();
                let query = LyricsCacheQuery {
                    media_uri: cached_uri.into(),
                    authority: authority.into(),
                    role: "original".into(),
                    language: String::new(),
                    script: String::new(),
                    input: [7; 32],
                };
                let shared = b.fetch_cached_lyrics(query.clone()).await.unwrap();
                assert_eq!(shared.lyrics, payload);
                assert_eq!(shared.cache_input_digest, [7; 32]);
                assert_eq!(shared.updated_at, 42);
                assert_eq!(
                    lyrics::LyricsBundle::from_cache_payload(&shared.lyrics)
                        .unwrap()
                        .origin,
                    origin
                );
                if authority == "source" {
                    assert!(
                        b.fetch_cached_lyrics(LyricsCacheQuery {
                            input: [8; 32],
                            ..query
                        })
                        .await
                        .is_none()
                    );
                }
            }
            assert!(
                a.database
                    .connect_changes()
                    .await
                    .unwrap()
                    .iter()
                    .all(|change| change.record.kind != "cache_lyrics")
            );
            // Keep the same transport reachable after closing its profile.
            b.connected_network()
                .await
                .unwrap()
                .close_profile()
                .await
                .unwrap();
            assert_eq!(b.status().identity.as_deref(), Some(peer.as_str()));
            a.connected_network()
                .await
                .unwrap()
                .remember_peer(&b.status().invitation.unwrap())
                .await
                .unwrap();
            assert!(
                execute(a, Action::TestConnection { peer: peer.clone() })
                    .await
                    .is_err()
            );
            assert!(
                a.status()
                    .devices
                    .iter()
                    .any(|device| device.id == peer && !device.reachable)
            );
            for inputs in [guest, host] {
                inputs.products.connect.leave_profile().await.unwrap();
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
        }))
    })
    .unwrap();
}

#[test]
fn folder_exchange_preserves_disconnected_edits_without_rewriting_peer_files() {
    let root = tempfile::tempdir().unwrap();
    let diagnostics = diagnostics();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            let host = device(
                root.path(),
                "folder-host",
                diagnostics.clone(),
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let guest = device(
                root.path(),
                "folder-guest",
                diagnostics,
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let a = &host.products.connect;
            let b = &guest.products.connect;
            execute(a, Action::Create).await.unwrap();
            pair(a, b).await;
            a.network
                .lock()
                .await
                .take()
                .unwrap()
                .shutdown()
                .await
                .unwrap();
            b.network
                .lock()
                .await
                .take()
                .unwrap()
                .shutdown()
                .await
                .unwrap();
            let folders = [root.path().join("sync-a"), root.path().join("sync-b")];
            for (owner, folder) in [(a, &folders[0]), (b, &folders[1])] {
                execute(
                    owner,
                    Action::Destination {
                        destination: Some(portable::Destination::Local {
                            path: folder.clone(),
                        }),
                    },
                )
                .await
                .unwrap();
            }
            let first = library::FavoriteTarget::Track("https://example.invalid/first.flac".into());
            let second =
                library::FavoriteTarget::Track("https://example.invalid/second.flac".into());
            a.database.set_favorite(&first, true).await.unwrap();
            b.database.set_favorite(&second, true).await.unwrap();
            for owner in [a, b] {
                owner.synchronize().await.unwrap();
                owner.exchange().await.unwrap();
            }
            assert!(!a.database.favorite(&second).await.unwrap());
            assert!(!b.database.favorite(&first).await.unwrap());
            let names = [
                format!("{}.rufin-connect", a.status().identity.unwrap()),
                format!("{}.rufin-connect", b.status().identity.unwrap()),
            ];
            // Deliver the two independent writes as a file-sync tool would.
            std::fs::copy(folders[0].join(&names[0]), folders[1].join(&names[0])).unwrap();
            std::fs::copy(folders[1].join(&names[1]), folders[0].join(&names[1])).unwrap();
            let peer_files = [
                std::fs::read(folders[0].join(&names[1])).unwrap(),
                std::fs::read(folders[1].join(&names[0])).unwrap(),
            ];
            for (index, owner) in [a, b].into_iter().enumerate() {
                owner.exchange().await.unwrap();
                assert!(owner.database.favorite(&first).await.unwrap());
                assert!(owner.database.favorite(&second).await.unwrap());
                prepare_profile_snapshot(owner).await;
                owner.exchange().await.unwrap();
                let own = folders[index].join(&names[index]);
                let bytes = std::fs::read(&own).unwrap();
                owner.exchange().await.unwrap();
                owner.exchange().await.unwrap();
                assert!(
                    std::fs::read(&own).unwrap() == bytes,
                    "unchanged exchange leaves the file intact"
                );
                assert_eq!(
                    std::fs::read(folders[index].join(&names[1 - index])).unwrap(),
                    peer_files[index]
                );
            }
            for owner in [a, b] {
                owner
                    .status
                    .send_modify(|status| status.settings.enabled = false);
            }
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                8,
                8,
                image::Rgba([30, 60, 90, 255]),
            ))
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
            let encoded = encoded.into_inner();
            let cover = root.path().join("shared-cover.png");
            std::fs::write(&cover, encoded).unwrap();
            let binding = sources::LocalImageRef::File {
                source_id: sources::SourceId::new("shared-images"),
                path: cover.to_string_lossy().into_owned(),
                revision: "one".into(),
            }
            .artwork_binding();
            assert!(
                a.source
                    .shared
                    .artwork
                    .cache_source(&binding)
                    .await
                    .unwrap()
            );
            let image = a
                .source
                .shared
                .artwork
                .cache_entries()
                .unwrap()
                .map(Result::unwrap)
                .find(|image| image.key.ends_with("/512.png"))
                .unwrap();
            let image_key = image.key;
            let image_family = image_key
                .strip_prefix("ready/")
                .unwrap()
                .rsplit_once('/')
                .unwrap()
                .0
                .to_owned();
            let image_query = artwork::CacheImageRequest {
                family: image_family.clone(),
                size: artwork::ImageSize::Thumbnail(512),
                fallback: false,
            };
            let encoded = std::fs::read(&image.path).unwrap();
            let arbitrary_key = format!("ready/{image_family}/800.png");
            a.source
                .shared
                .artwork
                .import_missing_cache_file(&arbitrary_key, &image.path)
                .unwrap();
            let a_session = a.active().await.unwrap();
            let b_session = b.active().await.unwrap();
            let cached_uri = "https://example.invalid/archive-lyrics.flac";
            for authority in ["source", "external"] {
                a.database
                    .write_lyrics_cache(
                        cached_uri,
                        authority,
                        "original",
                        "",
                        "",
                        [4; 32],
                        "Archive lyrics",
                        9,
                    )
                    .await
                    .unwrap();
            }
            a.publish_cache(&a_session, None).await.unwrap();
            b.publish_cache(&b_session, None).await.unwrap();
            let own_assets = folders[0].join("cache");
            let received_assets = folders[1].join("cache");
            let own_image_parent = own_assets
                .join(blake3::hash(a_session.profile.as_bytes()).to_hex().as_str())
                .join("images")
                .join(image_family.rsplit_once('/').unwrap().0);
            std::fs::create_dir_all(&received_assets).unwrap();
            for entry in std::fs::read_dir(&own_assets)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_file())
                .chain(
                    std::fs::read_dir(&own_image_parent)
                        .unwrap()
                        .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap()),
                )
            {
                let entry = entry.unwrap();
                let target = received_assets.join(entry.path().strip_prefix(&own_assets).unwrap());
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::copy(entry.path(), target).unwrap();
            }
            assert!(
                b.source
                    .shared
                    .artwork
                    .cache_file(&image_key)
                    .unwrap()
                    .is_none(),
                "publication does not fill another device's cache"
            );
            b.status.send_modify(|status| {
                status.settings.enabled = true;
                for peer in &mut status.devices {
                    peer.reachable = false;
                }
            });
            for (size, fallback) in [
                (artwork::ImageSize::Thumbnail(700), false),
                (artwork::ImageSize::Original, true),
            ] {
                let (selected, _) = b
                    .fetch_cached_artwork(artwork::CacheImageRequest {
                        family: image_family.clone(),
                        size,
                        fallback,
                    })
                    .await
                    .unwrap();
                assert_eq!(
                    selected, arbitrary_key,
                    "shared-folder lookup selects the cached rendition by actual size"
                );
            }
            let (key, received) = b.fetch_cached_artwork(image_query.clone()).await.unwrap();
            assert_eq!(std::fs::read(&received).unwrap(), encoded);
            assert!(
                b.source
                    .shared
                    .artwork
                    .import_missing_cache_file(&key, &received)
                    .unwrap()
            );
            for authority in ["source", "external"] {
                let row = b
                    .fetch_cached_lyrics(LyricsCacheQuery {
                        media_uri: cached_uri.into(),
                        authority: authority.into(),
                        role: "original".into(),
                        language: String::new(),
                        script: String::new(),
                        input: [4; 32],
                    })
                    .await
                    .unwrap();
                assert_eq!(row.lyrics, "Archive lyrics");
                assert_eq!(row.cache_input_digest, [4; 32]);
                assert_eq!(row.updated_at, 9);
                assert!(
                    b.database
                        .import_lyrics_cache(
                            vec![library::LyricsCacheRow {
                                lyrics: "Replacement".into(),
                                ..row
                            }],
                            false
                        )
                        .await
                        .unwrap()
                        .is_empty()
                );
            }
            b.status
                .send_modify(|status| status.settings.enabled = false);
            let before: BTreeMap<_, _> = std::fs::read_dir(&own_assets)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_file())
                .chain(
                    std::fs::read_dir(&own_image_parent)
                        .unwrap()
                        .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap()),
                )
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry
                            .path()
                            .strip_prefix(&own_assets)
                            .unwrap()
                            .to_path_buf(),
                        std::fs::read(entry.path()).unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                before.len(),
                4,
                "two renditions and two lyrics rows, without a marker"
            );
            b.publish_cache(&b_session, None).await.unwrap();
            let peer_files: BTreeMap<_, _> = std::fs::read_dir(&received_assets)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_file())
                .chain(
                    std::fs::read_dir(
                        received_assets.join(own_image_parent.strip_prefix(&own_assets).unwrap()),
                    )
                    .unwrap()
                    .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap()),
                )
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry
                            .path()
                            .strip_prefix(&received_assets)
                            .unwrap()
                            .to_path_buf(),
                        std::fs::read(entry.path()).unwrap(),
                    )
                })
                .collect();
            assert_eq!(peer_files, before, "incoming cache data is not republished");
            a.publish_cache(&a_session, None).await.unwrap();
            a.open(a_session.profile.clone(), false).await.unwrap();
            a.status
                .send_modify(|status| status.settings.enabled = false);
            let a_session = a.active().await.unwrap();
            a.publish_cache(&a_session, None).await.unwrap();
            let restarted: BTreeMap<_, _> = std::fs::read_dir(&own_assets)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_file())
                .chain(
                    std::fs::read_dir(&own_image_parent)
                        .unwrap()
                        .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap()),
                )
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry
                            .path()
                            .strip_prefix(&own_assets)
                            .unwrap()
                            .to_path_buf(),
                        std::fs::read(entry.path()).unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                before, restarted,
                "unchanged restart does not encrypt assets again"
            );
            b.database.clear_lyrics_cache(&[]).await.unwrap();
            std::fs::remove_file(
                b.source
                    .shared
                    .artwork
                    .cache_file(&image_key)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            b.publish_cache(&b_session, None).await.unwrap();
            assert!(
                b.source
                    .shared
                    .artwork
                    .cache_file(&image_key)
                    .unwrap()
                    .is_none()
            );
            for authority in ["source", "external"] {
                assert!(
                    b.database
                        .lyrics_cache_for_role(
                            cached_uri,
                            "original",
                            "",
                            "",
                            [4; 32],
                            authority,
                            &library::ReadCancellation::new()
                        )
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    a.database
                        .lyrics_cache_for_role(
                            cached_uri,
                            "original",
                            "",
                            "",
                            [4; 32],
                            authority,
                            &library::ReadCancellation::new()
                        )
                        .await
                        .unwrap()
                        .is_some()
                );
            }
            let mut notices = a.source.shared.artwork.subscribe_cache_writes();
            let mut replacement = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                8,
                8,
                image::Rgba([90, 60, 30, 255]),
            ))
            .write_to(&mut replacement, image::ImageFormat::Png)
            .unwrap();
            std::fs::write(&cover, replacement.into_inner()).unwrap();
            a.source
                .shared
                .artwork
                .invalidate_image(&artwork::ArtworkBinding::opaque(&binding))
                .unwrap();
            a.source
                .shared
                .artwork
                .cache_source(&binding)
                .await
                .unwrap();
            let (first_key, first_revision) = notices.recv().await.unwrap();
            let later = sources::LocalImageRef::File {
                source_id: sources::SourceId::new("shared-images"),
                path: cover.to_string_lossy().into_owned(),
                revision: "later".into(),
            }
            .artwork_binding();
            a.source.shared.artwork.cache_source(&later).await.unwrap();
            let (later_key, later_revision) = notices.recv().await.unwrap();
            assert!(later_revision > first_revision);
            a.publish_cache(&a_session, Some((&[first_key], first_revision)))
                .await
                .unwrap();
            let state: serde_json::Value = serde_json::from_str(
                &a_session
                    .documents
                    .cache_file_exchange()
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                state["artwork"], first_revision,
                "a later notice is not acknowledged before its asset"
            );
            let changed: BTreeMap<_, _> = std::fs::read_dir(&own_assets)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_file())
                .chain(
                    std::fs::read_dir(&own_image_parent)
                        .unwrap()
                        .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap()),
                )
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry
                            .path()
                            .strip_prefix(&own_assets)
                            .unwrap()
                            .to_path_buf(),
                        std::fs::read(entry.path()).unwrap(),
                    )
                })
                .collect();
            assert_eq!(changed.len(), before.len());
            assert_eq!(
                before
                    .iter()
                    .filter(|(name, bytes)| changed.get(*name) == Some(*bytes))
                    .count(),
                3,
                "changing one artwork preserves the other rendition and both lyrics files"
            );
            a.publish_cache(&a_session, Some((&[later_key], later_revision)))
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_dir(&own_assets)
                    .unwrap()
                    .filter(|entry| entry.as_ref().unwrap().path().is_file())
                    .chain(
                        std::fs::read_dir(&own_image_parent)
                            .unwrap()
                            .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap())
                    )
                    .count(),
                before.len() + 1
            );
            a.source
                .shared
                .artwork
                .invalidate_image(&artwork::ArtworkBinding::opaque(&binding))
                .unwrap();
            a.source
                .shared
                .artwork
                .cache_source(&binding)
                .await
                .unwrap();
            let (failed_key, failed_revision) = notices.recv().await.unwrap();
            let unavailable = own_assets.with_extension("unavailable");
            std::fs::rename(&own_assets, &unavailable).unwrap();
            std::fs::write(&own_assets, b"not a directory").unwrap();
            assert!(
                a.publish_cache(&a_session, Some((&[failed_key], failed_revision)))
                    .await
                    .is_err()
            );
            std::fs::remove_file(&own_assets).unwrap();
            std::fs::rename(&unavailable, &own_assets).unwrap();
            let retry = sources::LocalImageRef::File {
                source_id: sources::SourceId::new("shared-images"),
                path: cover.to_string_lossy().into_owned(),
                revision: "after-failure".into(),
            }
            .artwork_binding();
            a.source.shared.artwork.cache_source(&retry).await.unwrap();
            let (_, retry_revision) = notices.recv().await.unwrap();
            a.publish_cache(&a_session, None).await.unwrap();
            let state: serde_json::Value = serde_json::from_str(
                &a_session
                    .documents
                    .cache_file_exchange()
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(state["artwork"], retry_revision);
            assert_eq!(
                std::fs::read_dir(&own_assets)
                    .unwrap()
                    .filter(|entry| entry.as_ref().unwrap().path().is_file())
                    .chain(
                        std::fs::read_dir(&own_image_parent)
                            .unwrap()
                            .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap())
                    )
                    .count(),
                before.len() + 2
            );
            for entry in std::fs::read_dir(&own_assets)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_file())
                .chain(
                    std::fs::read_dir(&own_image_parent)
                        .unwrap()
                        .flat_map(|family| std::fs::read_dir(family.unwrap().path()).unwrap()),
                )
            {
                let entry = entry.unwrap();
                let target = received_assets.join(entry.path().strip_prefix(&own_assets).unwrap());
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::copy(entry.path(), target).unwrap();
            }
            b.publish_cache(&b_session, None).await.unwrap();
            assert!(
                b.source
                    .shared
                    .artwork
                    .cache_file(&image_key)
                    .unwrap()
                    .is_none(),
                "changed shared files wait for local demand"
            );
            b.status
                .send_modify(|status| status.settings.enabled = true);
            let (key, image) = b.fetch_cached_artwork(image_query.clone()).await.unwrap();
            assert_eq!(
                std::fs::read(&image).unwrap(),
                std::fs::read(
                    a.source
                        .shared
                        .artwork
                        .cache_file(&image_key)
                        .unwrap()
                        .unwrap()
                )
                .unwrap()
            );
            assert!(
                b.source
                    .shared
                    .artwork
                    .import_missing_cache_file(&key, &image)
                    .unwrap()
            );
            for authority in ["source", "external"] {
                assert!(
                    b.fetch_cached_lyrics(LyricsCacheQuery {
                        media_uri: cached_uri.into(),
                        authority: authority.into(),
                        role: "original".into(),
                        language: String::new(),
                        script: String::new(),
                        input: [4; 32]
                    })
                    .await
                    .is_some(),
                    "cleared lyrics return on demand from shared files"
                );
            }
            b.status
                .send_modify(|status| status.settings.enabled = false);
            // Pruning changes representation, not document versions. The next
            // exchange must still replace this device's old, larger file.
            let session = a.active().await.unwrap();
            for index in 0..80 {
                session
                    .documents
                    .write_records(&[ConnectRecord {
                        kind: "device".into(),
                        key: session.identity.clone(),
                        value: Some(serde_json::json!(format!("Laptop {index}"))),
                    }])
                    .await
                    .unwrap();
            }
            a.exchange().await.unwrap();
            let before = a
                .exchange_import(&session, folders[0].join(&names[0]))
                .await
                .unwrap();
            std::fs::copy(folders[0].join(&names[0]), folders[1].join(&names[0])).unwrap();
            b.exchange().await.unwrap();
            std::fs::copy(folders[1].join(&names[1]), folders[0].join(&names[1])).unwrap();
            a.exchange().await.unwrap();
            while session
                .documents
                .prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE)
                .await
                .unwrap()
                > 0
            {}
            a.exchange().await.unwrap();
            let after = a
                .exchange_import(&session, folders[0].join(&names[0]))
                .await
                .unwrap();
            let before =
                std::io::copy(&mut before.reader().unwrap(), &mut std::io::sink()).unwrap();
            let after = std::io::copy(&mut after.reader().unwrap(), &mut std::io::sink()).unwrap();
            assert!(after < before);
            let compact = std::fs::read(folders[0].join(&names[0])).unwrap();
            a.exchange().await.unwrap();
            assert!(std::fs::read(folders[0].join(&names[0])).unwrap() == compact);
            for inputs in [host, guest] {
                inputs.receivers.visualizer.close();
                let playback = inputs.products.playback.clone();
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .unwrap();
            }
        }))
    })
    .unwrap();
}

#[test]
fn webdav_exchange_leaves_an_unchanged_profile_file_untouched() {
    use axum::{body::Bytes, extract::State, http::StatusCode, response::IntoResponse};
    let root = tempfile::tempdir().unwrap();
    let diagnostics = diagnostics();
    app::with_runtime(|runtime| {
        runtime.block_on(Box::pin(async {
            type Files = Arc<Mutex<(BTreeMap<String, Vec<u8>>, usize, BTreeSet<String>, usize)>>;
            let file: Files = Arc::new(Mutex::new((BTreeMap::new(), 0, BTreeSet::from(["/".into()]), 0)));
            let router = axum::Router::new().fallback(axum::routing::any(
                |State(files): State<Files>, method: axum::http::Method, uri: axum::http::Uri, body: Bytes| async move {
                    let path = uri.path().to_owned();
                    let mut files = files.lock().unwrap();
                    match method.as_str() {
                        "MKCOL" => {
                            files.2.insert(path.trim_end_matches('/').to_owned());
                            StatusCode::CREATED.into_response()
                        }
                        "PROPFIND" => {
                            let base = if path.trim_end_matches('/').is_empty() { "/" } else { path.trim_end_matches('/') };
                            let directory = files.2.contains(base);
                            if !directory && !files.0.contains_key(&path) { return StatusCode::NOT_FOUND.into_response(); }
                            let kind = if directory { "<d:collection/>" } else { "" };
                            let mut entries = format!("<d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype>{kind}</d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>");
                            if directory {
                                for name in files.0.keys().chain(files.2.iter()) {
                                    let parent = name.rsplit_once('/').map_or("/", |(parent, _)| if parent.is_empty() { "/" } else { parent });
                                    if name == base || parent != base { continue; }
                                    let kind = if files.2.contains(name) { "<d:collection/>" } else { "" };
                                    entries.push_str(&format!("<d:response><d:href>{name}</d:href><d:propstat><d:prop><d:resourcetype>{kind}</d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"));
                                }
                            }
                            (StatusCode::MULTI_STATUS, format!("<d:multistatus xmlns:d=\"DAV:\">{entries}</d:multistatus>")).into_response()
                        }
                        "GET" => {
                            if path.contains("/cache/") { files.3 += 1; }
                            match files.0.get(&path) {
                                Some(bytes) => bytes.clone().into_response(),
                                None => StatusCode::NOT_FOUND.into_response(),
                            }
                        }
                        "DELETE" => {
                            if files.0.remove(&path).is_some() { StatusCode::NO_CONTENT.into_response() }
                            else { StatusCode::NOT_FOUND.into_response() }
                        }
                        "PUT" => {
                            assert!(path.starts_with("/profiles/"));
                            assert!(files.2.contains(path.rsplit_once('/').unwrap().0), "create the chosen folder before uploading");
                            files.0.insert(path, body.to_vec());
                            files.1 += 1;
                            StatusCode::CREATED.into_response()
                        }
                        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
                    }
                }
            )).with_state(file.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let inputs = device(
                root.path(),
                "file-exchange",
                diagnostics,
                Arc::new(secrets::MemorySecretStore::new()),
            )
            .await;
            let settings = sources::FileSourceSettings {
                excluded_folders: Vec::new(),
                url: format!("http://{address}/"),
                alternate_urls: vec![], folders: vec![], username: String::new(), domain: String::new(),
                authentication: sources::FileAuthentication::Anonymous,
                trust_invalid_certificate: false, certificate_pem: None, require_smb_encryption: false,
            };
            let source_id = inputs.products.source.configure_file_integration(
                sources::SourceSetup::WebDav {
                    name: "Profile storage".into(), settings,
                    credentials: sources::FileCredentials { secret: String::new(), headers: vec![] },
                },
            ).recv().await.unwrap().unwrap();
            assert!(inputs.products.source.list_sources().sources.is_empty());
            assert!(inputs.products.source.selected_library().is_none());
            assert_eq!(inputs.products.source.file_integrations(), vec![crate::source::FileIntegration {
                id: source_id.clone(), name: "Profile storage".into(), kind: "webdav".into(), music_source: false,
            }]);
            let preset = inputs.products.source.editable(source_id.clone()).unwrap().unwrap();
            inputs.products.source.update_file_integration(sources::SourceSettingsChange::Files {
                source_id: source_id.clone(), name: "Files".into(), settings: preset.file_settings.unwrap(),
                credentials: sources::FileCredentialsEdit { secret: None, headers: None },
            }).recv().await.unwrap().unwrap();
            assert_eq!(inputs.products.source.file_integrations()[0].name, "Files");
            assert!(inputs.products.source.list_sources().sources.is_empty());
            let settings = inputs.products.source.shared.settings.clone();
            tokio::task::spawn_blocking(move || settings.update(|stored| {
                let mut music = stored.sources.integrations[0].clone();
                music.configuration.source_id = sources::SourceId::new("music-server");
                music.configuration.name = "Music server".into();
                stored.sources.configured.push(music);
                Ok(())
            })).await.unwrap().unwrap();
            let connections = inputs.products.source.file_integrations();
            assert_eq!(connections.len(), 2);
            assert_eq!(connections[0].id.as_str(), "music-server");
            assert!(connections[0].music_source);
            assert_eq!(inputs.products.source.list_sources().sources[0].id, connections[0].id);
            assert_eq!(inputs.products.source.shared.settings.load().sources.integrations.len(), 1);
            let owner = &inputs.products.connect;
            execute(owner, Action::Create).await.unwrap();
            execute(
                owner,
                Action::Destination {
                    destination: Some(portable::Destination::Remote {
                        source_id,
                        path: "profiles".into(),
                    }),
                },
            )
            .await
            .unwrap();
            prepare_profile_snapshot(owner).await;
            owner.exchange().await.unwrap();
            let uploads = file.lock().unwrap().1;
            assert!(uploads > 0);
            for _ in 0..3 {
                owner.exchange().await.unwrap();
            }
            assert_eq!(
                file.lock().unwrap().1,
                uploads,
                "reading the same profile must not PUT it again, even without an ETag"
            );
            execute(
                owner,
                Action::Rename {
                    name: "Renamed".into(),
                },
            )
            .await
            .unwrap();
            owner.exchange().await.unwrap();
            assert_eq!(file.lock().unwrap().1, uploads + 1, "local edits must reach the file");
            owner.exchange().await.unwrap();
            assert_eq!(file.lock().unwrap().1, uploads + 1);
            owner.status.send_modify(|status| status.settings.enabled = false);
            let session = owner.active().await.unwrap();
            owner.publish_cache(&session, None).await.unwrap();
            let cache_uploads = file.lock().unwrap().1;
            owner.status.send_modify(|status| status.settings.enabled = true);
            for (index, text) in [(1, "First cached lyrics"), (2, "Updated cached lyrics")] {
                owner.database.write_lyrics_cache("https://example.invalid/remote-cache.flac", "external", "original", "", "", [0; 32], text, index).await.unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    while file.lock().unwrap().1 < cache_uploads + index as usize {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.expect("idle lyrics write wakes publication");
                assert_eq!(file.lock().unwrap().1, cache_uploads + index as usize,
                    "only the changed asset is uploaded");
                let saved = file.lock().unwrap().0.clone();
                owner.publish_cache(&session, None).await.unwrap();
                assert_eq!(file.lock().unwrap().0, saved, "unchanged no-ETag exchange preserves ciphertext");
                assert_eq!(file.lock().unwrap().3, 0, "unchanged publication does not read shared payloads");
                assert_eq!(saved.keys().filter(|path| path.contains("/cache/")).count(), 1,
                    "asset replacement leaves no older payload file");
            }
            owner.leave_profile().await.unwrap();
            inputs.receivers.visualizer.close();
            let playback = inputs.products.playback.clone();
            tokio::task::spawn_blocking(move || playback.shutdown())
                .await
                .unwrap();
            server.abort();
        }))
    })
    .unwrap();
}

async fn execute(owner: &Arc<ConnectOwner>, action: Action) -> Result<Status, String> {
    let owner = Arc::clone(owner);
    tokio::spawn(async move { owner.execute(action).await })
        .await
        .unwrap()
        .map_err(|error| error.to_string())
}

// Finish the writes that background startup and pruning would otherwise race
// against assertions about an unchanged profile file.
async fn prepare_profile_snapshot(owner: &Arc<ConnectOwner>) {
    loop {
        let seeded = owner.database.connect_seed_page().await.unwrap();
        let captured = owner.synchronize().await.unwrap();
        if !seeded && !captured {
            break;
        }
    }
    let session = owner.active().await.unwrap();
    while session
        .documents
        .prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE)
        .await
        .unwrap()
        > 0
    {}
}

async fn device(
    root: &std::path::Path,
    name: &str,
    diagnostics: crate::runtime::DiagnosticsHandle,
    secret_store: Arc<dyn SecretStore>,
) -> RuntimeInputs {
    let root = root.join(name);
    let paths = Paths {
        config: root.join("config"),
        cache: root.join("cache"),
        data: root.join("data"),
        state: root.join("state"),
    };
    let settings = app::startup_settings(&paths);
    let inputs = app::runtime_inputs(
        diagnostics,
        false,
        settings,
        paths,
        || Ok(Box::new(IdleBackend) as Box<dyn PlaybackBackend>),
        Vec::new,
        Arc::new(|_, _| {}),
        Arc::new(|_, _| {}),
    )
    .await
    .unwrap();
    inputs
        .products
        .source
        .change_secret_storage(secrets::SecretStorageMode::ConfigFile)
        .await
        .unwrap();
    inputs.products.connect.secrets.replace(secret_store);
    inputs
}

async fn wait(owner: &ConnectOwner, predicate: impl Fn(&Status) -> bool) -> Status {
    let mut status = owner.subscribe();
    tokio::time::timeout(Duration::from_secs(90), status.wait_for(predicate))
        .await
        .unwrap_or_else(|_| panic!("Connect did not settle: {:?}", owner.status()))
        .unwrap()
        .clone()
}

async fn approve_pair(host: &Arc<ConnectOwner>, guest: &Arc<ConnectOwner>) {
    let invitation = host.status().invitation.unwrap();
    execute(
        guest,
        Action::Join {
            invitation,
            replace: true,
        },
    )
    .await
    .unwrap();
    let a = wait(host, |status| status.pairing.is_some())
        .await
        .pairing
        .unwrap();
    let b = wait(guest, |status| status.pairing.is_some())
        .await
        .pairing
        .unwrap();
    assert_eq!(a.emoji, b.emoji);
    let already_enrolled = host
        .status()
        .devices
        .iter()
        .any(|device| device.enrolled && device.id == a.peer);
    for _ in 0..2 {
        let confirmed = execute(
            host,
            Action::Pair {
                session: a.session.clone(),
                approve: true,
                replace: false,
            },
        )
        .await
        .unwrap();
        assert!(confirmed.pairing.as_ref().unwrap().approved);
        assert_eq!(
            confirmed
                .devices
                .iter()
                .any(|device| device.enrolled && device.id == a.peer),
            already_enrolled,
        );
    }
    execute(
        guest,
        Action::Pair {
            session: b.session,
            approve: true,
            replace: false,
        },
    )
    .await
    .unwrap();
}

async fn prepare_pair(host: &Arc<ConnectOwner>, guest: &Arc<ConnectOwner>) {
    approve_pair(host, guest).await;
    wait(guest, |status| {
        !status.connecting
            && status.pairing.is_none()
            && status.devices.iter().any(|device| {
                Some(&device.id) == host.status().identity.as_ref() && device.enrolled
            })
    })
    .await;
    assert!(guest.status().settings.setup_pending);
    assert!(!guest.status().receiving_collection);
    assert!(!guest.synchronize().await.unwrap());
    guest.exchange().await.unwrap();
}

async fn pair(host: &Arc<ConnectOwner>, guest: &Arc<ConnectOwner>) {
    prepare_pair(host, guest).await;
    execute(guest, Action::FinishSetup).await.unwrap();
    assert!(!guest.status().settings.setup_pending);
}
