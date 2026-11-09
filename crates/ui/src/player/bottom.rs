use super::icons::*;
use crate::favorites::{favorite_icon_button, set_favorite_button_active};
use crate::ratings::RatingControl;
use crate::shell::shell::Shell;
use crate::widgets::interactions::add_widget_click;
use crate::widgets::media_drag::{MediaDragSource, install_compact_media_drag_source};
use crate::widgets::{
    artwork::ArtworkTile,
    detail_links::DetailLinkBinding,
    layout::{AllocationOwner, allocation_owner},
    localization::{bind_widget_accessible_label, bind_widget_tooltip},
    scale::{
        install_pointer_value_bubble, install_scale_fill_updates, install_value_bubble_dismissal,
        point_value_bubble_at_slider, scale_value_at,
    },
    swipe::{SWIPE_ACTION_DISTANCE, SwipeAxis, install_touch_swipe},
};
use adw::prelude::*;
use gtk::glib;
use localization::{msgid, tr, tr_with};
use playback::{
    CurrentMediaId, PlaybackOutput, PlaybackView, QueueRepeatMode, RemoteOutput,
    RemoteOutputProtocol, TransportStatus,
};
use std::time::Duration;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
pub const BOTTOM_PLAYER_HEIGHT: i32 = 90;
pub const BOTTOM_PLAYER_COVER_START_INSET: i32 = 14;
pub const BOTTOM_PLAYER_COVER_SIZE: i32 = 60;
pub const BOTTOM_PLAYER_EDGE_PADDING: i32 = 8;
pub const BOTTOM_PLAYER_HORIZONTAL_PADDING: i32 =
    BOTTOM_PLAYER_COVER_START_INSET - BOTTOM_PLAYER_EDGE_PADDING;
pub const BOTTOM_PLAYER_NOW_PLAYING_SPACING: i32 = 8;
// Six side buttons and the play button, with the same gap between all of them.
pub const BOTTOM_PLAYER_TRANSPORT_WIDTH: i32 = BOTTOM_PLAYER_SIDE_BUTTON_SIZE * 6
    + BOTTOM_PLAYER_PLAY_BUTTON_SIZE
    + BOTTOM_PLAYER_BUTTON_GAP * 6;
pub const BOTTOM_PLAYER_PROGRESS_WIDTH: i32 = 320;
pub const BOTTOM_PLAYER_PROGRESS_MIN_WIDTH: i32 = 140;
pub const BOTTOM_PLAYER_BUTTON_ROW_HEIGHT: i32 = 58;
pub const BOTTOM_PLAYER_SIDE_BUTTON_SIZE: i32 = 36;
pub const BOTTOM_PLAYER_PLAY_BUTTON_SIZE: i32 = 44;
// Keeps the refill ring, 4px outside the play button, inside the button row.
pub const BOTTOM_PLAYER_BUTTON_OFFSET_Y: f64 = 3.0;
pub const BOTTOM_PLAYER_BUTTON_GAP: i32 = 8;
pub const BOTTOM_PLAYER_WAVEFORM_HEIGHT: i32 = 24;
pub const BOTTOM_PLAYER_ACTION_BUTTON_SIZE: i32 = 32;
pub const BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE: i32 = 18;
pub const BOTTOM_PLAYER_TITLE_ROW_HEIGHT: i32 = 20;
pub const BOTTOM_PLAYER_META_ROW_HEIGHT: i32 = 18;
pub const BOTTOM_PLAYER_ACTION_SPACING: i32 = 2;
pub const BOTTOM_PLAYER_ACTION_ROW_OFFSET_Y: i32 = 5;
pub const BOTTOM_PLAYER_RATING_WIDTH: i32 = 85;
pub const BOTTOM_PLAYER_RATING_HEIGHT: i32 = 24;
pub const BOTTOM_PLAYER_PROGRESS_SPACING: i32 = 6;
pub const BOTTOM_PLAYER_VOLUME_SPACING: i32 = BOTTOM_PLAYER_ACTION_SPACING;
pub const BOTTOM_PLAYER_VOLUME_SLOT_WIDTH: i32 = 95;
pub const BOTTOM_PLAYER_VOLUME_SLOT_LAYOUT_WIDTH: i32 = 90;
pub const OUTPUT_DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub const BOTTOM_PLAYER_IDENTITY_MIN_WIDTH: i32 = 85;
pub const BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH: i32 = BOTTOM_PLAYER_EDGE_PADDING * 2
    + BOTTOM_PLAYER_TRANSPORT_WIDTH
    + (BOTTOM_PLAYER_HORIZONTAL_PADDING
        + BOTTOM_PLAYER_COVER_SIZE
        + BOTTOM_PLAYER_NOW_PLAYING_SPACING
        + BOTTOM_PLAYER_IDENTITY_MIN_WIDTH)
        * 2;
pub const BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH: i32 = BOTTOM_PLAYER_ACTION_BUTTON_SIZE * 3
    + BOTTOM_PLAYER_VOLUME_SPACING * 3
    + BOTTOM_PLAYER_VOLUME_SLOT_LAYOUT_WIDTH;
pub const BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH: i32 = BOTTOM_PLAYER_EDGE_PADDING * 2
    + BOTTOM_PLAYER_TRANSPORT_WIDTH
    + BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH * 2;
pub const BOTTOM_PLAYER_COMPACT_MIN_WIDTH: i32 =
    if BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH > BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH {
        BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH
    } else {
        BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH
    };
/// Below this width the bar uses its narrow layouts.
pub const BOTTOM_PLAYER_NARROW_WIDTH: i32 = BOTTOM_PLAYER_COMPACT_MIN_WIDTH;
/// Shuffle, previous, play, next and repeat.
pub const BOTTOM_PLAYER_CONTROLS_TRANSPORT_WIDTH: i32 = BOTTOM_PLAYER_SIDE_BUTTON_SIZE * 4
    + BOTTOM_PLAYER_PLAY_BUTTON_SIZE
    + BOTTOM_PLAYER_BUTTON_GAP * 4;
/// The strip's button row: just tall enough for the play button.
const BOTTOM_PLAYER_STRIP_ROW_HEIGHT: i32 = BOTTOM_PLAYER_PLAY_BUTTON_SIZE;
/// The strip: the seek bar over one row of buttons.
pub const BOTTOM_PLAYER_STRIP_HEIGHT: i32 =
    BOTTOM_PLAYER_WAVEFORM_HEIGHT + 3 + BOTTOM_PLAYER_STRIP_ROW_HEIGHT + 4;
/// Volume and favorite beside the controls layout's seek bar, small enough to keep its height.
const BOTTOM_PLAYER_SEEK_BUTTON_SIZE: i32 = 28;
/// The narrowest seek bar the controls layout keeps beside its buttons.
const BOTTOM_PLAYER_CONTROLS_SEEK_MIN_WIDTH: i32 = 60;
/// The slider the narrow layouts' volume button opens.
const BOTTOM_PLAYER_VOLUME_POPOVER_HEIGHT: i32 = 120;
/// Volume, previous, play, next and favorite in the compact layout.
const BOTTOM_PLAYER_COMPACT_TRANSPORT_WIDTH: i32 = BOTTOM_PLAYER_SIDE_BUTTON_SIZE * 4
    + BOTTOM_PLAYER_PLAY_BUTTON_SIZE
    + BOTTOM_PLAYER_BUTTON_GAP * 4;
/// The compact layout centers its buttons on the cover.
const BOTTOM_PLAYER_COMPACT_BUTTON_CENTER_Y: f64 = BOTTOM_PLAYER_BUTTON_ROW_HEIGHT as f64 / 2.0;
/// The narrowest bar: five transport buttons over the seek bar.
pub const BOTTOM_PLAYER_CONTROLS_MIN_WIDTH: i32 =
    BOTTOM_PLAYER_EDGE_PADDING * 2 + BOTTOM_PLAYER_CONTROLS_TRANSPORT_WIDTH;
const BOTTOM_PLAYER_PROGRESS_CENTER_PERCENT: i32 = 95;
const BOTTOM_PLAYER_LAYOUT_COLUMNS: i32 = 3;
pub const VOLUME_PERSIST_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BottomPlayerLayout {
    /// The desktop row: what is playing, the transport over the seek bar, and the actions.
    Desktop,
    /// Narrow windows, such as phones: what is playing beside previous, play and next, with a thin
    /// seek bar along the top edge. The other controls stay in the fullscreen player.
    Compact,
    /// The tall mini player, and narrow windows under the fullscreen player: the transport over
    /// the seek bar, with volume and favorite at its ends. The fullscreen player shows the cover.
    Controls,
    /// The thinnest bar, for short mini players: the plain seek bar over volume, previous, play,
    /// next and repeat.
    Strip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BottomPlayerActions {
    Volume,
    Favorite,
    Queue,
}

pub struct PlayerControls {
    pub root: AllocationOwner,
    pub width: Cell<i32>,
    bar: gtk::Box,
    edge: gtk::Overlay,
    pub layout: Cell<BottomPlayerLayout>,
    pub cover: ArtworkTile,
    pub title: gtk::Label,
    pub menu_button: gtk::Button,
    pub artist: gtk::Label,
    pub artist_links: RefCell<Option<DetailLinkBinding>>,
    pub album: gtk::Label,
    pub album_links: RefCell<Option<DetailLinkBinding>>,
    pub now_playing_wall: gtk::Box,
    pub left_slot: gtk::Overlay,
    pub right_slot: gtk::Overlay,
    pub transport: gtk::Box,
    pub transport_slot: gtk::Box,
    pub transport_buttons: gtk::Fixed,
    pub random_button: gtk::Button,
    pub previous_button: gtk::Button,
    pub play_button: gtk::Button,
    pub play_icon: gtk::Image,
    pub play_ring: crate::player::refill_ring::RefillRing,
    pub next_button: gtk::Button,
    pub shuffle_button: gtk::Button,
    pub repeat_button: gtk::Button,
    pub dj_button: gtk::Button,
    pub queue_button: gtk::Button,
    pub queue_icon: gtk::Image,
    pub favorite_button: gtk::Button,
    pub rating: RatingControl,
    pub progress_row: gtk::Box,
    pub elapsed: gtk::Label,
    pub progress_stack: gtk::Stack,
    pub progress: gtk::Scale,
    pub waveform: WaveformSeekBar,
    pub waveform_key: RefCell<Option<CurrentMediaId>>,
    pub waveform_peak_count: Cell<usize>,
    pub duration: gtk::Label,
    pub actions: gtk::Overlay,
    pub action_buttons: gtk::Box,
    pub mute_button: gtk::Button,
    pub mute_icon: gtk::Image,
    pub mute_icon_state: Rc<Cell<VolumeIcon>>,
    pub volume_group: gtk::Box,
    pub volume_slot: gtk::Overlay,
    pub volume_button: gtk::MenuButton,
    pub volume_button_icon: gtk::Image,
    pub volume_popover_box: gtk::Box,
    pub volume: gtk::Scale,
    pub sleep_timer: Rc<crate::player::sleep_timer::SleepTimerControl>,
    pub settings_button: gtk::MenuButton,
    pub output_button: gtk::Button,
    pub output_icon: gtk::Image,
}

pub struct NowPlayingControls {
    pub root: gtk::Box,
    pub cover: ArtworkTile,
    pub title: gtk::Label,
    pub menu_button: gtk::Button,
    pub artist: gtk::Label,
    pub album: gtk::Label,
}

pub struct TransportControls {
    pub root: gtk::Box,
    pub buttons: gtk::Fixed,
    pub random_button: gtk::Button,
    pub previous_button: gtk::Button,
    pub play_button: gtk::Button,
    pub play_icon: gtk::Image,
    pub play_ring: crate::player::refill_ring::RefillRing,
    pub next_button: gtk::Button,
    pub shuffle_button: gtk::Button,
    pub repeat_button: gtk::Button,
    pub dj_button: gtk::Button,
    pub progress_row: gtk::Box,
    pub elapsed: gtk::Label,
    pub progress_stack: gtk::Stack,
    pub progress: gtk::Scale,
    pub waveform: WaveformSeekBar,
    pub duration: gtk::Label,
}

pub struct PlayerActionControls {
    pub root: gtk::Overlay,
    pub buttons: gtk::Box,
    pub queue_button: gtk::Button,
    pub queue_icon: gtk::Image,
    pub favorite_button: gtk::Button,
    pub rating: RatingControl,
    pub mute_button: gtk::Button,
    pub mute_icon: gtk::Image,
    pub mute_icon_state: Rc<Cell<VolumeIcon>>,
    pub volume_group: gtk::Box,
    pub volume_slot: gtk::Overlay,
    pub volume_button: gtk::MenuButton,
    pub volume_button_icon: gtk::Image,
    pub volume_popover_box: gtk::Box,
    pub volume: gtk::Scale,
    pub sleep_timer: Rc<crate::player::sleep_timer::SleepTimerControl>,
    pub settings_button: gtk::MenuButton,
    pub output_button: gtk::Button,
    pub output_icon: gtk::Image,
}

#[derive(Clone)]
pub struct WaveformSeekBar {
    pub area: gtk::DrawingArea,
    pub peaks: Rc<RefCell<Vec<(f64, f64)>>>,
    pub position: Rc<Cell<f64>>,
}

impl WaveformSeekBar {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::new();
        area.add_css_class("player-waveform");
        area.set_content_width(BOTTOM_PLAYER_PROGRESS_WIDTH);
        area.set_content_height(BOTTOM_PLAYER_WAVEFORM_HEIGHT);
        area.set_width_request(BOTTOM_PLAYER_PROGRESS_WIDTH);
        area.set_focusable(true);
        area.set_valign(gtk::Align::Center);
        let seek_label = localization::tr(msgid("Seek playback"));
        area.update_property(&[
            gtk::accessible::Property::Label(&seek_label),
            gtk::accessible::Property::ValueMin(0.0),
            gtk::accessible::Property::ValueMax(1.0),
            gtk::accessible::Property::ValueNow(0.0),
        ]);

        let peaks = Rc::new(RefCell::new(Vec::new()));
        let position = Rc::new(Cell::new(0.0));
        let hover_position = Rc::new(Cell::new(None));
        let draw_peaks = Rc::clone(&peaks);
        let draw_position = Rc::clone(&position);
        let draw_hover_position = Rc::clone(&hover_position);
        area.set_draw_func(move |area, context, width, height| {
            draw_waveform_seekbar(
                area,
                context,
                width,
                height,
                &draw_peaks.borrow(),
                draw_position.get(),
                draw_hover_position.get(),
            );
        });

        let motion = gtk::EventControllerMotion::new();
        let motion_area = area.downgrade();
        let motion_hover = Rc::clone(&hover_position);
        motion.connect_motion(move |_, x, _| {
            if let Some(area) = motion_area.upgrade() {
                motion_hover.set(Some(waveform_fraction_for_x(&area, x)));
                area.queue_draw();
            }
        });
        let leave_area = area.downgrade();
        let leave_hover = Rc::clone(&hover_position);
        motion.connect_leave(move |_| {
            leave_hover.set(None);
            if let Some(area) = leave_area.upgrade() {
                area.queue_draw();
            }
        });
        area.add_controller(motion);

        Self {
            area,
            peaks,
            position,
        }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub fn set_peaks(&self, peaks: Option<&[(f64, f64)]>) {
        let next = peaks.map(normalize_waveform_peaks).unwrap_or_default();
        *self.peaks.borrow_mut() = next;
        self.area.queue_draw();
    }

    pub fn set_position_fraction(&self, position: f64) {
        let position = position.clamp(0.0, 1.0);
        self.position.set(position);
        self.area
            .update_property(&[gtk::accessible::Property::ValueNow(position)]);
        self.area.queue_draw();
    }

    pub fn connect_seek<F, C>(&self, seek: F, commit: C)
    where
        F: Fn(f64) + 'static,
        C: Fn() + 'static,
    {
        let seek = Rc::new(seek);
        let commit = Rc::new(commit);

        let click = gtk::GestureClick::new();
        click.set_button(1);
        let click_area = self.area.clone();
        let click_seek = Rc::clone(&seek);
        click.connect_pressed(move |_, _, x, _| {
            click_area.grab_focus();
            click_seek(waveform_fraction_for_x(&click_area, x));
        });
        self.area.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let drag_area = self.area.clone();
        let drag_seek = Rc::clone(&seek);
        drag.connect_drag_begin(move |gesture, x, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            drag_area.grab_focus();
            drag_seek(waveform_fraction_for_x(&drag_area, x));
        });
        let drag_area = self.area.clone();
        let drag_seek = Rc::clone(&seek);
        drag.connect_drag_update(move |gesture, x_offset, _| {
            let Some((start_x, _)) = gesture.start_point() else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            drag_seek(waveform_fraction_for_x(&drag_area, start_x + x_offset));
        });
        self.area.add_controller(drag);

        let key = gtk::EventControllerKey::new();
        let key_position = Rc::clone(&self.position);
        let key_seek = Rc::clone(&seek);
        let key_commit = Rc::clone(&commit);
        key.connect_key_pressed(move |_, key, _, _| {
            let delta = match key {
                gtk::gdk::Key::Left => -0.02,
                gtk::gdk::Key::Right => 0.02,
                _ => return glib::Propagation::Proceed,
            };
            key_seek((key_position.get() + delta).clamp(0.0, 1.0));
            key_commit();
            glib::Propagation::Stop
        });
        self.area.add_controller(key);
    }
}

