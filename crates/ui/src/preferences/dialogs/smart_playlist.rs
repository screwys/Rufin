use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use crate::layout::large_popup_content_width;
use crate::shell::Shell;
use crate::shell::actions::text_button;
use crate::shell::actions::{ADD_ICON, REMOVE_ICON};
use ::library::{
    SmartPlaylistActivityPeriod, SmartPlaylistDefinition, SmartPlaylistKey, SmartPlaylistRule,
    SmartPlaylistRuleField, SmartPlaylistRuleOperator, SmartPlaylistRuleValue,
    SmartPlaylistRuleValueKind, SmartPlaylistSort,
};
use adw::prelude::*;
use localization::{msgid, tr};

const SMART_PLAYLIST_DIALOG_WIDTH: i32 = 700;
const SMART_PLAYLIST_DIALOG_HEIGHT: i32 = 510;

#[derive(Clone)]
pub(crate) enum SmartPlaylistChange {
    Create {
        name: String,
        definition: SmartPlaylistDefinition,
    },
    Update {
        key: SmartPlaylistKey,
        name: String,
        definition: SmartPlaylistDefinition,
    },
    Delete(SmartPlaylistKey),
    Move {
        dragged: SmartPlaylistKey,
        target: SmartPlaylistKey,
        after: bool,
    },
}

impl Shell {
    pub(crate) fn publish_smart_playlist_change(
        self: &Rc<Self>,
        change: SmartPlaylistChange,
        settled: Option<Rc<dyn Fn(Result<(), String>)>>,
    ) {
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return;
        };
        let database = std::sync::Arc::clone(&selected.database);
        let source = selected.source_key;
        let epoch = selected.source_session_epoch;
        let task = selected.runtime.spawn(async move {
            let accepted = match change {
                SmartPlaylistChange::Create { name, definition } => database
                    .create_smart_playlist(source, &name, &definition)
                    .await
                    .map(|_| true),
                SmartPlaylistChange::Update {
                    key,
                    name,
                    definition,
                } => {
                    database
                        .update_smart_playlist(source, key, &name, &definition)
                        .await
                }
                SmartPlaylistChange::Delete(key) => {
                    database.delete_smart_playlist(source, key).await
                }
                SmartPlaylistChange::Move {
                    dragged,
                    target,
                    after,
                } => {
                    database
                        .move_smart_playlist(source, dragged, target, after)
                        .await
                }
            };
            accepted
                .map_err(|error| error.to_string())
                .and_then(|accepted| {
                    accepted
                        .then_some(())
                        .ok_or_else(|| "Smart Playlist is no longer current".to_string())
                })
        });
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let Some(shell) = shell.upgrade() else { return };
            let still_current = shell.selected_library().as_deref().is_some_and(|selected| {
                selected.source_key == source && selected.source_session_epoch == epoch
            });
            if result.is_ok() && still_current {
                shell.refresh_mounted_catalog();
                crate::routes::playlist_picker::refresh_context_playlist_picker(&shell);
                crate::shell::navigation::refresh_sidebar_pins(&shell);
            }
            if let Err(error) = result.as_ref() {
                shell.show_control_feedback_toast(error.clone());
            }
            if let Some(settled) = settled.as_ref() {
                settled(result);
            }
        });
    }
}

#[derive(Clone)]
struct SmartPlaylistTemplate {
    name: &'static str,
    definition: SmartPlaylistDefinition,
}

fn smart_playlist_templates() -> Vec<SmartPlaylistTemplate> {
    let most_played = |name, activity_period| SmartPlaylistTemplate {
        name,
        definition: SmartPlaylistDefinition {
            match_all: Vec::new(),
            match_any: Vec::new(),
            sort_field: SmartPlaylistSort::PlayCount,
            descending: true,
            activity_period,
            limit: Some(100),
        },
    };
    vec![
        most_played(
            msgid("Most Played (Weekly)"),
            SmartPlaylistActivityPeriod::Weekly,
        ),
        most_played(
            msgid("Most Played (Monthly)"),
            SmartPlaylistActivityPeriod::Monthly,
        ),
        most_played(
            msgid("Most Played (Yearly)"),
            SmartPlaylistActivityPeriod::Yearly,
        ),
        most_played(msgid("Most Played"), SmartPlaylistActivityPeriod::Lifetime),
        SmartPlaylistTemplate {
            name: msgid("Never Played"),
            definition: SmartPlaylistDefinition {
                match_all: vec![SmartPlaylistRule {
                    field: SmartPlaylistRuleField::Played,
                    operator: SmartPlaylistRuleOperator::Is,
                    value: Some(SmartPlaylistRuleValue::Bool(false)),
                }],
                match_any: Vec::new(),
                sort_field: SmartPlaylistSort::Title,
                descending: false,
                activity_period: SmartPlaylistActivityPeriod::Lifetime,
                limit: None,
            },
        },
    ]
}

type RerenderSlot = Rc<RefCell<Option<Weak<dyn Fn()>>>>;

