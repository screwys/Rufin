use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
    time::Duration,
};

use crate::shell::shell::Shell;
use crate::widgets::controls::sort_order_icon;
use crate::widgets::forms::{Suggestion, attach_suggestions};
use crate::widgets::layout::{AllocationOwner, large_popup_content_width, width_allocation_owner};
use ::library::{
    SmartPlaylistActivityPeriod, SmartPlaylistCondition, SmartPlaylistDefinition,
    SmartPlaylistMatch, SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleValue,
    SmartPlaylistRuleValueKind, SmartPlaylistSort, SmartPlaylistValueChoice,
};
use adw::prelude::*;
use gtk::{gio, glib};
use localization::{msgid, tr, track_count_text};
use rufin_core::playlists::{SmartPlaylistChange, SmartPlaylistTemplate};
use rufin_core::settings::presentation::{
    smart_field_section_title, smart_match_title, smart_number_scale, smart_operator_title,
    smart_period_title, smart_rule_field_title, smart_sort_title, smart_value_placeholder,
};

const SMART_PLAYLIST_DIALOG_WIDTH: i32 = 700;
const SMART_PLAYLIST_EDIT_WIDTH: i32 = 1000;
const SMART_PLAYLIST_ARTWORK_WIDTH: i32 = 320;
/// The narrowest artwork beside the rules; narrower, it goes above them.
const SMART_PLAYLIST_ARTWORK_MIN_WIDTH: i32 = 200;
/// How long edits settle before the header counts the tracks again.
const RECOUNT_DELAY: Duration = Duration::from_millis(250);
/// The match dropdowns' choices, in order.
const MATCHES: [SmartPlaylistMatch; 2] = [SmartPlaylistMatch::All, SmartPlaylistMatch::Any];

/// Where a rule is: its condition, and its place in that condition's group.
#[derive(Clone, Copy)]
struct RulePath {
    condition: usize,
    rule: Option<usize>,
}

/// The parts of a rule row that move when the list is too narrow for the row on one line.
struct RuleRow {
    row: gtk::ListBoxRow,
    grid: gtk::Grid,
    field: gtk::DropDown,
    operator: gtk::DropDown,
    value: gtk::Box,
    remove: gtk::Button,
    /// The group's row and box, for a rule in a group.
    group: Option<(gtk::ListBoxRow, gtk::Box)>,
}

fn minimum_width(widget: &impl IsA<gtk::Widget>) -> i32 {
    widget.measure(gtk::Orientation::Horizontal, -1).0
}

fn natural_width(widget: &impl IsA<gtk::Widget>) -> i32 {
    widget.measure(gtk::Orientation::Horizontal, -1).1
}

impl RuleRow {
    /// The width around the grid: its margins, the rows' padding, and the group's margins.
    fn chrome(&self) -> i32 {
        let group = self.group.as_ref().map_or(0, |(row, group)| {
            minimum_width(row) - minimum_width(group) + group.margin_start() + group.margin_end()
        });
        self.grid.margin_start() + self.grid.margin_end() + minimum_width(&self.row)
            - minimum_width(&self.grid)
            + group
    }

    /// The spacing between the grid's four columns.
    fn spacing(&self) -> i32 {
        3 * self.grid.column_spacing() as i32
    }

    /// The list width the row needs on one line.
    fn line_width(&self) -> i32 {
        natural_width(&self.field)
            + natural_width(&self.operator)
            + natural_width(&self.value)
            + natural_width(&self.remove)
            + self.spacing()
            + self.chrome()
    }

    /// The list width the row needs with the value on its own line.
    fn wrapped_width(&self) -> i32 {
        (minimum_width(&self.field)
            + minimum_width(&self.operator)
            + minimum_width(&self.remove)
            + self.spacing())
        .max(minimum_width(&self.value))
            + self.chrome()
    }

    /// Puts the value between the operator and the remove button, or on its own line under them.
    fn place(&self, narrow: bool) {
        let value = self
            .grid
            .layout_manager()
            .expect("grid layout")
            .layout_child(&self.value)
            .downcast::<gtk::GridLayoutChild>()
            .expect("grid layout child");
        value.set_row(i32::from(narrow));
        value.set_column(if narrow { 0 } else { 2 });
        value.set_column_span(if narrow { 4 } else { 1 });
        self.remove.set_hexpand(narrow);
    }
}

struct Editor {
    me: Weak<Editor>,
    shell: Weak<Shell>,
    title: adw::WindowTitle,
    name: gtk::Entry,
    matching: gtk::DropDown,
    list: gtk::ListBox,
    /// Gives the list its width and wraps the rows to it.
    list_owner: AllocationOwner,
    sort: gtk::DropDown,
    direction: gtk::Button,
    descending: Cell<bool>,
    limit: gtk::Entry,
    period: gtk::DropDown,
    period_row: adw::ActionRow,
    source: gtk::DropDown,
    source_ids: Vec<Option<library::SourceId>>,
    conditions: RefCell<Vec<SmartPlaylistCondition>>,
    rows: RefCell<Vec<RuleRow>>,
    narrow: Cell<bool>,
    pending_count: RefCell<Option<glib::SourceId>>,
    count_request: Cell<u64>,
}

