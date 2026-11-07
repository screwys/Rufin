use desktop_integration::{FolderTarget, folder_target};

pub async fn open_folder_uri(window: &gtk::ApplicationWindow, uri: &str) -> Result<(), String> {
    match folder_target(uri)? {
        FolderTarget::File(file) => gtk::FileLauncher::new(Some(&file))
            .launch_future(Some(window))
            .await
            .map_err(|error| error.to_string()),
        #[cfg(target_os = "macos")]
        FolderTarget::Finder(uri) => desktop_integration::open_in_finder(&uri).await,
    }
}
