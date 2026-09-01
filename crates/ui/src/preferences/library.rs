use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use downloads::{DownloadQueueState, DownloadRule};
use gtk::gio;
use playback::StreamQuality;

use localization::{msgid, tr, trn_with};

use super::{
    PreferencesNavigationControls,
    layout::button_row,
    quality_selection_row, row_action_content,
    source::{
        configured_source_display_name, configured_source_icon_name,
        configured_source_kind_display_name, half_stars_row,
    },
};
use crate::runtime::source::{SourceLocalAccessSummary, SourceSummary};
use crate::shell::Shell;
use localization::{album_count_text, track_count_text};

const SERVER_PROVIDER_ICON_SIZE: i32 = 28;
const DOWNLOAD_JOB_DRAG_PREFIX: &str = "rufin-download-job:";
const DEFAULT_DOWNLOAD_DIRECTORY_SUBTITLE: &str = msgid("Rufin data folder");
const DOWNLOAD_QUALITIES: [StreamQuality; 5] = [
    StreamQuality::Original,
    StreamQuality::MaxBitrateKbps(320),
    StreamQuality::MaxBitrateKbps(256),
    StreamQuality::MaxBitrateKbps(192),
    StreamQuality::MaxBitrateKbps(128),
];

struct DownloadQueueRow {
    row: adw::ActionRow,
    job: Option<downloads::DownloadQueueItem>,
    up: Option<gtk::Button>,
    down: Option<gtk::Button>,
}

pub(super) fn library_page(
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    navigation_controls: &PreferencesNavigationControls,
    open_add_server: bool,
    focus_download_queue: bool,
) -> gtk::Widget {
    let navigation = adw::NavigationView::new();
    navigation_controls.set_navigation(&navigation);
    navigation_controls.set_nested_page_visible(false);
    let page = library_sources_page(
        shell,
        dialog,
        &navigation,
        navigation_controls,
        focus_download_queue,
    );
    let root = adw::NavigationPage::new(&page, &tr("Library"));
    navigation.push(&root);
    let shell_for_pop = Rc::clone(shell);
    navigation.connect_popped(move |_, _| {
        shell_for_pop.release_inactive_add_server_form();
    });
    if open_add_server || shell.has_retained_add_server_form() {
        let page = shell.add_server_navigation_page(dialog);
        navigation.push(&page);
        navigation_controls.set_nested_page_visible(true);
    }
    navigation.upcast::<gtk::Widget>()
}

