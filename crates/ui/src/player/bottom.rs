use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;

use crate::format_duration;
use crate::routes::detail_links::{DetailLinkBinding, DetailLinks, track_artist_links};
use crate::routes::route::Route;
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;
use playback::{
    CurrentMediaId, PlaybackOutput, PlaybackView, RemoteOutput, RemoteOutputProtocol, RepeatMode,
    TransportStatus,
};

use crate::favorites::{favorite_icon_button, set_favorite_button_active};
use localization::{msgid, tr, tr_with};

use super::icons::{
    VolumeIcon, auto_dj_icon_button, play_icon_button, queue_sidebar_button,
    random_clover_icon_button, repeat_icon_button, set_repeat_button_icon, set_volume_icon,
    shuffle_icon_button, skip_icon_button, volume_icon_button, volume_icon_state,
};
use super::playback_settings::configure_playback_settings_popover;
use super::progress::seekbar_target_seconds;
use crate::interactions::add_widget_click;
use crate::layout::{AllocationOwner, allocation_owner};
use crate::localization::{bind_widget_accessible_label, bind_widget_tooltip};
use crate::ratings::RatingControl;
use crate::routes::collection_context::{
    install_current_track_context_menu, present_current_track_context_menu,
};
use crate::shell::Shell;
use crate::shell::actions::{MORE_ICON, icon_button_without_tooltip, set_active_class};
use crate::shell::cover::ArtworkTile;
use crate::shell::cover::MEDIUM_COVER_SIZE;

pub(crate) const BOTTOM_PLAYER_HEIGHT: i32 = 97;
const BOTTOM_PLAYER_COVER_START_INSET: i32 = 10;
const BOTTOM_PLAYER_COVER_SIZE: i32 = 64;
const BOTTOM_PLAYER_EDGE_PADDING: i32 = 8;
const BOTTOM_PLAYER_HORIZONTAL_PADDING: i32 =
    BOTTOM_PLAYER_COVER_START_INSET - BOTTOM_PLAYER_EDGE_PADDING;
const BOTTOM_PLAYER_NOW_PLAYING_SPACING: i32 = 8;
const BOTTOM_PLAYER_RAIL_END_PADDING: i32 = 2;
pub(crate) const NOW_PLAYING_RAIL_WIDTH: i32 = BOTTOM_PLAYER_EDGE_PADDING
    + BOTTOM_PLAYER_HORIZONTAL_PADDING
    + BOTTOM_PLAYER_COVER_SIZE
    + BOTTOM_PLAYER_RAIL_END_PADDING;
const BOTTOM_PLAYER_TRANSPORT_WIDTH: i32 = 300;
const BOTTOM_PLAYER_PROGRESS_WIDTH: i32 = 320;
const BOTTOM_PLAYER_PROGRESS_MIN_WIDTH: i32 = 140;
const BOTTOM_PLAYER_BUTTON_ROW_HEIGHT: i32 = 58;
const BOTTOM_PLAYER_SIDE_BUTTON_SIZE: i32 = 50;
const BOTTOM_PLAYER_PLAY_BUTTON_SIZE: i32 = 45;
const BOTTOM_PLAYER_BUTTON_OFFSET_Y: f64 = 3.0;
const BOTTOM_PLAYER_BUTTON_STEP: f64 = 38.0;
const BOTTOM_PLAYER_WAVEFORM_HEIGHT: i32 = 32;
const BOTTOM_PLAYER_ACTION_BUTTON_SIZE: i32 = 34;
const BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE: i32 = 18;
const BOTTOM_PLAYER_TITLE_MENU_GAP: i32 = 0;
const BOTTOM_PLAYER_IDENTITY_HEIGHT: i32 = 58;
const BOTTOM_PLAYER_TITLE_ROW_HEIGHT: i32 = 20;
const BOTTOM_PLAYER_META_ROW_HEIGHT: i32 = 18;
const BOTTOM_PLAYER_ACTION_SPACING: i32 = 0;
const BOTTOM_PLAYER_ACTION_ROW_OFFSET_Y: i32 = 5;
const BOTTOM_PLAYER_PROGRESS_SPACING: i32 = 6;
const BOTTOM_PLAYER_VOLUME_SPACING: i32 = 1;
const BOTTOM_PLAYER_VOLUME_SLOT_WIDTH: i32 = 95;
const BOTTOM_PLAYER_VOLUME_SLOT_LAYOUT_WIDTH: i32 = 90;
const BOTTOM_PLAYER_RATING_WIDTH: i32 = 85;
const BOTTOM_PLAYER_RATING_HEIGHT: i32 = 24;
const OUTPUT_DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const BOTTOM_PLAYER_TINY_TRANSPORT_WIDTH: i32 = 126;
const BOTTOM_PLAYER_TINY_CONTROL_SPACING: i32 = 2;
const BOTTOM_PLAYER_TINY_CONTROLS_WIDTH: i32 = BOTTOM_PLAYER_TINY_TRANSPORT_WIDTH;
const BOTTOM_PLAYER_TINY_ROW_SPACING: i32 = 6;
const BOTTOM_PLAYER_IDENTITY_MIN_WIDTH: i32 = 85;
const BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH: i32 = BOTTOM_PLAYER_EDGE_PADDING * 2
    + BOTTOM_PLAYER_TRANSPORT_WIDTH
    + (BOTTOM_PLAYER_HORIZONTAL_PADDING
        + BOTTOM_PLAYER_COVER_SIZE
        + BOTTOM_PLAYER_NOW_PLAYING_SPACING
        + BOTTOM_PLAYER_IDENTITY_MIN_WIDTH)
        * 2;
const BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH: i32 = BOTTOM_PLAYER_ACTION_BUTTON_SIZE * 3
    + BOTTOM_PLAYER_VOLUME_SPACING * 2
    + BOTTOM_PLAYER_VOLUME_SLOT_LAYOUT_WIDTH;
const BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH: i32 = BOTTOM_PLAYER_EDGE_PADDING * 2
    + BOTTOM_PLAYER_TRANSPORT_WIDTH
    + BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH * 2;
const BOTTOM_PLAYER_COMPACT_MIN_WIDTH: i32 =
    if BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH > BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH {
        BOTTOM_PLAYER_IDENTITY_COMPACT_MIN_WIDTH
    } else {
        BOTTOM_PLAYER_ACTIONS_COMPACT_MIN_WIDTH
    };
const BOTTOM_PLAYER_TINY_WIDTH: i32 = BOTTOM_PLAYER_COMPACT_MIN_WIDTH;
const BOTTOM_PLAYER_FULL_PROGRESS_WIDTH: i32 = 864;
const SEEK_PREVIEW_COMMIT_DELAY: Duration = Duration::from_millis(100);
const SEEK_PREVIEW_TOLERANCE_MILLIS: u64 = 1_500;
const VOLUME_PERSIST_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BottomPlayerActions {
    Volume,
    Favorite,
    Queue,
}

