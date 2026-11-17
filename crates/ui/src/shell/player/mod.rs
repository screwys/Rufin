pub(crate) mod desktop;
pub(crate) mod mix;

pub(crate) use desktop::{install_desktop_lifecycle, present_initial_window};
pub(crate) use desktop::{
    now_playing_notification_can_send, now_playing_notification_should_withdraw,
};

pub(crate) use mix::play_saved_mix;