fn library_sources_page(
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    navigation: &adw::NavigationView,
    navigation_controls: &PreferencesNavigationControls,
    focus_download_queue: bool,
) -> adw::PreferencesPage {
    let resource = crate::ui_resource::LIBRARY_PREFERENCES_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        library_page: adw::PreferencesPage,
        servers_group: adw::PreferencesGroup,
        empty_remote_row: adw::ActionRow,
        downloads_group: adw::PreferencesGroup,
        download_folder_row: adw::ActionRow,
        reset_folder: gtk::Button,
        choose_folder: gtk::Button,
        downloaded_badge: adw::SwitchRow,
        download_actions_row: adw::PreferencesRow,
        download_action_buttons: gtk::Box,
        remove_all: gtk::Button,
        download_queue_header: adw::PreferencesRow,
        pause_downloads: gtk::Button,
        local_group: adw::PreferencesGroup,
        empty_local_row: adw::ActionRow,
        local_actions_row: adw::PreferencesRow,
        add_local: gtk::Button,
        resync_local: gtk::Button,
    });

    let configured = shell.source.configured.borrow().clone();
    let local_source = configured
        .sources
        .iter()
        .find(|source| source.kind == "local")
        .cloned();
    let remote_sources = configured
        .sources
        .iter()
        .filter(|source| source.kind != "local")
        .cloned()
        .collect::<Vec<_>>();

    if remote_sources.is_empty() {
        servers_group.add(&empty_remote_row);
    } else {
        for server in &remote_sources {
            let selected = configured.selected_source_id.as_ref() == Some(&server.id);
            let summary = configured
                .local_access
                .iter()
                .find(|summary| summary.source_id == server.id);
            let credentials = shell
                .products
                .source
                .configured_source(&server.id)
                .ok()
                .flatten()
                .map(|saved| saved.credentials);
            let row = adw::ActionRow::builder()
                .title(configured_source_display_name(server))
                .subtitle(source_summary_subtitle(
                    server,
                    summary,
                    credentials
                        .as_ref()
                        .map(|credentials| credentials.server_url.as_str()),
                    credentials
                        .as_ref()
                        .map(|credentials| credentials.username.as_str()),
                ))
                .subtitle_lines(4)
                .build();
            let icon = gtk::Image::from_icon_name(configured_source_icon_name(server));
            icon.set_pixel_size(SERVER_PROVIDER_ICON_SIZE);
            icon.set_size_request(SERVER_PROVIDER_ICON_SIZE, SERVER_PROVIDER_ICON_SIZE);
            icon.set_valign(gtk::Align::Center);
            row.add_prefix(&icon);
            if selected {
                row.add_suffix(&gtk::Image::from_icon_name("rufin-object-select-symbolic"));
            }
            row.add_suffix(&gtk::Image::from_icon_name("rufin-go-next-symbolic"));
            row.set_activatable(true);
            let settings_shell = Rc::clone(shell);
            let navigation = navigation.downgrade();
            let navigation_controls = navigation_controls.clone();
            let dialog = dialog.downgrade();
            let server = server.clone();
            row.connect_activated(move |_| {
                let Some(dialog) = dialog.upgrade() else {
                    return;
                };
                let Some(navigation) = navigation.upgrade() else {
                    return;
                };
                let navigation_controls_for_close = navigation_controls.clone();
                let on_close: Rc<dyn Fn()> = Rc::new(move || {
                    navigation_controls_for_close.set_nested_page_visible(false);
                });
                let page = crate::preferences::source::local_access::manage_server_navigation_page(
                    &settings_shell,
                    server.clone(),
                    &navigation,
                    &dialog,
                    on_close,
                );
                navigation.push(&page);
                navigation_controls.set_nested_page_visible(true);
            });
            servers_group.add(&row);
        }
    }

    let add_server = button_row("Add server", "rufin-list-add-symbolic");
    let add_shell = Rc::clone(shell);
    let add_navigation = navigation.downgrade();
    let add_navigation_controls = navigation_controls.clone();
    let add_server_dialog = dialog.downgrade();
    add_server.connect_activated(move |_| {
        let Some(add_server_dialog) = add_server_dialog.upgrade() else {
            return;
        };
        let Some(add_navigation) = add_navigation.upgrade() else {
            return;
        };
        let page = add_shell.add_server_navigation_page(&add_server_dialog);
        add_navigation.push(&page);
        add_navigation_controls.set_nested_page_visible(true);
    });
    servers_group.add(&add_server);

    if let Some(server) = remote_sources
        .iter()
        .find(|server| configured.selected_source_id.as_ref() == Some(&server.id))
    {
        let download_settings = shell
            .settings
            .current
            .borrow()
            .download_settings(&server.id);
        download_folder_row.set_subtitle(
            &download_settings
                .directory
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| tr(DEFAULT_DOWNLOAD_DIRECTORY_SUBTITLE)),
        );
        reset_folder.set_visible(download_settings.directory.is_some());
        let choose_shell = Rc::clone(shell);
        let choose_source_id = server.id.clone();
        let choose_row = download_folder_row.downgrade();
        let choose_reset = reset_folder.downgrade();
        choose_folder.connect_clicked(move |_| {
            let shell = Rc::clone(&choose_shell);
            let source_id = choose_source_id.clone();
            let row = choose_row.clone();
            let reset = choose_reset.clone();
            gtk::glib::spawn_future_local(async move {
                let chooser = gtk::FileDialog::builder()
                    .title(tr("Select Download Folder"))
                    .build();
                let initial_directory = shell
                    .settings
                    .current
                    .borrow()
                    .download_directory(&source_id);
                if let Some(directory) = initial_directory.as_ref() {
                    chooser.set_initial_folder(Some(&gio::File::for_path(directory)));
                }
                let Ok(folder) = chooser
                    .select_folder_future(Some(&shell.chrome.window))
                    .await
                else {
                    return;
                };
                let Some(path) = folder.path() else {
                    return;
                };
                if shell
                    .update_app_settings("download folder", |settings| {
                        settings.set_download_directory(source_id, Some(path.clone()))
                    })
                    .is_some()
                {
                    if let Some(row) = row.upgrade() {
                        row.set_subtitle(&path.display().to_string());
                    }
                    if let Some(reset) = reset.upgrade() {
                        reset.set_visible(true);
                    }
                }
            });
        });
        let reset_shell = Rc::clone(shell);
        let reset_source_id = server.id.clone();
        let reset_row = download_folder_row.downgrade();
        let reset_button = reset_folder.downgrade();
        reset_folder.connect_clicked(move |_| {
            let Some(reset_button) = reset_button.upgrade() else {
                return;
            };
            if reset_shell
                .update_app_settings("reset download folder", |settings| {
                    settings.set_download_directory(reset_source_id.clone(), None)
                })
                .is_some()
            {
                if let Some(row) = reset_row.upgrade() {
                    row.set_subtitle(&tr(DEFAULT_DOWNLOAD_DIRECTORY_SUBTITLE));
                }
                reset_button.set_visible(false);
            }
        });
        downloads_group.add(&download_folder_row);

        let quality_shell = Rc::clone(shell);
        let quality_source_id = server.id.clone();
        let quality_choices =
            download_quality_choices(server.transcoded_download_bitrate_limit_kbps);
        let quality_index = quality_choices
            .iter()
            .position(|quality| *quality == download_settings.quality)
            .unwrap_or_default() as u32;
        let selected_qualities = quality_choices.clone();
        let quality = quality_selection_row(
            &tr("Download quality"),
            &quality_choices,
            quality_index,
            move |selected| {
                let quality = selected_qualities
                    .get(selected as usize)
                    .copied()
                    .unwrap_or(StreamQuality::Original);
                quality_shell.update_app_settings("download quality", |settings| {
                    settings.set_download_quality(quality_source_id.clone(), quality)
                });
            },
        );
        downloads_group.add(&quality);
        downloaded_badge.set_active(shell.settings.current.borrow().show_downloaded_badges);
        let downloaded_badge_shell = Rc::clone(shell);
        downloaded_badge.connect_active_notify(move |row| {
            downloaded_badge_shell.set_downloaded_badges_visible(row.is_active());
        });
        downloads_group.add(&downloaded_badge);
        let add_rule = add_download_rules(
            &downloads_group,
            shell,
            dialog,
            &server.id,
            download_settings.rules,
        );

        download_action_buttons.prepend(&add_rule);
        let remove_shell = Rc::clone(shell);
        let source_id = server.id.clone();
        let preferences_dialog = dialog.downgrade();
        remove_all.connect_clicked(move |_| {
            let Some(preferences_dialog) = preferences_dialog.upgrade() else {
                return;
            };
            confirm_remove_all_downloads(&remove_shell, &preferences_dialog, source_id.clone());
        });
        downloads_group.add(&download_actions_row);

        downloads_group.add(&download_queue_header);
        let queue_focus = add_download_queue(&downloads_group, shell, &server.id, &pause_downloads);
        if focus_download_queue {
            gtk::glib::idle_add_local_once(move || {
                queue_focus.grab_focus();
            });
        }
        library_page.add(&downloads_group);
    }

    if let Some(half_stars) = local_source
        .as_ref()
        .and_then(|source| half_stars_row(shell, source))
    {
        local_group.add(&half_stars);
    }
    if configured.local_folders.is_empty() {
        local_group.add(&empty_local_row);
    } else {
        for folder in configured.local_folders.iter() {
            let row = adw::ActionRow::builder()
                .title(local_folder_title(&folder.path))
                .subtitle(folder.path.clone())
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("rufin-folders-symbolic"));
            let remove = gtk::Button::from_icon_name("rufin-window-close-symbolic");
            remove.set_tooltip_text(Some(&tr("Remove")));
            remove.add_css_class("flat");
            remove.add_css_class("destructive-action");
            remove.set_valign(gtk::Align::Center);
            row.add_suffix(&remove);
            row.set_activatable(false);
            let remove_shell = Rc::clone(shell);
            let path = folder.path.clone();
            let row_for_remove = row.downgrade();
            remove.connect_clicked(move |_| {
                let Some(row) = row_for_remove.upgrade() else {
                    return;
                };
                confirm_remove_local_folder(&remove_shell, path.clone(), row);
            });
            local_group.add(&row);
        }
    }

    let add_shell = Rc::clone(shell);
    let add_dialog = dialog.downgrade();
    add_local.connect_clicked(move |_| {
        let Some(dialog) = add_dialog.upgrade() else {
            return;
        };
        let shell = Rc::clone(&add_shell);
        gtk::glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title(tr("Select Music Folder"))
                .build();
            let Ok(folder) = chooser
                .select_folder_future(Some(&shell.chrome.window))
                .await
            else {
                return;
            };
            let Some(path) = folder.path() else {
                return;
            };
            shell.products.source.add_local_folder(path);
            dialog.close();
        });
    });
    let local_source_id = local_source.map(|source| source.id);
    resync_local.set_sensitive(!configured.local_folders.is_empty() && local_source_id.is_some());
    let source = shell.products.source.clone();
    let resync_dialog = dialog.downgrade();
    resync_local.connect_clicked(move |_| {
        if let Some(source_id) = local_source_id.clone() {
            source.refresh_source(source_id);
        }
        if let Some(dialog) = resync_dialog.upgrade() {
            dialog.close();
        }
    });
    local_group.add(&local_actions_row);
    library_page.add(&local_group);

    library_page
}

