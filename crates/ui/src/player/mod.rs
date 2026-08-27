mod bottom;
pub(crate) mod desktop;
mod equalizer;
pub(crate) mod fullscreen;
mod icons;
pub(crate) mod lyrics;
mod outputs;
mod playback_settings;
mod progress;
pub(crate) mod queue;
mod random_play;
pub(crate) mod right_panel;
pub(crate) mod state;

pub(crate) use bottom::{
    BOTTOM_PLAYER_HEIGHT, NOW_PLAYING_RAIL_WIDTH, PlayerControls, build_bottom_player,
    connect_player_controls,
};
pub(crate) use desktop::{install_desktop_lifecycle, present_initial_window};
pub(crate) use desktop::{
    now_playing_notification_can_send, now_playing_notification_should_withdraw,
};
pub(crate) use equalizer::{
    build_equalizer_preset_row, connect_equalizer_scale_commit, equalizer_band_title,
    equalizer_default_preset_bands, equalizer_preset_bands, equalizer_preset_name_at,
    equalizer_preset_position, equalizer_selected_preset, install_equalizer_scroll,
};
pub(crate) use fullscreen::{
    FullscreenPlayerParts, build_fullscreen_player, connect_fullscreen_player_controls,
};
pub(crate) use outputs::{
    audio_output_dropdown, casting_network_dropdown, default_audio_output_options,
    select_next_audio_output, select_previous_audio_output, warm_audio_output_cache,
};
pub(crate) use playback_settings::{
    crossfade_duration_row, install_sliding_value_bubble, playback_rate_row, preserve_pitch_row,
};
pub(crate) use queue::connect_queue_panel_controls;
pub(crate) use random_play::play_saved_random;
pub(crate) use right_panel::{
    apply_sidebar_media_visibility, build_right_panel, connect_queue_lyrics_overlay,
};

pub(crate) struct PlayerDesktopWidgets {
    pub(crate) fullscreen_player: FullscreenPlayerParts,
    pub(crate) player_controls: PlayerControls,
}
