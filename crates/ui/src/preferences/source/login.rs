use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::runtime::source::{
    ConfiguredSources, CredentialInput, CredentialPreset, DiscoveryStatus, EditableSource,
    OpenSubsonicAuthentication, OpenSubsonicKind, SourceHandle, SourceOperation,
    SourceSettingsChange, SourceSetup, SourceSummary,
};
use adw::prelude::*;

use super::{
    field_layout::{
        compact_field_row_group, install_compact_field_row_responsiveness, style_compact_field_row,
    },
    folder_selected_text,
    local_access::{confirm_forget_source, credential_source_settings_group},
    source_operation_text,
};
use crate::layout::large_popup_content_width;
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::shell::Shell;
use crate::shell::actions::text_button;
use localization::{msgid, tr, tr_with};

const ADD_SERVER_CLAMP_WIDTH: i32 = 560;
const SETUP_FORM_PAGE: &str = "form";
const SETUP_PROGRESS_PAGE: &str = "progress";
const JELLYFIN_SOURCE_KIND: &str = "jellyfin";
const LOCAL_SOURCE_KIND: &str = "local";

type SetupFlowFactory = fn(&Rc<Shell>, &'static SourcePresentation) -> Rc<dyn SourceSetupFlow>;
type SettingsGroupFactory =
    fn(&Rc<Shell>, &EditableSource, &'static SourcePresentation) -> Result<gtk::Widget, String>;

#[derive(Clone, Copy)]
struct SourcePresentation {
    kind: &'static str,
    title: &'static str,
    icon_name: &'static str,
    setup_flow: SetupFlowFactory,
    settings_group: Option<SettingsGroupFactory>,
}

static JELLYFIN: SourcePresentation = SourcePresentation {
    kind: JELLYFIN_SOURCE_KIND,
    title: msgid("Jellyfin"),
    icon_name: "io.github.screwys.Rufin.source.jellyfin",
    setup_flow: jellyfin_setup_flow,
    settings_group: Some(jellyfin_settings_group),
};
static NAVIDROME: SourcePresentation = SourcePresentation {
    kind: "navidrome",
    title: msgid("Navidrome"),
    icon_name: "io.github.screwys.Rufin.source.navidrome",
    setup_flow: navidrome_setup_flow,
    settings_group: Some(navidrome_settings_group),
};
static SUBSONIC: SourcePresentation = SourcePresentation {
    kind: "subsonic",
    title: msgid("OpenSubsonic"),
    icon_name: "io.github.screwys.Rufin.source.opensubsonic",
    setup_flow: subsonic_setup_flow,
    settings_group: Some(subsonic_settings_group),
};
static LOCAL: SourcePresentation = SourcePresentation {
    kind: LOCAL_SOURCE_KIND,
    title: msgid("Local"),
    icon_name: "rufin-folders-symbolic",
    setup_flow: local_setup_flow,
    settings_group: None,
};
static SOURCE_PRESENTATIONS: [&SourcePresentation; 4] = [&JELLYFIN, &NAVIDROME, &SUBSONIC, &LOCAL];

fn source_presentations() -> &'static [&'static SourcePresentation] {
    &SOURCE_PRESENTATIONS
}

fn default_source_presentation() -> &'static SourcePresentation {
    &JELLYFIN
}

fn source_presentation(kind: &str) -> Option<&'static SourcePresentation> {
    source_presentations()
        .iter()
        .copied()
        .find(|presentation| presentation.kind == kind)
}

fn selected_source(configured: &ConfiguredSources) -> Option<&SourceSummary> {
    let selected = configured.selected_source_id.as_ref()?;
    configured
        .sources
        .iter()
        .find(|source| &source.id == selected)
}

pub(crate) fn source_kind_title(kind: &str) -> Option<&'static str> {
    source_presentation(kind).map(|presentation| presentation.title)
}

pub(crate) fn source_kind_icon_name(kind: &str) -> Option<&'static str> {
    source_presentation(kind).map(|presentation| presentation.icon_name)
}

pub(crate) fn source_settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
) -> Option<Result<gtk::Widget, String>> {
    let presentation = source_presentation(&saved.source.kind)?;
    presentation
        .settings_group
        .map(|factory| factory(shell, saved, presentation))
}

pub(crate) trait SourceSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget;
}

#[derive(Clone)]
pub(crate) struct SourceSetupViewHandle {
    context: SetupViewContext,
}

#[derive(Clone)]
pub(crate) struct SetupViewContext {
    content: gtk::Box,
    surface: gtk::Stack,
    form: gtk::Box,
    progress_status: gtk::Label,
    flow: Rc<RefCell<Rc<dyn SourceSetupFlow>>>,
    actions: Rc<RefCell<Option<SetupActions>>>,
    discovery: Rc<RefCell<Option<DiscoveredServersView>>>,
    retry_focus: Rc<RefCell<Option<gtk::Widget>>>,
    preferences_dialog: Option<adw::Dialog>,
}

#[derive(Clone)]
struct SetupActions {
    status: gtk::Label,
    connect: gtk::Button,
    ready: Rc<dyn Fn() -> bool>,
}

#[derive(Clone)]
struct DiscoveredServersView {
    group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<gtk::Widget>>>,
    host: CredentialHost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupSurfaceState<'a> {
    Form,
    Progress,
    Failed(&'a str),
}

impl SetupViewContext {
    fn is_first_run(&self) -> bool {
        self.preferences_dialog.is_none()
    }

    fn is_mounted(&self) -> bool {
        if self.is_first_run() {
            self.content.parent().is_some()
        } else {
            self.content.root().is_some()
        }
    }
}

#[derive(Clone, Debug)]
struct CredentialHostDraft {
    name: String,
    url: String,
    username: String,
    password: String,
    cert_verify: bool,
}

struct CredentialSetupFlow {
    presentation: &'static SourcePresentation,
    draft: Rc<RefCell<CredentialHostDraft>>,
    authentication: Rc<Cell<OpenSubsonicAuthentication>>,
    offer_api_key: bool,
    submit: Rc<dyn Fn(&SourceHandle, CredentialInput, OpenSubsonicAuthentication)>,
}

struct JellyfinSetupFlow {
    presentation: &'static SourcePresentation,
    draft: Rc<RefCell<CredentialHostDraft>>,
    use_instant_mix: Rc<Cell<bool>>,
    submit: Rc<dyn Fn(&SourceHandle, CredentialInput, bool)>,
}

struct LocalSetupFlow {
    presentation: &'static SourcePresentation,
    folders: Rc<RefCell<Vec<PathBuf>>>,
    submit: Rc<dyn Fn(&SourceHandle, Vec<PathBuf>)>,
}

#[derive(Clone)]
struct CredentialHost {
    widget: gtk::Box,
    name: adw::EntryRow,
    url: adw::EntryRow,
    username: adw::EntryRow,
    password: adw::PasswordEntryRow,
    cert_verify: adw::SwitchRow,
    authentication: Option<Rc<Cell<OpenSubsonicAuthentication>>>,
    api_key: Option<adw::SwitchRow>,
}

impl CredentialHost {
    fn input(&self) -> CredentialInput {
        CredentialInput {
            source_name: trimmed_optional_text(&self.name),
            server_url: self.url.text().to_string(),
            username: self.username.text().to_string(),
            secret: self.password.text().to_string(),
            trust_invalid_cert: !self.cert_verify.is_active(),
        }
    }