#[derive(Clone, Default)]
struct RuleValueSuggestions {
    genres: Vec<String>,
    moods: Vec<String>,
}

#[derive(Clone)]
struct SmartPlaylistEditor {
    name: gtk::Entry,
    match_all: Rc<RefCell<Vec<SmartPlaylistRule>>>,
    match_any: Rc<RefCell<Vec<SmartPlaylistRule>>>,
    sort: gtk::DropDown,
    descending: gtk::CheckButton,
    activity_period: gtk::DropDown,
    limit: gtk::Entry,
}

struct TemplatePicker {
    button: gtk::Button,
    apply: Rc<dyn Fn()>,
}

impl Shell {
    pub(crate) fn new_smart_playlist_dialog(self: &Rc<Self>) {
        self.load_smart_playlist_suggestions(None);
    }

    pub(crate) fn edit_smart_playlist_dialog(self: &Rc<Self>, playlist: library::SmartPlaylistRow) {
        self.load_smart_playlist_suggestions(Some(playlist));
    }

    fn load_smart_playlist_suggestions(
        self: &Rc<Self>,
        playlist: Option<library::SmartPlaylistRow>,
    ) {
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return;
        };
        let database = std::sync::Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let task = selected.runtime.spawn(async move {
            database
                .smart_playlist_value_suggestions(source, folder, &library::ReadCancellation::new())
                .await
        });
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            let suggestions = task
                .await
                .ok()
                .and_then(Result::ok)
                .map(|values| RuleValueSuggestions {
                    genres: values.genres,
                    moods: values.moods,
                })
                .unwrap_or_default();
            if let Some(playlist) = playlist {
                shell.present_edit_smart_playlist_dialog(playlist, suggestions);
            } else {
                shell.present_new_smart_playlist_dialog(suggestions);
            }
        });
    }

    fn present_new_smart_playlist_dialog(self: &Rc<Self>, value_suggestions: RuleValueSuggestions) {
        let templates = smart_playlist_templates();
        let editor = smart_playlist_editor(None, None);
        let (content, template_picker) =
            smart_playlist_editor_content(&editor, &templates, value_suggestions);
        let actions = dialog_action_row();
        let cancel = dialog_button(msgid("Cancel"), None);
        let create = dialog_button(msgid("Create"), Some("suggested-action"));
        sync_editor_button_enabled(&create, &editor);
        actions.append(&cancel);
        actions.append(&create);

        let dialog = smart_playlist_dialog(msgid("New Smart Playlist"), &content, &actions);
        connect_editor_name_validation(&create, &editor);

        {
            let dialog = dialog.downgrade();
            cancel.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
        }

        if let Some(template_picker) = template_picker {
            let TemplatePicker { button, apply } = template_picker;
            button.connect_clicked(move |_| {
                apply();
            });
        }
        {
            let shell = Rc::clone(self);
            let dialog = dialog.downgrade();
            create.connect_clicked(move |_| {
                let Some((name, definition)) = editor.definition() else {
                    return;
                };
                let dialog = dialog.clone();
                shell.publish_smart_playlist_change(
                    SmartPlaylistChange::Create { name, definition },
                    Some(Rc::new(move |result| {
                        if result.is_ok()
                            && let Some(dialog) = dialog.upgrade()
                        {
                            dialog.close();
                        }
                    })),
                );
            });
        }
        self.present_selected_dialog(&dialog);
    }

    fn present_edit_smart_playlist_dialog(
        self: &Rc<Self>,
        playlist: library::SmartPlaylistRow,
        value_suggestions: RuleValueSuggestions,
    ) {
        let editor = smart_playlist_editor(Some(&playlist.name), Some(&playlist.definition));
        let (content, _) = smart_playlist_editor_content(&editor, &[], value_suggestions);
        let actions = dialog_action_row();
        let cancel = dialog_button(msgid("Cancel"), None);
        let save = dialog_button(msgid("Save"), Some("suggested-action"));
        sync_editor_button_enabled(&save, &editor);
        actions.append(&cancel);
        actions.append(&save);
        let dialog = smart_playlist_dialog(msgid("Edit Smart Playlist"), &content, &actions);
        connect_editor_name_validation(&save, &editor);
        let cancel_dialog = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = cancel_dialog.upgrade() {
                dialog.close();
            }
        });
        let save_dialog = dialog.downgrade();
        let shell = Rc::clone(self);
        let key = playlist.smart_playlist_key;
        save.connect_clicked(move |_| {
            let Some((name, definition)) = editor.definition() else {
                return;
            };
            let dialog = save_dialog.clone();
            shell.publish_smart_playlist_change(
                SmartPlaylistChange::Update {
                    key,
                    name,
                    definition,
                },
                Some(Rc::new(move |result| {
                    if result.is_ok()
                        && let Some(dialog) = dialog.upgrade()
                    {
                        dialog.close();
                    }
                })),
            );
        });
        self.present_selected_dialog(&dialog);
    }
}

