mod panel;
pub(crate) mod search;
pub(crate) mod settings;
pub(crate) mod state;
mod timing;
mod view;
mod wrapping_line;

pub(crate) use view::LyricsPane;

pub(super) fn lyrics_popup_content_width() -> i32 {
    (crate::layout::large_popup_content_width(crate::layout::LARGE_POPUP_BASE_WIDTH) * 3 + 2) / 4
}

pub(super) fn lyrics_popup_content_height(app_height: i32) -> i32 {
    (crate::layout::large_popup_content_height(app_height, crate::layout::LARGE_POPUP_BASE_HEIGHT)
        * 3
        + 2)
        / 4
}