    fn ready(&self) -> bool {
        remote_login_ready(
            &self.url,
            &self.username,
            &self.password,
            self.authentication.as_ref().is_none_or(|authentication| {
                authentication.get() == OpenSubsonicAuthentication::Password
            }),
        )
    }
}

impl Shell {
    pub(crate) fn add_server_navigation_page(
        self: &Rc<Self>,
        _navigation: &adw::NavigationView,
        preferences_dialog: &adw::Dialog,
    ) -> adw::NavigationPage {
        // Keep the form mounted while the operation runs. A failed connection
        // returns to the same values so the user can correct them and retry.
        let context = self.setup_view_context(Some(preferences_dialog.clone()));
        mount_setup_flow(self, &context);
        *self.source.add_server.borrow_mut() = Some(SourceSetupViewHandle {
            context: context.clone(),
        });
        adw::NavigationPage::new(&context.content, &tr("Add server"))
    }

    pub(crate) fn add_server_view(self: &Rc<Self>) -> gtk::Widget {
        let retained = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .filter(|handle| handle.context.is_first_run())
            .map(|handle| handle.context.clone());
        if let Some(context) = retained {
            update_setup_surface(self, &context);
            return context.content.upcast();
        }
        let context = self.setup_view_context(None);
        mount_setup_flow(self, &context);
        *self.source.add_server.borrow_mut() = Some(SourceSetupViewHandle {
            context: context.clone(),
        });
        context.content.upcast()
    }

    pub(crate) fn first_run_setup_mounted(&self) -> bool {
        self.source
            .add_server
            .borrow()
            .as_ref()
            .is_some_and(|handle| handle.context.is_first_run() && handle.context.is_mounted())
    }

    pub(crate) fn take_first_run_setup_view(&self) -> Option<gtk::Widget> {
        let context = {
            let mut handle = self.source.add_server.borrow_mut();
            if !handle
                .as_ref()
                .is_some_and(|handle| handle.context.is_first_run())
            {
                return None;
            }
            let Some(handle) = handle.take() else {
                return None;
            };
            handle.context
        };
        Some(context.content.upcast())
    }

    pub(crate) fn update_add_server_dialog(self: &Rc<Self>) {
        let Some(handle) = self.source.add_server.borrow().clone() else {
            return;
        };
        if !handle.context.is_mounted() {
            if handle.context.preferences_dialog.is_none()
                || !self.source.operation.borrow().add_form_active()
            {
                self.source.add_server.borrow_mut().take();
            }
            return;
        }
        update_setup_surface(self, &handle.context);
    }

    pub(crate) fn update_add_server_discovery(self: &Rc<Self>) {
        let Some(handle) = self.source.add_server.borrow().clone() else {
            return;
        };
        if !handle.context.is_mounted() {
            if handle.context.preferences_dialog.is_none()
                || !self.source.operation.borrow().add_form_active()
            {
                self.source.add_server.borrow_mut().take();
            }
            return;
        }
        if let Some(discovery) = handle.context.discovery.borrow().as_ref() {
            refresh_discovered_servers_view(self, discovery);
        }
    }

    pub(crate) fn complete_add_server_dialog(&self) {
        let preferences_dialog = {
            let mut handle = self.source.add_server.borrow_mut();
            if !handle
                .as_ref()
                .is_some_and(|handle| !handle.context.is_first_run())
            {
                return;
            }
            handle
                .take()
                .and_then(|handle| handle.context.preferences_dialog)
        };
        if let Some(dialog) = preferences_dialog {
            dialog.close();
        }
    }

    pub(crate) fn clear_retained_add_server_form(&self) {
        self.source.add_server.borrow_mut().take();
    }

    pub(crate) fn restore_add_server_dialog_after_failure(self: &Rc<Self>) -> bool {
        let Some(context) = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .map(|handle| handle.context.clone())
            .filter(|context| !context.is_first_run())
        else {
            return false;
        };
        let Some(dialog) = context.preferences_dialog.clone() else {
            return false;
        };
        self.preferences.set_active_dialog(&dialog);
        present_light_dismiss_dialog(&dialog, &self.chrome.window);
        update_setup_surface(self, &context);
        true
    }

    fn setup_view_context(
        self: &Rc<Self>,
        preferences_dialog: Option<adw::Dialog>,
    ) -> SetupViewContext {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let surface = gtk::Stack::new();
        surface.set_hexpand(true);
        surface.set_vexpand(true);
        surface.set_hhomogeneous(false);
        surface.set_vhomogeneous(false);
        let form = gtk::Box::new(gtk::Orientation::Vertical, 0);
        form.set_hexpand(true);
        form.set_vexpand(true);
        surface.add_named(&form, Some(SETUP_FORM_PAGE));
        let (progress, progress_status) = self.connection_progress_view();
        surface.add_named(&progress, Some(SETUP_PROGRESS_PAGE));
        content.append(&surface);
        let flow = self.default_source_setup_flow();
        SetupViewContext {
            content,
            surface,
            form,
            progress_status,
            flow: Rc::new(RefCell::new(flow)),
            actions: Rc::new(RefCell::new(None)),
            discovery: Rc::new(RefCell::new(None)),
            retry_focus: Rc::new(RefCell::new(None)),
            preferences_dialog,
        }
    }

    fn default_source_setup_flow(self: &Rc<Self>) -> Rc<dyn SourceSetupFlow> {
        let source_setup_active = self.source.operation.borrow().add_form_active();
        let registration = {
            let configured = self.source.configured.borrow();
            (configured.first_run || source_setup_active)
                .then(|| selected_source(&configured))
                .flatten()
                .and_then(|source| source_presentation(&source.kind))
                .unwrap_or_else(default_source_presentation)
        };
        (registration.setup_flow)(self, registration)
    }

