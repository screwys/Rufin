use super::Shell;
use gtk_widgets::media_drag::MediaDragSource;
use rufin_core::playback::PlayRequest;
use rufin_core::playback::QueuePlacement;
use std::rc::Rc;
use tracing::warn;
pub(crate) fn download(payload: &MediaDragSource, shell: &Shell) {
    let payload = payload.clone();
    let database = shell.products.library.clone();
    let source = shell.products.source.clone();
    shell.products.runtime.spawn(async move {
        match payload.resolve(&database).await {
            Ok((uris, subject)) => source.download_media(subject, uris),
            Err(error) => warn!(%error, "failed to identify download tracks"),
        }
    });
}

pub(crate) fn remove_download(payload: &MediaDragSource, shell: &Shell) {
    let payload = payload.clone();
    let database = shell.products.library.clone();
    let source = shell.products.source.clone();
    shell.products.runtime.spawn(async move {
        match payload.resolve(&database).await {
            Ok((uris, _)) => source.remove_download_media(uris),
            Err(error) => warn!(%error, "failed to identify downloaded tracks"),
        }
    });
}

pub(crate) fn play_target(
    target: &PlayRequest,
    shell: &Shell,
    placement: QueuePlacement,
    shuffled: bool,
) {
    let mut request = target.clone().shuffled(shuffled);
    request.placement = placement;
    shell.products.playback.play_selection(request);
}

use gtk_player::state::current_playback_track;
pub(super) fn add_current_to_playlist(
    shell: &Rc<Shell>,
    playlist: library::PlaylistKey,
    destination: String,
) {
    let Some(media) = current_playback_track(shell.player_ui.selected_playback().as_deref()) else {
        return;
    };
    let title = media.metadata.title.clone();
    let feedback_shell = Rc::downgrade(shell);
    let destination = destination.clone();
    let preview_uris = vec![media.media_uri.clone()];
    gtk_widgets::playlists::add_media_to_playlist(
        &shell.products.source,
        playlist,
        library::CatalogQuery::uris(library::CatalogKind::Track, vec![media.media_uri]).selection(),
        false,
        Rc::new(move |accepted| {
            if accepted > 0
                && let Some(shell) = feedback_shell.upgrade()
            {
                shell.show_operation_feedback(&gtk_widgets::downloads::OperationFeedback {
                    title: title.clone(),
                    artwork: gtk_widgets::downloads::OperationArtwork::MediaUris(
                        preview_uris.clone(),
                    ),
                    item_count: accepted,
                    kind: gtk_widgets::downloads::OperationFeedbackKind::PlaylistAdded {
                        destination: destination.clone(),
                    },
                });
            }
        }),
    );
}
