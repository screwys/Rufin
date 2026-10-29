use library::ReadCancellation;

use super::support::{connection, fixture};

#[tokio::test]
async fn repeated_album_cards_keep_whole_counts_and_separate_source_activity() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    sqlx::raw_sql("UPDATE sources SET kind='subsonic';
      INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at)
      VALUES('source','lifetime','track','track-0',7,0,100),('source','lifetime','track','track-1',99,0,900);
      UPDATE tracks SET native_play_count=3,native_last_played=200 WHERE object_id='track-1';")
        .execute(&mut raw).await.unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;

    let cancel = ReadCancellation::new();
    let keys = [fixture.albums[0], fixture.albums[0]];
    let rows = fixture.catalog.albums(&keys, true, &cancel).await.unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.summary.track_count, 2);
        assert_eq!(row.state.play_count, 3);
        assert_eq!(row.state.last_played, Some(200));
        assert_eq!(row.links.album_artists.len(), 1);
    }
    sqlx::query("UPDATE sources SET kind='local'")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_links WHERE track_key=?1 AND role=4")
        .bind(fixture.tracks[1])
        .execute(&mut raw)
        .await
        .unwrap();
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let rows = fixture.catalog.albums(&keys, true, &cancel).await.unwrap();
    for row in &rows {
        assert_eq!(row.summary.track_count, 2);
        assert_eq!(row.state.play_count, 106);
        assert_eq!(row.state.last_played, Some(900));
    }
    let artist = fixture
        .catalog
        .artists(&[fixture.artists[0]], false, true, &cancel)
        .await
        .unwrap();
    assert_eq!(artist[0].state.play_count, 106);
    assert_eq!(artist[0].state.last_played, Some(900));
}

#[tokio::test]
async fn detail_folder_bounds_follow_collection_roles_and_selected_folder() {
    let mut fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let paths = [
        "Artist/Album/Disc 1/a.flac",
        "Artist/Album/Disc 2/z.flac",
        "Other/Album/c.flac",
        "Other/Album/d.flac",
    ]
    .map(|path| {
        fixture
            ._directory
            .path()
            .join(path)
            .to_str()
            .unwrap()
            .to_owned()
    });
    for (track, path) in fixture.tracks.iter().zip(&paths) {
        sqlx::query("INSERT INTO track_details(track_key,source_path) VALUES(?2,?1) ON CONFLICT(track_key) DO UPDATE SET source_path=excluded.source_path")
            .bind(path)
            .bind(track)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    // A guest appearance belongs to the artist view, not their album-artist view.
    sqlx::query("INSERT INTO track_links(track_key,role,position,target) VALUES(?1,0,1,?2)")
        .bind(fixture.tracks[2])
        .bind(fixture.artists[0])
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_links WHERE role=4 AND track_key<>?1")
        .bind(fixture.tracks[0])
        .execute(&mut raw)
        .await
        .unwrap();
    drop(raw);
    fixture.catalog = super::support::reopen(fixture.catalog, &fixture.path).await;
    let catalog = &fixture.catalog;
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.track_uris[2], false, Some(fixture.folder))
            .await
            .unwrap(),
        Some((paths[2].clone(), paths[2].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.album_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.artist_uris[0], false, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[2].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.artist_uris[0], true, None)
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[1].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.artist_uris[0], false, Some(fixture.folder))
            .await
            .unwrap(),
        Some((paths[0].clone(), paths[0].clone()))
    );
    assert_eq!(
        catalog
            .collection_source_path_bounds(&fixture.album_uris[1], false, Some(fixture.folder))
            .await
            .unwrap(),
        None
    );
}