impl Editor {
    fn source_id(&self) -> Option<library::SourceId> {
        self.source_ids
            .get(self.source.selected() as usize)
            .cloned()
            .flatten()
    }

    /// The playlist as edited so far; a limit that does not save counts as none.
    fn definition(&self) -> SmartPlaylistDefinition {
        SmartPlaylistDefinition {
            source_id: self.source_id(),
            matching: MATCHES
                .get(self.matching.selected() as usize)
                .copied()
                .unwrap_or_default(),
            conditions: self.conditions.borrow().clone(),
            sort_field: SmartPlaylistSort::ALL
                .get(self.sort.selected() as usize)
                .copied()
                .unwrap_or(SmartPlaylistSort::Title),
            descending: self.descending.get(),
            activity_period: SmartPlaylistActivityPeriod::ALL
                .get(self.period.selected() as usize)
                .copied()
                .unwrap_or_default(),
            limit: playlist_limit(&self.limit.text()).flatten(),
        }
    }

    /// The name and definition to save, when they can be.
    fn saved(&self) -> Option<(String, SmartPlaylistDefinition)> {
        let name = playlist_name(&self.name.text())?;
        playlist_limit(&self.limit.text())?;
        Some((name, self.definition()))
    }

    fn set_descending(&self, descending: bool) {
        self.descending.set(descending);
        self.direction.set_icon_name(sort_order_icon(descending));
    }

    /// Rebuilds the rule list from the conditions.
    fn render(&self) {
        self.list.remove_all();
        self.rows.borrow_mut().clear();
        let conditions = self.conditions.borrow().clone();
        for (index, condition) in conditions.iter().enumerate() {
            let row = match condition {
                SmartPlaylistCondition::Rule(rule) => self.rule_row(
                    RulePath {
                        condition: index,
                        rule: None,
                    },
                    rule,
                    None,
                ),
                SmartPlaylistCondition::Group { matching, rules } => {
                    self.group_row(index, *matching, rules)
                }
            };
            self.list.append(&row);
        }
        self.list.set_visible(!conditions.is_empty());
        self.changed();
    }

    /// Follows an edit: the rows that depend on the definition, and the count.
    fn changed(&self) {
        let definition = self.definition();
        self.period_row.set_visible(definition.reads_plays());
        self.direction
            .set_sensitive(definition.sort_field != SmartPlaylistSort::Random);
        if let Some(pending) = self.pending_count.take() {
            pending.remove();
        }
        let editor = self.me.clone();
        let pending = glib::timeout_add_local_once(RECOUNT_DELAY, move || {
            if let Some(editor) = editor.upgrade() {
                editor.pending_count.take();
                editor.count();
            }
        });
        self.pending_count.replace(Some(pending));
    }

    /// Shows how many tracks the definition matches; a count asked for earlier is dropped.
    fn count(&self) {
        let request = self.count_request.get() + 1;
        self.count_request.set(request);
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let Some(selected) = shell.selected_library().as_deref().cloned() else {
            return;
        };
        let catalog = Arc::clone(&shell.products.library);
        let definition = self.definition();
        let task = shell.products.runtime.spawn(async move {
            rufin_core::playlists::smart_playlist_match_count(&catalog, &selected, &definition)
                .await
        });
        let editor = self.me.clone();
        glib::spawn_future_local(async move {
            let Ok(Ok(count)) = task.await else {
                return;
            };
            if let Some(editor) = editor.upgrade()
                && editor.count_request.get() == request
            {
                editor.title.set_subtitle(&track_count_text(count));
            }
        });
    }