impl RuleValueSuggestions {
    fn for_field(&self, field: SmartPlaylistRuleField) -> Option<&[String]> {
        match field {
            SmartPlaylistRuleField::Genre => Some(&self.genres),
            SmartPlaylistRuleField::Mood => Some(&self.moods),
            _ => None,
        }
    }
}

impl SmartPlaylistEditor {
    fn definition(&self) -> Option<(String, SmartPlaylistDefinition)> {
        let name = playlist_name(&self.name.text())?;
        let limit = self
            .limit
            .text()
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0);
        let sort_field = SmartPlaylistSort::ALL
            .get(self.sort.selected() as usize)
            .copied()
            .unwrap_or(SmartPlaylistSort::Title);
        let definition = SmartPlaylistDefinition {
            match_all: self.match_all.borrow().clone(),
            match_any: self.match_any.borrow().clone(),
            sort_field,
            descending: self.descending.is_active(),
            activity_period: [
                SmartPlaylistActivityPeriod::Weekly,
                SmartPlaylistActivityPeriod::Monthly,
                SmartPlaylistActivityPeriod::Yearly,
                SmartPlaylistActivityPeriod::Lifetime,
            ]
            .get(self.activity_period.selected() as usize)
            .copied()
            .unwrap_or(SmartPlaylistActivityPeriod::Lifetime),
            limit,
        };
        Some((name, definition))
    }
}

fn smart_playlist_editor(
    name: Option<&str>,
    definition: Option<&SmartPlaylistDefinition>,
) -> SmartPlaylistEditor {
    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some(&tr("Playlist name")));
    if let Some(name) = name {
        name_entry.set_text(name);
    }

    let definition = definition.cloned().unwrap_or_default();
    let sort_labels = sort_labels();
    let sort = dropdown_from_labels(&sort_labels, sort_index(definition.sort_field));
    let descending = gtk::CheckButton::with_label(&tr("Descending"));
    descending.set_active(definition.descending);
    let limit = gtk::Entry::new();
    limit.set_placeholder_text(Some(&tr("No limit")));
    limit.set_width_chars(8);
    if let Some(value) = definition.limit {
        limit.set_text(&value.to_string());
    }
    SmartPlaylistEditor {
        name: name_entry,
        match_all: Rc::new(RefCell::new(definition.match_all)),
        match_any: Rc::new(RefCell::new(definition.match_any)),
        sort,
        descending,
        activity_period: dropdown_from_titles(
            &["Weekly", "Monthly", "Yearly", "Lifetime"],
            match definition.activity_period {
                SmartPlaylistActivityPeriod::Weekly => 0,
                SmartPlaylistActivityPeriod::Monthly => 1,
                SmartPlaylistActivityPeriod::Yearly => 2,
                SmartPlaylistActivityPeriod::Lifetime => 3,
            },
        ),
        limit,
    }
}

fn smart_playlist_editor_content(
    editor: &SmartPlaylistEditor,
    templates: &[SmartPlaylistTemplate],
    value_suggestions: RuleValueSuggestions,
) -> (gtk::Widget, Option<TemplatePicker>) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.set_margin_top(4);
    content.set_margin_bottom(4);
    content.set_margin_start(4);
    content.set_margin_end(4);

    let template_controls = if templates.is_empty() {
        None
    } else {
        let default_titles = templates
            .iter()
            .map(|template| tr(template.name))
            .collect::<Vec<_>>();
        let default_refs = default_titles
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let default_dropdown = dropdown_from_titles(&default_refs, 0);
        default_dropdown.set_hexpand(true);
        let apply = dialog_button(msgid("Apply Template"), None);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&default_dropdown);
        row.append(&apply);
        content.append(&labeled_control(msgid("Template"), &row));
        Some((default_dropdown, apply))
    };

    content.append(&editor.name);

    let settings = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    settings.append(&labeled_control(msgid("Sort"), &editor.sort));
    settings.append(&labeled_control(msgid("Direction"), &editor.descending));
    settings.append(&labeled_control(msgid("Activity"), &editor.activity_period));
    settings.append(&labeled_control(msgid("Limit"), &editor.limit));
    content.append(&settings);

    let rules = gtk::Box::new(gtk::Orientation::Vertical, 10);
    rules.set_hexpand(true);
    let value_suggestions = Rc::new(value_suggestions);
    let rerender_slot: RerenderSlot = Rc::new(RefCell::new(None));
    let rerender: Rc<dyn Fn()> = {
        let rules = rules.downgrade();
        let match_all = Rc::clone(&editor.match_all);
        let match_any = Rc::clone(&editor.match_any);
        let value_suggestions = Rc::clone(&value_suggestions);
        let rerender_slot = Rc::clone(&rerender_slot);
        Rc::new(move || {
            let Some(rules) = rules.upgrade() else {
                return;
            };
            clear_box(&rules);
            let Some(rerender) = rerender_slot.borrow().as_ref().and_then(Weak::upgrade) else {
                return;
            };
            append_rule_list(
                &rules,
                msgid("All"),
                Rc::clone(&match_all),
                Rc::clone(&value_suggestions),
                Rc::clone(&rerender),
            );
            append_rule_list(
                &rules,
                msgid("Any"),
                Rc::clone(&match_any),
                Rc::clone(&value_suggestions),
                rerender,
            );
        })
    };
    *rerender_slot.borrow_mut() = Some(Rc::downgrade(&rerender));
    rerender();
    let template_picker = template_controls.map(|(dropdown, button)| {
        let templates = templates.to_vec();
        let editor = editor.clone();
        let rerender = Rc::clone(&rerender);
        TemplatePicker {
            button,
            apply: Rc::new(move || {
                let Some(template) = templates.get(dropdown.selected() as usize) else {
                    return;
                };
                editor.name.set_text(&tr(template.name));
                editor
                    .match_all
                    .replace(template.definition.match_all.clone());
                editor
                    .match_any
                    .replace(template.definition.match_any.clone());
                editor
                    .sort
                    .set_selected(sort_index(template.definition.sort_field) as u32);
                editor.descending.set_active(template.definition.descending);
                editor
                    .activity_period
                    .set_selected(match template.definition.activity_period {
                        SmartPlaylistActivityPeriod::Weekly => 0,
                        SmartPlaylistActivityPeriod::Monthly => 1,
                        SmartPlaylistActivityPeriod::Yearly => 2,
                        SmartPlaylistActivityPeriod::Lifetime => 3,
                    });
                editor.limit.set_text(
                    &template
                        .definition
                        .limit
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                );
                rerender();
            }),
        }
    });
    content.append(&rules);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_max_content_height(SMART_PLAYLIST_DIALOG_HEIGHT);
    scroller.set_child(Some(&content));
    (scroller.upcast(), template_picker)
}