pub fn waveform_fraction_for_x(area: &gtk::DrawingArea, x: f64) -> f64 {
    let width = f64::from(area.width()).max(1.0);
    let fraction = (x / width).clamp(0.0, 1.0);
    if area.direction() == gtk::TextDirection::Rtl {
        1.0 - fraction
    } else {
        fraction
    }
}

pub fn normalize_waveform_peaks(peaks: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let max = peaks
        .iter()
        .flat_map(|(left, right)| [*left, *right])
        .filter(|value| value.is_finite())
        .fold(0.0_f64, f64::max);
    if max <= f64::EPSILON {
        return Vec::new();
    }
    peaks
        .iter()
        .filter(|(left, right)| left.is_finite() && right.is_finite())
        .map(|(left, right)| ((left / max).clamp(0.0, 1.0), (right / max).clamp(0.0, 1.0)))
        .collect()
}

pub fn draw_waveform_seekbar(
    area: &gtk::DrawingArea,
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    peaks: &[(f64, f64)],
    position: f64,
    hover_position: Option<f64>,
) {
    let width = f64::from(width.max(0));
    let height = f64::from(height.max(0));
    if width <= 0.0 || height <= 0.0 {
        return;
    }

    let is_rtl = area.direction() == gtk::TextDirection::Rtl;
    let position = visual_waveform_fraction(position, is_rtl) * width;
    let hover_position =
        hover_position.map(|value| visual_waveform_fraction(value, is_rtl) * width);
    let color = area.color();
    let center_y = height / 2.0;
    let bar_width = 2.0;
    let gap = 2.0;
    let block = bar_width + gap;
    let bar_count = ((width + gap) / block).floor().max(1.0) as usize;

    if peaks.is_empty() {
        set_waveform_source(context, &color, 0.32);
        context.rectangle(0.0, center_y - 1.0, width, 2.0);
        let _ = context.fill();
        return;
    }

    for index in 0..bar_count {
        let start = index * peaks.len() / bar_count;
        let end = ((index + 1) * peaks.len() / bar_count)
            .max(start + 1)
            .min(peaks.len());
        let samples = &peaks[start..end];
        let amplitude = samples
            .iter()
            .map(|(left, right)| (left + right) / 2.0)
            .sum::<f64>()
            / samples.len() as f64;
        let amplitude = amplitude.powf(0.72);
        let bar_height = (amplitude * height * 0.86).clamp(2.0, height);
        let x = index as f64 * block;
        let center_x = x + bar_width / 2.0;
        let opacity = waveform_bar_opacity(center_x, position, hover_position, is_rtl);
        set_waveform_source(context, &color, opacity);
        context.rectangle(x, center_y - bar_height / 2.0, bar_width, bar_height);
        let _ = context.fill();
    }
}

pub fn visual_waveform_fraction(value: f64, is_rtl: bool) -> f64 {
    let value = value.clamp(0.0, 1.0);
    if is_rtl { 1.0 - value } else { value }
}

pub fn waveform_bar_opacity(
    x: f64,
    position: f64,
    hover_position: Option<f64>,
    is_rtl: bool,
) -> f64 {
    if let Some(hover_position) = hover_position {
        let start = position.min(hover_position);
        let end = position.max(hover_position);
        if (start..=end).contains(&x) {
            return 0.58;
        }
    }
    if (!is_rtl && x <= position) || (is_rtl && x >= position) {
        1.0
    } else {
        0.28
    }
}

pub fn set_waveform_source(context: &gtk::cairo::Context, color: &gtk::gdk::RGBA, opacity: f64) {
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()) * opacity,
    );
}

pub fn build_bottom_player() -> PlayerControls {
    let resource = "player/bottom.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        root: gtk::Box,
        now_playing_wall: gtk::Box,
        transport_slot: gtk::Box,
    });
    root.set_height_request(BOTTOM_PLAYER_HEIGHT);
    transport_slot.set_width_request(BOTTOM_PLAYER_TRANSPORT_WIDTH);

    let NowPlayingControls {
        root: now_playing,
        cover,
        title,
        menu_button,
        artist,
        album,
    } = build_now_playing_controls(&builder, resource);

    let TransportControls {
        root: transport,
        buttons: transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_ring,
        next_button,
        shuffle_button,
        repeat_button,
        dj_button,
        progress_row,
        elapsed,
        progress_stack,
        progress,
        waveform,
        duration,
    } = build_transport_controls(&builder, resource);

    now_playing_wall.append(&now_playing);

    let PlayerActionControls {
        root: actions,
        buttons: action_buttons,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume_group,
        volume_slot,
        volume_button,
        volume_button_icon,
        volume_popover_box,
        volume,
        sleep_timer,
        settings_button,
        output_button,
        output_icon,
    } = build_player_action_controls(&builder, resource);

    let left_slot = bottom_player_allocated_slot(&now_playing_wall, 1, 1);
    left_slot.set_hexpand(true);
    left_slot.set_halign(gtk::Align::Fill);
    left_slot.set_valign(gtk::Align::Fill);
    let right_slot = bottom_player_allocated_slot(&actions, 1, 1);
    right_slot.set_hexpand(true);
    right_slot.set_halign(gtk::Align::Fill);
    right_slot.set_valign(gtk::Align::Fill);

    transport_slot.append(&transport);

    root.append(&left_slot);
    root.append(&transport_slot);
    root.append(&right_slot);
    // Bottom-player topology follows the real allocation. Applying it during GTK's tentative
    // height-for-width measurement lets a provisional width and the allocated width alternate,
    // continuously invalidating the shell layout.
    // The compact layout lays its seek bar over the top edge.
    let edge = gtk::Overlay::new();
    edge.set_hexpand(true);
    edge.set_valign(gtk::Align::End);
    edge.set_child(Some(&root));
    let allocation_root = allocation_owner(&edge, |_, _| {});

    PlayerControls {
        root: allocation_root,
        width: Cell::new(0),
        bar: root.clone(),
        edge,
        layout: Cell::new(BottomPlayerLayout::Desktop),
        cover,
        title,
        menu_button,
        artist,
        artist_links: RefCell::new(None),
        album,
        album_links: RefCell::new(None),
        now_playing_wall,
        left_slot,
        right_slot,
        transport,
        transport_slot,
        transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_ring,
        next_button,
        shuffle_button,
        repeat_button,
        dj_button,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        progress_row,
        elapsed,
        progress_stack,
        progress,
        waveform,
        waveform_key: RefCell::new(None),
        waveform_peak_count: Cell::new(0),
        duration,
        actions,
        action_buttons,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume_group,
        volume_slot,
        volume_button,
        volume_button_icon,
        volume_popover_box,
        volume,
        sleep_timer,
        settings_button,
        output_button,
        output_icon,
    }
}

pub fn build_now_playing_controls(builder: &gtk::Builder, resource: &str) -> NowPlayingControls {
    crate::objects!(builder, resource, {
        now_playing: gtk::Box,
        identity: gtk::Box,
        title_row: gtk::Box,
        title_slide: gtk::ScrolledWindow, title_viewport: gtk::Viewport,
        artist_slide: gtk::ScrolledWindow, artist_viewport: gtk::Viewport,
        album_slide: gtk::ScrolledWindow, album_viewport: gtk::Viewport,
        title: gtk::Label,
        menu_button: gtk::Button,
        artist: gtk::Label,
        album: gtk::Label,
    });
    now_playing.set_spacing(BOTTOM_PLAYER_NOW_PLAYING_SPACING);
    now_playing.set_margin_start(BOTTOM_PLAYER_HORIZONTAL_PADDING);

    let cover = ArtworkTile::new(BOTTOM_PLAYER_COVER_SIZE);
    cover.area.set_valign(gtk::Align::Center);
    cover.area.set_cursor_from_name(Some("pointer"));
    let cover_label = tr("Open fullscreen player");
    cover.area.set_tooltip_text(Some(&cover_label));
    cover
        .area
        .update_property(&[gtk::accessible::Property::Label(&cover_label)]);
    now_playing.append(&cover.area);

    bind_widget_accessible_label(&menu_button, "More actions");
    menu_button.set_size_request(
        BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE,
        BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE,
    );
    title_row.set_height_request(BOTTOM_PLAYER_TITLE_ROW_HEIGHT);
    crate::widgets::text_slide::install(&title_slide, &title_viewport, &title);
    crate::widgets::text_slide::install(&artist_slide, &artist_viewport, &artist);
    crate::widgets::text_slide::install(&album_slide, &album_viewport, &album);
    title.set_height_request(BOTTOM_PLAYER_TITLE_ROW_HEIGHT);
    artist.set_height_request(BOTTOM_PLAYER_META_ROW_HEIGHT);
    album.set_height_request(BOTTOM_PLAYER_META_ROW_HEIGHT);
    now_playing.append(&identity);

    NowPlayingControls {
        root: now_playing,
        cover,
        title,
        menu_button,
        artist,
        album,
    }
}

