mod bottom;
pub(crate) mod desktop;
pub(crate) mod lyrics;
pub(crate) mod queue;
mod random_play;
pub(crate) mod right_panel;

pub(crate) use bottom::connect_player_controls;
pub(crate) use desktop::{install_desktop_lifecycle, present_initial_window};
pub(crate) use desktop::{
    now_playing_notification_can_send, now_playing_notification_should_withdraw,
};

pub(crate) use random_play::play_saved_random;