fn add_download_rules(
    group: &adw::PreferencesGroup,
    shell: &Rc<Shell>,
    preferences_dialog: &adw::Dialog,
    source_id: &sources::SourceId,
    rules: crate::settings::DownloadRules,
) -> gtk::MenuButton {
    let mut rows = Vec::new();
    for rule in DownloadRule::ALL {
        let row = adw::ActionRow::builder()
            .title(tr(download_rule_title(rule)))
            .subtitle(tr(download_rule_subtitle(rule)))
            .build();
        row.set_visible(rules.contains(rule));
        let artwork = shell.download_subject_artwork(&downloads::DownloadSubject::Rule(rule), 48);
        artwork.set_margin_top(6);
        artwork.set_margin_bottom(6);
        row.add_prefix(&artwork);
        let more = gtk::MenuButton::new();
        more.set_icon_name("rufin-view-more-symbolic");
        more.add_css_class("flat");
        more.set_valign(gtk::Align::Center);
        more.set_tooltip_text(Some(&tr("Rule actions")));
        let menu = gio::Menu::new();
        menu.append(
            Some(&tr("Remove Rule, Keep Downloads")),
            Some("download-rule.remove"),
        );
        menu.append(
            Some(&tr("Remove Rule and Delete Downloads")),
            Some("download-rule.delete"),
        );
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        more.set_popover(Some(&popover));
        let actions = gio::SimpleActionGroup::new();
        let remove = gio::SimpleAction::new("remove", None);
        let remove_shell = Rc::clone(shell);
        let remove_source_id = source_id.clone();
        let remove_row = row.downgrade();
        remove.connect_activate(move |_, _| {
            let Some(row) = remove_row.upgrade() else {
                return;
            };
            remove_download_rule(&remove_shell, remove_source_id.clone(), rule, false, &row);
        });
        actions.add_action(&remove);
        let delete = gio::SimpleAction::new("delete", None);
        let delete_shell = Rc::clone(shell);
        let delete_source_id = source_id.clone();
        let delete_row = row.downgrade();
        let delete_dialog = preferences_dialog.downgrade();
        delete.connect_activate(move |_, _| {
            let (Some(row), Some(dialog)) = (delete_row.upgrade(), delete_dialog.upgrade()) else {
                return;
            };
            confirm_remove_download_rule(
                &delete_shell,
                &dialog,
                delete_source_id.clone(),
                rule,
                row,
            );
        });
        actions.add_action(&delete);
        more.insert_action_group("download-rule", Some(&actions));
        row.add_suffix(&more);
        group.add(&row);
        rows.push((rule, row));
    }

    let menu = gio::Menu::new();
    let actions = gio::SimpleActionGroup::new();
    for (rule, row) in rows {
        let action_name = download_rule_action_name(rule);
        menu.append(
            Some(&tr(download_rule_title(rule))),
            Some(&format!("download-rules.{action_name}")),
        );
        let action = gio::SimpleAction::new(action_name, None);
        let add_shell = Rc::clone(shell);
        let add_source_id = source_id.clone();
        let add_row = row.downgrade();
        action.connect_activate(move |_, _| {
            let Some(row) = add_row.upgrade() else {
                return;
            };
            if add_shell
                .update_app_settings("add download rule", |settings| {
                    let mut rules = settings.download_rules(&add_source_id);
                    rules.set(rule, true);
                    settings.set_download_rules(add_source_id.clone(), rules)
                })
                .is_some()
                || add_shell
                    .settings
                    .current
                    .borrow()
                    .download_rules(&add_source_id)
                    .contains(rule)
            {
                row.set_visible(true);
            }
        });
        actions.add_action(&action);
    }
    let add = gtk::MenuButton::new();
    add.set_child(Some(&row_action_content(
        "New download rule",
        "rufin-list-add-symbolic",
    )));
    add.add_css_class("flat");
    add.set_halign(gtk::Align::Fill);
    add.set_hexpand(true);
    add.set_tooltip_text(Some(&tr("New download rule")));
    add.insert_action_group("download-rules", Some(&actions));
    add.set_popover(Some(&gtk::PopoverMenu::from_model(Some(&menu))));
    add
}