    /// Hands `done` the library's values of `field` for `text`.
    fn values(
        &self,
        field: SmartPlaylistRuleField,
        text: &str,
        done: impl FnOnce(Vec<SmartPlaylistValueChoice>) + 'static,
    ) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let Some(selected) = shell.selected_library().as_deref().cloned() else {
            return;
        };
        let catalog = Arc::clone(&shell.products.library);
        let (source, text) = (self.source_id(), text.to_owned());
        let task = shell.products.runtime.spawn(async move {
            rufin_core::playlists::smart_playlist_values(
                &catalog,
                &selected,
                field,
                &text,
                source.as_ref(),
            )
            .await
        });
        glib::spawn_future_local(async move {
            if let Ok(Ok(choices)) = task.await {
                done(choices);
            }
        });
    }

    /// Changes the conditions' structure, then rebuilds the list.
    fn edit(&self, change: impl FnOnce(&mut Vec<SmartPlaylistCondition>)) {
        change(&mut self.conditions.borrow_mut());
        self.render();
    }

    fn with_rule(&self, path: RulePath, change: impl FnOnce(&mut SmartPlaylistRule)) {
        let mut conditions = self.conditions.borrow_mut();
        let rule = match (conditions.get_mut(path.condition), path.rule) {
            (Some(SmartPlaylistCondition::Rule(rule)), None) => Some(rule),
            (Some(SmartPlaylistCondition::Group { rules, .. }), Some(index)) => {
                rules.get_mut(index)
            }
            _ => None,
        };
        if let Some(rule) = rule {
            change(rule);
        }
    }

    fn apply_template(&self, template: &SmartPlaylistTemplate) {
        let definition = &template.definition;
        self.conditions.replace(definition.conditions.clone());
        self.matching
            .set_selected(match_index(definition.matching) as u32);
        self.sort
            .set_selected(sort_index(definition.sort_field) as u32);
        self.set_descending(definition.descending);
        self.period
            .set_selected(period_index(definition.activity_period) as u32);
        self.limit.set_text(
            &definition
                .limit
                .map(|limit| limit.to_string())
                .unwrap_or_default(),
        );
        if playlist_name(&self.name.text()).is_none() {
            self.name.set_text(&tr(template.name));
        }
        self.render();
    }

    /// The list width every row needs on one line.
    fn line_width(&self) -> i32 {
        self.rows
            .borrow()
            .iter()
            .map(RuleRow::line_width)
            .max()
            .unwrap_or(0)
    }

    /// The list width every row needs with its value on its own line.
    fn wrapped_width(&self) -> i32 {
        self.rows
            .borrow()
            .iter()
            .map(RuleRow::wrapped_width)
            .max()
            .unwrap_or(0)
    }

    /// Wraps the rows when one of them does not fit on one line in `width`.
    fn fit_rows(&self, width: i32) {
        let rows = self.rows.borrow();
        let narrow = rows.iter().any(|row| row.line_width() > width);
        if self.narrow.replace(narrow) != narrow {
            for row in rows.iter() {
                row.place(narrow);
            }
        }
    }

    fn rule_row(
        &self,
        path: RulePath,
        rule: &SmartPlaylistRule,
        group: Option<(&gtk::ListBoxRow, &gtk::Box)>,
    ) -> gtk::ListBoxRow {
        let resource = "preferences/dialogs/smart_playlist_rule.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            row: gtk::ListBoxRow,
            grid: gtk::Grid,
            field: gtk::DropDown,
            operator: gtk::DropDown,
            value: gtk::Box,
            remove: gtk::Button,
        });
        configure_field_dropdown(&field, rule.field);
        let operators = rule.field.operators();
        set_dropdown_labels(
            &operator,
            &operators
                .iter()
                .map(|operator| tr(smart_operator_title(rule.field, *operator)))
                .collect::<Vec<_>>(),
            operators
                .iter()
                .position(|operator| *operator == rule.operator)
                .unwrap_or(0),
        );
        self.value_editor(&value, path, rule);

        let editor = self.me.clone();
        field.connect_selected_notify(move |field| {
            let (Some(editor), Some(selected)) = (
                editor.upgrade(),
                SmartPlaylistRuleField::ALL
                    .get(field.selected() as usize)
                    .copied(),
            ) else {
                return;
            };
            editor.with_rule(path, |rule| *rule = selected.default_rule());
            editor.render();
        });
        let editor = self.me.clone();
        let rule_field = rule.field;
        operator.connect_selected_notify(move |operator| {
            let (Some(editor), Some(selected)) = (
                editor.upgrade(),
                rule_field
                    .operators()
                    .get(operator.selected() as usize)
                    .copied(),
            ) else {
                return;
            };
            // A value of the same kind stays: equals and contains take the same text.
            let mut kind_changed = false;
            editor.with_rule(path, |rule| {
                kind_changed =
                    rule_field.value_kind(selected) != rule_field.value_kind(rule.operator);
                rule.operator = selected;
                if kind_changed {
                    rule.value = rule_field.default_value(selected);
                }
            });
            if kind_changed {
                editor.render();
            } else {
                editor.changed();
            }
        });
        let editor = self.me.clone();
        remove.connect_clicked(move |_| {
            let Some(editor) = editor.upgrade() else {
                return;
            };
            editor.edit(|conditions| match path.rule {
                None => {
                    conditions.remove(path.condition);
                }
                Some(index) => {
                    if let Some(SmartPlaylistCondition::Group { rules, .. }) =
                        conditions.get_mut(path.condition)
                    {
                        rules.remove(index);
                    }
                }
            });
        });

        let parts = RuleRow {
            row: row.clone(),
            grid,
            field,
            operator,
            value,
            remove,
            group: group.map(|(row, group)| (row.clone(), group.clone())),
        };
        parts.place(self.narrow.get());
        self.rows.borrow_mut().push(parts);
        row
    }

    fn group_row(
        &self,
        index: usize,
        group_matching: SmartPlaylistMatch,
        group_rules: &[SmartPlaylistRule],
    ) -> gtk::ListBoxRow {
        let resource = "preferences/dialogs/smart_playlist_group.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            row: gtk::ListBoxRow,
            group: gtk::Box,
            matching: gtk::DropDown,
            remove: gtk::Button,
            rules: gtk::ListBox,
            add_rule: gtk::Button,
        });
        matching.set_selected(match_index(group_matching) as u32);
        for (position, rule) in group_rules.iter().enumerate() {
            rules.append(&self.rule_row(
                RulePath {
                    condition: index,
                    rule: Some(position),
                },
                rule,
                Some((&row, &group)),
            ));
        }
        rules.set_visible(!group_rules.is_empty());

        let editor = self.me.clone();
        matching.connect_selected_notify(move |dropdown| {
            let Some(editor) = editor.upgrade() else {
                return;
            };
            if let Some(SmartPlaylistCondition::Group { matching, .. }) =
                editor.conditions.borrow_mut().get_mut(index)
            {
                *matching = MATCHES
                    .get(dropdown.selected() as usize)
                    .copied()
                    .unwrap_or_default();
            }
            editor.changed();
        });
        let editor = self.me.clone();
        remove.connect_clicked(move |_| {
            if let Some(editor) = editor.upgrade() {
                editor.edit(|conditions| {
                    conditions.remove(index);
                });
            }
        });
        let editor = self.me.clone();
        add_rule.connect_clicked(move |_| {
            if let Some(editor) = editor.upgrade() {
                editor.edit(|conditions| {
                    if let Some(SmartPlaylistCondition::Group { rules, .. }) =
                        conditions.get_mut(index)
                    {
                        rules.push(SmartPlaylistRuleField::Title.default_rule());
                    }
                });
            }
        });
        row
    }

    fn value_editor(&self, host: &gtk::Box, path: RulePath, rule: &SmartPlaylistRule) {
        use SmartPlaylistRuleField as F;
        use SmartPlaylistRuleValue as V;
        use SmartPlaylistRuleValueKind as K;
        let editor = self.me.clone();
        let set: Rc<dyn Fn(V)> = Rc::new(move |value| {
            if let Some(editor) = editor.upgrade() {
                editor.with_rule(path, |rule| rule.value = Some(value));
                editor.changed();
            }
        });
        let scale = smart_number_scale(rule.field);
        let (min, max, first) = rule.field.number_bounds(rule.operator);
        let kind = rule.field.value_kind(rule.operator).unwrap_or(K::None);
        match kind {
            K::None => host.set_visible(false),
            K::Text => {
                let entry = gtk::Entry::builder()
                    .hexpand(true)
                    .placeholder_text(tr(smart_value_placeholder(rule.field)))
                    .build();
                if let Some(V::Text(value)) = &rule.value {
                    entry.set_text(value);
                }
                entry.connect_changed(move |entry| set(V::Text(entry.text().into())));
                if matches!(rule.field, F::Artist | F::Album | F::Genre | F::Mood) {
                    let (editor, field) = (self.me.clone(), rule.field);
                    attach_suggestions(&entry, move |text, show| {
                        if let Some(editor) = editor.upgrade() {
                            editor.values(field, text, move |choices| {
                                show(
                                    choices
                                        .into_iter()
                                        .map(|choice| Suggestion {
                                            text: choice.value,
                                            detail: Some(choice.tracks.to_string()),
                                        })
                                        .collect(),
                                );
                            });
                        }
                    });
                }
                host.append(&entry);
            }
            K::Number | K::Days => {
                let value = match rule.value {
                    Some(V::Number(value)) => value,
                    _ => first,
                };
                let spin = number_spin(value, min, max, scale);
                spin.connect_value_changed(move |spin| set(V::Number(spin_value(spin, scale))));
                host.append(&spin);
                if kind == K::Days {
                    host.append(&gtk::Label::new(Some(&tr("days"))));
                }
            }
            K::NumberRange => {
                let (low, high) = match rule.value {
                    Some(V::NumberRange { min, max }) => (min, max),
                    _ => (first, first),
                };
                let low = number_spin(low, min, max, scale);
                let high = number_spin(high, min, max, scale);
                let update: Rc<dyn Fn()> = {
                    let (low, high) = (low.downgrade(), high.downgrade());
                    Rc::new(move || {
                        if let (Some(low), Some(high)) = (low.upgrade(), high.upgrade()) {
                            set(V::NumberRange {
                                min: spin_value(&low, scale),
                                max: spin_value(&high, scale),
                            });
                        }
                    })
                };
                for spin in [&low, &high] {
                    let update = Rc::clone(&update);
                    spin.connect_value_changed(move |_| update());
                }
                host.append(&low);
                host.append(&gtk::Label::new(Some(&tr("to"))));
                host.append(&high);
            }
            K::Date => {
                let entry = date_entry();
                if let Some(V::Date(value)) = &rule.value {
                    entry.set_text(value);
                }
                entry.connect_changed(move |entry| set(V::Date(entry.text().into())));
                host.append(&entry);
            }
            K::DateRange => {
                let (start, end) = (date_entry(), date_entry());
                if let Some(V::DateRange {
                    start: from,
                    end: to,
                }) = &rule.value
                {
                    start.set_text(from);
                    end.set_text(to);
                }
                let update: Rc<dyn Fn()> = {
                    let (start, end) = (start.downgrade(), end.downgrade());
                    Rc::new(move || {
                        if let (Some(start), Some(end)) = (start.upgrade(), end.upgrade()) {
                            set(V::DateRange {
                                start: start.text().into(),
                                end: end.text().into(),
                            });
                        }
                    })
                };
                for entry in [&start, &end] {
                    let update = Rc::clone(&update);
                    entry.connect_changed(move |_| update());
                }
                host.append(&start);
                host.append(&gtk::Label::new(Some(&tr("to"))));
                host.append(&end);
            }
            K::Bool => {
                let active = matches!(rule.value, Some(V::Bool(true)));
                let dropdown = dropdown_from_labels(
                    &[tr(msgid("Yes")), tr(msgid("No"))],
                    usize::from(!active),
                );
                dropdown.connect_selected_notify(move |dropdown| {
                    set(V::Bool(dropdown.selected() == 0));
                });
                host.append(&dropdown);
            }
            K::Playlist => self.playlist_choice(host, rule, set),
        }
    }

    /// A dropdown of the playlists by name, keeping a saved one that is no longer listed.
    fn playlist_choice(
        &self,
        host: &gtk::Box,
        rule: &SmartPlaylistRule,
        set: Rc<dyn Fn(SmartPlaylistRuleValue)>,
    ) {
        let dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
        dropdown.set_hexpand(true);
        let saved = match &rule.value {
            Some(SmartPlaylistRuleValue::Playlist(id)) if !id.is_empty() => Some(id.clone()),
            _ => None,
        };
        let ids = Rc::new(RefCell::new(Vec::<String>::new()));
        let show: Rc<dyn Fn(Vec<SmartPlaylistValueChoice>)> = {
            let (dropdown, ids) = (dropdown.downgrade(), Rc::clone(&ids));
            Rc::new(move |choices| {
                let Some(dropdown) = dropdown.upgrade() else {
                    return;
                };
                let (mut values, mut names): (Vec<_>, Vec<_>) = choices
                    .into_iter()
                    .map(|choice| (choice.value, choice.name))
                    .unzip();
                if let Some(saved) = &saved
                    && !values.contains(saved)
                {
                    values.push(saved.clone());
                    names.push(saved.clone());
                }
                let selected = saved
                    .as_ref()
                    .and_then(|saved| values.iter().position(|value| value == saved))
                    .unwrap_or(0);
                ids.replace(values);
                set_dropdown_labels(&dropdown, &names, selected);
            })
        };
        show(Vec::new());
        dropdown.connect_selected_notify(move |dropdown| {
            let id = ids.borrow().get(dropdown.selected() as usize).cloned();
            if let Some(id) = id {
                set(SmartPlaylistRuleValue::Playlist(id));
            }
        });
        self.values(SmartPlaylistRuleField::Playlist, "", move |choices| {
            show(choices)
        });
        host.append(&dropdown);
    }
}