pub(crate) struct PlayerControls {
    pub(crate) root: AllocationOwner,
    surface: gtk::Box,
    pub(crate) cover: ArtworkTile,
    title: gtk::Label,
    title_links: RefCell<Option<DetailLinkBinding>>,
    pub(crate) menu_button: gtk::Button,
    artist: gtk::Label,
    artist_links: RefCell<Option<DetailLinkBinding>>,
    album: gtk::Label,
    album_links: RefCell<Option<DetailLinkBinding>>,
    now_playing_wall: gtk::Box,
    left_slot: gtk::Overlay,
    right_slot: gtk::Overlay,
    tiny_row: gtk::Box,
    tiny_controls: gtk::Box,
    tiny_layout: Cell<bool>,
    transport: gtk::Box,
    transport_slot: gtk::Box,
    transport_buttons: gtk::Fixed,
    pub(crate) random_button: gtk::Button,
    pub(crate) previous_button: gtk::Button,
    play_button: gtk::Button,
    play_icon: gtk::DrawingArea,
    play_icon_playing: Rc<Cell<bool>>,
    pub(crate) next_button: gtk::Button,
    pub(crate) shuffle_button: gtk::Button,
    repeat_button: gtk::Button,
    dj_button: gtk::Button,
    pub(super) queue_button: gtk::Button,
    pub(super) queue_icon: gtk::Image,
    pub(crate) favorite_button: gtk::Button,
    rating: RatingControl,
    rating_available: Cell<bool>,
    progress_row: gtk::Box,
    elapsed: gtk::Label,
    progress_stack: gtk::Stack,
    progress: gtk::Scale,
    waveform: WaveformSeekBar,
    waveform_key: RefCell<Option<CurrentMediaId>>,
    waveform_peak_count: Cell<usize>,
    duration: gtk::Label,
    actions: gtk::Overlay,
    pub(crate) mute_button: gtk::Button,
    mute_icon: gtk::Image,
    mute_icon_state: Rc<Cell<VolumeIcon>>,
    volume: gtk::Scale,
    pub(crate) settings_button: gtk::MenuButton,
    pub(crate) output_button: gtk::Button,
    output_icon: gtk::Image,
}

struct NowPlayingControls {
    root: gtk::Box,
    cover: ArtworkTile,
    title: gtk::Label,
    menu_button: gtk::Button,
    artist: gtk::Label,
    album: gtk::Label,
}

struct TransportControls {
    root: gtk::Box,
    buttons: gtk::Fixed,
    random_button: gtk::Button,
    previous_button: gtk::Button,
    play_button: gtk::Button,
    play_icon: gtk::DrawingArea,
    play_icon_playing: Rc<Cell<bool>>,
    next_button: gtk::Button,
    shuffle_button: gtk::Button,
    repeat_button: gtk::Button,
    dj_button: gtk::Button,
    progress_row: gtk::Box,
    elapsed: gtk::Label,
    progress_stack: gtk::Stack,
    progress: gtk::Scale,
    waveform: WaveformSeekBar,
    duration: gtk::Label,
}

struct PlayerActionControls {
    root: gtk::Overlay,
    queue_button: gtk::Button,
    queue_icon: gtk::Image,
    favorite_button: gtk::Button,
    rating: RatingControl,
    mute_button: gtk::Button,
    mute_icon: gtk::Image,
    mute_icon_state: Rc<Cell<VolumeIcon>>,
    volume: gtk::Scale,
    settings_button: gtk::MenuButton,
    output_button: gtk::Button,
    output_icon: gtk::Image,
}

#[derive(Clone)]
struct WaveformSeekBar {
    area: gtk::DrawingArea,
    peaks: Rc<RefCell<Vec<(f64, f64)>>>,
    position: Rc<Cell<f64>>,
}