fn remove_download_rule(
    shell: &Rc<Shell>,
    source_id: sources::SourceId,
    rule: DownloadRule,
    delete_downloads: bool,
    row: &adw::ActionRow,
) {
    let removed = shell
        .update_app_settings("remove download rule", |settings| {
            let mut rules = settings.download_rules(&source_id);
            rules.set(rule, false);
            settings.set_download_rules(source_id.clone(), rules)
        })
        .is_some();
    if removed {
        shell
            .products
            .downloads
            .remove_rule(source_id, rule, delete_downloads);
        row.set_visible(false);
    }
}

fn confirm_remove_download_rule(
    shell: &Rc<Shell>,
    preferences_dialog: &adw::Dialog,
    source_id: sources::SourceId,
    rule: DownloadRule,
    row: adw::ActionRow,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr("Remove Rule and Downloads"))
        .body(tr(
            "Only downloads from this rule will be deleted. Shared and manual downloads stay",
        ))
        .build();
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("remove", &tr("Remove and Delete"));
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let shell = Rc::clone(shell);
    dialog.choose(
        Some(preferences_dialog),
        None::<&gio::Cancellable>,
        move |response| {
            if response.as_str() == "remove" {
                remove_download_rule(&shell, source_id.clone(), rule, true, &row);
            }
        },
    );
}