fn append_rule_list(
    parent: &gtk::Box,
    title: &'static str,
    rules: Rc<RefCell<Vec<SmartPlaylistRule>>>,
    value_suggestions: Rc<RuleValueSuggestions>,
    rerender: Rc<dyn Fn()>,
) {
    let frame = gtk::Frame::new(None);
    frame.set_hexpand(true);
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.set_margin_top(10);
    box_.set_margin_bottom(10);
    box_.set_margin_start(10);
    box_.set_margin_end(10);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_hexpand(true);
    let title = gtk::Label::new(Some(&tr(title)));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    header.append(&title);

    let add_rule = text_button(ADD_ICON, "Add Rule");
    {
        let rules = Rc::clone(&rules);
        let rerender = Rc::clone(&rerender);
        add_rule.connect_clicked(move |_| {
            rules
                .borrow_mut()
                .push(SmartPlaylistRuleField::Title.default_rule());
            rerender();
        });
    }
    header.append(&add_rule);
    box_.append(&header);

    let current_rules = rules.borrow().clone();
    for (index, rule) in current_rules.into_iter().enumerate() {
        append_rule_row(
            &box_,
            Rc::clone(&rules),
            Rc::clone(&value_suggestions),
            index,
            rule,
            Rc::clone(&rerender),
        );
    }

    frame.set_child(Some(&box_));
    parent.append(&frame);
}

fn append_rule_row(
    parent: &gtk::Box,
    rules: Rc<RefCell<Vec<SmartPlaylistRule>>>,
    value_suggestions: Rc<RuleValueSuggestions>,
    index: usize,
    rule: SmartPlaylistRule,
    rerender: Rc<dyn Fn()>,
) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_hexpand(true);

    let field_labels = field_labels();
    let field = dropdown_from_labels(&field_labels, field_index(rule.field));
    field.set_hexpand(false);
    field.set_size_request(150, -1);
    {
        let rules = Rc::clone(&rules);
        let rerender = Rc::clone(&rerender);
        field.connect_selected_notify(move |dropdown| {
            let selected = SmartPlaylistRuleField::ALL
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or(SmartPlaylistRuleField::Title);
            if let Some(rule) = rules.borrow_mut().get_mut(index) {
                *rule = selected.default_rule();
            }
            rerender();
        });
    }
    row.append(&field);

    let operators = rule.field.operators();
    let operator_titles = op_labels(rule.field, operators);
    let operator = dropdown_from_labels(&operator_titles, operator_index(operators, rule.operator));
    operator.set_size_request(150, -1);
    {
        let rules = Rc::clone(&rules);
        let rerender = Rc::clone(&rerender);
        operator.connect_selected_notify(move |dropdown| {
            change_rule_operator(&rules, index, dropdown.selected(), || {
                rerender();
            });
        });
    }
    row.append(&operator);

    let value_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    value_box.set_hexpand(true);
    append_value_editor(
        &value_box,
        Rc::clone(&rules),
        &value_suggestions,
        index,
        &rule,
    );
    row.append(&value_box);

    let remove = gtk::Button::from_icon_name(REMOVE_ICON);
    remove.add_css_class("flat");
    remove.set_tooltip_text(Some(&tr("Remove rule")));
    {
        let rules = Rc::clone(&rules);
        let rerender = Rc::clone(&rerender);
        remove.connect_clicked(move |_| {
            remove_rule(&rules, index, || rerender());
        });
    }
    row.append(&remove);
    parent.append(&row);
}