impl WaveformSeekBar {
    fn new() -> Self {
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

    fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    fn set_peaks(&self, peaks: Option<&[(f64, f64)]>) {
        let next = peaks.map(normalize_waveform_peaks).unwrap_or_default();
        *self.peaks.borrow_mut() = next;
        self.area.queue_draw();
    }

    fn set_position_fraction(&self, position: f64) {
        let position = position.clamp(0.0, 1.0);
        self.position.set(position);
        self.area
            .update_property(&[gtk::accessible::Property::ValueNow(position)]);
        self.area.queue_draw();
    }

    fn connect_seek<F, C>(&self, seek: F, commit: C)
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
        let click_commit = Rc::clone(&commit);
        click.connect_released(move |_, _, _, _| {
            click_commit();
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
        let drag_commit = Rc::clone(&commit);
        drag.connect_drag_end(move |_, _, _| {
            drag_commit();
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

fn waveform_fraction_for_x(area: &gtk::DrawingArea, x: f64) -> f64 {
    let width = f64::from(area.width()).max(1.0);
    let fraction = (x / width).clamp(0.0, 1.0);
    if area.direction() == gtk::TextDirection::Rtl {
        1.0 - fraction
    } else {
        fraction
    }
}

fn normalize_waveform_peaks(peaks: &[(f64, f64)]) -> Vec<(f64, f64)> {
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

fn draw_waveform_seekbar(
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

fn visual_waveform_fraction(value: f64, is_rtl: bool) -> f64 {
    let value = value.clamp(0.0, 1.0);
    if is_rtl { 1.0 - value } else { value }
}

fn waveform_bar_opacity(x: f64, position: f64, hover_position: Option<f64>, is_rtl: bool) -> f64 {
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

fn set_waveform_source(context: &gtk::cairo::Context, color: &gtk::gdk::RGBA, opacity: f64) {
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()) * opacity,
    );
}

impl Shell {
    pub(crate) fn sync_bottom_player_favorite(&self) {
        let player = self.selected_playback();
        let current = player
            .as_ref()
            .and_then(|player| player.transport.current.as_ref());
        let favorite = current.is_some_and(|entry| {
            self.projected_track_favorite(&entry.track.id, entry.track.favorite)
        });
        set_favorite_button_active(&self.player_view.player_controls.favorite_button, favorite);
    }

    pub(crate) fn update_bottom_player(self: &Rc<Self>) {
        self.update_bottom_player_view(true);
    }

    pub(crate) fn update_bottom_player_transport(self: &Rc<Self>) {
        self.update_bottom_player_view(false);
    }

    pub(crate) fn update_bottom_player_position(&self) {
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
        let controls = &self.player_view.player_controls;
        let displayed_seconds = self
            .selected_ui
            .seek_preview_seconds()
            .unwrap_or(position_seconds);
        self.playback.updating_controls.set(true);
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
        self.playback.updating_controls.set(false);
    }

    fn update_bottom_player_view(self: &Rc<Self>, update_identity: bool) {
        let player = self.selected_playback().as_deref().cloned();
        let current = player
            .as_ref()
            .and_then(|player| player.transport.current.as_ref());
        let source_id = player
            .as_ref()
            .map(|player| player.transport.source_id.clone());
        let state = player
            .as_ref()
            .map(|player| player.transport.effective_state())
            .unwrap_or(TransportStatus::Stopped);
        let duration_seconds = player
            .as_ref()
            .map(|player| {
                (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32
            })
            .unwrap_or_default();
        let position_seconds = player
            .as_ref()
            .map(|player| {
                (player.transport.position_millis / 1_000).min(u64::from(u32::MAX)) as u32
            })
            .unwrap_or_default();
        let repeat_mode = player
            .as_ref()
            .map(|player| player.controls.repeat_mode)
            .unwrap_or(RepeatMode::Off);
        let controls = &self.player_view.player_controls;
        self.playback.updating_controls.set(true);

        if update_identity {
            let window_title = crate::shell::chrome::playback_window_title(
                current.map(|entry| entry.track.title.as_str()),
                current.map(|entry| entry.track.artist.as_str()),
            );
            self.chrome.window.set_title(Some(&window_title));

            if let (Some(entry), Some(source_id)) = (current, source_id.as_ref()) {
                self.bind_playback_artwork_tile(
                    &controls.cover,
                    source_id,
                    ArtworkBinding::track(&entry.track),
                    BOTTOM_PLAYER_COVER_SIZE,
                    MEDIUM_COVER_SIZE,
                );
            } else {
                self.clear_artwork_tile(&controls.cover);
            }

            let title = current
                .map(|entry| entry.track.title.as_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| localization::tr("Nothing playing"));
            let artist = current
                .map(|entry| entry.track.artist.as_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| localization::tr("Queue a track to begin"));
            let album = current
                .map(|entry| entry.track.album.as_str())
                .unwrap_or("");
            let album_route = current
                .and_then(|entry| entry.track.album_id.clone())
                .map(Route::AlbumDetail);
            if let Some(title_links) = controls.title_links.borrow().as_ref() {
                title_links.bind(DetailLinks::route(&title, album_route.clone()));
            } else {
                controls.title.set_text(&title);
            }
            if let Some(artist_links) = controls.artist_links.borrow().as_ref() {
                artist_links.bind(
                    current
                        .map(|entry| track_artist_links(&entry.track))
                        .unwrap_or_else(|| DetailLinks::text(&artist)),
                );
            } else {
                controls.artist.set_text(&artist);
            }
            if let Some(album_links) = controls.album_links.borrow().as_ref() {
                album_links.bind(DetailLinks::route(album, album_route));
            } else {
                controls.album.set_text(album);
            }
            controls.title.set_sensitive(current.is_some());
            controls.menu_button.set_sensitive(current.is_some());
            controls
                .artist
                .set_sensitive(current.is_some_and(|entry| !entry.track.artist.is_empty()));
            controls
                .album
                .set_sensitive(current.is_some_and(|entry| !entry.track.album.is_empty()));
            controls.favorite_button.set_sensitive(current.is_some());
            let rating_available = current.is_some_and(|entry| {
                self.rating_available(&library::FavoriteItemId::Track(entry.track.id.clone()))
            });
            controls.rating_available.set(rating_available);
            controls.rating.widget().set_sensitive(rating_available);
            controls.rating.widget().set_visible(rating_available);
            controls
                .rating
                .set_rating(current.and_then(|entry| entry.track.user_rating));
        }

        controls.play_icon_playing.set(matches!(
            state,
            TransportStatus::Playing | TransportStatus::Buffering
        ));
        controls.play_icon.queue_draw();
        controls
            .play_button
            .set_tooltip_text(Some(&playback_state_label(state)));

        let shuffle_enabled = player
            .as_ref()
            .is_some_and(|player| player.controls.shuffle_enabled);
        let auto_dj_enabled = player
            .as_ref()
            .is_some_and(|player| player.controls.auto_dj_enabled);
        set_active_class(&controls.shuffle_button, shuffle_enabled);
        set_active_class(&controls.repeat_button, repeat_mode != RepeatMode::Off);
        set_repeat_button_icon(&controls.repeat_button, repeat_mode);
        set_active_class(&controls.dj_button, auto_dj_enabled);
        controls
            .dj_button
            .set_tooltip_text(Some(&if auto_dj_enabled {
                tr("Auto DJ on")
            } else {
                tr("Auto DJ")
            }));
        controls
            .repeat_button
            .set_tooltip_text(Some(&repeat_label(repeat_mode)));

        let preview_seconds = self.selected_ui.seek_preview_seconds();
        let displayed_seconds = preview_seconds.unwrap_or(position_seconds);
        controls
            .elapsed
            .set_text(&format_duration(displayed_seconds));
        let max = f64::from(duration_seconds.max(1));
        controls.progress.set_range(0.0, max);
        let can_seek = player
            .as_ref()
            .is_some_and(|player| player.transport.can_seek && duration_seconds > 0);
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
        let waveform_enabled = self.settings.current.borrow().seekbar_waveform_enabled;
        let waveform = self.selected_ui.waveform().unwrap_or_default();
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
        if waveform_matches {
            controls
                .progress_stack
                .set_visible_child(controls.waveform.widget());
        } else {
            controls
                .progress_stack
                .set_visible_child(&controls.progress);
        }
        controls
            .duration
            .set_text(&format_duration(duration_seconds));

        let (muted, volume) = player
            .as_ref()
            .map(|player| (player.controls.muted, player.controls.volume))
            .unwrap_or((false, 1.0));
        let volume_icon = volume_icon_state(muted, volume);
        if controls.mute_icon_state.get() != volume_icon {
            controls.mute_icon_state.set(volume_icon);
            set_volume_icon(&controls.mute_icon, volume_icon);
        }
        controls.volume.set_value(volume);
        let output = player
            .as_ref()
            .map(|player| &player.controls.playback_output)
            .cloned()
            .unwrap_or_default();
        set_output_icon_active(&controls.output_icon, !output.is_local());
        controls
            .output_button
            .set_tooltip_text(Some(&playback_output_label(&output)));
        self.playback.updating_controls.set(false);
    }

    pub(crate) fn maybe_clear_player_seek_preview(
        &self,
        player: &PlaybackView,
        track_changed: bool,
    ) {
        let Some(target_seconds) = self.selected_ui.seek_preview_seconds() else {
            return;
        };
        if should_clear_seek_preview(
            target_seconds,
            player.transport.position_millis,
            track_changed,
            player.transport.current.is_some(),
            player.transport.can_seek,
        ) {
            self.clear_player_seek_preview();
        }
    }

    fn clear_player_seek_preview(&self) {
        self.selected_ui.set_seek_preview_seconds(None);
    }
}

fn seek_preview_matches_position(target_seconds: u32, position_millis: u64) -> bool {
    let target_millis = u64::from(target_seconds) * 1_000;
    let lower = target_millis.saturating_sub(SEEK_PREVIEW_TOLERANCE_MILLIS);
    let upper = target_millis.saturating_add(SEEK_PREVIEW_TOLERANCE_MILLIS);
    (lower..=upper).contains(&position_millis)
}

fn should_clear_seek_preview(
    target_seconds: u32,
    position_millis: u64,
    track_changed: bool,
    has_current: bool,
    can_seek: bool,
) -> bool {
    track_changed
        || !has_current
        || !can_seek
        || seek_preview_matches_position(target_seconds, position_millis)
}

pub(crate) fn build_bottom_player() -> PlayerControls {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("bottom-player");
    root.set_hexpand(true);
    root.set_vexpand(false);
    root.set_height_request(BOTTOM_PLAYER_HEIGHT);
    root.set_valign(gtk::Align::Center);

    let NowPlayingControls {
        root: now_playing,
        cover,
        title,
        menu_button,
        artist,
        album,
    } = build_now_playing_controls();

    let TransportControls {
        root: transport,
        buttons: transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_icon_playing,
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
    } = build_transport_controls();

    let now_playing_wall = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    now_playing_wall.add_css_class("player-now-playing-wall");
    now_playing_wall.set_hexpand(true);
    now_playing_wall.set_vexpand(false);
    now_playing_wall.set_valign(gtk::Align::Center);
    now_playing_wall.set_width_request(1);
    now_playing_wall.set_overflow(gtk::Overflow::Hidden);
    now_playing_wall.append(&now_playing);

    let PlayerActionControls {
        root: actions,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume,
        settings_button,
        output_button,
        output_icon,
    } = build_player_action_controls();

    let left_slot = bottom_player_allocated_slot(&now_playing_wall, 1, 1);
    left_slot.set_hexpand(true);
    left_slot.set_halign(gtk::Align::Fill);
    left_slot.set_valign(gtk::Align::Fill);
    let right_slot = bottom_player_allocated_slot(&actions, 1, 1);
    right_slot.set_hexpand(true);
    right_slot.set_halign(gtk::Align::Fill);
    right_slot.set_valign(gtk::Align::Fill);

    let transport_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    transport_slot.set_width_request(BOTTOM_PLAYER_TRANSPORT_WIDTH);
    transport_slot.set_halign(gtk::Align::Center);
    transport_slot.set_valign(gtk::Align::Center);
    transport_slot.append(&transport);

    let tiny_controls = gtk::Box::new(
        gtk::Orientation::Horizontal,
        BOTTOM_PLAYER_TINY_CONTROL_SPACING,
    );
    tiny_controls.set_halign(gtk::Align::End);
    tiny_controls.set_valign(gtk::Align::Center);
    tiny_controls.set_width_request(BOTTOM_PLAYER_TINY_CONTROLS_WIDTH);

    let tiny_row = gtk::Box::new(gtk::Orientation::Horizontal, BOTTOM_PLAYER_TINY_ROW_SPACING);
    tiny_row.set_hexpand(true);
    tiny_row.set_halign(gtk::Align::Fill);
    tiny_row.set_valign(gtk::Align::Center);
    tiny_row.set_width_request(1);
    tiny_row.append(&tiny_controls);

    root.append(&left_slot);
    root.append(&transport_slot);
    root.append(&right_slot);
    // Bottom-player topology follows the real allocation. Applying it during GTK's tentative
    // height-for-width measurement lets a provisional width and the allocated width alternate,
    // continuously invalidating the shell layout.
    let allocation_root = allocation_owner(&root, |_, _| {});

    PlayerControls {
        root: allocation_root,
        surface: root,
        cover,
        title,
        title_links: RefCell::new(None),
        menu_button,
        artist,
        artist_links: RefCell::new(None),
        album,
        album_links: RefCell::new(None),
        now_playing_wall,
        left_slot,
        right_slot,
        tiny_row,
        tiny_controls,
        tiny_layout: Cell::new(false),
        transport,
        transport_slot,
        transport_buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_icon_playing,
        next_button,
        shuffle_button,
        repeat_button,
        dj_button,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        rating_available: Cell::new(false),
        progress_row,
        elapsed,
        progress_stack,
        progress,
        waveform,
        waveform_key: RefCell::new(None),
        waveform_peak_count: Cell::new(0),
        duration,
        actions,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume,
        settings_button,
        output_button,
        output_icon,
    }
}

fn build_now_playing_controls() -> NowPlayingControls {
    let root = gtk::Box::new(
        gtk::Orientation::Horizontal,
        BOTTOM_PLAYER_NOW_PLAYING_SPACING,
    );
    root.add_css_class("player-now-playing");
    root.set_hexpand(true);
    root.set_width_request(1);
    root.set_halign(gtk::Align::Fill);
    root.set_valign(gtk::Align::Center);
    root.set_margin_start(BOTTOM_PLAYER_HORIZONTAL_PADDING);

    let cover = ArtworkTile::new(BOTTOM_PLAYER_COVER_SIZE);
    cover.area.set_valign(gtk::Align::Center);
    cover.area.set_cursor_from_name(Some("pointer"));
    let cover_label = tr("Open fullscreen player");
    cover.area.set_tooltip_text(Some(&cover_label));
    cover
        .area
        .update_property(&[gtk::accessible::Property::Label(&cover_label)]);
    root.append(&cover.area);

    let identity = gtk::Box::new(gtk::Orientation::Vertical, 1);
    identity.add_css_class("player-identity");
    identity.set_hexpand(true);
    identity.set_width_request(1);
    identity.set_halign(gtk::Align::Fill);
    identity.set_valign(gtk::Align::Center);
    let title = player_link("player-title");
    let menu_button = icon_button_without_tooltip(MORE_ICON, "More actions");
    menu_button.add_css_class("player-title-menu-button");
    menu_button.set_size_request(
        BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE,
        BOTTOM_PLAYER_TITLE_MENU_BUTTON_SIZE,
    );
    menu_button.set_valign(gtk::Align::Center);
    menu_button.set_sensitive(false);
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, BOTTOM_PLAYER_TITLE_MENU_GAP);
    title_row.add_css_class("player-title-row");
    title_row.set_hexpand(true);
    title_row.set_width_request(1);
    title_row.set_halign(gtk::Align::Fill);
    title_row.set_valign(gtk::Align::Center);
    title_row.set_height_request(BOTTOM_PLAYER_TITLE_ROW_HEIGHT);
    title.set_height_request(BOTTOM_PLAYER_TITLE_ROW_HEIGHT);
    title_row.append(&title);
    title_row.append(&menu_button);
    let artist = player_link("player-primary");
    let album = player_link("player-primary");
    artist.set_height_request(BOTTOM_PLAYER_META_ROW_HEIGHT);
    album.set_height_request(BOTTOM_PLAYER_META_ROW_HEIGHT);
    artist.set_hexpand(true);
    artist.set_width_request(1);
    album.set_hexpand(true);
    album.set_width_request(1);
    identity.append(&title_row);
    identity.append(&artist);
    identity.append(&album);
    let identity_slot = bottom_player_identity_slot(&identity);
    root.append(&identity_slot);

    NowPlayingControls {
        root,
        cover,
        title,
        menu_button,
        artist,
        album,
    }
}

fn bottom_player_identity_slot(identity: &gtk::Box) -> gtk::Overlay {
    let slot = bottom_player_allocated_slot(identity, 1, BOTTOM_PLAYER_IDENTITY_HEIGHT);
    slot.set_hexpand(true);
    slot.set_halign(gtk::Align::Fill);
    slot.set_valign(gtk::Align::Center);
    slot
}

fn bottom_player_allocated_slot(
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

fn build_transport_controls() -> TransportControls {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 5);
    root.add_css_class("player-transport");
    root.set_width_request(BOTTOM_PLAYER_TRANSPORT_WIDTH);
    root.set_valign(gtk::Align::Center);

    let buttons = gtk::Fixed::new();
    buttons.add_css_class("player-button-row");
    buttons.set_halign(gtk::Align::Center);
    buttons.set_valign(gtk::Align::Center);
    buttons.set_size_request(
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        BOTTOM_PLAYER_BUTTON_ROW_HEIGHT,
    );

    let dj_button = auto_dj_icon_button("Auto DJ");
    let previous_button = skip_icon_button(false, "Previous");
    let (play_button, play_icon, play_icon_playing) = play_icon_button("Play");
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
        &buttons,
        &dj_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -3.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &buttons,
        &shuffle_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -2.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &buttons,
        &previous_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        -1.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &buttons,
        &play_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        0.0,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
    put_transport_button(
        &buttons,
        &next_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        1.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &buttons,
        &repeat_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        2.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
    put_transport_button(
        &buttons,
        &random_button,
        BOTTOM_PLAYER_TRANSPORT_WIDTH,
        3.0,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );

    let progress_row = gtk::Box::new(gtk::Orientation::Horizontal, BOTTOM_PLAYER_PROGRESS_SPACING);
    progress_row.add_css_class("player-progress-row");
    progress_row.set_halign(gtk::Align::Center);
    progress_row.set_valign(gtk::Align::Center);
    let elapsed = gtk::Label::new(Some("0:00"));
    elapsed.add_css_class("muted");
    elapsed.set_width_chars(4);
    elapsed.set_xalign(1.0);
    let progress = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    progress.add_css_class("player-progress");
    progress.set_draw_value(false);
    progress.set_width_request(BOTTOM_PLAYER_PROGRESS_WIDTH);
    let waveform = WaveformSeekBar::new();
    let progress_stack = gtk::Stack::new();
    progress_stack.set_size_request(BOTTOM_PLAYER_PROGRESS_WIDTH, BOTTOM_PLAYER_WAVEFORM_HEIGHT);
    progress_stack.set_hhomogeneous(false);
    progress_stack.set_vhomogeneous(true);
    progress_stack.add_named(&progress, Some("scale"));
    progress_stack.add_named(waveform.widget(), Some("waveform"));
    progress_stack.set_visible_child(&progress);
    let duration = gtk::Label::new(Some("0:00"));
    duration.add_css_class("muted");
    duration.set_width_chars(4);
    progress_row.append(&elapsed);
    progress_row.append(&progress_stack);
    progress_row.append(&duration);

    root.append(&buttons);
    root.append(&progress_row);

    TransportControls {
        root,
        buttons,
        random_button,
        previous_button,
        play_button,
        play_icon,
        play_icon_playing,
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

fn build_player_action_controls() -> PlayerActionControls {
    let root = gtk::Overlay::new();
    root.set_halign(gtk::Align::End);
    root.set_valign(gtk::Align::Fill);
    root.set_vexpand(true);
    let rating = RatingControl::new(None);
    rating
        .widget()
        .set_size_request(BOTTOM_PLAYER_RATING_WIDTH, BOTTOM_PLAYER_RATING_HEIGHT);
    rating.widget().set_halign(gtk::Align::End);
    rating.widget().set_valign(gtk::Align::Start);
    rating.widget().set_margin_top(5);
    rating.widget().set_margin_end(15);
    root.add_overlay(rating.widget());
    root.set_measure_overlay(rating.widget(), false);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, BOTTOM_PLAYER_ACTION_SPACING);
    buttons.set_halign(gtk::Align::End);
    buttons.set_valign(gtk::Align::Center);
    buttons.set_margin_top(BOTTOM_PLAYER_ACTION_ROW_OFFSET_Y * 2);
    let output_button = icon_button_without_tooltip(
        "rufin-video-display-symbolic",
        msgid("Choose playback output"),
    );
    output_button.add_css_class("player-output-button");
    configure_player_action_button(&output_button);
    let output_icon = output_button
        .child()
        .and_then(|child| child.downcast::<gtk::Image>().ok())
        .expect("an icon button contains an image");
    output_icon.add_css_class("player-output-icon");
    buttons.append(&output_button);
    let settings_button = gtk::MenuButton::new();
    settings_button.set_icon_name("rufin-preferences-system-symbolic");
    settings_button.set_direction(gtk::ArrowType::Up);
    settings_button.set_has_frame(false);
    settings_button.add_css_class("icon-button");
    settings_button.add_css_class("flat");
    settings_button.add_css_class("circular");
    settings_button.add_css_class("player-settings-button");
    settings_button.set_valign(gtk::Align::Center);
    bind_widget_tooltip(&settings_button, "Playback settings");
    bind_widget_accessible_label(&settings_button, "Playback settings");
    buttons.append(&settings_button);

    let volume_group = gtk::Box::new(gtk::Orientation::Horizontal, BOTTOM_PLAYER_VOLUME_SPACING);
    volume_group.set_valign(gtk::Align::Center);
    let favorite_button = favorite_icon_button("Favorite");
    favorite_button.add_css_class("player-favorite-button");
    configure_player_action_button(&favorite_button);
    volume_group.append(&favorite_button);
    let (queue_button, queue_icon) = queue_sidebar_button("Hide sidebar");
    configure_player_action_button(&queue_button);
    volume_group.append(&queue_button);
    let (mute_button, mute_icon, mute_icon_state) = volume_icon_button("Mute");
    mute_button.add_css_class("player-mute-button");
    configure_player_action_button(&mute_button);
    volume_group.append(&mute_button);
    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    volume.add_css_class("volume-slider");
    volume.set_hexpand(true);
    volume.set_halign(gtk::Align::Fill);
    volume.set_valign(gtk::Align::Center);
    volume.set_value(1.0);
    volume.set_draw_value(false);
    let volume_slot = bottom_player_allocated_slot(&volume, BOTTOM_PLAYER_VOLUME_SLOT_WIDTH, 1);
    volume_slot.add_css_class("volume-slider-slot");
    volume_slot.set_valign(gtk::Align::Fill);
    volume_group.append(&volume_slot);
    buttons.append(&volume_group);
    root.set_child(Some(&buttons));

    PlayerActionControls {
        root,
        queue_button,
        queue_icon,
        favorite_button,
        rating,
        mute_button,
        mute_icon,
        mute_icon_state,
        volume,
        settings_button,
        output_button,
        output_icon,
    }
}

fn configure_transport_side_button(button: &gtk::Button) {
    button.add_css_class("player-transport-button");
    button.set_size_request(
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
        BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
    );
}

fn configure_play_button(button: &gtk::Button) {
    button.add_css_class("player-transport-button");
    button.add_css_class("player-play-button");
    button.set_size_request(
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
        BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
    );
}

fn configure_player_action_button(button: &gtk::Button) {
    button.set_valign(gtk::Align::Center);
}

fn bottom_player_action_count(actions: BottomPlayerActions) -> i32 {
    match actions {
        BottomPlayerActions::Volume => 0,
        BottomPlayerActions::Favorite => 1,
        BottomPlayerActions::Queue => 2,
    }
}

fn bottom_player_action_min_width(actions: BottomPlayerActions) -> i32 {
    let visible_action_count = bottom_player_action_count(actions);
    BOTTOM_PLAYER_VOLUME_GROUP_MIN_WIDTH
        + BOTTOM_PLAYER_ACTION_BUTTON_SIZE * visible_action_count
        + BOTTOM_PLAYER_ACTION_SPACING * visible_action_count
}

fn bottom_player_side_width(player_width: i32) -> i32 {
    bottom_player_content_width(player_width).saturating_sub(BOTTOM_PLAYER_TRANSPORT_WIDTH) / 2
}

fn bottom_player_content_width(player_width: i32) -> i32 {
    (player_width - BOTTOM_PLAYER_EDGE_PADDING * 2).max(1)
}

fn bottom_player_progress_width(player_width: i32) -> i32 {
    if player_width >= BOTTOM_PLAYER_FULL_PROGRESS_WIDTH {
        return BOTTOM_PLAYER_PROGRESS_WIDTH;
    }

    let span = BOTTOM_PLAYER_FULL_PROGRESS_WIDTH - BOTTOM_PLAYER_COMPACT_MIN_WIDTH;
    let width_span = BOTTOM_PLAYER_PROGRESS_WIDTH - BOTTOM_PLAYER_PROGRESS_MIN_WIDTH;
    let progress = (player_width - BOTTOM_PLAYER_COMPACT_MIN_WIDTH).clamp(0, span);
    BOTTOM_PLAYER_PROGRESS_MIN_WIDTH + width_span * progress / span
}

fn centered_progress_width(
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

fn widget_natural_width(widget: &impl IsA<gtk::Widget>) -> i32 {
    let (_, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    natural
}

fn bottom_player_actions(player_width: i32) -> BottomPlayerActions {
    let side_width = bottom_player_side_width(player_width);
    if side_width >= bottom_player_action_min_width(BottomPlayerActions::Queue) {
        BottomPlayerActions::Queue
    } else if side_width >= bottom_player_action_min_width(BottomPlayerActions::Favorite) {
        BottomPlayerActions::Favorite
    } else {
        BottomPlayerActions::Volume
    }
}

fn bottom_player_tiny(player_width: i32) -> bool {
    player_width < BOTTOM_PLAYER_TINY_WIDTH
}

fn put_transport_button(
    buttons: &gtk::Fixed,
    button: &gtk::Button,
    width: i32,
    slot: f64,
    size: i32,
) {
    let (x, y) = transport_button_position(width, slot, size);
    buttons.put(button, x, y);
}

fn move_transport_button(
    buttons: &gtk::Fixed,
    button: &gtk::Button,
    width: i32,
    slot: f64,
    size: i32,
) {
    let (x, y) = transport_button_position(width, slot, size);
    buttons.move_(button, x, y);
}

fn transport_button_position(width: i32, slot: f64, size: i32) -> (f64, f64) {
    let center_x = f64::from(width) / 2.0 + BOTTOM_PLAYER_BUTTON_STEP * slot;
    let radius = f64::from(size) / 2.0;
    let y = f64::from(BOTTOM_PLAYER_BUTTON_ROW_HEIGHT - size) / 2.0 + BOTTOM_PLAYER_BUTTON_OFFSET_Y;
    (center_x - radius, y)
}

fn player_link(css_class: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("player-link");
    label.add_css_class(css_class);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    label.set_lines(1);
    label.set_width_chars(1);
    label.set_halign(gtk::Align::Fill);
    label.set_valign(gtk::Align::Center);
    label.set_hexpand(false);
    label.set_yalign(0.5);
    label
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

fn repeat_label(repeat_mode: RepeatMode) -> String {
    match repeat_mode {
        RepeatMode::Off => tr("Repeat off"),
        RepeatMode::One => tr("Repeat one"),
        RepeatMode::All => tr("Repeat all"),
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

fn remember_remote_output(shell: &Shell, remote: &RemoteOutput) {
    let mut cached = shell.playback.remote_output_options.borrow_mut();
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
) {
    if let PlaybackOutput::Remote(selected) = selected
        && !discovered
            .iter()
            .any(|output| same_remote_output(output, selected))
    {
        discovered.push(selected.clone());
    }
    *cached = discovered;
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

    fn fail_connection(&self) {
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.label.add_css_class("error");
        self.label.set_text(&tr("Connection failed"));
    }
}

fn present_output_popover(anchor: &gtk::Button, shell: &Rc<Shell>) {
    let selected = shell.products.playback.transport.playback_output();
    let popover = gtk::Popover::new();
    popover.add_css_class("playback-output-popover");
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Top);
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

    let mut shown = shell.playback.remote_output_options.borrow().clone();
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
        remote_rows.push((output.clone(), row));
    }
    let shown = Rc::new(RefCell::new(shown));
    let remote_rows = Rc::new(RefCell::new(remote_rows));

    let (sender, receiver) = std::sync::mpsc::channel();
    let transport = shell.products.playback.transport.clone();
    let discovery_running = Arc::new(AtomicBool::new(true));
    let worker_running = Arc::clone(&discovery_running);
    thread::spawn(move || {
        while worker_running.load(Ordering::Acquire) {
            if sender.send(transport.discover_remote_outputs()).is_err() {
                break;
            }
            let slices = OUTPUT_DISCOVERY_REFRESH_INTERVAL.as_millis() / 100;
            for _ in 0..slices {
                if !worker_running.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
    let outputs_for_discovery = outputs.clone();
    let selected_for_discovery = selected.clone();
    let popover_for_discovery = popover.clone();
    let shell_for_discovery = Rc::clone(shell);
    let status_for_discovery = status.clone();
    let shown_for_discovery = Rc::clone(&shown);
    let rows_for_discovery = Rc::clone(&remote_rows);
    let poll_running = Arc::clone(&discovery_running);
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        if !popover_for_discovery.is_visible() {
            poll_running.store(false, Ordering::Release);
            return glib::ControlFlow::Break;
        }
        match receiver.try_recv() {
            Ok(Ok(discovered)) => {
                status_for_discovery.finish_search(discovered.is_empty());
                let mut shown = shown_for_discovery.borrow_mut();
                let previous = shown.clone();
                replace_discovered_remote_outputs(&mut shown, discovered, &selected_for_discovery);
                if *shown == previous {
                    return glib::ControlFlow::Continue;
                }
                let mut rows = rows_for_discovery.borrow_mut();
                for (_, row) in rows.drain(..) {
                    outputs_for_discovery.remove(&row);
                }
                for output in shown.iter() {
                    let row = add_output_row(
                        &outputs_for_discovery,
                        PlaybackOutput::Remote(output.clone()),
                        &selected_for_discovery,
                        &popover_for_discovery,
                        &status_for_discovery,
                        &shell_for_discovery,
                    );
                    rows.push((output.clone(), row));
                }
                *shell_for_discovery
                    .playback
                    .remote_output_options
                    .borrow_mut() = shown.clone();
                glib::ControlFlow::Continue
            }
            Ok(Err(error)) => {
                if !status_for_discovery.selection_started.get() {
                    status_for_discovery.spinner.stop();
                    status_for_discovery.spinner.set_visible(false);
                    status_for_discovery.root.set_visible(true);
                    status_for_discovery.label.add_css_class("error");
                    status_for_discovery.label.set_text(&error);
                }
                glib::ControlFlow::Continue
            }
            Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => {
                status_for_discovery.finish_search(shown_for_discovery.borrow().is_empty());
                glib::ControlFlow::Break
            }
        }
    });

    popover.set_child(Some(&content));
    let close_running = Arc::clone(&discovery_running);
    popover.connect_closed(move |popover| {
        close_running.store(false, Ordering::Release);
        popover.unparent();
    });
    popover.popup();
}

fn add_output_row(
    group: &adw::PreferencesGroup,
    output: PlaybackOutput,
    selected: &PlaybackOutput,
    popover: &gtk::Popover,
    status: &OutputPopoverStatus,
    shell: &Rc<Shell>,
) -> adw::ActionRow {
    let title = match &output {
        PlaybackOutput::Local => glib::markup_escape_text(&playback_output_label(&output)),
        PlaybackOutput::Remote(remote) => {
            let name = glib::markup_escape_text(&remote.name);
            let protocol = glib::markup_escape_text(&match remote.protocol {
                RemoteOutputProtocol::Upnp => tr("UPnP"),
                RemoteOutputProtocol::GoogleCast => tr("Google Cast"),
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
    if &output == selected {
        row.add_css_class("selected-output");
        row.add_suffix(&gtk::Image::from_icon_name("rufin-object-select-symbolic"));
    }
    let row_output = output.clone();
    let row_group = group.clone();
    let row_popover = popover.clone();
    let row_status = status.clone();
    let row_shell = Rc::clone(shell);
    row.connect_activated(move |_| {
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
    shell: &Rc<Shell>,
    outputs: &adw::PreferencesGroup,
    popover: &gtk::Popover,
    status: &OutputPopoverStatus,
    output: PlaybackOutput,
) {
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
    let (sender, receiver) = std::sync::mpsc::channel();
    let transport = shell.products.playback.transport.clone();
    let selected = output.clone();
    thread::spawn(move || {
        let _ = sender.send(transport.select_playback_output(selected));
    });
    let shell = Rc::clone(shell);
    let outputs = outputs.clone();
    let popover = popover.clone();
    let status = status.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        match receiver.try_recv() {
            Ok(Ok(())) => {
                if let PlaybackOutput::Remote(remote) = &output {
                    remember_remote_output(&shell, remote);
                }
                set_output_icon_active(
                    &shell.player_view.player_controls.output_icon,
                    !output.is_local(),
                );
                shell
                    .player_view
                    .player_controls
                    .output_button
                    .set_tooltip_text(Some(&playback_output_label(&output)));
                let message = match &output {
                    PlaybackOutput::Local => tr("Playing on this device"),
                    PlaybackOutput::Remote(output) => {
                        tr_with("Playing on {device}", &[("device", output.name.as_str())])
                    }
                };
                shell.show_feedback_toast(message.clone());
                outputs.set_sensitive(true);
                status.spinner.stop();
                popover.popdown();
                glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                outputs.set_sensitive(true);
                status.fail_connection();
                shell.show_feedback_toast(error);
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => {
                outputs.set_sensitive(true);
                status.fail_connection();
                glib::ControlFlow::Break
            }
        }
    });
}

fn preview_player_seek(shell: &Rc<Shell>, seconds: u32) {
    shell.selected_ui.set_seek_preview_seconds(Some(seconds));
    shell
        .player_view
        .player_controls
        .progress
        .set_value(f64::from(seconds));
    shell
        .player_view
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
        .player_view
        .player_controls
        .waveform
        .set_position_fraction(position);
}

fn preview_player_seek_fraction(shell: &Rc<Shell>, position: f64) {
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

fn queue_player_seek_preview_commit(shell: &Rc<Shell>) {
    let generation = shell.playback.next_seek_generation();

    let shell = Rc::clone(shell);
    glib::timeout_add_local_once(SEEK_PREVIEW_COMMIT_DELAY, move || {
        if shell.playback.seek_generation() == generation {
            commit_player_seek_preview(&shell);
        }
    });
}

fn commit_player_seek_preview_now(shell: &Rc<Shell>) {
    shell.playback.next_seek_generation();
    commit_player_seek_preview(shell);
}

fn commit_player_seek_preview(shell: &Rc<Shell>) {
    let Some(seconds) = shell.selected_ui.seek_preview_seconds() else {
        return;
    };
    shell.products.playback.transport.seek_seconds(seconds);
}

pub(crate) fn connect_player_controls(shell: &Rc<Shell>) {
    connect_bottom_player_resize(shell);
    let controls = &shell.player_view.player_controls;
    controls
        .title_links
        .replace(Some(DetailLinkBinding::new(&controls.title, shell)));
    controls
        .artist_links
        .replace(Some(DetailLinkBinding::new(&controls.artist, shell)));
    controls
        .album_links
        .replace(Some(DetailLinkBinding::new(&controls.album, shell)));
    install_current_track_context_menu(&shell.player_view.player_controls.cover.area, shell);
    let menu_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .menu_button
        .connect_clicked(move |button| present_current_track_context_menu(button, &menu_shell));
    let fullscreen_shell = Rc::clone(shell);
    add_widget_click(
        shell.player_view.player_controls.cover.area.upcast_ref(),
        move || {
            fullscreen_shell.toggle_fullscreen_player();
        },
    );

    let transport = shell.products.playback.transport.clone();
    shell
        .player_view
        .player_controls
        .previous_button
        .connect_clicked(move |_| transport.previous());

    let transport = shell.products.playback.transport.clone();
    shell
        .player_view
        .player_controls
        .play_button
        .connect_clicked(move |_| transport.play_pause());

    let transport = shell.products.playback.transport.clone();
    shell
        .player_view
        .player_controls
        .next_button
        .connect_clicked(move |_| transport.next());

    let feedback_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .shuffle_button
        .connect_clicked(move |_| {
            let Some(enabled) = feedback_shell
                .selected_playback()
                .as_deref()
                .map(|player| !player.controls.shuffle_enabled)
            else {
                return;
            };
            feedback_shell.products.playback.transport.toggle_shuffle();
            feedback_shell.show_control_feedback_toast(if enabled {
                tr("Shuffle on")
            } else {
                tr("Shuffle off")
            });
        });

    let feedback_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .repeat_button
        .connect_clicked(move |_| {
            let Some(repeat_mode) = feedback_shell
                .selected_playback()
                .as_deref()
                .map(|player| player.controls.repeat_mode)
            else {
                return;
            };
            let title = match repeat_mode {
                RepeatMode::Off => tr("Repeat all"),
                RepeatMode::All => tr("Repeat one"),
                RepeatMode::One => tr("Repeat off"),
            };
            feedback_shell.products.playback.transport.cycle_repeat();
            feedback_shell.show_control_feedback_toast(title);
        });

    let feedback_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .dj_button
        .connect_clicked(move |_| {
            let Some(enabled) = feedback_shell
                .selected_playback()
                .as_deref()
                .map(|player| !player.controls.auto_dj_enabled)
            else {
                return;
            };
            feedback_shell.products.playback.transport.toggle_auto_dj();
            feedback_shell.show_control_feedback_toast(if enabled {
                tr("Auto DJ on")
            } else {
                tr("Auto DJ off")
            });
        });

    let random_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .random_button
        .connect_clicked(move |_| super::random_play::present_random_play_dialog(&random_shell));

    let queue_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .queue_button
        .connect_clicked(move |_| queue_shell.toggle_right_panel());

    let favorite_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .favorite_button
        .connect_clicked(move |_| favorite_shell.toggle_current_track_favorite());

    let rating_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .rating
        .connect_commit(move |rating| rating_shell.set_current_track_rating(rating));

    let mute_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .mute_button
        .connect_clicked(move |_| {
            let Some(muted) = mute_shell
                .selected_playback()
                .as_deref()
                .map(|player| !player.controls.muted)
            else {
                return;
            };
            mute_shell.apply_user_muted(muted);
        });
    configure_playback_settings_popover(&shell.player_view.player_controls.settings_button, shell);
    let output_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .output_button
        .connect_clicked(move |button| present_output_popover(button, &output_shell));
    let seek_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .progress
        .connect_change_value(move |scale, scroll, value| {
            if seek_shell.playback.updating_controls.get() {
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
            if scroll != gtk::ScrollType::Jump {
                commit_player_seek_preview_now(&seek_shell);
            } else {
                queue_player_seek_preview_commit(&seek_shell);
            }
            glib::Propagation::Stop
        });

    let seek_shell = Rc::clone(shell);
    let seek_click = gtk::GestureClick::new();
    seek_click.connect_released(move |_, _, _, _| {
        commit_player_seek_preview_now(&seek_shell);
    });
    shell
        .player_view
        .player_controls
        .progress
        .add_controller(seek_click);

    let seek_shell = Rc::clone(shell);
    let commit_shell = Rc::clone(shell);
    shell.player_view.player_controls.waveform.connect_seek(
        move |position| preview_player_seek_fraction(&seek_shell, position),
        move || commit_player_seek_preview_now(&commit_shell),
    );

    let volume_shell = Rc::clone(shell);
    shell
        .player_view
        .player_controls
        .volume
        .connect_value_changed(move |scale| {
            if volume_shell.playback.updating_controls.get() {
                return;
            }
            volume_shell.apply_user_volume(scale.value());
        });
}

impl Shell {
    pub(crate) fn apply_user_volume(self: &Rc<Self>, volume: f64) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.products.playback.transport.set_volume(volume);
        let local = self
            .selected_playback()
            .as_deref()
            .map(|player| player.controls.playback_output.is_local())
            .unwrap_or_else(|| {
                self.products
                    .playback
                    .transport
                    .playback_output()
                    .is_local()
            });
        if !local {
            return;
        }
        self.settings.current.borrow_mut().playback.volume = volume;
        if let Some(source) = self.playback.volume_persist_source.borrow_mut().take() {
            source.remove();
        }
        let shell = Rc::clone(self);
        let source = glib::timeout_add_local_once(VOLUME_PERSIST_DELAY, move || {
            *shell.playback.volume_persist_source.borrow_mut() = None;
            shell.products.playback.transport.persist_volume(volume);
        });
        *self.playback.volume_persist_source.borrow_mut() = Some(source);
    }

    pub(crate) fn apply_user_muted(&self, muted: bool) {
        self.products.playback.transport.set_muted(muted);
        let local = self
            .selected_playback()
            .as_deref()
            .map(|player| player.controls.playback_output.is_local())
            .unwrap_or_else(|| {
                self.products
                    .playback
                    .transport
                    .playback_output()
                    .is_local()
            });
        if local {
            self.settings.current.borrow_mut().playback.muted = muted;
        }
    }
}

fn connect_bottom_player_resize(shell: &Rc<Shell>) {
    let resize_shell = Rc::downgrade(shell);
    let allocated_width = Cell::new(0);
    shell
        .player_view
        .player_controls
        .root
        .set_size_callback(move |width, _height| {
            if width <= 0 || allocated_width.replace(width) == width {
                return;
            }
            let Some(resize_shell) = resize_shell.upgrade() else {
                return;
            };
            resize_shell.apply_bottom_player_width(width);
        });
}

impl Shell {
    pub(crate) fn apply_bottom_player_width(&self, player_width: i32) {
        if player_width > 0 {
            let tiny = bottom_player_tiny(player_width);
            let actions = bottom_player_actions(player_width);
            self.apply_bottom_player_tiny(tiny);
            self.apply_bottom_player_actions(actions);
            let player = &self.player_view.player_controls;
            let desired_progress_width = bottom_player_progress_width(player_width);
            let progress_width = if tiny {
                desired_progress_width
            } else {
                // Keep the action surface inside its equal side allocation. The progress row yields
                // when necessary so the two side slots remain the same width around the transport.
                let action_width = widget_natural_width(&player.actions);
                let progress_row_fixed_width = widget_natural_width(&player.elapsed)
                    + widget_natural_width(&player.duration)
                    + BOTTOM_PLAYER_PROGRESS_SPACING * 2;
                centered_progress_width(
                    desired_progress_width,
                    bottom_player_content_width(player_width),
                    action_width,
                    progress_row_fixed_width,
                )
            };
            self.player_view
                .player_controls
                .progress
                .set_width_request(progress_width);
            self.player_view
                .player_controls
                .progress_stack
                .set_size_request(progress_width, BOTTOM_PLAYER_WAVEFORM_HEIGHT);
            self.player_view
                .player_controls
                .waveform
                .widget()
                .set_width_request(progress_width);
            self.player_view
                .player_controls
                .waveform
                .widget()
                .set_content_width(progress_width);
        }
    }

    fn apply_bottom_player_tiny(&self, tiny: bool) {
        let player = &self.player_view.player_controls;
        let transport_width = if tiny {
            BOTTOM_PLAYER_TINY_TRANSPORT_WIDTH
        } else {
            BOTTOM_PLAYER_TRANSPORT_WIDTH
        };
        player.transport.set_width_request(transport_width);
        player.transport_slot.set_width_request(transport_width);
        player
            .transport_buttons
            .set_size_request(transport_width, BOTTOM_PLAYER_BUTTON_ROW_HEIGHT);
        player.tiny_row.set_width_request(1);
        player.now_playing_wall.set_width_request(1);
        player.transport_slot.set_halign(if tiny {
            gtk::Align::End
        } else {
            gtk::Align::Center
        });
        move_transport_button(
            &player.transport_buttons,
            &player.previous_button,
            transport_width,
            -1.0,
            BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
        );
        move_transport_button(
            &player.transport_buttons,
            &player.play_button,
            transport_width,
            0.0,
            BOTTOM_PLAYER_PLAY_BUTTON_SIZE,
        );
        move_transport_button(
            &player.transport_buttons,
            &player.next_button,
            transport_width,
            1.0,
            BOTTOM_PLAYER_SIDE_BUTTON_SIZE,
        );
        player.dj_button.set_visible(!tiny);
        player.shuffle_button.set_visible(!tiny);
        player.repeat_button.set_visible(!tiny);
        player.random_button.set_visible(!tiny);
        player.progress_row.set_visible(!tiny);
        player.actions.set_visible(!tiny);
        if player.tiny_layout.replace(tiny) == tiny {
            return;
        }
        if tiny {
            player.surface.remove(&player.left_slot);
            player.surface.remove(&player.transport_slot);
            player.surface.remove(&player.right_slot);
            player.tiny_row.prepend(&player.left_slot);
            player.tiny_controls.prepend(&player.transport_slot);
            player.surface.append(&player.tiny_row);
        } else {
            player.surface.remove(&player.tiny_row);
            player.tiny_row.remove(&player.left_slot);
            player.tiny_controls.remove(&player.transport_slot);
            player.surface.append(&player.left_slot);
            player.surface.append(&player.transport_slot);
            player.surface.append(&player.right_slot);
        }
    }

    fn apply_bottom_player_actions(&self, actions: BottomPlayerActions) {
        let player = &self.player_view.player_controls;
        player.favorite_button.set_visible(matches!(
            actions,
            BottomPlayerActions::Favorite | BottomPlayerActions::Queue
        ));
        player
            .queue_button
            .set_visible(matches!(actions, BottomPlayerActions::Queue));
        player
            .rating
            .widget()
            .set_visible(player.rating_available.get());
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
        for player_width in super::BOTTOM_PLAYER_TINY_WIDTH..=4096 {
            let actions = super::bottom_player_actions(player_width);
            let action_width = super::bottom_player_action_min_width(actions);

            assert!(
                action_width <= super::bottom_player_side_width(player_width),
                "actions outgrew their side at player width {player_width}"
            );
        }
    }

    #[test]
    fn non_tiny_player_preserves_now_playing_identity_width() {
        let fixed_now_playing_width = super::BOTTOM_PLAYER_HORIZONTAL_PADDING
            + super::BOTTOM_PLAYER_COVER_SIZE
            + super::BOTTOM_PLAYER_NOW_PLAYING_SPACING;
        let identity_width = super::bottom_player_side_width(super::BOTTOM_PLAYER_TINY_WIDTH)
            - fixed_now_playing_width;

        assert!(identity_width >= super::BOTTOM_PLAYER_IDENTITY_MIN_WIDTH);
        assert!(super::bottom_player_tiny(
            super::BOTTOM_PLAYER_TINY_WIDTH - 1
        ));
        assert!(!super::bottom_player_tiny(super::BOTTOM_PLAYER_TINY_WIDTH));
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
            19, 5_000, false, true, true
        ));
        assert!(super::should_clear_seek_preview(
            19, 19_000, false, true, true
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, true, true, true
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, false, false, true
        ));
        assert!(super::should_clear_seek_preview(
            19, 5_000, false, true, false
        ));
    }
}