fn download_rule_title(rule: DownloadRule) -> &'static str {
    match rule {
        DownloadRule::EntireLibrary => msgid("Entire Library"),
        DownloadRule::Favorites => msgid("Favorites"),
        DownloadRule::AllPlaylists => msgid("All Playlists"),
        DownloadRule::LatestFiveAlbums => msgid("5 Latest Albums"),
    }
}

fn download_rule_subtitle(rule: DownloadRule) -> &'static str {
    match rule {
        DownloadRule::EntireLibrary => msgid("Everything, including new tracks"),
        DownloadRule::Favorites => {
            msgid("Favorite tracks, plus tracks from favorite albums and artists")
        }
        DownloadRule::AllPlaylists => msgid("Tracks from every playlist"),
        DownloadRule::LatestFiveAlbums => msgid("Tracks from your five latest albums"),
    }
}

fn download_rule_action_name(rule: DownloadRule) -> &'static str {
    match rule {
        DownloadRule::EntireLibrary => "add-entire-library",
        DownloadRule::Favorites => "add-favorites",
        DownloadRule::AllPlaylists => "add-all-playlists",
        DownloadRule::LatestFiveAlbums => "add-latest-five-albums",
    }
}

fn add_download_queue(
    queue: &adw::PreferencesGroup,
    shell: &Rc<Shell>,
    source_id: &sources::SourceId,
    pause_downloads: &gtk::Button,
) -> gtk::Widget {
    let queue_rows = Rc::new(RefCell::new(Vec::<DownloadQueueRow>::new()));
    let focus_rows = Rc::clone(&queue_rows);
    let weak_shell = Rc::downgrade(shell);
    let weak_queue = queue.downgrade();
    let source_id = source_id.clone();
    let downloads = shell.products.downloads.clone();
    let pause_shell = Rc::downgrade(shell);
    let pause_source_id = source_id.clone();
    pause_downloads.connect_clicked(move |_| {
        let Some(shell) = pause_shell.upgrade() else {
            return;
        };
        let paused = shell
            .downloads
            .snapshots
            .borrow()
            .get(&pause_source_id)
            .is_some_and(|snapshot| snapshot.paused);
        downloads.set_paused(!paused);
    });
    let weak_pause_downloads = pause_downloads.downgrade();
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let (Some(shell), Some(queue), Some(pause_downloads)) = (
            weak_shell.upgrade(),
            weak_queue.upgrade(),
            weak_pause_downloads.upgrade(),
        ) else {
            return;
        };
        let snapshot = shell
            .downloads
            .snapshots
            .borrow()
            .get(&source_id)
            .cloned()
            .unwrap_or_default();
        pause_downloads.set_visible(!snapshot.jobs.is_empty());
        let (icon, label) = if snapshot.paused {
            ("rufin-media-playback-start-symbolic", tr("Continue"))
        } else {
            ("rufin-media-playback-pause-symbolic", tr("Pause"))
        };
        pause_downloads.set_icon_name(icon);
        pause_downloads.set_tooltip_text(Some(&label));
        pause_downloads.update_property(&[gtk::accessible::Property::Label(&label)]);
        {
            let mut rows = queue_rows.borrow_mut();
            let same_jobs = rows.len() == snapshot.jobs.len()
                && rows.iter().zip(snapshot.jobs.iter()).all(|(row, job)| {
                    row.job.as_ref().is_some_and(|rendered| {
                        rendered.id == job.id
                            && rendered.source_id == job.source_id
                            && rendered.subject == job.subject
                    })
                });
            if same_jobs && !rows.is_empty() {
                for (index, (rendered, job)) in
                    rows.iter_mut().zip(snapshot.jobs.iter()).enumerate()
                {
                    rendered
                        .row
                        .set_subtitle(&download_queue_item_subtitle(job));
                    if let Some(up) = &rendered.up {
                        up.set_sensitive(index > 0);
                    }
                    if let Some(down) = &rendered.down {
                        down.set_sensitive(index + 1 < snapshot.jobs.len());
                    }
                    rendered.job = Some(job.clone());
                }
                return;
            }
            if snapshot.jobs.is_empty()
                && rows.len() == 1
                && rows.first().is_some_and(|row| row.job.is_none())
            {
                return;
            }
            for row in rows.drain(..) {
                queue.remove(&row.row);
            }
        }
        if snapshot.jobs.is_empty() {
            let row = adw::ActionRow::builder()
                .title(tr("Nothing queued"))
                .build();
            row.set_focusable(true);
            queue.add(&row);
            queue_rows.borrow_mut().push(DownloadQueueRow {
                row,
                job: None,
                up: None,
                down: None,
            });
            return;
        }
        for (index, job) in snapshot.jobs.iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(shell.download_subject_title(&job.subject))
                .subtitle(download_queue_item_subtitle(job))
                .build();
            row.set_focusable(true);
            let drag = gtk::Image::from_icon_name("rufin-list-drag-handle-symbolic");
            drag.add_css_class("dim-label");
            drag.set_tooltip_text(Some(&tr("Drag to reorder")));
            let artwork = shell.download_subject_artwork(&job.subject, 48);
            artwork.set_margin_top(6);
            artwork.set_margin_bottom(6);
            row.add_prefix(&artwork);
            row.add_prefix(&drag);
            let up = gtk::Button::from_icon_name("rufin-go-up-symbolic");
            up.add_css_class("flat");
            up.set_valign(gtk::Align::Center);
            up.set_tooltip_text(Some(&tr("Move up")));
            up.set_sensitive(index > 0);
            let move_shell = Rc::clone(&shell);
            let job_source_id = job.source_id.clone();
            let job_id = job.id.clone();
            let target_job_id = (index > 0).then(|| snapshot.jobs[index - 1].id.clone());
            up.connect_clicked(move |_| {
                if let Some(target_job_id) = target_job_id.clone() {
                    move_shell.move_download_job(
                        job_source_id.clone(),
                        job_id.clone(),
                        target_job_id,
                        false,
                    );
                }
            });
            row.add_suffix(&up);
            let down = gtk::Button::from_icon_name("rufin-go-down-symbolic");
            down.add_css_class("flat");
            down.set_valign(gtk::Align::Center);
            down.set_tooltip_text(Some(&tr("Move down")));
            down.set_sensitive(index + 1 < snapshot.jobs.len());
            let move_shell = Rc::clone(&shell);
            let job_source_id = job.source_id.clone();
            let job_id = job.id.clone();
            let target_job_id = snapshot.jobs.get(index + 1).map(|job| job.id.clone());
            down.connect_clicked(move |_| {
                if let Some(target_job_id) = target_job_id.clone() {
                    move_shell.move_download_job(
                        job_source_id.clone(),
                        job_id.clone(),
                        target_job_id,
                        true,
                    );
                }
            });
            row.add_suffix(&down);
            let cancel = gtk::Button::from_icon_name("rufin-window-close-symbolic");
            cancel.add_css_class("flat");
            cancel.set_valign(gtk::Align::Center);
            cancel.set_tooltip_text(Some(&tr("Cancel download")));
            let downloads = shell.products.downloads.clone();
            let job_source_id = job.source_id.clone();
            let job_id = job.id.clone();
            cancel.connect_clicked(move |_| {
                downloads.cancel(job_source_id.clone(), job_id.clone());
            });
            row.add_suffix(&cancel);
            let clear = gtk::Button::from_icon_name("rufin-user-trash-symbolic");
            clear.add_css_class("flat");
            clear.add_css_class("destructive-action");
            clear.set_valign(gtk::Align::Center);
            clear.set_tooltip_text(Some(&tr("Cancel download and clear downloaded items")));
            let downloads = shell.products.downloads.clone();
            let job_source_id = job.source_id.clone();
            let job_id = job.id.clone();
            clear.connect_clicked(move |_| {
                downloads.clear_job(job_source_id.clone(), job_id.clone());
            });
            row.add_suffix(&clear);

            let drag_source = gtk::DragSource::builder()
                .actions(gtk::gdk::DragAction::MOVE)
                .build();
            let drag_id = format!("{DOWNLOAD_JOB_DRAG_PREFIX}{}", job.id);
            drag_source.connect_prepare(move |_, _, _| {
                Some(gtk::gdk::ContentProvider::for_value(&drag_id.to_value()))
            });
            drag.add_controller(drag_source);

            let drop_target =
                gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
            let move_shell = Rc::clone(&shell);
            let job_source_id = job.source_id.clone();
            let target_job_id = job.id.clone();
            let row_for_drop = row.downgrade();
            drop_target.connect_drop(move |_, value, _, y| {
                let Ok(drag_id) = value.get::<String>() else {
                    return false;
                };
                let Some(job_id) = drag_id.strip_prefix(DOWNLOAD_JOB_DRAG_PREFIX) else {
                    return false;
                };
                if job_id == target_job_id {
                    return false;
                }
                let Some(row) = row_for_drop.upgrade() else {
                    return false;
                };
                let after = y > f64::from(row.height()) / 2.0;
                move_shell.move_download_job(
                    job_source_id.clone(),
                    job_id.to_string(),
                    target_job_id.clone(),
                    after,
                )
            });
            row.add_controller(drop_target);

            queue.add(&row);
            queue_rows.borrow_mut().push(DownloadQueueRow {
                row,
                job: Some(job.clone()),
                up: Some(up),
                down: Some(down),
            });
        }
    });
    shell.downloads.set_queue_refresh(&refresh);
    refresh();
    let focus = focus_rows
        .borrow()
        .first()
        .map(|row| row.row.clone().upcast())
        .expect("the download queue always renders one row");
    queue.connect_root_notify(move |_| {
        let _ = &refresh;
    });
    focus
}