fn append_value_editor(
    container: &gtk::Box,
    rules: Rc<RefCell<Vec<SmartPlaylistRule>>>,
    value_suggestions: &RuleValueSuggestions,
    index: usize,
    rule: &SmartPlaylistRule,
) {
    match rule
        .field
        .value_kind(rule.operator)
        .unwrap_or(SmartPlaylistRuleValueKind::None)
    {
        SmartPlaylistRuleValueKind::None => {
            let label = gtk::Label::new(None);
            label.set_hexpand(true);
            container.append(&label);
        }
        SmartPlaylistRuleValueKind::Text => {
            if let Some(suggestions) = value_suggestions.for_field(rule.field)
                && !suggestions.is_empty()
            {
                let (labels, selected) = rule_value_labels(suggestions, rule);
                let dropdown = searchable_dropdown_from_labels(&labels, selected);
                dropdown.set_hexpand(true);
                if let Some(value) = labels.get(selected).cloned()
                    && let Some(rule) = rules.borrow_mut().get_mut(index)
                {
                    rule.value = Some(SmartPlaylistRuleValue::Text(value));
                }
                dropdown.connect_selected_notify(move |dropdown| {
                    if let Some(value) = labels.get(dropdown.selected() as usize)
                        && let Some(rule) = rules.borrow_mut().get_mut(index)
                    {
                        rule.value = Some(SmartPlaylistRuleValue::Text(value.clone()));
                    }
                });
                container.append(&dropdown);
            } else {
                let entry = gtk::Entry::new();
                entry.set_hexpand(true);
                entry.set_placeholder_text(Some(&text_placeholder(rule.field)));
                if let Some(SmartPlaylistRuleValue::Text(value)) = rule.value.as_ref() {
                    entry.set_text(value);
                }
                entry.connect_changed(move |entry| {
                    if let Some(rule) = rules.borrow_mut().get_mut(index) {
                        rule.value = Some(SmartPlaylistRuleValue::Text(entry.text().to_string()));
                    }
                });
                container.append(&entry);
            }
        }
        SmartPlaylistRuleValueKind::Number => {
            let (min, max, default) = rule.field.number_bounds();
            let value = match rule.value.as_ref() {
                Some(SmartPlaylistRuleValue::Number(value)) => *value,
                _ => default,
            };
            let rating = rule.field == SmartPlaylistRuleField::Rating;
            let spin = number_spin(value, min, max, rating);
            spin.connect_value_changed(move |spin| {
                if let Some(rule) = rules.borrow_mut().get_mut(index) {
                    rule.value = Some(SmartPlaylistRuleValue::Number(spin_value(spin, rating)));
                }
            });
            container.append(&spin);
        }
        SmartPlaylistRuleValueKind::NumberRange => {
            let (min_bound, max_bound, default) = rule.field.number_bounds();
            let (min_value, max_value) = match rule.value.as_ref() {
                Some(SmartPlaylistRuleValue::NumberRange { min, max }) => (*min, *max),
                _ => (default, default),
            };
            let rating = rule.field == SmartPlaylistRuleField::Rating;
            let min_spin = number_spin(min_value, min_bound, max_bound, rating);
            let max_spin = number_spin(max_value, min_bound, max_bound, rating);
            connect_number_range(rules, index, min_spin.clone(), max_spin.clone(), rating);
            container.append(&min_spin);
            container.append(&gtk::Label::new(Some(&tr("to"))));
            container.append(&max_spin);
        }
        SmartPlaylistRuleValueKind::Date => {
            let entry = gtk::Entry::new();
            entry.set_hexpand(true);
            entry.set_placeholder_text(Some("YYYY-MM-DD"));
            if let Some(SmartPlaylistRuleValue::Date(value)) = rule.value.as_ref() {
                entry.set_text(value);
            }
            entry.connect_changed(move |entry| {
                if let Some(rule) = rules.borrow_mut().get_mut(index) {
                    rule.value = Some(SmartPlaylistRuleValue::Date(entry.text().to_string()));
                }
            });
            container.append(&entry);
        }
        SmartPlaylistRuleValueKind::DateRange => {
            let start = gtk::Entry::new();
            let end = gtk::Entry::new();
            start.set_placeholder_text(Some("YYYY-MM-DD"));
            end.set_placeholder_text(Some("YYYY-MM-DD"));
            start.set_hexpand(true);
            end.set_hexpand(true);
            if let Some(SmartPlaylistRuleValue::DateRange { start: s, end: e }) =
                rule.value.as_ref()
            {
                start.set_text(s);
                end.set_text(e);
            }
            connect_date_range(rules, index, start.clone(), end.clone());
            container.append(&start);
            container.append(&gtk::Label::new(Some(&tr("to"))));
            container.append(&end);
        }
        SmartPlaylistRuleValueKind::Bool => {
            let active = matches!(rule.value, Some(SmartPlaylistRuleValue::Bool(true)));
            let dropdown = dropdown_from_titles(&[msgid("Yes"), msgid("No")], usize::from(!active));
            dropdown.connect_selected_notify(move |dropdown| {
                if let Some(rule) = rules.borrow_mut().get_mut(index) {
                    rule.value = Some(SmartPlaylistRuleValue::Bool(dropdown.selected() == 0));
                }
            });
            container.append(&dropdown);
        }
    }
}

