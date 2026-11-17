//! Rufin's Flatpak reads the folders and files people chose through the document portal, which
//! grants them to one app ID. Before the app ID changes to `app.rufin.rufin`, the current app
//! grants the new one each document with the rights it holds itself, so the renamed app keeps the
//! local library, backup folder and other chosen places. The release with the new ID deletes
//! this.

const NEXT_APP_ID: &str = "app.rufin.rufin";
/// Where a sandboxed app sees the documents granted to it.
const DOCUMENTS: &str = "/run/flatpak/doc";

pub(crate) fn grant_to_next_app_id() {
    if std::env::var("FLATPAK_ID").as_deref() != Ok(app_identity::STABLE_APP_ID) {
        return;
    }
    let Ok(documents) = std::fs::read_dir(DOCUMENTS) else {
        return;
    };
    let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(connection) => connection,
        Err(error) => {
            tracing::warn!(%error, "could not reach the document portal");
            return;
        }
    };
    for document in documents.flatten() {
        let Some(id) = document.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // A document holds the one file or folder it grants; its mode shows a read-only grant.
        let writable = std::fs::read_dir(document.path())
            .ok()
            .and_then(|mut children| children.next())
            .and_then(Result::ok)
            .and_then(|child| child.metadata().ok())
            .is_some_and(|metadata| !metadata.permissions().readonly());
        let mut permissions = vec!["read", "grant-permissions"];
        if writable {
            permissions.push("write");
        }
        // Documents opened without the right to grant them, such as a single file, stay here.
        if let Err(error) = connection.call_sync(
            Some("org.freedesktop.portal.Documents"),
            "/org/freedesktop/portal/documents",
            "org.freedesktop.portal.Documents",
            "GrantPermissions",
            Some(&gio::glib::Variant::from((
                id.as_str(),
                NEXT_APP_ID,
                permissions,
            ))),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        ) {
            tracing::debug!(%error, document = id, "could not grant a document to the next app ID");
        }
    }
}