    fn reconnect_saved_source(
        &self,
        registration: &'static SourcePresentation,
    ) -> Option<EditableSource> {
        let configured = self.source.configured.borrow();
        if !configured.first_run && !self.source.operation.borrow().add_form_active() {
            return None;
        }
        let source = selected_source(&configured)?;
        let resolved = source_presentation(&source.kind)?;
        same_registration(resolved, registration)
            .then(|| self.products.source.configured_source(&source.id).ok())
            .flatten()
            .flatten()
    }

    fn begin_source_add_loading(self: &Rc<Self>) {
        let first_run = self.source.configured.borrow().first_run;
        self.cancel_startup_route_reveal();
        if !first_run {
            self.close_preferences_dialog();
            self.enter_startup_loading();
        }
    }

    fn connection_progress_view(self: &Rc<Self>) -> (gtk::Widget, gtk::Label) {
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(440);
        clamp.set_tightening_threshold(320);
        clamp.set_margin_top(72);
        clamp.set_margin_bottom(72);
        clamp.set_margin_start(24);
        clamp.set_margin_end(24);
        clamp.set_valign(gtk::Align::Center);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.add_css_class("first-run-progress");
        content.set_halign(gtk::Align::Center);
        content.set_valign(gtk::Align::Center);
        content.set_hexpand(true);

        let spinner = gtk::Spinner::new();
        spinner.set_halign(gtk::Align::Center);
        spinner.start();
        content.append(&spinner);

        let title = gtk::Label::new(Some(&tr("Caching Library")));
        title.add_css_class("title-1");
        title.set_justify(gtk::Justification::Center);
        title.set_wrap(true);
        content.append(&title);

        let status_text = self.source_connection_progress_text();
        let status = gtk::Label::new(Some(&status_text));
        status.add_css_class("muted");
        status.set_justify(gtk::Justification::Center);
        status.set_wrap(true);
        status.set_xalign(0.5);
        content.append(&status);

        clamp.set_child(Some(&content));
        scroller.set_child(Some(&clamp));
        (scroller.upcast(), status)
    }

    fn source_connection_progress_text(&self) -> String {
        source_operation_text(&self.source.operation.borrow())
            .unwrap_or_else(|| tr("Preparing library..."))
    }

    fn start_server_discovery_once(&self) {
        if self.source.discovery_started.replace(true) {
            return;
        }
        self.source.discovery_running.set(true);
        *self.source.discovery_status.borrow_mut() = DiscoveryStatus::Searching;
        self.products.source.discover_servers();
    }

    fn refresh_server_discovery(self: &Rc<Self>) {
        if self.source.discovery_running.get() {
            return;
        }
        self.source.discovery_running.set(true);
        *self.source.discovered_servers.borrow_mut() = Vec::new();
        *self.source.discovery_status.borrow_mut() = DiscoveryStatus::Searching;
        self.products.source.discover_servers();
        self.update_add_server_discovery();
    }
}

fn local_setup_flow(
    _shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    Rc::new(LocalSetupFlow {
        presentation,
        folders: Rc::new(RefCell::new(Vec::new())),
        submit: Rc::new(|source, roots| {
            source.configure_source(SourceSetup::Local { roots });
        }),
    })
}

fn jellyfin_setup_flow(
    shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    let saved = shell.reconnect_saved_source(presentation);
    Rc::new(JellyfinSetupFlow {
        presentation,
        draft: Rc::new(RefCell::new(credential_draft(
            saved.as_ref().map(|saved| saved.credentials.clone()),
        ))),
        use_instant_mix: Rc::new(Cell::new(
            saved
                .as_ref()
                .and_then(|saved| saved.jellyfin_use_instant_mix)
                .unwrap_or(false),
        )),
        submit: Rc::new(move |source, credentials, use_instant_mix| {
            source.configure_source(SourceSetup::Jellyfin {
                credentials,
                use_instant_mix,
            });
        }),
    })
}

fn subsonic_setup_flow_for(
    shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
    kind: OpenSubsonicKind,
) -> Rc<dyn SourceSetupFlow> {
    let preset = shell
        .reconnect_saved_source(presentation)
        .as_ref()
        .map(|saved| saved.credentials.clone());
    let authentication = preset
        .as_ref()
        .and_then(|preset| preset.open_subsonic_authentication)
        .unwrap_or(OpenSubsonicAuthentication::Password);
    let offer_api_key = kind == OpenSubsonicKind::OpenSubsonic;
    Rc::new(CredentialSetupFlow {
        presentation,
        draft: Rc::new(RefCell::new(credential_draft(preset))),
        authentication: Rc::new(Cell::new(authentication)),
        offer_api_key,
        submit: Rc::new(move |source, input, authentication| {
            source.configure_source(SourceSetup::OpenSubsonic {
                kind,
                authentication,
                credentials: input,
            });
        }),
    })
}

fn navidrome_setup_flow(
    shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    subsonic_setup_flow_for(shell, presentation, OpenSubsonicKind::Navidrome)
}

fn subsonic_setup_flow(
    shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    subsonic_setup_flow_for(shell, presentation, OpenSubsonicKind::OpenSubsonic)
}

fn jellyfin_settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    let instant_mix = adw::SwitchRow::builder()
        .title(tr("Use Jellyfin Instant Mix for recommendations"))
        .subtitle(tr("This uses Jellyfin API for play radio, necessary if you want recommendation plugins to work"))
        .active(saved.jellyfin_use_instant_mix.unwrap_or(false))
        .build();
    let instant_mix_for_submit = instant_mix.clone();
    let source_id = saved.source.id.clone();
    Ok(credential_source_settings_group(
        shell,
        saved.credentials.clone(),
        presentation.title,
        None,
        Some(instant_mix),
        move |source, credentials, _| {
            source.update_source(SourceSettingsChange::Jellyfin {
                source_id: source_id.clone(),
                credentials,
                use_instant_mix: instant_mix_for_submit.is_active(),
            });
        },
    ))
}

fn subsonic_settings_group_for(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
    kind: OpenSubsonicKind,
) -> Result<gtk::Widget, String> {
    let source_id = saved.source.id.clone();
    let authentication = (kind == OpenSubsonicKind::OpenSubsonic).then_some(
        saved
            .credentials
            .open_subsonic_authentication
            .unwrap_or(OpenSubsonicAuthentication::Password),
    );
    Ok(credential_source_settings_group(
        shell,
        saved.credentials.clone(),
        presentation.title,
        authentication,
        None,
        move |source, input, authentication| {
            source.update_source(SourceSettingsChange::OpenSubsonic {
                source_id: source_id.clone(),
                kind,
                authentication: authentication.unwrap_or(OpenSubsonicAuthentication::Password),
                credentials: input,
            });
        },
    ))
}