fn connect_number_range(
    rules: Rc<RefCell<Vec<SmartPlaylistRule>>>,
    index: usize,
    min_spin: gtk::SpinButton,
    max_spin: gtk::SpinButton,
    rating: bool,
) {
    let max_for_min = max_spin.downgrade();
    let min_rules = Rc::clone(&rules);
    min_spin.connect_value_changed(move |min_spin| {
        let Some(max_spin) = max_for_min.upgrade() else {
            return;
        };
        update_number_range(&min_rules, index, min_spin, &max_spin, rating);
    });

    let min_for_max = min_spin.downgrade();
    max_spin.connect_value_changed(move |max_spin| {
        let Some(min_spin) = min_for_max.upgrade() else {
            return;
        };
        update_number_range(&rules, index, &min_spin, max_spin, rating);
    });
}

fn connect_date_range(
    rules: Rc<RefCell<Vec<SmartPlaylistRule>>>,
    index: usize,
    start: gtk::Entry,
    end: gtk::Entry,
) {
    let end_for_start = end.downgrade();
    let start_rules = Rc::clone(&rules);
    start.connect_changed(move |start| {
        let Some(end) = end_for_start.upgrade() else {
            return;
        };
        update_date_range(&start_rules, index, start, &end);
    });

    let start_for_end = start.downgrade();
    end.connect_changed(move |end| {
        let Some(start) = start_for_end.upgrade() else {
            return;
        };
        update_date_range(&rules, index, &start, end);
    });
}

fn update_number_range(
    rules: &Rc<RefCell<Vec<SmartPlaylistRule>>>,
    index: usize,
    min_spin: &gtk::SpinButton,
    max_spin: &gtk::SpinButton,
    rating: bool,
) {
    if let Some(rule) = rules.borrow_mut().get_mut(index) {
        rule.value = Some(SmartPlaylistRuleValue::NumberRange {
            min: spin_value(min_spin, rating),
            max: spin_value(max_spin, rating),
        });
    }
}

fn update_date_range(
    rules: &Rc<RefCell<Vec<SmartPlaylistRule>>>,
    index: usize,
    start: &gtk::Entry,
    end: &gtk::Entry,
) {
    if let Some(rule) = rules.borrow_mut().get_mut(index) {
        rule.value = Some(SmartPlaylistRuleValue::DateRange {
            start: start.text().to_string(),
            end: end.text().to_string(),
        });
    }
}

fn rule_value_labels(suggestions: &[String], rule: &SmartPlaylistRule) -> (Vec<String>, usize) {
    let current = match rule.value.as_ref() {
        Some(SmartPlaylistRuleValue::Text(value)) if !value.is_empty() => Some(value.as_str()),
        _ => None,
    };
    let mut labels = suggestions.to_vec();
    let selected = current
        .and_then(|value| labels.iter().position(|candidate| candidate == value))
        .unwrap_or(0);
    if let Some(value) = current
        && !labels.iter().any(|candidate| candidate == value)
    {
        labels.push(value.to_string());
        let selected = labels.len() - 1;
        return (labels, selected);
    }
    (labels, selected)
}

fn change_rule_operator(
    rules: &Rc<RefCell<Vec<SmartPlaylistRule>>>,
    index: usize,
    selected: u32,
    after_change: impl FnOnce(),
) {
    {
        let mut rules = rules.borrow_mut();
        let Some(rule) = rules.get_mut(index) else {
            return;
        };
        let operators = rule.field.operators();
        let Some(operator) = operators
            .get(selected as usize)
            .copied()
            .or_else(|| operators.first().copied())
        else {
            return;
        };
        rule.operator = operator;
        rule.value = rule.field.default_value(operator);
    }
    after_change();
}

fn remove_rule(
    rules: &Rc<RefCell<Vec<SmartPlaylistRule>>>,
    index: usize,
    after_change: impl FnOnce(),
) {
    {
        let mut rules = rules.borrow_mut();
        if index < rules.len() {
            rules.remove(index);
        }
    }
    after_change();
}

fn field_labels() -> Vec<String> {
    SmartPlaylistRuleField::ALL
        .iter()
        .map(|field| tr(field_title(*field)))
        .collect()
}

fn sort_labels() -> Vec<String> {
    SmartPlaylistSort::ALL
        .iter()
        .map(|field| tr(sort_title(*field)))
        .collect()
}

fn op_labels(
    field: SmartPlaylistRuleField,
    operators: &[SmartPlaylistRuleOperator],
) -> Vec<String> {
    operators
        .iter()
        .map(|operator| tr(op_title(field, *operator)))
        .collect()
}

