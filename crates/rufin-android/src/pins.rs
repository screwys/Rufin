use crate::{
    host::{AndroidError, error},
    library::AndroidLibrary,
};

#[uniffi::export]
impl AndroidLibrary {
    pub async fn import_playlist_pins(&self) -> Result<bool, AndroidError> {
        self.source
            .import_playlist_pins_once(self.settings.clone())
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
}
