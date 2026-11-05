pub(crate) mod desktop;
pub(crate) mod random_play;

pub(crate) use desktop::{install_desktop_lifecycle, present_initial_window};
pub(crate) use desktop::{
    now_playing_notification_can_send, now_playing_notification_should_withdraw,
};

pub(crate) use random_play::play_saved_random;