fn download_queue_item_subtitle(job: &downloads::DownloadQueueItem) -> String {
    let progress = format!(
        "{} / {}",
        job.completed_tracks.min(job.total_tracks),
        job.total_tracks
    );
    let state = match job.state {
        DownloadQueueState::Queued => tr("Queued"),
        DownloadQueueState::Downloading => tr("Downloading"),
        DownloadQueueState::WaitingForConnection => tr("Waiting for connection"),
        DownloadQueueState::NeedsAttention => tr("Needs attention"),
    };
    let quality = match job.quality {
        StreamQuality::Original => tr("Original"),
        StreamQuality::MaxBitrateKbps(value) => format!("{value} kbps"),
    };
    format!("{state} · {progress} · {quality}")
}

fn confirm_remove_all_downloads(
    shell: &Rc<Shell>,
    preferences_dialog: &adw::Dialog,
    source_id: sources::SourceId,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr("Remove All Downloads"))
        .body(tr(
            "Downloads from this server will be removed, and automatic rules will be turned off",
        ))
        .build();
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("remove", &tr("Remove"));
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let shell = Rc::clone(shell);
    let window = shell.chrome.window.clone();
    let preferences_dialog = preferences_dialog.downgrade();
    dialog.choose(Some(&window), None::<&gio::Cancellable>, move |response| {
        if response.as_str() == "remove" {
            let rules_are_empty = shell
                .settings
                .current
                .borrow()
                .download_rules(&source_id)
                .is_empty();
            let rules_disabled = rules_are_empty
                || shell
                    .update_app_settings("remove all downloads", |settings| {
                        settings
                            .set_download_rules(source_id.clone(), crate::DownloadRules::default())
                    })
                    .is_some();
            if rules_disabled {
                shell.products.downloads.clear(source_id.clone(), true);
                if let Some(preferences_dialog) = preferences_dialog.upgrade() {
                    preferences_dialog.close();
                }
            }
        }
    });
}

