use super::PreferencesNavigationControls;
use crate::Preferences;
use adw::prelude::*;
use rufin_core::settings::app::ActivityOverviewSettings;
use std::rc::Rc;

pub(super) fn wrap_general(
    shell: &Rc<Preferences>,
    general: &adw::PreferencesPage,
    entry: &adw::ActionRow,
    controls: &PreferencesNavigationControls,
) -> gtk::Widget {
    let resource = crate::ui_resource::ACTIVITY_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        navigation: adw::NavigationView, root: adw::NavigationPage, settings_page: adw::NavigationPage,
        monthly_enabled: adw::SwitchRow, yearly_enabled: adw::SwitchRow,
        monthly_before: adw::SpinRow, monthly_after: adw::SpinRow,
        yearly_before: adw::SpinRow, yearly_after: adw::SpinRow,
    });
    root.set_child(Some(general));
    controls.set_navigation(&navigation);
    controls.set_nested_page_visible(false);
    let weak_navigation = navigation.downgrade();
    let page_controls = controls.clone();
    entry.connect_activated(move |_| {
        if let Some(navigation) = weak_navigation.upgrade() {
            navigation.push(&settings_page);
            page_controls.set_nested_page_visible(true);
        }
    });
    let page_controls = controls.clone();
    navigation.connect_popped(move |_, _| page_controls.set_nested_page_visible(false));

    let mut settings = shell.settings.current.borrow().activity_overview.clone();
    for (toggle, rows, field) in [
        (
            &monthly_enabled,
            [&monthly_before, &monthly_after],
            (|s: &mut ActivityOverviewSettings| &mut s.monthly_enabled)
                as fn(&mut ActivityOverviewSettings) -> &mut bool,
        ),
        (
            &yearly_enabled,
            [&yearly_before, &yearly_after],
            (|s: &mut ActivityOverviewSettings| &mut s.yearly_enabled)
                as fn(&mut ActivityOverviewSettings) -> &mut bool,
        ),
    ] {
        let active = *field(&mut settings);
        toggle.set_active(active);
        for row in rows {
            row.set_sensitive(active);
        }
        let rows = rows.map(|row| row.downgrade());
        let weak = Rc::downgrade(shell);
        toggle.connect_active_notify(move |row| {
            for target in &rows {
                if let Some(target) = target.upgrade() {
                    target.set_sensitive(row.is_active());
                }
            }
            if let Some(shell) = weak.upgrade() {
                shell
                    .settings
                    .update_app_settings("automatic activity overview", |settings| {
                        *field(&mut settings.activity_overview) = row.is_active();
                        true
                    });
            }
        });
    }
    let fields: [fn(&mut ActivityOverviewSettings) -> &mut u32; 4] = [
        |s| &mut s.monthly_days_before_end,
        |s| &mut s.monthly_days_after_end,
        |s| &mut s.yearly_days_before_end,
        |s| &mut s.yearly_days_after_end,
    ];
    for (row, field) in [monthly_before, monthly_after, yearly_before, yearly_after]
        .into_iter()
        .zip(fields)
    {
        row.set_value(f64::from(*field(&mut settings)));
        let weak = Rc::downgrade(shell);
        row.connect_value_notify(move |row| {
            if let Some(shell) = weak.upgrade() {
                shell
                    .settings
                    .update_app_settings("activity overview dates", |settings| {
                        *field(&mut settings.activity_overview) = row.value() as u32;
                        true
                    });
            }
        });
    }
    navigation.upcast()
}