/// Takes a bar button out of the box or button row that holds it.
fn remove_from_parent(widget: &impl IsA<gtk::Widget>) {
    let Some(parent) = widget.parent() else {
        return;
    };
    if let Some(row) = parent.downcast_ref::<gtk::Fixed>() {
        row.remove(widget);
    } else if let Some(row) = parent.downcast_ref::<gtk::Box>() {
        row.remove(widget);
    }
}

/// Sets the size an allocated slot requests; its overlaid child is clipped to it.
pub fn set_allocated_slot_size(slot: &gtk::Overlay, width: i32, height: i32) {
    if let Some(measure) = slot.child() {
        measure.set_size_request(width, height);
    }
}

pub fn bottom_player_allocated_slot(
    child: &impl IsA<gtk::Widget>,
    width: i32,
    height: i32,
) -> gtk::Overlay {
    let slot = gtk::Overlay::new();
    let measure = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    measure.set_size_request(width, height);
    measure.set_accessible_role(gtk::AccessibleRole::Presentation);
    slot.set_child(Some(&measure));
    slot.add_overlay(child);
    slot.set_measure_overlay(child, false);
    slot.set_clip_overlay(child, true);
    slot
}

pub fn build_transport_controls(builder: &gtk::Builder, resource: &str) -> TransportControls {
    crate::objects!(builder, resource, {
        transport: gtk::Box,
        transport_buttons: gtk::Fixed,
        progress_row: gtk::Box,
        elapsed: gtk::Label,
        progress_stack: gtk::Stack,
        progress: gtk::Scale,
        duration: gtk::Label,
        seek_preview: gtk::Popover,
        seek_preview_time: gtk::Label,
    });
    transport.set_width_request(BOTTOM_PLAYER_TRANSPORT_WIDTH);
    transport_buttons.set_size_request(
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        BOTTOM_PLAYER_BUTTON_ROW_HEIGHT,
    );
    progress_row.set_spacing(BOTTOM_PLAYER_PROGRESS_SPACING);
    progress.set_width_request(BOTTOM_PLAYER_PROGRESS_WIDTH);
    progress_stack.set_size_request(BOTTOM_PLAYER_PROGRESS_WIDTH, BOTTOM_PLAYER_WAVEFORM_HEIGHT);
    elapsed.set_xalign(1.0);
    duration.set_xalign(0.0);
    // The labels keep their size group alive.
    let timestamp_size_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    timestamp_size_group.add_widget(&elapsed);
    timestamp_size_group.add_widget(&duration);

    let dj_button = auto_dj_icon_button("Auto DJ");
    let previous_button = skip_icon_button(false, "Previous");
    let (play_button, play_icon) = play_icon_button("Play");
    let play_ring = crate::player::refill_ring::RefillRing::new(BOTTOM_PLAYER_PLAY_BUTTON_SIZE);
    let next_button = skip_icon_button(true, "Next");
    let shuffle_button = shuffle_icon_button("Shuffle");
    let repeat_button = repeat_icon_button("Repeat off");
    let random_button = random_clover_icon_button("Play random");

    configure_transport_side_button(&dj_button);
    configure_transport_side_button(&shuffle_button);
    configure_transport_side_button(&previous_button);
    configure_play_button(&play_button);
    configure_transport_side_button(&next_button);
    configure_transport_side_button(&repeat_button);
    configure_transport_side_button(&random_button);

    put_transport_button(
        &transport_buttons,
        &dj_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -3.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &shuffle_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -2.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &previous_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -1.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &play_ring,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        0.0,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &play_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        0.0,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &next_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        1.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &repeat_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        2.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &transport_buttons,
        &random_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        3.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );

    let waveform = WaveformSeekBar::new();
    progress_stack.add_named(&progress, Some("scale"));
    progress_stack.add_named(waveform.widget(), Some("waveform"));
    progress_stack.set_visible_child(&progress);
    let weak_scale = progress.downgrade();
    install_scale_fill_updates(&progress);
    install_pointer_value_bubble(
        &progress_stack,
        seek_preview,
        seek_preview_time,
        move |widget, x, y| {
            let stack = widget.downcast_ref::<gtk::Stack>()?;
            let scale = weak_scale.upgrade()?;
            let child = stack.visible_child().filter(|child| child.is_sensitive())?;
            let point =
                stack.compute_point(&child, &gtk::graphene::Point::new(x as f32, y as f32))?;
            let (seconds, track_center_y) = if child == scale {
                let seconds = scale_value_at(&scale, f64::from(point.x()), f64::from(point.y()));
                scale.set_fill_level(seconds);
                let range = scale.range_rect();
                (seconds, range.y() as f32 + range.height() as f32 / 2.0)
            } else {
                let seconds = waveform_fraction_for_x(
                    child.downcast_ref::<gtk::DrawingArea>()?,
                    f64::from(point.x()),
                ) * scale.adjustment().upper();
                (seconds, child.height() as f32 / 2.0)
            };
            let center = child
                .compute_point(stack, &gtk::graphene::Point::new(point.x(), track_center_y))?;
            Some((
                format_duration(seconds.round() as u32),
                f64::from(center.y()),
            ))
        },
    );

    TransportControls {
        root: transport,
        buttons: transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_ring,
        next_button,
        shuffle_button,
        repeat_button,
        dj_button,
        progress_row,
        elapsed,
        progress_stack,
        progress,
        waveform,
        duration,
    }
}

pub fn build_player_action_controls(
    builder: &gtk::Builder,
    resource: &str,
) -> PlayerActionControls {
    crate::objects!(builder, resource, {
        actions: gtk::Overlay,
        action_buttons: gtk::Box,
        output_button: gtk::Button,
        output_icon: gtk::Image,
        settings_button: gtk::MenuButton,
        sleep_button: gtk::MenuButton,
        volume_group: gtk::Box,
        volume_button: gtk::MenuButton,
        volume_button_icon: gtk::Image,
        volume_popover_box: gtk::Box,
        volume: gtk::Scale,
        volume_preview: gtk::Popover,
        volume_preview_level: gtk::Label,
    });
    let root = actions;
    let buttons = action_buttons;
    buttons.set_spacing(BOTTOM_PLAYER_ACTION_SPACING);
    let rating = RatingControl::new(None, true);
    rating
        .widget()
        .set_size_request(BOTTOM_PLAYER_RATING_WIDTH, BOTTOM_PLAYER_RATING_HEIGHT);
    rating.widget().set_halign(gtk::Align::End);
    rating.widget().set_valign(gtk::Align::Start);
    rating.widget().set_margin_top(5);
    rating.widget().set_margin_end(15);
    rating.widget().set_visible(false);
    root.add_overlay(rating.widget());
    root.set_measure_overlay(rating.widget(), false);

    bind_widget_accessible_label(&output_button, msgid("Choose playback output"));
    bind_widget_tooltip(&settings_button, "Settings");
    let sleep_timer = crate::player::sleep_timer::SleepTimerControl::new(&sleep_button);
    volume_button.add_css_class("player-seek-button");
    volume_button.set_size_request(
        BOTTOM_PLAYER_ACTION_BUTTON_SIZE,
        BOTTOM_PLAYER_ACTION_BUTTON_SIZE,
    );
    bind_widget_accessible_label(&settings_button, "Settings");
    volume_group.set_spacing(BOTTOM_PLAYER_VOLUME_SPACING);
    volume_group.set_valign(gtk::Align::Center);
    let favorite_button = favorite_icon_button("Favorite");
    favorite_button.add_css_class("player-favorite-button");
    favorite_button.add_css_class("player-seek-button");
    configure_player_action_button(&favorite_button);
    volume_group.append(&favorite_button);
    let (queue_button, queue_icon) = queue_sidebar_button("Hide sidebar");
    configure_player_action_button(&queue_button);
    volume_group.append(&queue_button);
    let (mute_button, mute_icon, mute_icon_state) = volume_icon_button("Mute");
    mute_button.add_css_class("player-mute-button");
    configure_player_action_button(&mute_button);
    volume_group.append(&mute_button);
    let volume_slot = bottom_player_allocated_slot(&volume, BOTTOM_PLAYER_VOLUME_SLOT_WIDTH, 1);
    volume_slot.add_css_class("volume-slider-slot");
    volume_slot.set_valign(gtk::Align::Fill);
    volume_group.append(&volume_slot);
    install_scale_fill_updates(&volume);
    volume_preview.set_parent(&volume);
    volume_preview.set_offset(0, -4);
    volume
        .adjustment()
        .bind_property("value", &volume_preview_level, "label")
        .transform_to(|_, value: f64| Some(format!("{:.0}", value * 100.0)))
        .sync_create()
        .build();
    let preview = volume_preview.downgrade();
    volume.connect_value_changed(move |scale| {
        if let Some(preview) = preview.upgrade() {
            point_value_bubble_at_slider(scale, &preview);
        }
    });
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_volume = volume.downgrade();
    let weak_slot = volume_slot.downgrade();
    let preview = volume_preview.downgrade();
    let show_preview = Rc::new(move |x: f64, y: f64| {
        if let (Some(volume), Some(slot), Some(preview)) = (
            weak_volume.upgrade(),
            weak_slot.upgrade(),
            preview.upgrade(),
        ) && volume.is_sensitive()
        {
            if slot.contains(x, y)
                && let Some(point) =
                    slot.compute_point(&volume, &gtk::graphene::Point::new(x as f32, y as f32))
            {
                volume.set_fill_level(scale_value_at(
                    &volume,
                    f64::from(point.x()),
                    f64::from(point.y()),
                ));
            }
            point_value_bubble_at_slider(&volume, &preview);
            if !preview.is_visible() {
                preview.popup();
            }
        }
    });
    let enter_preview = Rc::clone(&show_preview);
    motion.connect_enter(move |_, x, y| enter_preview(x, y));
    motion.connect_motion(move |_, x, y| show_preview(x, y));
    install_value_bubble_dismissal(&motion, &volume_preview);
    volume_slot.add_controller(motion);
    volume.connect_destroy(move |_| volume_preview.unparent());
    PlayerActionControls {
        root,
        buttons,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume_group,
        volume_slot,
        volume_button,
        volume_button_icon,
        volume_popover_box,
        volume,
        sleep_timer,
        settings_button,
        output_button,
        output_icon,
    }
}

