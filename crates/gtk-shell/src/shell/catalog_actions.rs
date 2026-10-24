use super::Shell;
use gtk_widgets::media_drag::MediaDragSource;
use rufin_core::playback::PlayRequest;
use rufin_core::playback::QueuePlacement;
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