fn navidrome_settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    subsonic_settings_group_for(shell, saved, presentation, OpenSubsonicKind::Navidrome)
}

fn subsonic_settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    subsonic_settings_group_for(shell, saved, presentation, OpenSubsonicKind::OpenSubsonic)
}

impl SourceSetupFlow for CredentialSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        let (scroller, content) = setup_scaffold(shell, context, self.presentation);
        let host = credential_host(
            &self.draft,
            !context.is_first_run(),
            self.offer_api_key.then(|| Rc::clone(&self.authentication)),
        );
        content.append(&host.widget);
        let submit = Rc::clone(&self.submit);
        let authentication = Rc::clone(&self.authentication);
        append_credential_connect(shell, context, &content, host, move |source, input| {
            submit(source, input, authentication.get());
        });
        finish_setup_scaffold(shell, scroller, content, context.is_first_run())
    }
}

impl SourceSetupFlow for JellyfinSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        shell.start_server_discovery_once();
        let (scroller, content) = setup_scaffold(shell, context, self.presentation);
        let host = credential_host(&self.draft, !context.is_first_run(), None);
        content.append(&host.widget);

        let instant_mix = adw::SwitchRow::builder()
            .title(tr("Use Jellyfin Instant Mix for recommendations"))
            .subtitle(tr("This uses Jellyfin API for play radio, necessary if you want recommendation plugins to work"))
            .active(self.use_instant_mix.get())
            .build();
        let instant_group = adw::PreferencesGroup::new();
        instant_group.add(&instant_mix);
        content.append(&instant_group);
        let use_instant_mix = Rc::clone(&self.use_instant_mix);
        instant_mix.connect_active_notify(move |row| use_instant_mix.set(row.is_active()));

        let discovery = discovered_servers_view(&host);
        content.append(&discovery.group);
        *context.discovery.borrow_mut() = Some(discovery);
        let use_instant_mix = Rc::clone(&self.use_instant_mix);
        let submit = Rc::clone(&self.submit);
        append_credential_connect(
            shell,
            context,
            &content,
            host,
            move |source, credentials| {
                submit(source, credentials, use_instant_mix.get());
            },
        );
        finish_setup_scaffold(shell, scroller, content, context.is_first_run())
    }
}

impl SourceSetupFlow for LocalSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        let (scroller, content) = setup_scaffold(shell, context, self.presentation);
        let group = adw::PreferencesGroup::builder()
            .title(tr("Local library"))
            .description(tr(
                "Choose one or more folders to scan and play directly from this computer",
            ))
            .build();
        let summary = adw::ActionRow::builder()
            .title(tr("Folders"))
            .subtitle(local_folders_subtitle(&self.folders.borrow()))
            .build();
        let add = gtk::Button::with_label(&tr("Add folder"));
        add.set_valign(gtk::Align::Center);
        summary.add_suffix(&add);
        summary.set_activatable_widget(Some(&add));
        group.add(&summary);
        content.append(&group);

        let status = setup_status_label(shell);
        let login = text_button("rufin-folder-music-symbolic", "Connect");
        login.add_css_class("suggested-action");
        let actions = setup_actions(&login);
        content.append(&actions);
        content.append(&status);

        let rows = Rc::new(RefCell::new(Vec::new()));
        let selection = LocalFolderSelectionRows {
            group,
            summary,
            rows,
            folders: Rc::clone(&self.folders),
            login: login.clone(),
        };
        refresh_local_folder_selection_rows(&selection);
        let folders_for_ready = Rc::clone(&self.folders);
        *context.actions.borrow_mut() = Some(SetupActions {
            status: status.clone(),
            connect: login.clone(),
            ready: Rc::new(move || !folders_for_ready.borrow().is_empty()),
        });
        connect_add_local_folder_button(&shell.chrome.window, &add, Rc::clone(&self.folders), {
            let group = selection.group.downgrade();
            let summary = selection.summary.downgrade();
            let rows = Rc::clone(&selection.rows);
            let folders = Rc::clone(&selection.folders);
            let login = selection.login.downgrade();
            move || {
                let (Some(group), Some(summary), Some(login)) =
                    (group.upgrade(), summary.upgrade(), login.upgrade())
                else {
                    return;
                };
                refresh_local_folder_selection_rows(&LocalFolderSelectionRows {
                    group,
                    summary,
                    rows: Rc::clone(&rows),
                    folders: Rc::clone(&folders),
                    login,
                });
            }
        });

        let source = shell.products.source.clone();
        let folders = Rc::clone(&self.folders);
        let shell_for_login = Rc::downgrade(shell);
        let status_for_login = status.clone();
        let submit = Rc::clone(&self.submit);
        login.connect_clicked(move |login| {
            let roots = folders.borrow().clone();
            if roots.is_empty() {
                status_for_login.set_text(&tr("Choose at least one local music folder"));
                status_for_login.set_visible(true);
                return;
            }
            let Some(shell) = shell_for_login.upgrade() else {
                return;
            };
            let message = tr("Caching local library...");
            begin_connect_attempt(&status_for_login, login, &message);
            shell.begin_source_add_loading();
            submit(&source, roots);
        });
        source_enter_controller(&login);

        finish_setup_scaffold(shell, scroller, content, context.is_first_run())
    }
}

fn setup_scaffold(
    shell: &Rc<Shell>,
    context: &SetupViewContext,
    registration: &'static SourcePresentation,
) -> (gtk::ScrolledWindow, gtk::Box) {
    let compact = !context.is_first_run();
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(large_popup_content_width(ADD_SERVER_CLAMP_WIDTH));
    clamp.set_tightening_threshold(360);
    clamp.set_margin_top(if compact { 8 } else { 36 });
    clamp.set_margin_bottom(if compact { 20 } else { 36 });
    clamp.set_margin_start(24);
    clamp.set_margin_end(24);
    clamp.set_valign(gtk::Align::Start);
    let content = gtk::Box::new(gtk::Orientation::Vertical, if compact { 10 } else { 18 });
    content.add_css_class("first-run-content");
    if compact {
        content.add_css_class("add-server-compact-content");
    }
    content.set_hexpand(true);
    if let Some(saved_sources) = saved_source_recovery_group(shell, compact) {
        content.append(&saved_sources);
    }
    content.append(&source_choice_selector(shell, registration, compact));
    clamp.set_child(Some(&content));
    scroller.set_child(Some(&clamp));
    (scroller, content)
}

