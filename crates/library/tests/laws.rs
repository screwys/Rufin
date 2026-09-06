use library::ReadCancellation;
use proptest::prelude::*;

use super::support::fixture;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]
    #[test]
    fn playlist_occurrence_projection_preserves_arbitrary_duplicates(
        selected in prop::collection::vec(0usize..4, 1..96),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("property runtime");
        let (expected, projection) = runtime.block_on(async {
            let fixture = fixture().await;
            let expected = selected
                .iter()
                .map(|index| fixture.track_uris[*index].clone())
                .collect::<Vec<_>>();
            let playlist = fixture
                .database
                .create_playlist(Some(fixture.source), "Property Playlist", &expected)
                .await
                .expect("create property Playlist")
                .expect("all property Tracks exist")
                .0;
            let projection = fixture
                .database
                .playlist_media_uri_order(playlist,
                    None,
                    &ReadCancellation::new(),
                )
                .await
                .expect("property Playlist projection");
            (expected, projection)
        });

        prop_assert_eq!(&projection, &expected);
        prop_assert_eq!(projection.len(), selected.len());
    }
}
