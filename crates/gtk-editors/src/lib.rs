pub mod metadata;
pub mod smart_playlist;
pub mod ui_resource;
pub fn register_resources() -> Result<(), String> {
    gtk_widgets::register_resources()?;
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gtk::gio::resources_register_include!("gtk-editors.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}