fn saved_source_recovery_group(shell: &Rc<Shell>, compact: bool) -> Option<adw::PreferencesGroup> {
    if compact {
        return None;
    }
    let sources = {
        let configured = shell.source.configured.borrow();
        let selected_registration =
            selected_source(&configured).and_then(|source| source_presentation(&source.kind));
        if configured.first_run && selected_registration.is_none() {
            configured.sources.to_vec()
        } else {
            Vec::new()
        }
    };
    if sources.is_empty() {
        return None;
    }
    let group = adw::PreferencesGroup::builder()
        .title(tr("Saved Sources"))
        .build();
    for source in sources {
        let registration = source_presentation(&source.kind);
        let fallback_title = registration
            .map(|registration| tr(registration.title))
            .unwrap_or_else(|| source.kind.clone());
        let title = source.name.trim();
        let row = adw::ActionRow::builder()
            .title(if title.is_empty() {
                fallback_title.as_str()
            } else {
                title
            })
            .subtitle(fallback_title.as_str())
            .activatable(registration.is_some())
            .build();
        let icon = gtk::Image::from_icon_name(
            registration.map_or("rufin-network-server-symbolic", |registration| {
                registration.icon_name
            }),
        );
        row.add_prefix(&icon);
        let forget = gtk::Button::from_icon_name("rufin-window-close-symbolic");
        forget.set_tooltip_text(Some(&tr("Forget Server")));
        forget.set_valign(gtk::Align::Center);
        forget.add_css_class("flat");
        forget.add_css_class("destructive-action");
        let forget_shell = Rc::downgrade(shell);
        let forgotten_source_id = source.id.clone();
        let forgotten_source_name = if title.is_empty() {
            fallback_title.clone()
        } else {
            title.to_string()
        };
        forget.connect_clicked(move |_| {
            let Some(shell) = forget_shell.upgrade() else {
                return;
            };
            confirm_forget_source(
                &shell,
                forgotten_source_id.clone(),
                &forgotten_source_name,
                Rc::new(|| {}),
            );
        });
        row.add_suffix(&forget);
        if registration.is_some() {
            let source_id = source.id;
            let source = shell.products.source.clone();
            row.connect_activated(move |_| {
                source.select_source(source_id.clone());
            });
        }
        group.add(&row);
    }
    Some(group)
}

fn finish_setup_scaffold(
    shell: &Rc<Shell>,
    scroller: gtk::ScrolledWindow,
    content: gtk::Box,
    embedded: bool,
) -> gtk::Widget {
    if embedded {
        let privacy_group = adw::PreferencesGroup::builder()
            .title(tr("Privacy and Security"))
            .build();
        let private_mode = adw::SwitchRow::builder()
            .title(tr("Private mode"))
            .active(shell.settings.current.borrow().private_mode)
            .build();
        let private_shell = Rc::downgrade(shell);
        private_mode.connect_active_notify(move |row| {
            if let Some(shell) = private_shell.upgrade() {
                shell.set_private_mode(row.is_active());
            }
        });
        privacy_group.add(&private_mode);
        content.append(&privacy_group);
    }
    let view = scroller.upcast::<gtk::Widget>();
    view
}

fn source_choice_selector(
    shell: &Rc<Shell>,
    selected: &'static SourcePresentation,
    compact: bool,
) -> gtk::Box {
    let wrapper = gtk::Box::new(gtk::Orientation::Horizontal, if compact { 4 } else { 8 });
    wrapper.add_css_class("source-choice-list");
    if compact {
        wrapper.add_css_class("compact-source-choice-list");
    }
    wrapper.set_homogeneous(true);
    wrapper.set_hexpand(true);
    for presentation in source_presentations() {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("source-choice-button");
        if compact {
            button.add_css_class("compact-source-choice-button");
        }
        set_source_choice_active(&button, same_registration(presentation, selected));
        button.update_property(&[gtk::accessible::Property::Label(&tr(presentation.title))]);
        let child = gtk::Box::new(gtk::Orientation::Vertical, if compact { 2 } else { 4 });
        child.set_halign(gtk::Align::Center);
        let icon = gtk::Image::from_icon_name(presentation.icon_name);
        let icon_size = if compact { 24 } else { 34 };
        icon.set_pixel_size(icon_size);
        child.append(&icon);
        let label = gtk::Label::new(Some(&tr(presentation.title)));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(14);
        child.append(&label);
        button.set_child(Some(&child));
        if !same_registration(presentation, selected) {
            let shell = Rc::downgrade(shell);
            let presentation = *presentation;
            button.connect_clicked(move |_| {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let Some(context) = shell
                    .source
                    .add_server
                    .borrow()
                    .as_ref()
                    .map(|handle| handle.context.clone())
                else {
                    return;
                };
                *context.flow.borrow_mut() = (presentation.setup_flow)(&shell, presentation);
                mount_setup_flow(&shell, &context);
            });
        }
        wrapper.append(&button);
    }
    wrapper
}

fn set_source_choice_active(button: &gtk::Button, active: bool) {
    if active {
        button.add_css_class("active");
    } else {
        button.remove_css_class("active");
    }
}

fn same_registration(
    left: &'static SourcePresentation,
    right: &'static SourcePresentation,
) -> bool {
    std::ptr::eq(left, right)
}

fn mount_setup_flow(shell: &Rc<Shell>, context: &SetupViewContext) {
    context.actions.borrow_mut().take();
    context.discovery.borrow_mut().take();
    context.retry_focus.borrow_mut().take();
    let flow = context.flow.borrow().clone();
    let view = flow.view(shell, context);
    replace_add_server_content(&context.form, view);
    if let Some(discovery) = context.discovery.borrow().as_ref() {
        refresh_discovered_servers_view(shell, discovery);
    }
    update_setup_surface(shell, context);
}

fn update_setup_surface(shell: &Rc<Shell>, context: &SetupViewContext) {
    let operation = shell.source.operation.borrow();
    match setup_surface_state(&operation) {
        SetupSurfaceState::Progress => {
            remember_setup_focus(context);
            context
                .progress_status
                .set_text(&shell.source_connection_progress_text());
            if let Some(actions) = context.actions.borrow().as_ref() {
                actions.connect.set_sensitive(false);
            }
            context.surface.set_visible_child_name(SETUP_PROGRESS_PAGE);
        }
        SetupSurfaceState::Failed(message) => {
            if let Some(actions) = context.actions.borrow().as_ref() {
                actions.status.set_text(message);
                actions.status.add_css_class("error-text");
                actions.status.set_visible(true);
                actions.connect.set_sensitive((actions.ready)());
            }
            context.surface.set_visible_child_name(SETUP_FORM_PAGE);
            restore_setup_focus(context);
        }
        SetupSurfaceState::Form => {
            if let Some(actions) = context.actions.borrow().as_ref() {
                actions.status.remove_css_class("error-text");
                actions.status.set_text("");
                actions.status.set_visible(false);
                actions.connect.set_sensitive((actions.ready)());
            }
            context.surface.set_visible_child_name(SETUP_FORM_PAGE);
            restore_setup_focus(context);
        }
    }
}