fn field_title(field: SmartPlaylistRuleField) -> &'static str {
    match field {
        SmartPlaylistRuleField::Title => msgid("Title"),
        SmartPlaylistRuleField::Artist => msgid("Artist"),
        SmartPlaylistRuleField::Album => msgid("Album"),
        SmartPlaylistRuleField::Comment => msgid("Comment"),
        SmartPlaylistRuleField::Genre => msgid("Genre"),
        SmartPlaylistRuleField::Mood => msgid("Mood"),
        SmartPlaylistRuleField::Bpm => msgid("BPM"),
        SmartPlaylistRuleField::Rating => msgid("Rating"),
        SmartPlaylistRuleField::Year => msgid("Year"),
        SmartPlaylistRuleField::Favorite => msgid("Favorite"),
        SmartPlaylistRuleField::Played => msgid("Played"),
        SmartPlaylistRuleField::PlayCount => msgid("Play count"),
        SmartPlaylistRuleField::SkipCount => msgid("Skip count"),
        SmartPlaylistRuleField::LastPlayed => msgid("Last played"),
        SmartPlaylistRuleField::DateAdded => msgid("Date added"),
    }
}

fn sort_title(field: SmartPlaylistSort) -> &'static str {
    match field {
        SmartPlaylistSort::Title => msgid("Title"),
        SmartPlaylistSort::Artist => msgid("Artist"),
        SmartPlaylistSort::Album => msgid("Album"),
        SmartPlaylistSort::Year => msgid("Year"),
        SmartPlaylistSort::DateAdded => msgid("Date added"),
        SmartPlaylistSort::LastPlayed => msgid("Last played"),
        SmartPlaylistSort::PlayCount => msgid("Play count"),
        SmartPlaylistSort::SkipCount => msgid("Skip count"),
        SmartPlaylistSort::Bpm => msgid("BPM"),
        SmartPlaylistSort::Rating => msgid("Rating"),
        SmartPlaylistSort::Duration => msgid("Duration"),
    }
}

fn op_title(field: SmartPlaylistRuleField, operator: SmartPlaylistRuleOperator) -> &'static str {
    match (field, operator) {
        (
            SmartPlaylistRuleField::Genre | SmartPlaylistRuleField::Mood,
            SmartPlaylistRuleOperator::NotContains,
        ) => msgid("excludes"),
        (
            SmartPlaylistRuleField::Genre | SmartPlaylistRuleField::Mood,
            SmartPlaylistRuleOperator::NotEquals,
        ) => msgid("is not"),
        (_, SmartPlaylistRuleOperator::Contains) => msgid("contains"),
        (_, SmartPlaylistRuleOperator::NotContains) => msgid("does not contain"),
        (_, SmartPlaylistRuleOperator::Equals) => msgid("equals"),
        (_, SmartPlaylistRuleOperator::NotEquals) => msgid("does not equal"),
        (_, SmartPlaylistRuleOperator::Above) => msgid("above"),
        (_, SmartPlaylistRuleOperator::Below) => msgid("below"),
        (_, SmartPlaylistRuleOperator::Between) => msgid("range"),
        (_, SmartPlaylistRuleOperator::Is) => msgid("is"),
        (_, SmartPlaylistRuleOperator::IsNot) => msgid("is not"),
        (_, SmartPlaylistRuleOperator::Before) => msgid("before"),
        (_, SmartPlaylistRuleOperator::After) => msgid("after"),
        (_, SmartPlaylistRuleOperator::IsEmpty) => msgid("is empty"),
        (_, SmartPlaylistRuleOperator::IsNotEmpty) => msgid("is not empty"),
    }
}

fn dropdown_from_titles(titles: &[&str], selected: usize) -> gtk::DropDown {
    let labels = titles.iter().map(|title| tr(title)).collect::<Vec<_>>();
    dropdown_from_labels(&labels, selected)
}

fn dropdown_from_labels(labels: &[String], selected: usize) -> gtk::DropDown {
    let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let model = gtk::StringList::new(&refs);
    let dropdown = gtk::DropDown::new(Some(model), None::<gtk::Expression>);
    dropdown.set_selected(selected as u32);
    dropdown
}

fn searchable_dropdown_from_labels(labels: &[String], selected: usize) -> gtk::DropDown {
    let dropdown = dropdown_from_labels(labels, selected);
    dropdown.set_enable_search(true);
    dropdown
}

fn field_index(field: SmartPlaylistRuleField) -> usize {
    SmartPlaylistRuleField::ALL
        .iter()
        .position(|candidate| *candidate == field)
        .unwrap_or(0)
}

fn operator_index(
    operators: &[SmartPlaylistRuleOperator],
    operator: SmartPlaylistRuleOperator,
) -> usize {
    operators
        .iter()
        .position(|candidate| *candidate == operator)
        .unwrap_or(0)
}