fn confirm_remove_local_folder(shell: &Rc<Shell>, path: String, row: adw::ActionRow) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr("Remove Local Folder"))
        .body(path.clone())
        .build();
    let cancel = tr("Cancel");
    let remove = tr("Remove");
    dialog.add_responses(&[("cancel", cancel.as_str()), ("remove", remove.as_str())]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let source = shell.products.source.clone();
    dialog.choose(
        Some(&shell.chrome.window),
        None::<&gio::Cancellable>,
        move |response| {
            if response.as_str() == "remove" {
                source.remove_local_folder(path.clone());
                row.set_visible(false);
            }
        },
    );
}

pub(crate) fn locate_local_folder(shell: &Rc<Shell>, current: String) {
    let shell = Rc::clone(shell);
    gtk::glib::spawn_future_local(async move {
        let chooser = gtk::FileDialog::builder()
            .title(tr("Select Music Folder"))
            .build();
        let Ok(folder) = chooser
            .select_folder_future(Some(&shell.chrome.window))
            .await
        else {
            return;
        };
        let Some(replacement) = folder.path() else {
            return;
        };
        shell
            .products
            .source
            .replace_local_folder(current, replacement);
    });
}

fn source_summary_subtitle(
    server: &SourceSummary,
    summary: Option<&SourceLocalAccessSummary>,
    address: Option<&str>,
    username: Option<&str>,
) -> String {
    let address = address
        .filter(|address| !address.trim().is_empty())
        .unwrap_or_default()
        .to_string();
    let folder = summary
        .and_then(|summary| summary.selected_music_folder_name.as_deref())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| tr("All Music"));
    let mapping = local_mapping_status(summary);
    let account = username
        .filter(|username| !username.trim().is_empty())
        .map(|username| format!("{}: {}", tr("User"), username))
        .unwrap_or_default();
    let cache = summary.map(source_cache_line).unwrap_or_default();
    let provider_line = metadata_line([configured_source_kind_display_name(&server.kind), address]);
    let folder_line = metadata_line([account, format!("{}: {}", tr("Music Folder"), folder)]);
    let cache_line = metadata_line([cache, mapping]);
    [provider_line, folder_line, cache_line]
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn metadata_line(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" - ")
}