fn remember_setup_focus(context: &SetupViewContext) {
    if context.retry_focus.borrow().is_some() {
        return;
    }
    let focus = context
        .content
        .root()
        .and_then(|root| root.focus())
        .filter(|focus| focus.is_ancestor(&context.form));
    *context.retry_focus.borrow_mut() = focus;
}

fn restore_setup_focus(context: &SetupViewContext) {
    if let Some(focus) = context.retry_focus.borrow_mut().take() {
        focus.grab_focus();
    }
}

fn setup_surface_state(operation: &SourceOperation) -> SetupSurfaceState<'_> {
    match operation {
        SourceOperation::Adding { .. } => SetupSurfaceState::Progress,
        SourceOperation::Failed {
            message,
            add_form: true,
            ..
        } => SetupSurfaceState::Failed(message),
        SourceOperation::Idle
        | SourceOperation::Switching { .. }
        | SourceOperation::Refreshing { .. }
        | SourceOperation::Failed {
            add_form: false, ..
        } => SetupSurfaceState::Form,
    }
}

fn credential_draft(preset: Option<CredentialPreset>) -> CredentialHostDraft {
    preset.map_or_else(
        || CredentialHostDraft {
            name: String::new(),
            url: "http://".to_string(),
            username: String::new(),
            password: String::new(),
            cert_verify: true,
        },
        |preset| CredentialHostDraft {
            name: preset.source_name,
            url: preset.server_url,
            username: preset.username,
            password: String::new(),
            cert_verify: !preset.trust_invalid_cert,
        },
    )
}

fn credential_host(
    draft: &Rc<RefCell<CredentialHostDraft>>,
    compact: bool,
    authentication: Option<Rc<Cell<OpenSubsonicAuthentication>>>,
) -> CredentialHost {
    let snapshot = draft.borrow().clone();
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let name = adw::EntryRow::builder()
        .title(tr("Name"))
        .text(&snapshot.name)
        .build();
    style_compact_field_row(&name);
    let url = adw::EntryRow::builder()
        .title(tr("Server Address"))
        .text(&snapshot.url)
        .build();
    style_compact_field_row(&url);
    let fields = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    fields.set_homogeneous(true);
    fields.set_hexpand(true);
    fields.append(&compact_field_row_group(&name));
    fields.append(&compact_field_row_group(&url));
    let fields = install_compact_field_row_responsiveness(&fields);
    let fields_group = if compact {
        adw::PreferencesGroup::new()
    } else {
        adw::PreferencesGroup::builder().title(tr("Server")).build()
    };
    fields_group.add(&fields);
    section.append(&fields_group);

    let username = adw::EntryRow::builder()
        .title(tr("Username"))
        .text(&snapshot.username)
        .build();
    style_compact_field_row(&username);
    let password = adw::PasswordEntryRow::builder()
        .title(tr("Password"))
        .build();
    password.set_text(&snapshot.password);
    style_compact_field_row(&password);
    let cert_verify = adw::SwitchRow::builder()
        .title(tr("Verify server certificate"))
        .subtitle(tr("Turn off only for a server you control"))
        .active(snapshot.cert_verify)
        .build();
    let api_key = authentication.as_ref().map(|authentication| {
        open_subsonic_authentication_switch(Rc::clone(authentication), &username, &password)
    });
    let rows = adw::PreferencesGroup::new();
    if let Some(api_key) = api_key.as_ref() {
        rows.add(api_key);
    }
    rows.add(&username);
    rows.add(&password);
    rows.add(&cert_verify);
    section.append(&rows);

    bind_credential_draft(draft, &name, &url, &username, &password, &cert_verify);
    CredentialHost {
        widget: section,
        name,
        url,
        username,
        password,
        cert_verify,
        authentication,
        api_key,
    }
}

pub(super) fn update_open_subsonic_authentication_fields(
    authentication: OpenSubsonicAuthentication,
    username: &adw::EntryRow,
    secret: &adw::PasswordEntryRow,
) {
    let api_key = authentication == OpenSubsonicAuthentication::ApiKey;
    username.set_visible(!api_key);
    secret.set_title(&if api_key {
        tr("API key")
    } else {
        tr("Password")
    });
}

pub(super) fn open_subsonic_authentication_switch(
    authentication: Rc<Cell<OpenSubsonicAuthentication>>,
    username: &adw::EntryRow,
    secret: &adw::PasswordEntryRow,
) -> adw::SwitchRow {
    update_open_subsonic_authentication_fields(authentication.get(), username, secret);
    let api_key = adw::SwitchRow::builder()
        .title(tr("Use API key"))
        .subtitle(tr("Recommended when your server provides one"))
        .active(authentication.get() == OpenSubsonicAuthentication::ApiKey)
        .build();
    let username = username.downgrade();
    let secret = secret.downgrade();
    api_key.connect_active_notify(move |row| {
        let next = if row.is_active() {
            OpenSubsonicAuthentication::ApiKey
        } else {
            OpenSubsonicAuthentication::Password
        };
        authentication.set(next);
        if let (Some(username), Some(secret)) = (username.upgrade(), secret.upgrade()) {
            update_open_subsonic_authentication_fields(next, &username, &secret);
        }
    });
    api_key
}

fn bind_credential_draft(
    draft: &Rc<RefCell<CredentialHostDraft>>,
    name: &adw::EntryRow,
    url: &adw::EntryRow,
    username: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
    cert_verify: &adw::SwitchRow,
) {
    let value = Rc::clone(draft);
    name.connect_text_notify(move |row| value.borrow_mut().name = row.text().to_string());
    let value = Rc::clone(draft);
    url.connect_text_notify(move |row| value.borrow_mut().url = row.text().to_string());
    let value = Rc::clone(draft);
    username.connect_text_notify(move |row| value.borrow_mut().username = row.text().to_string());
    let value = Rc::clone(draft);
    password.connect_text_notify(move |row| value.borrow_mut().password = row.text().to_string());
    let value = Rc::clone(draft);
    cert_verify.connect_active_notify(move |row| value.borrow_mut().cert_verify = row.is_active());
}

