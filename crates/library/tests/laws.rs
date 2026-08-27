use library::{PlaylistEntrySort, ReadCancellation};
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
                .map(|index| fixture.tracks[*index])
                .collect::<Vec<_>>();
            let playlist = fixture
                .database
                .create_playlist(fixture.source, "Property Playlist", &expected)
                .await
                .expect("create property Playlist")
                .expect("all property Tracks exist");
            let projection = fixture
                .database
                .playlist_entry_order(
                    fixture.source,
                    playlist,
                    None,
                    PlaylistEntrySort::Position,
                    false,
                    "",
                    &ReadCancellation::new(),
                )
                .await
                .expect("property Playlist projection");
            (expected, projection)
        });

        prop_assert_eq!(projection.tracks, expected);
        prop_assert_eq!(
            projection.track_positions,
            (0..selected.len()).collect::<Vec<_>>()
        );
        prop_assert_eq!(projection.entries.len(), selected.len());
    }
}