/// The fields in their sections, each section under its title.
fn configure_field_dropdown(dropdown: &gtk::DropDown, selected: SmartPlaylistRuleField) {
    let sections = gio::ListStore::new::<gtk::StringList>();
    let mut fields = SmartPlaylistRuleField::ALL.iter().peekable();
    while let Some(first) = fields.next() {
        let titles = gtk::StringList::new(&[]);
        titles.append(&tr(smart_rule_field_title(*first)));
        while let Some(field) = fields.next_if(|field| field.section() == first.section()) {
            titles.append(&tr(smart_rule_field_title(*field)));
        }
        sections.append(&titles);
    }
    dropdown.set_model(Some(&gtk::FlattenListModel::new(Some(sections))));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, header| {
        let header = header
            .downcast_ref::<gtk::ListHeader>()
            .expect("list header");
        let title = gtk::Label::builder()
            .xalign(0.0)
            .margin_top(6)
            .margin_start(6)
            .css_classes(["caption-heading", "dim-label"])
            .build();
        header.set_child(Some(&title));
    });
    factory.connect_bind(|_, header| {
        let header = header
            .downcast_ref::<gtk::ListHeader>()
            .expect("list header");
        let title = header
            .child()
            .and_downcast::<gtk::Label>()
            .expect("section title");
        if let Some(field) = SmartPlaylistRuleField::ALL.get(header.start() as usize) {
            title.set_label(&tr(smart_field_section_title(field.section())));
        }
    });
    dropdown.set_header_factory(Some(&factory));
    dropdown.set_selected(
        SmartPlaylistRuleField::ALL
            .iter()
            .position(|field| *field == selected)
            .unwrap_or(0) as u32,
    );
}