fn source_cache_line(summary: &SourceLocalAccessSummary) -> String {
    format!(
        "{}: {}, {}",
        tr("Cached"),
        album_count_text(summary.album_count as u64),
        track_count_text(summary.track_count as u64)
    )
}

fn local_mapping_status(summary: Option<&SourceLocalAccessSummary>) -> String {
    let Some(summary) = summary else {
        return tr("No local file mapping");
    };
    if summary.access.is_none() {
        return tr("No local file mapping");
    }
    let status = &summary.status;
    if status.total_track_count == 0 {
        return tr("Saved, sync to preview matches");
    }
    let matched = status.matched_track_count.to_string();
    let total = status.total_track_count.to_string();
    trn_with(
        "Local file mapping: {matched} of {total} server path matches",
        "Local file mapping: {matched} of {total} server paths match",
        status.total_track_count as u64,
        &[("matched", matched.as_str()), ("total", total.as_str())],
    )
}

fn local_folder_title(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| path.to_string())
}

fn download_quality_choices(limit_kbps: Option<u32>) -> Vec<StreamQuality> {
    DOWNLOAD_QUALITIES
        .into_iter()
        .filter(|quality| {
            quality
                .max_bitrate_kbps()
                .is_none_or(|bitrate| limit_kbps.is_none_or(|limit| bitrate <= limit))
        })
        .collect()
}
