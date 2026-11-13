//! Release types from MusicBrainz for the albums no source typed.

use std::sync::Arc;

use library::Catalog;
use tracing::{info, warn};

use crate::settings::SettingsFile;

/// Asks MusicBrainz for every album that still has no answer, a hundred a request, until
/// none is left.
pub(crate) async fn fill_release_types(catalog: Arc<Catalog>, settings: SettingsFile) {
    let allowed = || settings.load().ui.allows_external_metadata_lookup();
    if !allowed() {
        return;
    }
    // Albums a refresh adds meanwhile are asked for in the next round.
    loop {
        let identities = match catalog.untyped_release_identities().await {
            Ok(identities) if identities.is_empty() => return,
            Ok(identities) => identities,
            Err(error) => {
                warn!(%error, "could not read the albums without a release type");
                return;
            }
        };
        for identities in identities.chunks(100) {
            if !allowed() {
                return;
            }
            let asked = identities.to_vec();
            let answers =
                tokio::task::spawn_blocking(move || metadata_lookup::lookup_release_types(&asked))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|answers| answers);
            let accepted = match answers {
                Ok(answers) => catalog
                    .accept_release_types(&answers)
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            };
            // A later refresh asks again for what is left.
            if let Err(error) = accepted {
                warn!(%error, "release type lookup failed");
                return;
            }
        }
        info!(
            asked = identities.len(),
            "asked MusicBrainz for release types"
        );
    }
}
