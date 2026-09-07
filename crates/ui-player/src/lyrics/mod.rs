pub mod search;
pub mod state;
mod timing;
mod view;
mod wrapping_line;

mod panel;
pub mod settings;
pub fn lyrics_popup_content_width() -> i32 {
    (ui_shared::layout::large_popup_content_width(ui_shared::layout::LARGE_POPUP_BASE_WIDTH) * 3
        + 2)
        / 4
}

pub fn lyrics_popup_content_height(app_height: i32) -> i32 {
    (ui_shared::layout::large_popup_content_height(
        app_height,
        ui_shared::layout::LARGE_POPUP_BASE_HEIGHT,
    ) * 3
        + 2)
        / 4
}