fn dropdown_from_labels(labels: &[String], selected: usize) -> gtk::DropDown {
    let dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    set_dropdown_labels(&dropdown, labels, selected);
    dropdown
}

fn set_dropdown_labels(dropdown: &gtk::DropDown, labels: &[String], selected: usize) {
    let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    dropdown.set_model(Some(&gtk::StringList::new(&refs)));
    dropdown.set_selected(selected as u32);
}

fn date_entry() -> gtk::Entry {
    gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text("YYYY-MM-DD")
        .build()
}

fn match_index(matching: SmartPlaylistMatch) -> usize {
    MATCHES
        .iter()
        .position(|candidate| *candidate == matching)
        .unwrap_or(0)
}

fn period_index(period: SmartPlaylistActivityPeriod) -> usize {
    SmartPlaylistActivityPeriod::ALL
        .iter()
        .position(|value| *value == period)
        .unwrap_or(SmartPlaylistActivityPeriod::ALL.len() - 1)
}

fn sort_index(sort: SmartPlaylistSort) -> usize {
    SmartPlaylistSort::ALL
        .iter()
        .position(|field| *field == sort)
        .unwrap_or(0)
}

/// A spin button showing a stored number divided by `scale`.
fn number_spin(value: i64, min: i64, max: i64, scale: i64) -> gtk::SpinButton {
    let scale_by = scale as f64;
    let adjustment = gtk::Adjustment::new(
        value as f64 / scale_by,
        min as f64 / scale_by,
        max as f64 / scale_by,
        1.0 / scale_by,
        10.0 / scale_by,
        0.0,
    );
    let spin = gtk::SpinButton::new(Some(&adjustment), 1.0 / scale_by, u32::from(scale > 1));
    spin.set_numeric(true);
    spin.set_width_chars(7);
    spin.set_valign(gtk::Align::Center);
    spin
}

