use adw::prelude::*;
use downloads::DOWNLOAD_BITRATES;
use gtk_widgets::scale::{install_scale_scroll_forwarding, install_sliding_value_bubble};
use library::{AudioChannels, AudioCodec, AudioEncoding};
use sources::DownloadQuality;
use std::{cell::Cell, rc::Rc};

pub(crate) fn rows(
    initial: DownloadQuality,
    changed: impl Fn(DownloadQuality, Rc<dyn Fn(DownloadQuality)>) + 'static,
) -> [adw::PreferencesRow; 3] {
    let resource = crate::ui_resource::DOWNLOAD_QUALITY_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        format: adw::ComboRow,
        bitrate: adw::ActionRow,
        bitrate_scale: gtk::Scale,
        channels: adw::ComboRow,
    });
    let codecs = [
        AudioCodec::Opus,
        AudioCodec::Aac,
        AudioCodec::Mp3,
        AudioCodec::Vorbis,
        AudioCodec::Flac,
    ];
    let channel_choices = [
        AudioChannels::Preserve,
        AudioChannels::StereoLossy,
        AudioChannels::Stereo,
    ];
    let lower = f64::from(DOWNLOAD_BITRATES[0]);
    let upper = f64::from(*DOWNLOAD_BITRATES.last().unwrap());
    let step = f64::from(DOWNLOAD_BITRATES[1] - DOWNLOAD_BITRATES[0]);
    bitrate_scale.adjustment().set_step_increment(step);
    install_scale_scroll_forwarding(&bitrate_scale);
    install_sliding_value_bubble(&bitrate_scale, |rate| format!("{rate:.0} kbps"));
    for rate in DOWNLOAD_BITRATES.iter().step_by(4) {
        bitrate_scale.add_mark(
            f64::from(*rate),
            gtk::PositionType::Bottom,
            Some(&rate.to_string()),
        );
    }
    let weak = (
        format.downgrade(),
        bitrate.downgrade(),
        bitrate_scale.downgrade(),
        channels.downgrade(),
    );
    let updating = Rc::new(Cell::new(false));
    let current = Rc::new(Cell::new(initial));
    let rate = Rc::new(Cell::new(None::<u32>));
    let set: Rc<dyn Fn(DownloadQuality)> = {
        let (weak, updating, current, rate) = (
            weak.clone(),
            updating.clone(),
            current.clone(),
            rate.clone(),
        );
        Rc::new(move |quality| {
            let (Some(format), Some(bitrate), Some(scale), Some(channels)) = (
                weak.0.upgrade(),
                weak.1.upgrade(),
                weak.2.upgrade(),
                weak.3.upgrade(),
            ) else {
                return;
            };
            updating.set(true);
            current.set(quality);
            let encoding = quality.encoding();
            rate.set(encoding.map_or(Some(128), |encoding| encoding.bitrate_kbps));
            format.set_selected(encoding.map_or(0, |encoding| {
                1 + codecs
                    .iter()
                    .position(|codec| *codec == encoding.codec)
                    .unwrap() as u32
            }));
            let value = f64::from(rate.get().unwrap_or(128));
            scale.set_range(lower.min(value), upper.max(value));
            scale.set_value(value);
            channels.set_selected(encoding.map_or(0, |encoding| {
                channel_choices
                    .iter()
                    .position(|channel| *channel == encoding.channels)
                    .unwrap() as u32
            }));
            bitrate
                .set_visible(encoding.is_some_and(|encoding| encoding.codec != AudioCodec::Flac));
            channels.set_visible(encoding.is_some());
            updating.set(false);
        })
    };
    set(initial);
    let commit: Rc<dyn Fn()> = {
        let (weak, updating, current, rate, set) = (
            weak.clone(),
            updating.clone(),
            current.clone(),
            rate.clone(),
            set.clone(),
        );
        Rc::new(move || {
            if updating.get() {
                return;
            }
            let (Some(format), Some(bitrate), Some(channels)) =
                (weak.0.upgrade(), weak.1.upgrade(), weak.3.upgrade())
            else {
                return;
            };
            let codec = format
                .selected()
                .checked_sub(1)
                .and_then(|index| codecs.get(index as usize))
                .copied();
            let quality = codec.map_or(DownloadQuality::Original, |codec| {
                DownloadQuality::Transcoded(AudioEncoding {
                    codec,
                    bitrate_kbps: if codec == AudioCodec::Flac {
                        None
                    } else if current
                        .get()
                        .encoding()
                        .is_none_or(|previous| previous.codec != codec)
                    {
                        rate.get().or(Some(128))
                    } else {
                        rate.get()
                    },
                    channels: channel_choices[channels.selected() as usize],
                })
            });
            bitrate.set_visible(codec.is_some_and(|codec| codec != AudioCodec::Flac));
            channels.set_visible(codec.is_some());
            if current.replace(quality) != quality {
                changed(quality, set.clone());
            }
        })
    };
    for row in [&format, &channels] {
        let commit = commit.clone();
        row.connect_selected_notify(move |_| commit());
    }
    let interacting = Rc::new(Cell::new(false));
    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (interacting, commit, reset, current) = (
            interacting.clone(),
            commit.clone(),
            set.clone(),
            current.clone(),
        );
        events.connect_event(move |_, event| {
            use gtk::gdk::EventType;
            match event.event_type() {
                EventType::ButtonPress | EventType::TouchBegin | EventType::KeyPress => {
                    interacting.set(true)
                }
                EventType::ButtonRelease | EventType::TouchEnd | EventType::KeyRelease => {
                    interacting.set(false);
                    commit();
                }
                EventType::TouchCancel | EventType::GrabBroken => {
                    interacting.set(false);
                    reset(current.get());
                }
                _ => {}
            }
            gtk::glib::Propagation::Proceed
        });
    }
    bitrate_scale.add_controller(events);
    {
        let (interacting, commit) = (interacting.clone(), commit.clone());
        bitrate_scale.connect_has_focus_notify(move |scale| {
            if !scale.has_focus() && interacting.replace(false) {
                commit();
            }
        });
    }
    bitrate_scale.connect_value_changed(move |scale| {
        if updating.get() {
            return;
        }
        let value = (scale.value() / step).round() * step;
        let value = value.clamp(scale.adjustment().lower(), scale.adjustment().upper());
        if scale.value() != value {
            scale.set_value(value);
            return;
        }
        rate.set(Some(value as u32));
        if !interacting.get() {
            commit();
        }
    });
    [format.upcast(), bitrate.upcast(), channels.upcast()]
}

pub(crate) fn label(quality: DownloadQuality) -> String {
    match quality {
        DownloadQuality::Original => localization::tr("Original"),
        DownloadQuality::Transcoded(encoding) => encoding
            .bitrate_kbps
            .filter(|_| encoding.codec != AudioCodec::Flac)
            .map_or_else(
                || encoding.codec.name().to_ascii_uppercase(),
                |rate| format!("{} {rate} kbps", encoding.codec.name().to_ascii_uppercase()),
            ),
    }
}
