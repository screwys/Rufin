use library::{Database, Freshness, ReadCancellation, Scan, ScanLink, ScanOutcome};

async fn track(scan: &mut Scan, favorite: Option<bool>) {
    scan.write_track(
        "track",
        Some("album"),
        "Track",
        "track",
        "Album",
        "Artist",
        "track",
        180000,
        1,
        1,
        Some(2025),
        Some("2025-01-01"),
        None,
        None,
        Some("FLAC"),
        Some("Comment"),
        Some(120),
        Some("recording"),
        Some("release-track"),
        None,
        None,
        None,
        None,
        favorite,
        Some(8),
        None,
        None,
        None,
        None,
        None,
        [1; 32],
    )
    .await
    .unwrap();
}

async fn fixture() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("library.sqlite3"))
        .await
        .unwrap();
    let mut scan = Scan::begin(
        &database,
        "source",
        "Source",
        "source",
        Some(Freshness::new(b"catalog".to_vec()).unwrap()),
    )
    .await
    .unwrap();
    scan.write_album(
        "album",
        "Album",
        "album",
        "Artist",
        "album",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        Some(8),
        None,
    )
    .await
    .unwrap();
    scan.write_artist(
        "artist",
        "Artist",
        "artist",
        Some("artist"),
        None,
        None,
        Some(true),
        Some(8),
    )
    .await
    .unwrap();
    track(&mut scan, Some(true)).await;
    scan.write_folder("folder", "Folder", "folder", "folder", None)
        .await
        .unwrap();
    scan.write_track_folders(&[ScanLink::new("track", "folder", 0)])
        .await
        .unwrap();
    scan.finish().await.unwrap();
    (directory, database)
}

#[tokio::test]
async fn point_metadata_preserves_folders_and_complete_memberships_can_clear_them() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for replace in [false, true] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        track(&mut scan, None).await;
        if replace {
            scan.replace_track_folders("track", &[]).await.unwrap();
            track(&mut scan, None).await;
        }
        let outcome = scan.finish().await.unwrap();
        assert_eq!(matches!(outcome, ScanOutcome::Identical(_)), !replace);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_folders")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            i64::from(!replace)
        );
    }
}

#[tokio::test]
async fn favorite_snapshots_replace_only_source_favorites_and_keep_catalog_freshness() {
    let (directory, database) = fixture().await;
    let path = directory.path().join("library.sqlite3");
    let mut raw = super::support::connection(&path).await;
    let uri = library::source_entity_uri(&library::SourceId::new("source"), "track", "track");
    database
        .set_favorite(&library::FavoriteTarget::Track(uri.clone()), true)
        .await
        .unwrap();
    for (favorite, changed) in [(false, true), (false, false), (true, true)] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        let ids = |id: &str| {
            if favorite {
                vec![id.to_string()]
            } else {
                Vec::new()
            }
        };
        scan.replace_favorites(&ids("track"), &ids("album"), &ids("artist"))
            .await
            .unwrap();
        let outcome = scan.finish().await.unwrap();
        assert_eq!(matches!(outcome, ScanOutcome::Changed(_)), changed);
        for table in ["tracks", "albums", "artists"] {
            let sql = format!("SELECT source_favorite,source_rating FROM {table}");
            assert_eq!(
                sqlx::query_as::<_, (bool, Option<i64>)>(sqlx::AssertSqlSafe(sql))
                    .fetch_one(&mut raw)
                    .await
                    .unwrap(),
                (favorite, Some(80))
            );
        }
        assert!(
            database
                .favorite(&library::FavoriteTarget::Track(uri.clone()))
                .await
                .unwrap()
        );
        assert!(
            Scan::accept_freshness(
                &database,
                "source",
                &Freshness::new(b"catalog".to_vec()).unwrap(),
                &ReadCancellation::new()
            )
            .await
            .unwrap()
            .is_some()
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String, i64)>(
                "SELECT comment,release_date,(SELECT count(*) FROM track_folders) FROM tracks"
            )
            .fetch_one(&mut raw)
            .await
            .unwrap(),
            ("Comment".into(), "2025-01-01".into(), 1)
        );
    }
}

#[tokio::test]
async fn home_sections_refresh_and_clear_without_replacing_their_entities() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for (populated, changed) in [(true, true), (true, false), (false, true)] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        let entries = if populated {
            vec![library::HomeEntryInput {
                section_id: "most-played".into(),
                position: 0,
                kind: library::HomeEntryKind::Album,
                entity_object_id: "album".into(),
                title: "Album".into(),
                subtitle: "Artist".into(),
            }]
        } else {
            vec![]
        };
        scan.replace_home_section("most-played", &entries)
            .await
            .unwrap();
        assert_eq!(
            matches!(scan.finish().await.unwrap(), ScanOutcome::Changed(_)),
            changed
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM home_entries")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            i64::from(populated)
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT title FROM albums")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "Album"
        );
        assert!(
            Scan::accept_freshness(
                &database,
                "source",
                &Freshness::new(b"catalog".to_vec()).unwrap(),
                &ReadCancellation::new()
            )
            .await
            .unwrap()
            .is_some()
        );
    }
}

#[tokio::test]
async fn incomplete_catalog_keeps_home_sections_when_their_refresh_fails() {
    let (directory, database) = fixture().await;
    let mut scan = Scan::begin_items(&database, "source").await.unwrap();
    scan.replace_home_section(
        "most-played",
        &[library::HomeEntryInput {
            section_id: "most-played".into(),
            position: 0,
            kind: library::HomeEntryKind::Album,
            entity_object_id: "album".into(),
            title: "Album".into(),
            subtitle: "Artist".into(),
        }],
    )
    .await
    .unwrap();
    scan.finish().await.unwrap();
    let mut scan = Scan::begin(&database, "source", "Source", "source", None)
        .await
        .unwrap();
    scan.incomplete();
    scan.write_album(
        "album",
        "Album",
        "album",
        "Artist",
        "album",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        Some(8),
        None,
    )
    .await
    .unwrap();
    scan.retain_home_section("most-played").await.unwrap();
    scan.finish().await.unwrap();
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM home_entries")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn absent_favorites_preserve_known_state_and_explicit_false_clears_it() {
    let (directory, database) = fixture().await;
    let mut raw = super::support::connection(&directory.path().join("library.sqlite3")).await;
    for (favorite, expected) in [(None, true), (Some(false), false), (None, false)] {
        let mut scan = Scan::begin_items(&database, "source").await.unwrap();
        track(&mut scan, favorite).await;
        scan.finish().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, bool>("SELECT source_favorite FROM tracks")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            expected
        );
    }
}