fn spin_value(spin: &gtk::SpinButton, scale: i64) -> i64 {
    (spin.value() * scale as f64).round() as i64
}

fn playlist_name(value: &str) -> Option<String> {
    let name = value.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The limit `value` sets, none when blank; a value that is not a positive number does not save.
fn playlist_limit(value: &str) -> Option<Option<usize>> {
    let value = value.trim();
    let limit = value.parse::<usize>().ok().filter(|limit| *limit > 0);
    (value.is_empty() || limit.is_some()).then_some(limit)
}

fn build_dialog(
    shell: &Rc<Shell>,
    playlist: Option<library::SmartPlaylistRow>,
    folder: Option<library::PlaylistFolderKey>,
) -> adw::Dialog {
    let resource = "preferences/dialogs/smart_playlist.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        dialog: adw::Dialog,
        toolbar: adw::ToolbarView,
        title: adw::WindowTitle,
        cancel: gtk::Button,
        submit: gtk::Button,
        editor_content: gtk::Box,
        editor_layout: gtk::Box,
        artwork_host: gtk::Box,
        rules_column: gtk::Box,
        name_entry: gtk::Entry,
        templates: gtk::Box,
        template_chips: adw::WrapBox,
        matching: gtk::DropDown,
        conditions_host: gtk::Box,
        conditions: gtk::ListBox,
        add_rule: gtk::Button,
        add_group: gtk::Button,
        sort: gtk::DropDown,
        direction: gtk::Button,
        limit: gtk::Entry,
        period_row: adw::ActionRow,
        activity_period: gtk::DropDown,
        source_row: adw::ActionRow,
        source: gtk::DropDown,
    });
    let (dialog_title, submit_label) = if playlist.is_some() {
        (msgid("Edit Smart Playlist"), msgid("Save"))
    } else {
        (msgid("New Smart Playlist"), msgid("Create"))
    };
    dialog.set_title(&tr(dialog_title));
    title.set_title(&tr(dialog_title));
    submit.set_label(&tr(submit_label));
    if let Some(playlist) = &playlist {
        name_entry.set_text(&playlist.name);
    }
    let definition = playlist
        .as_ref()
        .map(|playlist| playlist.definition.clone())
        .unwrap_or_default();

    let configured = shell.source.configured.borrow().sources.clone();
    let mut source_ids = vec![None];
    let mut labels = vec![tr("All")];
    for source in &configured {
        source_ids.push(Some(source.id.clone()));
        labels.push(crate::widgets::source_labels::configured_source_display_name(source));
    }
    if let Some(id) = &definition.source_id
        && !source_ids.contains(&Some(id.clone()))
    {
        source_ids.push(Some(id.clone()));
        labels.push(id.to_string());
    }
    let selected = source_ids
        .iter()
        .position(|id| id == &definition.source_id)
        .unwrap_or(0);
    set_dropdown_labels(&source, &labels, selected);
    source_row.set_visible(configured.len() >= 2 || definition.source_id.is_some());
    set_dropdown_labels(
        &matching,
        &MATCHES.map(|matching| tr(smart_match_title(matching))),
        match_index(definition.matching),
    );
    set_dropdown_labels(
        &sort,
        &SmartPlaylistSort::ALL.map(|sort| tr(smart_sort_title(sort))),
        sort_index(definition.sort_field),
    );
    set_dropdown_labels(
        &activity_period,
        &SmartPlaylistActivityPeriod::ALL.map(|period| tr(smart_period_title(period))),
        period_index(definition.activity_period),
    );
    if let Some(value) = definition.limit {
        limit.set_text(&value.to_string());
    }

    conditions_host.remove(&conditions);
    let editor = Rc::new_cyclic(|me: &Weak<Editor>| Editor {
        me: me.clone(),
        shell: Rc::downgrade(shell),
        title,
        name: name_entry,
        matching,
        list_owner: {
            let editor = me.clone();
            let owner = width_allocation_owner(&conditions, move |width| {
                if let Some(editor) = editor.upgrade() {
                    editor.fit_rows(width);
                }
            });
            let editor = me.clone();
            owner.set_minimum_width_callback(move || {
                editor.upgrade().map_or(0, |editor| editor.wrapped_width())
            });
            owner
        },
        list: conditions,
        sort,
        direction,
        descending: Cell::new(definition.descending),
        limit,
        period: activity_period,
        period_row,
        source,
        source_ids,
        conditions: RefCell::new(definition.conditions),
        rows: RefCell::new(Vec::new()),
        narrow: Cell::new(false),
        pending_count: RefCell::new(None),
        count_request: Cell::new(0),
    });
    editor.set_descending(definition.descending);
    let changed = |editor: &Rc<Editor>| {
        let editor = Rc::downgrade(editor);
        move || {
            if let Some(editor) = editor.upgrade() {
                editor.changed();
            }
        }
    };
    for dropdown in [&editor.matching, &editor.sort, &editor.period] {
        let changed = changed(&editor);
        dropdown.connect_selected_notify(move |_| changed());
    }
    {
        let changed = changed(&editor);
        editor.limit.connect_changed(move |_| changed());
    }
    let weak = Rc::downgrade(&editor);
    editor.direction.connect_clicked(move |_| {
        if let Some(editor) = weak.upgrade() {
            editor.set_descending(!editor.descending.get());
            editor.changed();
        }
    });
    // Playlists are listed from the chosen source.
    let weak = Rc::downgrade(&editor);
    editor.source.connect_selected_notify(move |_| {
        if let Some(editor) = weak.upgrade() {
            editor.render();
        }
    });
    let weak = Rc::downgrade(&editor);
    add_rule.connect_clicked(move |_| {
        if let Some(editor) = weak.upgrade() {
            editor.edit(|conditions| {
                conditions.push(SmartPlaylistCondition::Rule(
                    SmartPlaylistRuleField::Title.default_rule(),
                ));
            });
        }
    });
    let weak = Rc::downgrade(&editor);
    add_group.connect_clicked(move |_| {
        if let Some(editor) = weak.upgrade() {
            editor.edit(|conditions| {
                conditions.push(SmartPlaylistCondition::Group {
                    matching: SmartPlaylistMatch::All,
                    rules: vec![SmartPlaylistRuleField::Title.default_rule()],
                });
            });
        }
    });
    if playlist.is_none() {
        for template in rufin_core::playlists::smart_playlist_templates() {
            let chip = gtk::Button::with_label(&tr(template.name));
            chip.add_css_class("detail-genre-chip");
            let weak = Rc::downgrade(&editor);
            chip.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.apply_template(&template);
                }
            });
            template_chips.append(&chip);
        }
        templates.set_visible(true);
    }
    conditions_host.append(&editor.list_owner);
    editor.render();

    let preferred_width = if playlist.is_some() {
        SMART_PLAYLIST_EDIT_WIDTH
    } else {
        large_popup_content_width(SMART_PLAYLIST_DIALOG_WIDTH)
    };
    dialog.set_child(None::<&gtk::Widget>);
    dialog.set_child(Some(&crate::widgets::layout::preferred_width_owner(
        &toolbar,
        preferred_width,
    )));

    let artwork = playlist.as_ref().map(|playlist| {
        let key = playlist.smart_playlist_key;
        let artwork = crate::editors::playlist_artwork::ArtworkEditor::new(
            &shell.products.source,
            &shell.artwork,
            rufin_core::playlists::PlaylistArtworkEditing {
                binding: playlist
                    .artwork_binding
                    .as_deref()
                    .map(library::authored_playlist_artwork)
                    .transpose()
                    .expect("accepted playlist artwork")
                    .flatten(),
                automatic_binding: playlist
                    .artwork_binding
                    .as_deref()
                    .map(library::automatic_playlist_artwork)
                    .transpose()
                    .expect("accepted playlist artwork")
                    .flatten(),
                writable: true,
            },
            &editor_content,
            &submit,
            move |owner| rufin_core::playlists::regenerate_smart_playlist_artwork(owner, key),
        );
        artwork.overlay.set_size_request(1, 1);
        artwork.overlay.set_halign(gtk::Align::Fill);
        artwork.overlay.set_valign(gtk::Align::Fill);
        artwork.frame.set_hexpand(true);
        artwork_host.append(&artwork.frame);
        artwork_host.append(&artwork.status);
        artwork_host.set_visible(true);
        editor_content.remove(&editor_layout);
        let layout = editor_layout.downgrade();
        let cover = artwork.frame.downgrade();
        let cover_host = artwork_host.downgrade();
        let rules = rules_column.downgrade();
        let rules_editor = Rc::downgrade(&editor);
        let owner = width_allocation_owner(&editor_layout, move |width| {
            let (Some(layout), Some(cover), Some(cover_host), Some(rules), Some(editor)) = (
                layout.upgrade(),
                cover.upgrade(),
                cover_host.upgrade(),
                rules.upgrade(),
                rules_editor.upgrade(),
            ) else {
                return;
            };
            // Beside the rules, the artwork takes what the rules leave on one line.
            let rules_width = minimum_width(&rules)
                .max(editor.line_width() + rules.margin_start() + rules.margin_end());
            let artwork_width =
                (width - rules_width - layout.spacing()).min(SMART_PLAYLIST_ARTWORK_WIDTH);
            let stacked = artwork_width < SMART_PLAYLIST_ARTWORK_MIN_WIDTH;
            let orientation = if stacked {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            };
            if layout.orientation() != orientation {
                layout.set_orientation(orientation);
            }
            if cover_host.hexpands() != stacked {
                cover_host.set_hexpand(stacked);
            }
            let cover_width = if stacked { width } else { artwork_width };
            let width_request = if stacked { -1 } else { artwork_width };
            if cover.width_request() != width_request {
                cover.set_width_request(width_request);
            }
            if cover.height_request() != cover_width {
                cover.set_height_request(cover_width);
            }
        });
        // Stacked, the artwork shrinks with the dialog and the rules do not.
        let rules = rules_column.downgrade();
        owner.set_minimum_width_callback(move || {
            rules.upgrade().map_or(0, |rules| minimum_width(&rules))
        });
        editor_content.append(&owner);
        artwork
    });
    let artwork_weak = artwork.as_ref().map(Rc::downgrade);
    // The dialog keeps the editor and the artwork while it is open.
    let lifetime = RefCell::new(Some((Rc::clone(&editor), artwork)));
    dialog.connect_closed(move |_| {
        if let Some((editor, _)) = lifetime.borrow_mut().take()
            && let Some(pending) = editor.pending_count.take()
        {
            pending.remove();
        }
    });
    connect_editor_validation(&submit, &editor);

    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });

    let key = playlist.map(|playlist| playlist.smart_playlist_key);
    let close = dialog.downgrade();
    let publish_shell = Rc::downgrade(shell);
    let editor = Rc::downgrade(&editor);
    submit.connect_clicked(move |_| {
        let (Some(shell), Some(editor)) = (publish_shell.upgrade(), editor.upgrade()) else {
            return;
        };
        let Some((name, definition)) = editor.saved() else {
            return;
        };
        let change = match key {
            Some(key) => SmartPlaylistChange::Update {
                key,
                name,
                definition,
                artwork: artwork_weak
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .and_then(|editor| editor.change()),
            },
            None => SmartPlaylistChange::Create { name, definition },
        };
        if let Some(artwork) = artwork_weak.as_ref().and_then(Weak::upgrade) {
            artwork.set_busy(true);
        }
        let artwork = artwork_weak.clone();
        let dialog = close.clone();
        shell.publish_smart_playlist_change(
            change,
            folder,
            Some(Rc::new(move |result| {
                if let Some(artwork) = artwork.as_ref().and_then(Weak::upgrade) {
                    artwork.set_busy(false);
                }
                if result.is_ok()
                    && let Some(dialog) = dialog.upgrade()
                {
                    dialog.close();
                }
            })),
        );
    });
    dialog
}

