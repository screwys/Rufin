use std::thread;
use std::time::Instant;

use tracing::warn;

use super::ArtworkRequests;

const SLOW_LOOKUP_MILLIS: u128 = 5_000;

pub(super) fn start(requests: ArtworkRequests) {
    let result = thread::Builder::new()
        .name("rufin-discord-cover".to_string())
        .spawn(move || {
            while let Some(request) = requests.recv() {
                let queued_millis = request.queued_for().as_millis();
                let started = Instant::now();
                let result = metadata_lookup::public_album_cover_url(
                    &request.key.album,
                    &request.key.lastfm_api_key,
                );
                let lookup_millis = started.elapsed().as_millis();
                let total_millis = queued_millis.saturating_add(lookup_millis);
                if total_millis >= SLOW_LOOKUP_MILLIS {
                    warn!(
                        queued_millis,
                        lookup_millis, total_millis, "slow Discord cover lookup"
                    );
                }
                request.complete(result);
            }
        });
    if let Err(error) = result {
        warn!(%error, "failed to start Discord cover lookup");
    }
}