fn append_credential_connect(
    shell: &Rc<Shell>,
    context: &SetupViewContext,
    content: &gtk::Box,
    host: CredentialHost,
    submit: impl Fn(&SourceHandle, CredentialInput) + 'static,
) {
    let status = setup_status_label(shell);
    let login = text_button("rufin-network-server-symbolic", "Connect");
    login.add_css_class("suggested-action");
    login.set_sensitive(host.ready());
    connect_entry_row_activation(&host.name, &login);
    connect_entry_row_activation(&host.url, &login);
    connect_entry_row_activation(&host.username, &login);
    connect_password_entry_row_activation(&host.password, &login);
    {
        let login = login.downgrade();
        let host = host.clone();
        host.url.clone().connect_text_notify(move |_| {
            if let Some(login) = login.upgrade() {
                login.set_sensitive(host.ready());
            }
        });
    }
    {
        let login = login.downgrade();
        let host = host.clone();
        host.username.clone().connect_text_notify(move |_| {
            if let Some(login) = login.upgrade() {
                login.set_sensitive(host.ready());
            }
        });
    }
    {
        let login = login.downgrade();
        let host = host.clone();
        host.password.clone().connect_text_notify(move |_| {
            if let Some(login) = login.upgrade() {
                login.set_sensitive(host.ready());
            }
        });
    }
    if let Some(api_key) = host.api_key.as_ref() {
        let login = login.downgrade();
        let host = host.clone();
        api_key.connect_active_notify(move |_| {
            if let Some(login) = login.upgrade() {
                login.set_sensitive(host.ready());
            }
        });
    }
    let host_for_ready = host.clone();
    *context.actions.borrow_mut() = Some(SetupActions {
        status: status.clone(),
        connect: login.clone(),
        ready: Rc::new(move || host_for_ready.ready()),
    });

    let source = shell.products.source.clone();
    let shell_for_login = Rc::downgrade(shell);
    let status_for_login = status.clone();
    let host_for_click = host.clone();
    login.connect_clicked(move |login| {
        if !host_for_click.ready() {
            let message = if host_for_click
                .authentication
                .as_ref()
                .is_some_and(|authentication| {
                    authentication.get() == OpenSubsonicAuthentication::ApiKey
                }) {
                tr("Enter a server address and API key")
            } else {
                tr("Enter a server address, username, and password")
            };
            status_for_login.set_text(&message);
            status_for_login.set_visible(true);
            return;
        }
        let Some(shell) = shell_for_login.upgrade() else {
            return;
        };
        let message = tr("Connecting to music server...");
        begin_connect_attempt(&status_for_login, login, &message);
        shell.begin_source_add_loading();
        submit(&source, host_for_click.input());
    });
    content.append(&setup_actions(&login));
    content.append(&status);
}

fn setup_actions(login: &gtk::Button) -> gtk::Box {
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    actions.set_halign(gtk::Align::End);
    actions.append(login);
    actions
}

fn setup_status_label(shell: &Rc<Shell>) -> gtk::Label {
    let status = gtk::Label::new(None);
    status.add_css_class("muted");
    status.set_wrap(true);
    status.set_xalign(0.0);
    if let SourceOperation::Failed {
        message,
        add_form: true,
        ..
    } = &*shell.source.operation.borrow()
    {
        status.set_text(message);
        status.add_css_class("error-text");
        status.set_visible(true);
    } else {
        status.set_visible(false);
    }
    status
}

fn begin_connect_attempt(status: &gtk::Label, login: &gtk::Button, message: &str) {
    status.remove_css_class("error-text");
    status.set_text(message);
    status.set_visible(true);
    login.set_sensitive(false);
}

fn discovered_servers_view(host: &CredentialHost) -> DiscoveredServersView {
    let group = adw::PreferencesGroup::builder()
        .title(tr("Found Servers"))
        .build();
    DiscoveredServersView {
        group,
        rows: Rc::new(RefCell::new(Vec::new())),
        host: host.clone(),
    }
}

fn refresh_discovered_servers_view(shell: &Rc<Shell>, view: &DiscoveredServersView) {
    let status = shell.source.discovery_status.borrow().clone();
    let running = shell.source.discovery_running.get();
    let servers = shell.source.discovered_servers.borrow().clone();
    view.group
        .set_description(Some(&discovery_status_label(&status)));
    for row in view.rows.borrow_mut().drain(..) {
        view.group.remove(&row);
    }
    if servers.is_empty() {
        let row = adw::ActionRow::builder()
            .title(if running {
                tr("Searching Local Network")
            } else {
                tr("No Servers Found")
            })
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("rufin-network-server-symbolic"));
        if running {
            let spinner = gtk::Spinner::new();
            spinner.start();
            row.add_suffix(&spinner);
        }
        view.group.add(&row);
        view.rows.borrow_mut().push(row.upcast());
    } else {
        for server in servers {
            let row = adw::ActionRow::builder()
                .title(server.name.clone())
                .subtitle(server.address.clone())
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(
                "io.github.screwys.Rufin.source.jellyfin",
            ));
            row.set_activatable(true);
            let name = view.host.name.clone();
            let url = view.host.url.clone();
            row.connect_activated(move |_| {
                name.set_text(&server.name);
                url.set_text(&server.address);
            });
            view.group.add(&row);
            view.rows.borrow_mut().push(row.upcast());
        }
    }
    let search = adw::ButtonRow::builder()
        .title(if running {
            tr("Searching...")
        } else {
            tr("Search Again")
        })
        .start_icon_name("rufin-view-refresh-symbolic")
        .build();
    search.set_sensitive(!running);
    let shell = Rc::downgrade(shell);
    search.connect_activated(move |_| {
        if let Some(shell) = shell.upgrade() {
            shell.refresh_server_discovery();
        }
    });
    view.group.add(&search);
    view.rows.borrow_mut().push(search.upcast());
}

fn discovery_status_label(status: &DiscoveryStatus) -> String {
    match status {
        DiscoveryStatus::Idle => tr("Searching will start automatically"),
        DiscoveryStatus::Searching => tr("Searching for Jellyfin servers on the local network..."),
        DiscoveryStatus::Empty => tr("No servers found, add one manually or try again"),
        DiscoveryStatus::Found(_) => String::new(),
        DiscoveryStatus::Failed(error) => {
            tr_with("Server discovery failed: {error}", &[("error", error)])
        }
    }
}

#[derive(Clone)]
struct LocalFolderSelectionRows {
    group: adw::PreferencesGroup,
    summary: adw::ActionRow,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    folders: Rc<RefCell<Vec<PathBuf>>>,
    login: gtk::Button,
}