fn sort_index(sort: SmartPlaylistSort) -> usize {
    SmartPlaylistSort::ALL
        .iter()
        .position(|field| *field == sort)
        .unwrap_or(0)
}

fn number_spin(value: i64, min: i64, max: i64, rating: bool) -> gtk::SpinButton {
    let scale = if rating { 2.0 } else { 1.0 };
    let adjustment = gtk::Adjustment::new(
        value as f64 / scale,
        min as f64 / scale,
        max as f64 / scale,
        1.0 / scale,
        10.0 / scale,
        0.0,
    );
    let spin = gtk::SpinButton::new(Some(&adjustment), 1.0 / scale, u32::from(rating));
    spin.set_numeric(true);
    spin.set_width_chars(7);
    spin
}

fn spin_value(spin: &gtk::SpinButton, rating: bool) -> i64 {
    (spin.value() * if rating { 2.0 } else { 1.0 }).round() as i64
}

fn text_placeholder(field: SmartPlaylistRuleField) -> String {
    match field {
        SmartPlaylistRuleField::Genre => tr("Genre"),
        SmartPlaylistRuleField::Mood => tr("Mood"),
        SmartPlaylistRuleField::Comment => tr("Comment text"),
        SmartPlaylistRuleField::Artist => tr("Artist name"),
        SmartPlaylistRuleField::Album => tr("Album title"),
        _ => tr("Text"),
    }
}

fn labeled_control(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Widget {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let label = gtk::Label::new(Some(&tr(label)));
    label.add_css_class("muted");
    label.set_xalign(0.0);
    box_.append(&label);
    box_.append(widget);
    box_.upcast()
}

fn clear_box(box_: &gtk::Box) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}

fn playlist_name(value: &str) -> Option<String> {
    let name = value.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn smart_playlist_dialog(
    title: &str,
    content: &impl IsA<gtk::Widget>,
    actions: &impl IsA<gtk::Widget>,
) -> adw::Dialog {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr(title), "")));
    toolbar.add_top_bar(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_vexpand(true);
    body.append(content);
    body.append(actions);
    toolbar.set_content(Some(&body));

    adw::Dialog::builder()
        .title(tr(title))
        .content_width(large_popup_content_width(SMART_PLAYLIST_DIALOG_WIDTH))
        .content_height(SMART_PLAYLIST_DIALOG_HEIGHT)
        .child(&toolbar)
        .build()
}

fn dialog_action_row() -> gtk::Box {
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.set_margin_top(12);
    actions.set_margin_bottom(14);
    actions.set_margin_start(18);
    actions.set_margin_end(18);
    actions
}

fn dialog_button(label: &str, css_class: Option<&str>) -> gtk::Button {
    let button = gtk::Button::with_label(&tr(label));
    if let Some(css_class) = css_class {
        button.add_css_class(css_class);
    }
    button
}

fn sync_editor_button_enabled(button: &gtk::Button, editor: &SmartPlaylistEditor) {
    button.set_sensitive(playlist_name(&editor.name.text()).is_some());
}

fn connect_editor_name_validation(button: &gtk::Button, editor: &SmartPlaylistEditor) {
    let button = button.downgrade();
    editor.name.connect_changed(move |name| {
        let Some(button) = button.upgrade() else {
            return;
        };
        button.set_sensitive(playlist_name(&name.text()).is_some());
    });
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::{change_rule_operator, remove_rule};
    use ::library::{
        SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleOperator,
        SmartPlaylistRuleValue,
    };

    #[test]
    fn new_read_state() {
        let rules = Rc::new(RefCell::new(vec![SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text("needle".to_string())),
        }]));
        let rerendered = std::cell::Cell::new(false);

        change_rule_operator(&rules, 0, 4, || {
            rerendered.set(true);
            assert!(
                rules.try_borrow().is_ok(),
                "rerender needs to read the editor state after the operator changes"
            );
        });

        assert!(rerendered.get());
        let rules = rules.borrow();
        assert_eq!(rules[0].operator, SmartPlaylistRuleOperator::IsEmpty);
        assert_eq!(rules[0].value, None);
    }

    #[test]
    fn new_remove_rule_releases_state_before_rerender() {
        let rules = Rc::new(RefCell::new(vec![
            SmartPlaylistRule {
                field: SmartPlaylistRuleField::Title,
                operator: SmartPlaylistRuleOperator::Contains,
                value: Some(SmartPlaylistRuleValue::Text("needle".to_string())),
            },
            SmartPlaylistRule {
                field: SmartPlaylistRuleField::Genre,
                operator: SmartPlaylistRuleOperator::Equals,
                value: Some(SmartPlaylistRuleValue::Text("Rock".to_string())),
            },
        ]));
        let rerendered = std::cell::Cell::new(false);

        remove_rule(&rules, 0, || {
            rerendered.set(true);
            assert!(
                rules.try_borrow().is_ok(),
                "rerender needs to read the editor state after a rule is removed"
            );
        });

        assert!(rerendered.get());
        let rules = rules.borrow();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].field, SmartPlaylistRuleField::Genre);
    }
}