pub fn configure_transport_side_button(button: &gtk::Button) {
    button.add_css_class("player-transport-button");
    button.set_size_request(
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
}

pub fn configure_play_button(button: &gtk::Button) {
    button.add_css_class("player-transport-button");
    button.add_css_class("player-play-button");
    button.set_size_request(
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
}

pub fn configure_player_action_button(button: &gtk::Button) {
    button.set_valign(gtk::Align::Center);
}

pub fn bottom_player_action_count(actions: BottomPlayerActions) -> i32 {
    match actions {
        BottomPlayerActions::Volume => 0,
        BottomPlayerActions::Favorite => 1,
        BottomPlayerActions::Queue => 2,
    }
}

pub fn bottom_player_action_min_width(actions: BottomPlayerActions) -> i32 {
    let visible_action_count = bottom_player_action_count(actions);
    BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH
        + BOTTOM_PLAYER_ACTION_BUTTON_SIZE * visible_action_count
        + BOTTOM_PLAYER_ACTION_SPACING * visible_action_count
}

pub fn bottom_player_side_width(player_width: i32) -> i32 {
    bottom_player_content_width(player_width).saturating_sub(BOTTOM_PLAYER_TRANSPORT_WIDTH) / 2
}

pub fn bottom_player_content_width(player_width: i32) -> i32 {
    (player_width - BOTTOM_PLAYER_EDGE_PADDING * 2).max(1)
}

pub fn bottom_player_progress_width(player_width: i32) -> i32 {
    let extra_width = player_width
        .saturating_sub(BOTTOM_PLAYER_COMPACT_MIN_WIDTH)
        .max(0);
    // Feishin gives the seekbar 95% of its one-third center column. Apply the same
    // share to width beyond Rufin's compact layout so the bar grows smoothly.
    BOTTOM_PLAYER_PROGRESS_MIN_WIDTH
        + extra_width * BOTTOM_PLAYER_PROGRESS_CENTER_PERCENT / (100 * BOTTOM_PLAYER_LAYOUT_COLUMNS)
}

pub fn centered_progress_width(
    desired_width: i32,
    content_width: i32,
    action_width: i32,
    progress_row_fixed_width: i32,
) -> i32 {
    let symmetric_capacity = content_width
        .saturating_sub(action_width.saturating_mul(2))
        .saturating_sub(progress_row_fixed_width)
        .max(1);
    desired_width.min(symmetric_capacity)
}

pub fn widget_natural_width(widget: &impl IsA<gtk::Widget>) -> i32 {
    let (_, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    natural
}

pub fn bottom_player_actions(player_width: i32) -> BottomPlayerActions {
    let side_width = bottom_player_side_width(player_width);
    if side_width >= bottom_player_action_min_width(BottomPlayerActions::Queue) {
        BottomPlayerActions::Queue
    } else if side_width >= bottom_player_action_min_width(BottomPlayerActions::Favorite) {
        BottomPlayerActions::Favorite
    } else {
        BottomPlayerActions::Volume
    }
}

pub fn bottom_player_layout(
    player_width: i32,
    fullscreen_visible: bool,
    mini: crate::player::MiniPlayer,
) -> BottomPlayerLayout {
    use crate::player::MiniPlayer;
    match mini {
        MiniPlayer::Tall => BottomPlayerLayout::Controls,
        MiniPlayer::Short => BottomPlayerLayout::Strip,
        MiniPlayer::Off if player_width >= BOTTOM_PLAYER_NARROW_WIDTH => {
            BottomPlayerLayout::Desktop
        }
        MiniPlayer::Off if fullscreen_visible => BottomPlayerLayout::Controls,
        MiniPlayer::Off => BottomPlayerLayout::Compact,
    }
}

/// Where the button row's buttons are centered: low enough in the desktop row to leave room for the
/// seek bar under it, and in the middle of the strip's row.
const BOTTOM_PLAYER_BUTTON_CENTER_Y: f64 =
    BOTTOM_PLAYER_BUTTON_ROW_HEIGHT as f64 / 2.0 + BOTTOM_PLAYER_BUTTON_OFFSET_Y;
const BOTTOM_PLAYER_STRIP_BUTTON_CENTER_Y: f64 = BOTTOM_PLAYER_STRIP_ROW_HEIGHT as f64 / 2.0;

pub fn put_transport_button(
    buttons: &gtk::Fixed,
    button: &impl IsA<gtk::Widget>,
    width: i32,
    slot: f64,
    size: i32,
) {
    let (x, y) = transport_button_position(width, BOTTOM_PLAYER_BUTTON_CENTER_Y, slot, size);
    buttons.put(button, x, y);
}

pub fn move_transport_button(
    buttons: &gtk::Fixed,
    button: &impl IsA<gtk::Widget>,
    width: i32,
    center_y: f64,
    slot: f64,
    size: i32,
) {
    let (x, y) = transport_button_position(width, center_y, slot, size);
    buttons.move_(button, x, y);
}

/// How many extras join the controls layout's five on its button row, a pair at a time so the
/// row stays symmetric, and the gap there: the usual one, narrower where the row is tight.
fn controls_row(width: i32) -> (usize, f64) {
    let gap = |extras: i32| {
        let count = 5 + extras;
        let buttons = BOTTOM_PLAYER_SIDE_BUTTON_SIZE * (count - 1) + BOTTOM_PLAYER_PLAY_BUTTON_SIZE;
        (f64::from(width - buttons) / f64::from(count - 1)).min(f64::from(BOTTOM_PLAYER_BUTTON_GAP))
    };
    [6, 4, 2]
        .into_iter()
        .find(|extras| gap(*extras) >= 0.0)
        .map_or((0, gap(0).max(0.0)), |extras| {
            (extras as usize, gap(extras))
        })
}

pub fn transport_button_position(width: i32, center_y: f64, slot: f64, size: i32) -> (f64, f64) {
    // Edges, not centers, are spaced evenly, so the larger play button keeps the same gap.
    let gap = f64::from(BOTTOM_PLAYER_BUTTON_GAP);
    let offset = if slot == 0.0 {
        0.0
    } else {
        slot.signum()
            * (f64::from(BOTTOM_PLAYER_PLAY_BUTTON_SIZE + BOTTOM_PLAYER_SIDE_BUTTON_SIZE) / 2.0
                + gap
                + (slot.abs() - 1.0) * (f64::from(BOTTOM_PLAYER_SIDE_BUTTON_SIZE) + gap))
    };
    let center_x = f64::from(width) / 2.0 + offset;
    let radius = f64::from(size) / 2.0;
    (center_x - radius, center_y - radius)
}

use crate::player::{progress::seekbar_target_seconds, state::NowPlayingPresentation};
use crate::widgets::controls::set_active_class;
use rufin_core::route::Route;
use rufin_core::settings::presentation::format_duration;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
impl crate::player::PlayerUi {
    pub fn update_bottom_player(self: &Rc<Self>) {
        let player = self.selected_playback().as_deref().cloned();
        let presentation = NowPlayingPresentation::new(player.as_ref());
        self.update_bottom_player_view(player.as_ref(), Some(&presentation));
    }

    pub fn update_bottom_player_with(
        self: &Rc<Self>,
        player: &PlaybackView,
        presentation: &NowPlayingPresentation,
    ) {
        self.update_bottom_player_view(Some(player), Some(presentation));
    }

    pub fn update_bottom_player_transport(self: &Rc<Self>) {
        let player = self.selected_playback().as_deref().cloned();
        self.update_bottom_player_view(player.as_ref(), None);
    }

    pub fn update_bottom_player_position(&self) {
        let (position_seconds, duration_seconds) = self
            .selected_playback()
            .as_deref()
            .map(|player| {
                (
                    (player.transport.position_millis / 1_000).min(u64::from(u32::MAX)) as u32,
                    (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32,
                )
            })
            .unwrap_or_default();
        let controls = &self.views.player_controls;
        let displayed_seconds = self.seek_preview_seconds().unwrap_or(position_seconds);
        self.controls.updating_controls.set(true);
        let time_chars = (format_duration(duration_seconds).chars().count() as i32).max(4);
        controls.elapsed.set_width_chars(time_chars);
        controls.duration.set_width_chars(time_chars);
        controls
            .elapsed
            .set_text(&format_duration(displayed_seconds));
        controls
            .progress
            .set_value(f64::from(displayed_seconds.min(duration_seconds)));
        let position_fraction = if duration_seconds == 0 {
            0.0
        } else {
            f64::from(displayed_seconds.min(duration_seconds)) / f64::from(duration_seconds)
        };
        controls.waveform.set_position_fraction(position_fraction);
        self.controls.updating_controls.set(false);
    }

    fn update_bottom_player_view(
        self: &Rc<Self>,
        player: Option<&PlaybackView>,
        presentation: Option<&NowPlayingPresentation>,
    ) {
        let Some(shell) = self.shell() else {
            return;
        };
        let current = player.and_then(|player| player.transport.current.as_ref());
        let state = player
            .map(|player| player.transport.effective_state())
            .unwrap_or(TransportStatus::Stopped);
        let duration_seconds = player
            .map(|player| {
                (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32
            })
            .unwrap_or_default();
        let position_seconds = player
            .map(|player| {
                (player.transport.position_millis / 1_000).min(u64::from(u32::MAX)) as u32
            })
            .unwrap_or_default();
        let repeat_mode = player
            .map(|player| player.controls.repeat_mode)
            .unwrap_or(QueueRepeatMode::Off);
        let controls = &self.views.player_controls;
        self.controls.updating_controls.set(true);

        if let Some(presentation) = presentation {
            self.apply_bottom_now_playing(presentation);
        }

        set_play_icon(
            &controls.play_icon,
            &controls.play_ring,
            matches!(
                state,
                TransportStatus::Resolving | TransportStatus::Playing | TransportStatus::Buffering
            ),
            player.is_some_and(|player| player.transport.refilling()),
        );
        controls
            .play_button
            .set_tooltip_text(Some(&playback_state_label(state)));

        let shuffle_enabled = player.is_some_and(|player| player.controls.shuffle_enabled);
        let auto_dj_enabled = player.is_some_and(|player| player.controls.auto_dj_enabled);
        set_active_class(&controls.shuffle_button, shuffle_enabled);
        set_active_class(&controls.repeat_button, repeat_mode != QueueRepeatMode::Off);
        set_repeat_button_icon(&controls.repeat_button, repeat_mode);
        set_active_class(&controls.dj_button, auto_dj_enabled);
        controls
            .sleep_timer
            .update(player.and_then(|player| player.controls.sleep_timer));
        controls.dj_button.set_tooltip_text(Some(&tr("Auto DJ")));
        controls
            .repeat_button
            .set_tooltip_text(Some(&repeat_label(repeat_mode)));

        let time_chars = (format_duration(duration_seconds).chars().count() as i32).max(4);
        controls.elapsed.set_width_chars(time_chars);
        controls.duration.set_width_chars(time_chars);
        let preview_seconds = self.seek_preview_seconds();
        let displayed_seconds = preview_seconds.unwrap_or(position_seconds);
        controls
            .elapsed
            .set_text(&format_duration(displayed_seconds));
        let max = f64::from(duration_seconds.max(1));
        controls.progress.set_range(0.0, max);
        let can_seek =
            player.is_some_and(|player| player.transport.can_seek && duration_seconds > 0);
        controls.progress.set_sensitive(can_seek);
        controls.waveform.widget().set_sensitive(can_seek);
        controls
            .progress
            .set_value(f64::from(displayed_seconds.min(duration_seconds)));
        let position_fraction = if duration_seconds == 0 {
            0.0
        } else {
            f64::from(displayed_seconds.min(duration_seconds)) / f64::from(duration_seconds)
        };
        controls.waveform.set_position_fraction(position_fraction);
        let waveform_enabled = shell
            .settings
            .current
            .borrow()
            .player_display
            .seekbar_waveform_enabled;
        let waveform = self.waveform().unwrap_or_default();
        let current_media_id = current.map(|media| &media.id);
        let waveform_matches = waveform_enabled
            && waveform.media_id.as_ref() == current_media_id
            && waveform
                .peaks
                .as_ref()
                .is_some_and(|peaks| !peaks.is_empty());
        let waveform_key = waveform_matches
            .then(|| waveform.media_id.clone())
            .flatten();
        let waveform_peaks = waveform_matches
            .then(|| waveform.peaks.as_deref().map(Vec::as_slice))
            .flatten();
        let waveform_peak_count = waveform_peaks.map_or(0, <[_]>::len);
        let waveform_key_changed = controls.waveform_key.borrow().as_ref() != waveform_key.as_ref();
        if waveform_key_changed || controls.waveform_peak_count.get() != waveform_peak_count {
            controls.waveform.set_peaks(waveform_peaks);
            *controls.waveform_key.borrow_mut() = waveform_key;
            controls.waveform_peak_count.set(waveform_peak_count);
        }
        self.show_bottom_player_seekbar();
        controls
            .duration
            .set_text(&format_duration(duration_seconds));

        let (muted, volume) = player
            .map(|player| (player.controls.muted, player.controls.volume))
            .unwrap_or((false, 1.0));
        let volume_icon = volume_icon_state(muted, volume);
        if controls.mute_icon_state.get() != volume_icon {
            controls.mute_icon_state.set(volume_icon);
            set_volume_icon(&controls.mute_icon, volume_icon);
            set_volume_icon(&controls.volume_button_icon, volume_icon);
        }
        controls.volume.set_value(volume);
        let output = player
            .map(|player| &player.controls.playback_output)
            .cloned()
            .unwrap_or_default();
        set_output_icon_active(&controls.output_icon, !output.is_local());
        controls
            .output_button
            .set_tooltip_text(Some(&playback_output_label(&output)));
        self.controls.updating_controls.set(false);
    }

    fn apply_bottom_now_playing(self: &Rc<Self>, presentation: &NowPlayingPresentation) {
        let Some(shell) = self.shell() else {
            return;
        };
        let controls = &self.views.player_controls;
        let window_title = crate::player::state::playback_window_title(
            presentation.current.then_some(presentation.title.as_str()),
            presentation.current.then_some(presentation.artist.as_str()),
        );
        shell.chrome.window.set_title(Some(&window_title));
        if presentation.current {
            shell.artwork.bind_playback_artwork_tile(
                &controls.cover,
                presentation.artwork.clone(),
                BOTTOM_PLAYER_COVER_SIZE,
            );
        } else {
            shell.artwork.clear_artwork_tile(&controls.cover);
        }
        controls.title.set_text(&presentation.title);
        if let Some(artist_links) = controls.artist_links.borrow().as_ref() {
            artist_links.bind(presentation.artist_links.clone());
        } else {
            controls.artist.set_text(&presentation.artist);
        }
        if let Some(album_links) = controls.album_links.borrow().as_ref() {
            album_links.bind(presentation.album_links.clone());
        } else {
            controls.album.set_text(&presentation.album);
        }
        controls.title.set_sensitive(presentation.current);
        controls.menu_button.set_sensitive(presentation.current);
        controls
            .artist
            .set_sensitive(presentation.current && !presentation.artist.is_empty());
        controls
            .album
            .set_sensitive(presentation.current && !presentation.album.is_empty());
        shell
            .chrome
            .topbar
            .set_now_playing(&presentation.title, &presentation.artist);
        self.sync_bottom_player_rating();
        self.refresh_current_controls();
    }

    pub fn navigate_current_track_album(self: &Rc<Self>) {
        let Some(current) = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .cloned()
        else {
            return;
        };
        if let Some(album) = &current.row.track.links.album_media_uri {
            self.navigate(Route::AlbumDetail(album.clone()));
        }
    }

    pub fn maybe_clear_player_seek_preview(&self, player: &PlaybackView, track_changed: bool) {
        let Some(target_seconds) = self.seek_preview_seconds() else {
            return;
        };
        if should_clear_seek_preview(
            target_seconds,
            player.transport.position_millis,
            track_changed,
            player.transport.current.is_some(),
            player.transport.can_seek,
            self.controls.seek_pointer_active.get(),
        ) {
            self.clear_player_seek_preview();
        }
    }

    fn clear_player_seek_preview(&self) {
        self.controls.seek_pointer_active.set(false);
        self.set_seek_preview_seconds(None);
    }
}

fn should_clear_seek_preview(
    target_seconds: u32,
    position_millis: u64,
    track_changed: bool,
    has_current: bool,
    can_seek: bool,
    pointer_active: bool,
) -> bool {
    track_changed
        || !has_current
        || !can_seek
        || (!pointer_active
            && rufin_core::playback::seek_preview_matches_position(
                u64::from(target_seconds) * 1_000,
                position_millis,
            ))
}

fn playback_state_label(state: TransportStatus) -> String {
    match state {
        TransportStatus::Stopped | TransportStatus::Failed => tr("Play"),
        TransportStatus::Paused => tr("Resume"),
        TransportStatus::Resolving | TransportStatus::Buffering | TransportStatus::Playing => {
            tr("Pause")
        }
    }
}

fn repeat_label(repeat_mode: QueueRepeatMode) -> String {
    match repeat_mode {
        QueueRepeatMode::Off => tr("Repeat off"),
        QueueRepeatMode::One => tr("Repeat one"),
        QueueRepeatMode::All => tr("Repeat all"),
    }
}

fn playback_output_label(output: &PlaybackOutput) -> String {
    match output {
        PlaybackOutput::Local => tr("This device"),
        PlaybackOutput::Remote(output) => output.name.clone(),
    }
}

fn set_output_icon_active(icon: &gtk::Image, active: bool) {
    if active {
        icon.add_css_class("active-output");
    } else {
        icon.remove_css_class("active-output");
    }
}

fn same_remote_output(left: &RemoteOutput, right: &RemoteOutput) -> bool {
    left.protocol == right.protocol && left.id == right.id
}

fn remember_remote_output(shell: &crate::player::PlayerUi, remote: &RemoteOutput) {
    let mut cached = shell.controls.remote_output_options.borrow_mut();
    if let Some(existing) = cached
        .iter_mut()
        .find(|existing| same_remote_output(existing, remote))
    {
        *existing = remote.clone();
    } else {
        cached.push(remote.clone());
    }
}

fn replace_discovered_remote_outputs(
    cached: &mut Vec<RemoteOutput>,
    mut discovered: Vec<RemoteOutput>,
    selected: &PlaybackOutput,
) -> bool {
    if let PlaybackOutput::Remote(selected) = selected
        && !discovered
            .iter()
            .any(|output| same_remote_output(output, selected))
    {
        discovered.push(selected.clone());
    }
    if *cached == discovered {
        return false;
    }
    *cached = discovered;
    true
}

#[derive(Clone)]
struct OutputPopoverStatus {
    selection_started: Rc<Cell<bool>>,
    root: gtk::Box,
    spinner: gtk::Spinner,
    label: gtk::Label,
}

impl OutputPopoverStatus {
    fn finish_search(&self, empty: bool) {
        if self.selection_started.get() {
            return;
        }
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.label.remove_css_class("error");
        if empty {
            self.root.set_visible(true);
            self.label.set_text(&tr("No network devices found"));
        } else {
            self.root.set_visible(false);
        }
    }
}

pub fn present_output_popover(anchor: &gtk::Button, shell: &Rc<crate::player::PlayerUi>) {
    let Some(app) = shell.shell() else {
        return;
    };
    let selected = app.products.playback.playback_output();
    let popover = gtk::Popover::new();
    popover.add_css_class("playback-output-popover");
    popover.connect_show(crate::widgets::interactions::align_topbar_popup);
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_parent(anchor);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_margin_top(2);
    content.set_margin_bottom(2);
    content.set_margin_start(2);
    content.set_margin_end(2);
    content.set_width_request(300);

    let outputs = adw::PreferencesGroup::new();
    outputs.set_title(&tr("Casting"));
    let searching = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    searching.set_margin_start(8);
    let spinner = gtk::Spinner::new();
    spinner.start();
    searching.append(&spinner);
    let searching_label = gtk::Label::new(Some(&tr("Searching for devices...")));
    searching_label.add_css_class("caption");
    searching_label.add_css_class("dim-label");
    searching.append(&searching_label);
    outputs.set_header_suffix(Some(&searching));
    let status = OutputPopoverStatus {
        selection_started: Rc::new(Cell::new(false)),
        root: searching,
        spinner,
        label: searching_label,
    };
    add_output_row(
        &outputs,
        PlaybackOutput::Local,
        &selected,
        &popover,
        &status,
        shell,
    );
    content.append(&outputs);

    let mut shown = shell.controls.remote_output_options.borrow().clone();
    if let PlaybackOutput::Remote(selected) = &selected
        && !shown
            .iter()
            .any(|output| same_remote_output(output, selected))
    {
        shown.push(selected.clone());
    }
    let mut remote_rows = Vec::new();
    for output in &shown {
        let row = add_output_row(
            &outputs,
            PlaybackOutput::Remote(output.clone()),
            &selected,
            &popover,
            &status,
            shell,
        );
        remote_rows.push(row);
    }
    let transport = app.products.playback.clone();
    let runtime = app.products.runtime.clone();
    let weak_popover = popover.downgrade();
    let weak_shell = Rc::downgrade(shell);
    let discovery = gtk::glib::spawn_future_local(async move {
        loop {
            let transport = transport.clone();
            let Ok(result) = runtime
                .spawn_blocking(move || transport.discover_remote_outputs())
                .await
            else {
                break;
            };
            {
                let (Some(popover), Some(shell)) = (weak_popover.upgrade(), weak_shell.upgrade())
                else {
                    break;
                };
                if !popover.is_visible() {
                    break;
                }
                match result {
                    Ok(discovered) => {
                        status.finish_search(discovered.is_empty());
                        if replace_discovered_remote_outputs(&mut shown, discovered, &selected) {
                            for row in remote_rows.drain(..) {
                                outputs.remove(&row);
                            }
                            for output in &shown {
                                remote_rows.push(add_output_row(
                                    &outputs,
                                    PlaybackOutput::Remote(output.clone()),
                                    &selected,
                                    &popover,
                                    &status,
                                    &shell,
                                ));
                            }
                            shell.controls.remote_output_options.replace(shown.clone());
                        }
                    }
                    Err(error) => {
                        if !status.selection_started.get() {
                            status.spinner.stop();
                            status.spinner.set_visible(false);
                            status.root.set_visible(true);
                            status.label.add_css_class("error");
                            status.label.set_text(&error);
                        }
                    }
                }
            }
            gtk::glib::timeout_future(OUTPUT_DISCOVERY_REFRESH_INTERVAL).await;
        }
        status.finish_search(shown.is_empty());
    });

    popover.set_child(Some(&content));
    popover.connect_closed(|popover| popover.unparent());
    let _ = popover.add_weak_ref_notify_local(move || discovery.abort());
    popover.popup();
}

fn add_output_row(
    group: &adw::PreferencesGroup,
    output: PlaybackOutput,
    selected: &PlaybackOutput,
    popover: &gtk::Popover,
    status: &OutputPopoverStatus,
    shell: &Rc<crate::player::PlayerUi>,
) -> adw::ActionRow {
    let title = match &output {
        PlaybackOutput::Local => glib::markup_escape_text(&playback_output_label(&output)),
        PlaybackOutput::Remote(remote) => {
            let name = glib::markup_escape_text(&remote.name);
            let protocol = glib::markup_escape_text(&match remote.protocol {
                RemoteOutputProtocol::Upnp => tr("UPnP"),
                RemoteOutputProtocol::GoogleCast => tr("Google Cast"),
                RemoteOutputProtocol::PlexCompanion => "Plex Companion".into(),
                RemoteOutputProtocol::RufinConnect => tr("Rufin Connect"),
            });
            format!("{name}  <span size=\"small\" alpha=\"55%\">{protocol}</span>").into()
        }
    };
    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(true)
        .activatable(true)
        .build();
    row.add_css_class("playback-output-row");
    if matches!(&output, PlaybackOutput::Remote(remote)
        if remote.protocol == RemoteOutputProtocol::RufinConnect)
    {
        row.add_prefix(&gtk::Image::from_icon_name("rufin-phonelink-symbolic"));
    }
    if &output == selected {
        row.add_css_class("selected-output");
        row.add_suffix(&gtk::Image::from_icon_name("rufin-object-select-symbolic"));
    }
    let row_output = output.clone();
    let row_group = group.downgrade();
    let row_popover = popover.downgrade();
    let row_status = status.clone();
    let row_shell = Rc::downgrade(shell);
    row.connect_activated(move |_| {
        let Some(row_shell) = row_shell.upgrade() else {
            return;
        };
        let Some(row_group) = row_group.upgrade() else {
            return;
        };
        let Some(row_popover) = row_popover.upgrade() else {
            return;
        };
        select_output_async(
            &row_shell,
            &row_group,
            &row_popover,
            &row_status,
            row_output.clone(),
        );
    });
    group.add(&row);
    row
}

fn select_output_async(
    shell: &Rc<crate::player::PlayerUi>,
    outputs: &adw::PreferencesGroup,
    popover: &gtk::Popover,
    status: &OutputPopoverStatus,
    output: PlaybackOutput,
) {
    let Some(app) = shell.shell() else {
        return;
    };
    outputs.set_sensitive(false);
    status.selection_started.set(true);
    status.root.set_visible(true);
    status.spinner.set_visible(true);
    status.spinner.start();
    status.label.remove_css_class("error");
    let device = playback_output_label(&output);
    status.label.set_text(&tr_with(
        "Connecting to {device}...",
        &[("device", device.as_str())],
    ));
    let transport = app.products.playback.clone();
    let selected = output.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let closing = Arc::clone(&cancelled);
    popover.connect_closed(move |_| {
        closing.store(true, Ordering::Release);
    });
    let destroying = Arc::clone(&cancelled);
    let _ = popover.add_weak_ref_notify_local(move || {
        destroying.store(true, Ordering::Release);
    });
    let selection_cancelled = Arc::clone(&cancelled);
    let selection = app
        .products
        .runtime
        .spawn_blocking(move || transport.select_playback_output(selected, cancelled));
    let shell = Rc::downgrade(shell);
    let outputs = outputs.downgrade();
    let popover = popover.downgrade();
    let spinner = status.spinner.downgrade();
    let status_label = status.label.downgrade();
    let fail_connection = {
        let spinner = spinner.clone();
        let status_label = status_label.clone();
        move || {
            if let Some(spinner) = spinner.upgrade() {
                spinner.stop();
                spinner.set_visible(false);
            }
            if let Some(label) = status_label.upgrade() {
                label.add_css_class("error");
                label.set_text(&tr("Connection failed"));
            }
        }
    };
    gtk::glib::spawn_future_local(async move {
        let result = selection.await;
        if selection_cancelled.load(Ordering::Acquire) {
            return;
        }
        if let Some(outputs) = outputs.upgrade() {
            outputs.set_sensitive(true);
        }
        match result {
            Ok(Ok(())) => {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                if let PlaybackOutput::Remote(remote) = &output {
                    remember_remote_output(&shell, remote);
                }
                set_output_icon_active(
                    &shell.views.player_controls.output_icon,
                    !output.is_local(),
                );
                shell
                    .views
                    .player_controls
                    .output_button
                    .set_tooltip_text(Some(&playback_output_label(&output)));
                let message = match &output {
                    PlaybackOutput::Local => tr("Playing on this device"),
                    PlaybackOutput::Remote(output) => {
                        tr_with("Playing on {device}", &[("device", output.name.as_str())])
                    }
                };
                shell.control_feedback.show_feedback_toast(message.clone());
                if let Some(spinner) = spinner.upgrade() {
                    spinner.stop();
                }
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            }
            result => {
                fail_connection();
                if let Ok(Err(error)) = result
                    && let Some(shell) = shell.upgrade()
                {
                    shell.control_feedback.show_feedback_toast(error);
                }
            }
        }
    });
}

pub fn preview_player_seek(shell: &Rc<crate::player::PlayerUi>, seconds: u32) {
    shell.set_seek_preview_seconds(Some(seconds));
    shell
        .views
        .player_controls
        .progress
        .set_value(f64::from(seconds));
    shell
        .views
        .player_controls
        .elapsed
        .set_text(&format_duration(seconds));
    let duration_seconds = shell
        .selected_playback()
        .as_deref()
        .map(|player| (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32)
        .unwrap_or_default();
    let position = if duration_seconds == 0 {
        0.0
    } else {
        f64::from(seconds.min(duration_seconds)) / f64::from(duration_seconds)
    };
    shell
        .views
        .player_controls
        .waveform
        .set_position_fraction(position);
}

pub fn preview_player_seek_fraction(shell: &Rc<crate::player::PlayerUi>, position: f64) {
    let duration_seconds = {
        let player = shell.selected_playback();
        let Some(player) = player.as_ref() else {
            return;
        };
        let duration_seconds =
            (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32;
        if player.transport.current.is_none() || duration_seconds == 0 {
            return;
        }
        duration_seconds
    };
    let seconds = seekbar_target_seconds(position * f64::from(duration_seconds), duration_seconds);
    preview_player_seek(shell, seconds);
}

pub fn commit_player_seek_preview_now(shell: &Rc<crate::player::PlayerUi>) {
    let Some(app) = shell.shell() else {
        return;
    };
    let Some(seconds) = shell.seek_preview_seconds() else {
        return;
    };
    app.products.playback.seek_seconds(seconds);
}

impl crate::player::PlayerUi {
    pub fn apply_user_volume(self: &Rc<Self>, volume: f64) {
        let Some(shell) = self.shell() else {
            return;
        };
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        shell.products.playback.set_volume(volume);
        let local = self
            .selected_playback()
            .as_deref()
            .map(|player| player.controls.playback_output.is_local())
            .unwrap_or_else(|| shell.products.playback.playback_output().is_local());
        if !local {
            return;
        }
        if let Some(source) = self.controls.volume_persist_source.borrow_mut().take() {
            source.remove();
        }
        let shell = Rc::clone(self);
        let source = glib::timeout_add_local_once(VOLUME_PERSIST_DELAY, move || {
            let Some(app) = shell.shell() else {
                return;
            };
            *shell.controls.volume_persist_source.borrow_mut() = None;
            app.products.playback.persist_volume(volume);
        });
        *self.controls.volume_persist_source.borrow_mut() = Some(source);
    }
}

pub fn connect_bottom_player_scale(shell: &Rc<crate::player::PlayerUi>) {
    let Some(app) = shell.shell() else {
        return;
    };
    {
        let window = app.chrome.window.clone();
        let weak = Rc::downgrade(shell);
        let watch_scale = move |window: &gtk::ApplicationWindow| {
            if let Some(surface) = window.surface() {
                let weak = weak.clone();
                surface.connect_scale_notify(move |_| {
                    if let Some(shell) = weak.upgrade() {
                        shell.update_bottom_player();
                    }
                });
            }
        };
        if window.is_realized() {
            watch_scale(&window);
        }
        window.connect_realize(watch_scale);
    }
}

impl crate::player::PlayerUi {
    /// Arranges the bar for its size, the fullscreen player and the mini player.
    pub fn apply_bottom_player_layout(&self) {
        let player = &self.views.player_controls;
        let width = player.width.get();
        if width <= 0 {
            return;
        }
        let layout = bottom_player_layout(width, self.fullscreen_player_visible(), self.mini.get());
        player.layout.set(layout);
        let desktop = layout == BottomPlayerLayout::Desktop;
        let compact = layout == BottomPlayerLayout::Compact;
        let strip = layout == BottomPlayerLayout::Strip;
        player.bar.set_height_request(if strip {
            BOTTOM_PLAYER_STRIP_HEIGHT
        } else {
            BOTTOM_PLAYER_HEIGHT
        });
        // Only the desktop row and the compact layout show what is playing; the fullscreen player
        // and the mini players show it themselves.
        player.left_slot.set_visible(desktop || compact);
        // The controls layout and the strip keep volume and favorite next to their controls, and
        // the compact layout leaves them to the fullscreen player, so only the desktop row keeps
        // the action surface.
        player.right_slot.set_visible(desktop);
        if desktop {
            self.apply_bottom_player_actions(bottom_player_actions(width));
        } else {
            player.favorite_button.set_visible(matches!(
                layout,
                BottomPlayerLayout::Controls | BottomPlayerLayout::Compact
            ));
            player.queue_button.set_visible(false);
            player.sleep_timer.set_spare_width(0);
        }
        self.apply_bottom_player_volume_popover(!desktop);
        self.sync_bottom_player_rating();

        let transport_width = match layout {
            BottomPlayerLayout::Desktop => BOTTOM_PLAYER_TRANSPORT_WIDTH,
            BottomPlayerLayout::Compact => BOTTOM_PLAYER_COMPACT_TRANSPORT_WIDTH,
            BottomPlayerLayout::Controls | BottomPlayerLayout::Strip => {
                bottom_player_content_width(width).max(BOTTOM_PLAYER_CONTROLS_TRANSPORT_WIDTH)
            }
        };
        let (row_height, center_y) = if strip {
            (
                BOTTOM_PLAYER_STRIP_ROW_HEIGHT,
                BOTTOM_PLAYER_STRIP_BUTTON_CENTER_Y,
            )
        } else if compact {
            (
                BOTTOM_PLAYER_BUTTON_ROW_HEIGHT,
                BOTTOM_PLAYER_COMPACT_BUTTON_CENTER_Y,
            )
        } else {
            (
                BOTTOM_PLAYER_BUTTON_ROW_HEIGHT,
                BOTTOM_PLAYER_BUTTON_CENTER_Y,
            )
        };
        player.transport.set_width_request(transport_width);
        player.transport_slot.set_width_request(transport_width);
        player
            .transport_buttons
            .set_size_request(transport_width, row_height);
        player.now_playing_wall.set_width_request(1);
        player.transport_slot.set_hexpand(!desktop && !compact);
        let (up, gap) = if layout == BottomPlayerLayout::Controls {
            controls_row(transport_width)
        } else {
            (0, 0.0)
        };
        self.place_bottom_player_seek_buttons(layout, up);
        let controls = layout == BottomPlayerLayout::Controls;
        let all_buttons =
            controls || !strip && !compact && transport_width >= BOTTOM_PLAYER_TRANSPORT_WIDTH;
        let side = BOTTOM_PLAYER_SIDE_BUTTON_SIZE;
        for (button, slot, visible) in [
            (
                player.dj_button.upcast_ref::<gtk::Widget>(),
                -3.0,
                all_buttons,
            ),
            (player.shuffle_button.upcast_ref(), -2.0, !strip && !compact),
            (player.volume_button.upcast_ref(), -2.0, strip || compact),
            (player.favorite_button.upcast_ref(), 2.0, compact),
            (player.previous_button.upcast_ref(), -1.0, true),
            (player.next_button.upcast_ref(), 1.0, true),
            (player.repeat_button.upcast_ref(), 2.0, !compact),
            (player.random_button.upcast_ref(), 3.0, all_buttons),
        ] {
            if button.parent().as_ref() != Some(player.transport_buttons.upcast_ref()) {
                continue;
            }
            button.set_visible(visible);
            if visible {
                move_transport_button(
                    &player.transport_buttons,
                    button,
                    transport_width,
                    center_y,
                    slot,
                    side,
                );
            }
        }
        let play = BOTTOM_PLAYER_PLAY_BUTTON_SIZE;
        for button in [
            player.play_ring.upcast_ref::<gtk::Widget>(),
            player.play_button.upcast_ref(),
        ] {
            move_transport_button(
                &player.transport_buttons,
                button,
                transport_width,
                center_y,
                0.0,
                play,
            );
        }
        // The controls layout's row: its five and the extras that went up, centered and evenly
        // apart, in the seek bar's row's order. The extras that stayed down show there.
        if controls {
            let extras = self.controls_extras();
            for button in &extras {
                button.set_visible(true);
            }
            let [dj, random, sleep, settings, volume, favorite] = &extras;
            let up_here = |button: &gtk::Widget| {
                extras[..up]
                    .contains(button)
                    .then(|| (button.clone(), side))
            };
            let row: Vec<(gtk::Widget, i32)> = [sleep, volume, dj]
                .into_iter()
                .filter_map(|button| up_here(button))
                .chain([
                    (player.shuffle_button.clone().upcast(), side),
                    (player.previous_button.clone().upcast(), side),
                    (player.play_button.clone().upcast(), play),
                    (player.next_button.clone().upcast(), side),
                    (player.repeat_button.clone().upcast(), side),
                ])
                .chain(
                    [random, favorite, settings]
                        .into_iter()
                        .filter_map(|button| up_here(button)),
                )
                .collect();
            let gaps = (row.len() - 1) as f64;
            let row_width = row.iter().map(|(_, size)| f64::from(*size)).sum::<f64>() + gaps * gap;
            let mut center = (f64::from(transport_width) - row_width + f64::from(side)) / 2.0;
            let mut previous_radius = None;
            for (button, size) in &row {
                let radius = f64::from(*size) / 2.0;
                if let Some(previous) = previous_radius {
                    center += previous + gap + radius;
                }
                previous_radius = Some(radius);
                button.set_visible(true);
                let (x, y) = (center - radius, center_y - radius);
                player.transport_buttons.move_(button, x, y);
                if button == player.play_button.upcast_ref::<gtk::Widget>() {
                    player.transport_buttons.move_(&player.play_ring, x, y);
                }
            }
        }

        self.place_bottom_player_progress(compact);
        // The seek bar's row keeps its buttons: the seek bar and its times give way to them.
        let seek_bar = !controls || {
            let down = 6 - up as i32;
            let spacing = BOTTOM_PLAYER_PROGRESS_SPACING;
            transport_width
                >= down * (BOTTOM_PLAYER_SEEK_BUTTON_SIZE + spacing)
                    + widget_natural_width(&player.elapsed)
                    + widget_natural_width(&player.duration)
                    + 2 * spacing
                    + BOTTOM_PLAYER_CONTROLS_SEEK_MIN_WIDTH
        };
        player.progress_stack.set_visible(seek_bar);
        player.elapsed.set_visible(seek_bar && !compact);
        player.duration.set_visible(seek_bar && !compact);
        // The strip puts the seek bar above its single row of buttons.
        if strip {
            player.progress_row.add_css_class("above-controls");
            player
                .transport
                .reorder_child_after(&player.progress_row, None::<&gtk::Widget>);
        } else if !compact {
            player.progress_row.remove_css_class("above-controls");
            player
                .transport
                .reorder_child_after(&player.progress_row, Some(&player.transport_buttons));
        }
        self.show_bottom_player_seekbar();
        player.actions.set_visible(desktop);
        // The desktop row sizes its seek bar from the window. The controls layout and the strip
        // stretch it across the transport's width, between the times and the buttons beside it.
        player.progress_row.set_halign(if desktop || !seek_bar {
            gtk::Align::Center
        } else {
            gtk::Align::Fill
        });
        player.progress_stack.set_hexpand(!desktop);
        let progress_width = if desktop {
            // Keep the action surface inside its equal side allocation. The progress row yields
            // when necessary so the two side slots remain the same width around the transport.
            centered_progress_width(
                bottom_player_progress_width(width),
                bottom_player_content_width(width),
                widget_natural_width(&player.actions),
                widget_natural_width(&player.elapsed)
                    + widget_natural_width(&player.duration)
                    + BOTTOM_PLAYER_PROGRESS_SPACING * 2,
            )
        } else {
            1
        };
        player.progress.set_width_request(progress_width);
        player.progress_stack.set_size_request(
            progress_width,
            if compact {
                -1
            } else {
                BOTTOM_PLAYER_WAVEFORM_HEIGHT
            },
        );
        player.waveform.widget().set_width_request(progress_width);
        player.waveform.widget().set_content_width(progress_width);
    }

    /// The controls layout's buttons beside its five: the order they go up to the button row in,
    /// a pair at a time as the width allows. The rest sit at the ends of the seek bar's row.
    fn controls_extras(&self) -> [gtk::Widget; 6] {
        let player = &self.views.player_controls;
        [
            player.dj_button.clone().upcast(),
            player.random_button.clone().upcast(),
            player.sleep_timer.button().clone().upcast(),
            player.settings_button.clone().upcast(),
            player.volume_button.clone().upcast(),
            player.favorite_button.clone().upcast(),
        ]
    }

    /// Volume and favorite sit in the action surface on the desktop row, beside previous and next
    /// in the compact layout, and the strip puts volume in shuffle's place. The controls layout hides no button: the first `up` of its extras join
    /// its button row and the rest sit at the ends of the seek bar's row.
    fn place_bottom_player_seek_buttons(&self, layout: BottomPlayerLayout, up: usize) {
        let player = &self.views.player_controls;
        let controls = layout == BottomPlayerLayout::Controls;
        let transport_buttons = player.transport_buttons.upcast_ref::<gtk::Widget>();
        let extras = self.controls_extras();
        let [dj, random, sleep, settings, volume, favorite] = &extras;
        for (index, button) in extras.iter().enumerate() {
            let host: &gtk::Widget = if controls && index < up {
                transport_buttons
            } else if controls {
                player.progress_row.upcast_ref()
            } else if button == dj
                || button == random
                || button == volume && layout == BottomPlayerLayout::Strip
                || (button == volume || button == favorite) && layout == BottomPlayerLayout::Compact
            {
                transport_buttons
            } else if button == favorite {
                player.volume_group.upcast_ref()
            } else {
                player.action_buttons.upcast_ref()
            };
            if button.parent().as_ref() == Some(host) {
                continue;
            }
            remove_from_parent(button);
            let size = if host == transport_buttons {
                player.transport_buttons.put(button, 0.0, 0.0);
                BOTTOM_PLAYER_SIDE_BUTTON_SIZE
            } else if controls {
                player.progress_row.append(button);
                BOTTOM_PLAYER_SEEK_BUTTON_SIZE
            } else if button == favorite {
                player.volume_group.prepend(button);
                -1
            } else if button == volume {
                player.action_buttons.prepend(button);
                BOTTOM_PLAYER_ACTION_BUTTON_SIZE
            } else {
                // The desktop row: the sleep timer, then settings, after volume.
                let after = if button == settings { sleep } else { volume };
                let after = (after.parent().as_ref() == Some(host)).then_some(after);
                player.action_buttons.insert_child_after(button, after);
                -1
            };
            button.set_size_request(size, size);
        }
        if !controls {
            return;
        }
        // Both rows keep one order: the sleep timer, volume and Auto DJ before the times, Play
        // random, favorite and settings after them.
        let row = player.progress_row.upcast_ref::<gtk::Widget>();
        let mut previous: Option<&gtk::Widget> = None;
        for button in [sleep, volume, dj] {
            if button.parent().as_ref() == Some(row) {
                player.progress_row.reorder_child_after(button, previous);
                previous = Some(button);
            }
        }
        let mut previous = player.duration.upcast_ref::<gtk::Widget>();
        for button in [random, favorite, settings] {
            if button.parent().as_ref() == Some(row) {
                player
                    .progress_row
                    .reorder_child_after(button, Some(previous));
                previous = button;
            }
        }
    }

    /// The compact layout moves the seek bar out from under its buttons to the bar's top edge,
    /// thin, full width and without the times.
    fn place_bottom_player_progress(&self, compact: bool) {
        let player = &self.views.player_controls;
        player.elapsed.set_visible(!compact);
        player.duration.set_visible(!compact);
        // The waveform's height would hold the plain bar open.
        player.progress_stack.set_vhomogeneous(!compact);
        player.progress_row.set_valign(if compact {
            gtk::Align::Start
        } else {
            gtk::Align::Center
        });
        let on_edge = player.progress_row.parent().as_ref() == Some(player.edge.upcast_ref());
        if on_edge == compact {
            return;
        }
        if compact {
            player.transport.remove(&player.progress_row);
            player.progress_row.add_css_class("edge");
            player.edge.add_overlay(&player.progress_row);
        } else {
            player.edge.remove_overlay(&player.progress_row);
            player.progress_row.remove_css_class("edge");
            player.transport.append(&player.progress_row);
        }
    }

    /// The seek bar shows the waveform when one is ready, except in the strip and the compact
    /// layout's edge, which keep the plain bar.
    fn show_bottom_player_seekbar(&self) {
        let controls = &self.views.player_controls;
        if controls.waveform_key.borrow().is_some()
            && !matches!(
                controls.layout.get(),
                BottomPlayerLayout::Strip | BottomPlayerLayout::Compact
            )
        {
            controls
                .progress_stack
                .set_visible_child(controls.waveform.widget());
        } else {
            controls
                .progress_stack
                .set_visible_child(&controls.progress);
        }
    }

    /// Outside the desktop row the volume button opens the slider, standing upright above the mute
    /// button, in a popover.
    fn apply_bottom_player_volume_popover(&self, popover: bool) {
        let player = &self.views.player_controls;
        let in_popover =
            player.volume_group.parent().as_ref() == Some(player.volume_popover_box.upcast_ref());
        if in_popover == popover {
            return;
        }
        if let Some(parent) = player.volume_group.parent().and_downcast::<gtk::Box>() {
            parent.remove(&player.volume_group);
        }
        let orientation = if popover {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        };
        player.volume_button.set_visible(popover);
        player.volume_group.set_orientation(orientation);
        player.volume.set_orientation(orientation);
        player.volume.set_inverted(popover);
        player.volume.set_hexpand(!popover);
        player.volume.set_vexpand(popover);
        if popover {
            player.volume_slot.add_css_class("volume-column");
            player.volume.set_halign(gtk::Align::Center);
            player.volume.set_valign(gtk::Align::Fill);
            player
                .volume_group
                .reorder_child_after(&player.volume_slot, None::<&gtk::Widget>);
            set_allocated_slot_size(
                &player.volume_slot,
                BOTTOM_PLAYER_ACTION_BUTTON_SIZE,
                BOTTOM_PLAYER_VOLUME_POPOVER_HEIGHT,
            );
            player.volume_popover_box.append(&player.volume_group);
        } else {
            player.volume_slot.remove_css_class("volume-column");
            player.volume.set_halign(gtk::Align::Fill);
            player.volume.set_valign(gtk::Align::Center);
            player
                .volume_group
                .reorder_child_after(&player.volume_slot, Some(&player.mute_button));
            set_allocated_slot_size(&player.volume_slot, BOTTOM_PLAYER_VOLUME_SLOT_WIDTH, 1);
            player.action_buttons.append(&player.volume_group);
        }
    }

    fn sync_bottom_player_rating(&self) {
        let Some(shell) = self.shell() else {
            return;
        };
        let controls = &self.views.player_controls;
        let visible = controls.layout.get() == BottomPlayerLayout::Desktop
            && shell.settings.current.borrow().show_bottom_bar_rating
            && self
                .selected_playback()
                .is_some_and(|player| player.transport.current.is_some());
        controls.rating.widget().set_visible(visible);
        controls.rating.widget().set_sensitive(visible);
        controls.action_buttons.set_margin_top(if visible {
            BOTTOM_PLAYER_ACTION_ROW_OFFSET_Y * 2
        } else {
            0
        });
    }

    /// A sideways swipe over what is playing steps through the queue like the skip buttons.
    pub fn swipe_track(&self, dx: f64) {
        let controls = &self.views.player_controls;
        if (dx < 0.0) != (controls.root.direction() == gtk::TextDirection::Rtl) {
            controls.next_button.emit_clicked();
        } else {
            controls.previous_button.emit_clicked();
        }
    }

    fn apply_bottom_player_actions(&self, actions: BottomPlayerActions) {
        let player = &self.views.player_controls;
        player.favorite_button.set_visible(matches!(
            actions,
            BottomPlayerActions::Favorite | BottomPlayerActions::Queue
        ));
        player
            .queue_button
            .set_visible(matches!(actions, BottomPlayerActions::Queue));
        player.sleep_timer.set_spare_width(
            bottom_player_side_width(player.width.get()) - bottom_player_action_min_width(actions),
        );
    }
}

#[cfg(test)]
mod tests {
    use playback::{PlaybackOutput, RemoteOutput, RemoteOutputProtocol};

    fn remote_output(id: &str) -> RemoteOutput {
        RemoteOutput {
            id: id.to_string(),
            name: id.to_string(),
            protocol: RemoteOutputProtocol::Upnp,
        }
    }

    #[test]
    fn discovery_replaces_stale_devices_but_keeps_the_selected_output() {
        let kodi = remote_output("kodi");
        let mut cached = vec![kodi.clone()];

        super::replace_discovered_remote_outputs(&mut cached, Vec::new(), &PlaybackOutput::Local);
        assert!(cached.is_empty());

        super::replace_discovered_remote_outputs(
            &mut cached,
            Vec::new(),
            &PlaybackOutput::Remote(kodi.clone()),
        );
        assert_eq!(cached, vec![kodi]);
    }

    #[test]
    fn bottom_player_actions_never_outgrow_their_equal_side() {
        for player_width in super::BOTTOM_PLAYER_NARROW_WIDTH..=4096 {
            let actions = super::bottom_player_actions(player_width);
            let action_width = super::bottom_player_action_min_width(actions);

            assert!(
                action_width <= super::bottom_player_side_width(player_width),
                "actions outgrew their side at player width {player_width}"
            );
        }
    }

    #[test]
    fn desktop_player_preserves_now_playing_identity_width() {
        let fixed_now_playing_width = super::BOTTOM_PLAYER_HORIZONTAL_PADDING
            + super::BOTTOM_PLAYER_COVER_SIZE
            + super::BOTTOM_PLAYER_NOW_PLAYING_SPACING;
        let identity_width = super::bottom_player_side_width(super::BOTTOM_PLAYER_NARROW_WIDTH)
            - fixed_now_playing_width;

        assert!(identity_width >= super::BOTTOM_PLAYER_IDENTITY_MIN_WIDTH);
        let off = crate::player::MiniPlayer::Off;
        assert_eq!(
            super::bottom_player_layout(super::BOTTOM_PLAYER_NARROW_WIDTH - 1, false, off),
            super::BottomPlayerLayout::Compact
        );
        assert_eq!(
            super::bottom_player_layout(super::BOTTOM_PLAYER_NARROW_WIDTH, false, off),
            super::BottomPlayerLayout::Desktop
        );
    }

    #[test]
    fn bottom_player_actions_use_the_available_side_width() {
        let tiers = [
            (
                super::BottomPlayerActions::Favorite,
                super::BottomPlayerActions::Volume,
            ),
            (
                super::BottomPlayerActions::Queue,
                super::BottomPlayerActions::Favorite,
            ),
        ];

        for (tier, previous_tier) in tiers {
            let threshold = super::BOTTOM_PLAYER_EDGE_PADDING * 2
                + super::BOTTOM_PLAYER_TRANSPORT_WIDTH
                + super::bottom_player_action_min_width(tier) * 2;

            assert_eq!(super::bottom_player_actions(threshold - 1), previous_tier);
            assert_eq!(super::bottom_player_actions(threshold), tier);
        }
    }

    #[test]
    fn bottom_player_progress_width_scales_with_the_center_column() {
        assert_eq!(
            super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH - 50),
            super::BOTTOM_PLAYER_PROGRESS_MIN_WIDTH
        );
        assert_eq!(
            super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH),
            super::BOTTOM_PLAYER_PROGRESS_MIN_WIDTH
        );
        assert_eq!(
            super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH + 300),
            super::BOTTOM_PLAYER_PROGRESS_MIN_WIDTH + 95
        );
        assert!(
            super::bottom_player_progress_width(1024)
                > super::bottom_player_progress_width(super::BOTTOM_PLAYER_COMPACT_MIN_WIDTH)
        );
        assert!(
            super::bottom_player_progress_width(1920) > super::bottom_player_progress_width(1024)
        );
    }

    #[test]
    fn centered_progress_budget_preserves_both_side_allocations() {
        for desired_width in (1..=512).step_by(17) {
            for action_width in (0..=320).step_by(11) {
                for fixed_width in (0..=128).step_by(7) {
                    for available_progress in [1, desired_width / 2 + 1, desired_width, 640] {
                        let content_width = action_width * 2 + fixed_width + available_progress;
                        let progress_width = super::centered_progress_width(
                            desired_width,
                            content_width,
                            action_width,
                            fixed_width,
                        );

                        assert!(progress_width <= desired_width);
                        assert!(
                            progress_width + fixed_width + action_width * 2 <= content_width,
                            "center claimed side allocation: content={content_width}, actions={action_width}, fixed={fixed_width}, desired={desired_width}, progress={progress_width}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn player_volume_uses_the_matching_symbolic_icon() {
        assert_eq!(
            super::volume_icon_state(false, 0.0),
            super::VolumeIcon::Muted
        );
        assert_eq!(
            super::volume_icon_state(false, 0.01),
            super::VolumeIcon::Low
        );
        assert_eq!(
            super::volume_icon_state(false, 0.5),
            super::VolumeIcon::Medium
        );
        assert_eq!(
            super::volume_icon_state(false, 0.9),
            super::VolumeIcon::High
        );
        assert_eq!(
            super::volume_icon_state(true, 1.0),
            super::VolumeIcon::Muted
        );
    }

    #[test]
    fn seek_preview_waits_for_backend_acknowledgement() {
        assert!(!super::should_clear_seek_preview(
            19, 5_000, false, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 19_000, false, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, true, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, false, false, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, false, true, false, false
        ));
    }

    #[test]
    fn seek_preview_stays_under_the_pointer_until_release() {
        assert!(!super::should_clear_seek_preview(
            19, 19_000, false, true, true, true
        ));
        assert!(super::should_clear_seek_preview(
            19, 19_000, false, true, true, false
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, true, true, true, true
        ));
    }
}

fn connect_transport_controls(shell: &Rc<crate::player::PlayerUi>) {
    let Some(app) = shell.shell() else {
        return;
    };
    connect_bottom_player_scale(shell);
    let fullscreen = &shell.views.fullscreen_player;
    for (label, binding) in [
        (&fullscreen.artist, &fullscreen.artist_links),
        (&fullscreen.album, &fullscreen.album_links),
    ] {
        let weak = Rc::downgrade(shell);
        binding.replace(Some(DetailLinkBinding::new(
            label,
            Rc::new(move |route| {
                if let Some(shell) = weak.upgrade() {
                    shell.close_fullscreen_player();
                    shell.navigate(route);
                }
            }),
        )));
    }
    let controls = &shell.views.player_controls;
    controls.artist_links.replace(Some(DetailLinkBinding::new(
        &controls.artist,
        shell.route_navigation(),
    )));
    controls.album_links.replace(Some(DetailLinkBinding::new(
        &controls.album,
        shell.route_navigation(),
    )));
    let transport = app.products.playback.clone();
    shell
        .views
        .player_controls
        .previous_button
        .connect_clicked(move |_| transport.previous());

    let transport = app.products.playback.clone();
    shell
        .views
        .player_controls
        .play_button
        .connect_clicked(move |_| transport.play_pause());

    let transport = app.products.playback.clone();
    shell
        .views
        .player_controls
        .next_button
        .connect_clicked(move |_| transport.next());

    let feedback_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .shuffle_button
        .connect_clicked(move |_| {
            let Some(app) = feedback_shell.shell() else {
                return;
            };
            let Some(enabled) = feedback_shell
                .selected_playback()
                .as_deref()
                .map(|player| !player.controls.shuffle_enabled)
            else {
                return;
            };
            app.products.playback.set_shuffle(enabled);
        });

    let feedback_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .repeat_button
        .connect_clicked(move |_| {
            let Some(app) = feedback_shell.shell() else {
                return;
            };
            if feedback_shell.selected_playback().is_none() {
                return;
            }
            app.products.playback.cycle_repeat();
        });

    let feedback_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .dj_button
        .connect_clicked(move |_| {
            let Some(app) = feedback_shell.shell() else {
                return;
            };
            if feedback_shell.selected_playback().is_none() {
                return;
            }
            app.products.playback.toggle_auto_dj();
        });

    let mute_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .mute_button
        .connect_clicked(move |_| {
            let Some(app) = mute_shell.shell() else {
                return;
            };
            let Some(muted) = mute_shell
                .selected_playback()
                .as_deref()
                .map(|player| !player.controls.muted)
            else {
                return;
            };
            app.products.playback.set_muted(muted);
        });
    let output_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .output_button
        .connect_clicked(move |button| present_output_popover(button, &output_shell));
    let seek_shell = Rc::downgrade(shell);
    shell
        .views
        .player_controls
        .progress
        .connect_change_value(move |scale, _, value| {
            let Some(seek_shell) = seek_shell.upgrade() else {
                return glib::Propagation::Stop;
            };
            if seek_shell.controls.updating_controls.get() {
                return glib::Propagation::Proceed;
            }
            let duration_seconds = {
                let player = seek_shell.selected_playback();
                let Some(player) = player.as_ref() else {
                    return glib::Propagation::Stop;
                };
                let duration_seconds =
                    (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32;
                if player.transport.current.is_none()
                    || !player.transport.can_seek
                    || duration_seconds == 0
                {
                    return glib::Propagation::Stop;
                }
                duration_seconds
            };

            let seconds = seekbar_target_seconds(value, duration_seconds);
            preview_player_seek(&seek_shell, seconds);
            scale.set_value(f64::from(seconds));
            if !seek_shell.controls.seek_pointer_active.get() {
                commit_player_seek_preview_now(&seek_shell);
            }
            glib::Propagation::Stop
        });

    for widget in [
        shell
            .views
            .player_controls
            .progress
            .upcast_ref::<gtk::Widget>(),
        shell
            .views
            .player_controls
            .waveform
            .widget()
            .upcast_ref::<gtk::Widget>(),
    ] {
        // Observe release even after the scale's own gesture claims the drag.
        let events = gtk::EventControllerLegacy::new();
        events.set_propagation_phase(gtk::PropagationPhase::Capture);
        let seek_shell = Rc::downgrade(shell);
        events.connect_event(move |_, event| {
            let Some(shell) = seek_shell.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match event.event_type() {
                gtk::gdk::EventType::ButtonPress | gtk::gdk::EventType::TouchBegin => {
                    shell.controls.seek_pointer_active.set(true);
                }
                gtk::gdk::EventType::ButtonRelease | gtk::gdk::EventType::TouchEnd => {
                    if shell.controls.seek_pointer_active.replace(false) {
                        commit_player_seek_preview_now(&shell);
                    }
                }
                gtk::gdk::EventType::TouchCancel | gtk::gdk::EventType::GrabBroken => {
                    shell.clear_player_seek_preview();
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        widget.add_controller(events);
    }

    let seek_shell = Rc::downgrade(shell);
    let commit_shell = Rc::downgrade(shell);
    shell.views.player_controls.waveform.connect_seek(
        move |position| {
            if let Some(shell) = seek_shell.upgrade() {
                preview_player_seek_fraction(&shell, position);
            }
        },
        move || {
            if let Some(shell) = commit_shell.upgrade() {
                commit_player_seek_preview_now(&shell);
            }
        },
    );

    let volume_shell = Rc::clone(shell);
    shell
        .views
        .player_controls
        .volume
        .connect_value_changed(move |scale| {
            if volume_shell.controls.updating_controls.get() {
                return;
            }
            volume_shell.apply_user_volume(scale.value());
        });
}

impl crate::player::PlayerUi {
    pub fn set_current_rating_and_favorite(
        &self,
        favorite: bool,
        rating: Option<u8>,
        half_stars: bool,
    ) {
        let controls = &self.views.player_controls;
        set_favorite_button_active(&controls.favorite_button, favorite);
        controls.rating.set_rating(rating, half_stars);
    }
}

impl Shell {
    pub(crate) fn refresh_bottom_player_owner_details(self: &Rc<Self>) {
        let player = self.player_ui.selected_playback();
        let Some(current) = player
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
        else {
            self.player_ui
                .set_current_rating_and_favorite(false, None, false);
            return;
        };
        let track = &current.row.track;
        self.player_ui.set_current_rating_and_favorite(
            track.state.favorite,
            track
                .state
                .rating
                .and_then(|value| u8::try_from(value).ok()),
            self.half_stars_enabled(&track.media_uri, track.source_id.as_deref()),
        );
    }
}
fn install_now_playing_drag_source(
    target: &impl IsA<gtk::Widget>,
    artwork: &gtk::Picture,
    shell: &Rc<Shell>,
) {
    let drag_shell = Rc::downgrade(shell);
    install_compact_media_drag_source(target, artwork, move || {
        let shell = drag_shell.upgrade()?;
        let player = shell.player_ui.selected_playback()?;
        let current = player.transport.current.as_ref()?;
        let source = MediaDragSource::media_uris([current.row.track.media_uri.clone()]);
        Some((source, current.row.track.metadata.title.clone()))
    });
}
pub(crate) fn connect_player_controls(shell: &Rc<Shell>) {
    connect_transport_controls(&shell.player_ui);
    let controls = &shell.player_ui.views.player_controls;
    let drag_artwork = controls.cover.drag_paintable_source();
    install_now_playing_drag_source(&controls.cover.area, &drag_artwork, shell);
    install_now_playing_drag_source(&controls.title, &drag_artwork, shell);
    let title_shell = Rc::clone(shell);
    add_widget_click(controls.title.upcast_ref(), move || {
        title_shell.player_ui.navigate_current_track_album();
    });
    install_current_track_context_menu(&shell.player_ui.views.player_controls.cover.area, shell);
    let menu_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .menu_button
        .connect_clicked(move |button| present_current_track_context_menu(button, &menu_shell));
    let fullscreen_shell = Rc::clone(shell);
    add_widget_click(
        shell
            .player_ui
            .views
            .player_controls
            .cover
            .area
            .upcast_ref(),
        move || {
            gtk::prelude::ActionGroupExt::activate_action(
                &fullscreen_shell.chrome.window,
                "toggle-fullscreen-player",
                None,
            );
        },
    );
    let claims_shell = Rc::downgrade(shell);
    let release_shell = Rc::downgrade(shell);
    install_touch_swipe(
        &shell.player_ui.views.player_controls.now_playing_wall,
        move |axis, _, dy| {
            // Swiping up opens the fullscreen player, as tapping the cover does.
            axis == SwipeAxis::Horizontal
                || (dy < 0.0
                    && claims_shell.upgrade().is_some_and(|shell| {
                        shell.player_ui.mini.get() == crate::player::MiniPlayer::Off
                            && !shell.player_ui.fullscreen_player_visible()
                    }))
        },
        |_, _, _| {},
        move |axis, dx, dy| {
            let Some(shell) = release_shell.upgrade() else {
                return;
            };
            match axis {
                SwipeAxis::Horizontal if dx.abs() >= SWIPE_ACTION_DISTANCE => {
                    shell.player_ui.swipe_track(dx);
                }
                SwipeAxis::Vertical if dy <= -SWIPE_ACTION_DISTANCE => {
                    shell.player_ui.open_fullscreen_player();
                }
                SwipeAxis::Horizontal | SwipeAxis::Vertical => {}
            }
        },
    );

    let random_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .random_button
        .connect_clicked(move |_| {
            crate::shell::player::random_play::present_random_play_dialog(&random_shell)
        });

    let queue_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .queue_button
        .connect_clicked(move |_| queue_shell.toggle_right_panel());

    let favorite_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .favorite_button
        .connect_clicked(move |_| favorite_shell.toggle_current_track_favorite());

    let rating_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .rating
        .connect_commit(move |rating| rating_shell.set_current_track_rating(rating));

    let sleep_shell = Rc::downgrade(shell);
    shell
        .player_ui
        .views
        .player_controls
        .sleep_timer
        .connect_request(move |request| {
            if let Some(shell) = sleep_shell.upgrade() {
                shell.products.playback.set_sleep_timer(request);
            }
        });

    crate::player::playback_settings::configure_playback_settings_popover(
        &shell.player_ui.views.player_controls.settings_button,
        shell,
    );
}

fn install_current_track_context_menu(target: &impl IsA<gtk::Widget>, shell: &Rc<Shell>) {
    let shell = Rc::clone(shell);
    crate::widgets::interactions::install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            if let Some(track) = crate::player::state::current_playback_track(
                shell.player_ui.selected_playback().as_deref(),
            ) {
                crate::menus::media_menus::present_track_menu(
                    target, &shell, track, position, None, None,
                );
            }
        }),
    );
}

fn present_current_track_context_menu(target: &impl IsA<gtk::Widget>, shell: &Rc<Shell>) {
    if let Some(track) =
        crate::player::state::current_playback_track(shell.player_ui.selected_playback().as_deref())
    {
        crate::menus::media_menus::present_track_menu(
            target.as_ref(),
            &shell,
            track,
            None,
            Some(gtk::PositionType::Top),
            None,
        );
    }
}