fn connect_editor_validation(button: &gtk::Button, editor: &Editor) {
    let (button, name, limit) = (
        button.downgrade(),
        editor.name.downgrade(),
        editor.limit.downgrade(),
    );
    let sync = Rc::new(move || {
        if let (Some(button), Some(name), Some(limit)) =
            (button.upgrade(), name.upgrade(), limit.upgrade())
        {
            button.set_sensitive(
                playlist_name(&name.text()).is_some() && playlist_limit(&limit.text()).is_some(),
            );
        }
    });
    sync();
    for entry in [&editor.name, &editor.limit] {
        let sync = Rc::clone(&sync);
        entry.connect_changed(move |_| sync());
    }
}

impl Shell {
    pub(crate) fn publish_smart_playlist_change(
        self: &Rc<Self>,
        change: SmartPlaylistChange,
        folder: Option<library::PlaylistFolderKey>,
        settled: Option<Rc<dyn Fn(Result<(), String>)>>,
    ) {
        let owner = self.products.source.clone();
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let result = rufin_core::playlists::apply_smart_playlist_change(&owner, change, folder)
                .await
                .map_err(|error| error.to_string())
                .and_then(|applied| {
                    applied
                        .map(|_| ())
                        .ok_or_else(|| "Smart Playlist is no longer current".to_string())
                });
            let Some(shell) = shell.upgrade() else { return };
            if let Err(error) = result.as_ref() {
                shell
                    .control_feedback
                    .show_control_feedback_toast(error.clone());
            }
            if let Some(settled) = settled.as_ref() {
                settled(result);
            }
        });
    }

    pub(crate) fn new_smart_playlist_dialog(self: &Rc<Self>) {
        self.present_smart_playlist_dialog(None);
    }

    pub(crate) fn edit_smart_playlist_dialog(self: &Rc<Self>, playlist: library::SmartPlaylistRow) {
        self.present_smart_playlist_dialog(Some(playlist));
    }

    fn present_smart_playlist_dialog(self: &Rc<Self>, playlist: Option<library::SmartPlaylistRow>) {
        let folder = if playlist.is_none() {
            match self.navigation.routes.borrow().current() {
                rufin_core::route::Route::PlaylistFolder { key, smart: true } => Some(*key),
                _ => None,
            }
        } else {
            None
        };
        let dialog = build_dialog(self, playlist, folder);
        crate::widgets::popup::present_light_dismiss_dialog(&dialog, &self.chrome.window);
    }
}