fn refresh_local_folder_selection_rows(selection: &LocalFolderSelectionRows) {
    let folders = selection.folders.borrow().clone();
    selection
        .summary
        .set_subtitle(&local_folders_subtitle(&folders));
    selection.login.set_sensitive(!folders.is_empty());
    for row in selection.rows.borrow_mut().drain(..) {
        selection.group.remove(&row);
    }
    for folder in folders {
        let row = adw::ActionRow::builder()
            .title(local_folder_title(&folder))
            .subtitle(path_subtitle(&folder))
            .subtitle_lines(2)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("rufin-folders-symbolic"));
        let remove = gtk::Button::from_icon_name("rufin-window-close-symbolic");
        remove.set_tooltip_text(Some(&tr("Remove folder")));
        remove.add_css_class("flat");
        remove.add_css_class("destructive-action");
        row.add_suffix(&remove);
        let group = selection.group.downgrade();
        let summary = selection.summary.downgrade();
        let rows = Rc::downgrade(&selection.rows);
        let folders = Rc::clone(&selection.folders);
        let login = selection.login.downgrade();
        let folder = folder.clone();
        remove.connect_clicked(move |_| {
            folders
                .borrow_mut()
                .retain(|candidate| candidate != &folder);
            let (Some(group), Some(summary), Some(rows), Some(login)) = (
                group.upgrade(),
                summary.upgrade(),
                rows.upgrade(),
                login.upgrade(),
            ) else {
                return;
            };
            refresh_local_folder_selection_rows(&LocalFolderSelectionRows {
                group,
                summary,
                rows,
                folders: Rc::clone(&folders),
                login,
            });
        });
        selection.group.add(&row);
        selection.rows.borrow_mut().push(row);
    }
}

fn source_enter_controller(login: &gtk::Button) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let login_for_key = login.downgrade();
    controller.connect_key_pressed(move |_, key, _, _| {
        let enter = key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter;
        if enter
            && login_for_key
                .upgrade()
                .is_some_and(|login| activate_connect_if_ready(&login))
        {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    login.add_controller(controller);
}

fn connect_entry_row_activation(entry: &adw::EntryRow, login: &gtk::Button) {
    let login = login.downgrade();
    entry.connect_entry_activated(move |_| {
        if let Some(login) = login.upgrade() {
            activate_connect_if_ready(&login);
        }
    });
}

fn connect_password_entry_row_activation(entry: &adw::PasswordEntryRow, login: &gtk::Button) {
    let login = login.downgrade();
    entry.connect_entry_activated(move |_| {
        if let Some(login) = login.upgrade() {
            activate_connect_if_ready(&login);
        }
    });
}

fn activate_connect_if_ready(login: &gtk::Button) -> bool {
    if !login.is_sensitive() {
        return false;
    }
    login.emit_clicked();
    true
}

fn trimmed_optional_text(row: &adw::EntryRow) -> Option<String> {
    let text = row.text();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn remote_login_ready(
    url: &adw::EntryRow,
    username: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
    username_required: bool,
) -> bool {
    let address = url.text();
    let address = address.trim().trim_end_matches('/');
    let address_without_scheme = address
        .strip_prefix("http://")
        .or_else(|| address.strip_prefix("https://"))
        .unwrap_or(address);
    !address_without_scheme.trim().is_empty()
        && (!username_required || !username.text().trim().is_empty())
        && !password.text().trim().is_empty()
}

fn default_music_folder() -> Option<PathBuf> {
    let user_dirs = directories::UserDirs::new()?;
    Some(
        user_dirs
            .audio_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| user_dirs.home_dir().join("Music")),
    )
}

fn path_subtitle(path: &Path) -> String {
    path.display().to_string()
}

fn local_folders_subtitle(folders: &[PathBuf]) -> String {
    match folders {
        [] => tr("No folders selected"),
        [folder] => path_subtitle(folder),
        folders => folder_selected_text(folders.len() as u64),
    }
}

fn append_local_folder(folders: &Rc<RefCell<Vec<PathBuf>>>, path: PathBuf) {
    let mut folders = folders.borrow_mut();
    if !folders.iter().any(|folder| folder == &path) {
        folders.push(path);
    }
}

fn local_folder_title(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| path_subtitle(path))
}

pub(super) fn connect_folder_button(
    window: &gtk::ApplicationWindow,
    button: &gtk::Button,
    row: &adw::ActionRow,
    target: Rc<RefCell<Option<PathBuf>>>,
    on_changed: impl Fn(PathBuf) + 'static,
) {
    let window = window.downgrade();
    let row = row.downgrade();
    let on_changed: Rc<dyn Fn(PathBuf)> = Rc::new(on_changed);
    button.connect_clicked(move |_| {
        let Some(window) = window.upgrade() else {
            return;
        };
        let target = Rc::clone(&target);
        let row = row.clone();
        let on_changed = Rc::downgrade(&on_changed);
        gtk::glib::spawn_future_local(async move {
            let selected_folder = target.borrow().as_ref().map(gtk::gio::File::for_path);
            let dialog = gtk::FileDialog::builder()
                .title(tr("Select Music Folder"))
                .build();
            if let Some(folder) = selected_folder.as_ref() {
                dialog.set_initial_folder(Some(folder));
            }
            let Ok(folder) = dialog.select_folder_future(Some(&window)).await else {
                return;
            };
            let Some(path) = folder.path() else {
                return;
            };
            let (Some(row), Some(on_changed)) = (row.upgrade(), on_changed.upgrade()) else {
                return;
            };
            row.set_subtitle(&path_subtitle(&path));
            *target.borrow_mut() = Some(path.clone());
            on_changed(path);
        });
    });
}

fn connect_add_local_folder_button(
    window: &gtk::ApplicationWindow,
    button: &gtk::Button,
    folders: Rc<RefCell<Vec<PathBuf>>>,
    on_changed: impl Fn() + 'static,
) {
    let window = window.downgrade();
    let on_changed: Rc<dyn Fn()> = Rc::new(on_changed);
    button.connect_clicked(move |_| {
        let Some(window) = window.upgrade() else {
            return;
        };
        let folders = Rc::clone(&folders);
        let on_changed = Rc::clone(&on_changed);
        gtk::glib::spawn_future_local(async move {
            let selected_folder = folders
                .borrow()
                .last()
                .cloned()
                .or_else(default_music_folder)
                .map(gtk::gio::File::for_path);
            let dialog = gtk::FileDialog::builder()
                .title(tr("Select Music Folder"))
                .build();
            if let Some(folder) = selected_folder.as_ref() {
                dialog.set_initial_folder(Some(folder));
            }
            let Ok(folder) = dialog.select_folder_future(Some(&window)).await else {
                return;
            };
            let Some(path) = folder.path() else {
                return;
            };
            append_local_folder(&folders, path);
            on_changed();
        });
    });
}

fn replace_add_server_content(content: &gtk::Box, child: gtk::Widget) {
    while let Some(current) = content.first_child() {
        content.remove(&current);
    }
    content.append(&child);
}
