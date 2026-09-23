mod edge_fade;
pub mod search;
pub mod state;
mod timing;
mod view;
pub use view::LyricsPane;
mod wrapping_line;

mod panel;
pub mod settings;
pub fn lyrics_popup_content_width() -> i32 {
    (gtk_widgets::layout::large_popup_content_width(gtk_widgets::layout::LARGE_POPUP_BASE_WIDTH)
        * 3
        + 2)
        / 4
}

pub fn lyrics_popup_content_height(app_height: i32) -> i32 {
    (gtk_widgets::layout::large_popup_content_height(
        app_height,
        gtk_widgets::layout::LARGE_POPUP_BASE_HEIGHT,
    ) * 3
        + 2)
        / 4
}
