use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::runtime::source::{
    CredentialInput, CredentialPreset, DiscoveryStatus, EditableSource, OpenSubsonicAuthentication,
    OpenSubsonicKind, SourceHandle, SourceOperation, SourceSettingsChange, SourceSetup,
};
use adw::prelude::*;

use super::{
    field_layout::{
        compact_field_row_group, install_compact_field_row_responsiveness_at,
        style_compact_field_row,
    },
    folder_selected_text, source_operation_text,
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

fn source_presentation(kind: &str) -> Option<&'static SourcePresentation> {
    source_presentations()
        .iter()
        .copied()
        .find(|presentation| presentation.kind == kind)
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

pub(crate) struct SourceSetupViewHandle {
    onboarding: bool,
    context: Option<SetupViewContext>,
    flow: Rc<RefCell<Rc<dyn SourceSetupFlow>>>,
}

impl SourceSetupViewHandle {
    fn new(context: SetupViewContext) -> Self {
        Self {
            onboarding: context.onboarding,
            flow: Rc::clone(&context.flow),
            context: Some(context),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SetupViewContext {
    onboarding: bool,
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

struct SetupActions {
    status: gtk::Label,
    connect: gtk::Button,
    ready: Rc<dyn Fn() -> bool>,
}

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
    fn is_mounted(&self) -> bool {
        self.content.root().is_some()
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
    offer_authentication: bool,
    submit: Rc<dyn Fn(&SourceHandle, CredentialInput, OpenSubsonicAuthentication)>,
}

struct JellyfinSetupFlow {
    presentation: &'static SourcePresentation,
    draft: Rc<RefCell<CredentialHostDraft>>,
    use_instant_mix: Rc<Cell<bool>>,
    submit: Rc<dyn Fn(&SourceHandle, CredentialInput, bool)>,
}

struct SourceChoiceFlow;

impl SourceSetupFlow for SourceChoiceFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        source_choice_selector(shell, context)
    }
}

struct LocalSetupFlow {
    presentation: &'static SourcePresentation,
    folders: Rc<RefCell<Vec<PathBuf>>>,
    submit: Rc<dyn Fn(&SourceHandle, Vec<PathBuf>)>,
}

#[derive(Clone)]
struct CredentialHost {
    widget: gtk::Box,
    fields_group: adw::PreferencesGroup,
    rows: adw::PreferencesGroup,
    name: adw::EntryRow,
    url: adw::EntryRow,
    username: adw::EntryRow,
    password: adw::PasswordEntryRow,
    cert_verify: adw::SwitchRow,
    authentication: Option<Rc<Cell<OpenSubsonicAuthentication>>>,
    authentication_toggles: Option<[adw::SwitchRow; 2]>,
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
                authentication.get() != OpenSubsonicAuthentication::ApiKey
            }),
        )
    }
}

impl Shell {
    pub(crate) fn present_onboarding(self: &Rc<Self>) {
        let context = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .and_then(|handle| handle.context.clone())
            .filter(|context| context.onboarding);
        if let Some(context) = context {
            if let Some(dialog) = context.preferences_dialog {
                dialog.present(Some(&self.chrome.window));
                return;
            }
        }
        let resource = crate::ui_resource::ONBOARDING_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            dialog: adw::Dialog, toolbar: adw::ToolbarView,
        });
        let flow = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .filter(|handle| handle.onboarding)
            .map(|handle| Rc::clone(&handle.flow));
        let context = self.setup_view_context(Some(dialog.clone()), flow, true);
        mount_setup_flow(self, &context);
        toolbar.set_content(Some(&context.content));
        *self.source.add_server.borrow_mut() = Some(SourceSetupViewHandle::new(context));
        let weak = Rc::downgrade(self);
        dialog.connect_closed(move |_| {
            if let Some(shell) = weak.upgrade() {
                shell.release_inactive_add_server_form();
            }
        });
        dialog.present(Some(&self.chrome.window));
    }

    pub(crate) fn add_server_navigation_page(
        self: &Rc<Self>,
        preferences_dialog: &adw::Dialog,
    ) -> adw::NavigationPage {
        // Keep only the flow draft across page replacement. A failed connection
        // rebuilds the form from those values so the user can correct and retry.
        let flow = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .filter(|handle| !handle.onboarding)
            .map(|handle| Rc::clone(&handle.flow));
        let context = self.setup_view_context(Some(preferences_dialog.clone()), flow, false);
        mount_setup_flow(self, &context);
        *self.source.add_server.borrow_mut() = Some(SourceSetupViewHandle::new(context.clone()));
        adw::NavigationPage::new(&context.content, &tr("Add Source"))
    }

    pub(crate) fn update_add_server_dialog(self: &Rc<Self>) {
        let Some(context) = self.mounted_add_server_context() else {
            return;
        };
        update_setup_surface(self, &context);
    }

    pub(crate) fn update_add_server_discovery(self: &Rc<Self>) {
        let Some(context) = self.mounted_add_server_context() else {
            return;
        };
        if let Some(discovery) = context.discovery.borrow().as_ref() {
            refresh_discovered_servers_view(self, discovery);
        }
    }

    pub(crate) fn complete_add_server_dialog(&self) {
        let preferences_dialog = {
            let mut handle = self.source.add_server.borrow_mut();
            handle
                .take()
                .and_then(|handle| handle.context)
                .and_then(|context| context.preferences_dialog)
        };
        if let Some(dialog) = preferences_dialog {
            dialog.close();
        }
    }

    pub(crate) fn clear_retained_add_server_form(&self) {
        self.source.add_server.borrow_mut().take();
    }

    fn mounted_add_server_context(&self) -> Option<SetupViewContext> {
        let context = self.source.add_server.borrow().as_ref()?.context.clone()?;
        context.is_mounted().then_some(context)
    }

    pub(crate) fn release_inactive_add_server_form(&self) {
        let mut handle = self.source.add_server.borrow_mut();
        let Some(handle) = handle.as_mut() else {
            return;
        };
        handle.context.take();
    }

    pub(crate) fn has_retained_add_server_form(&self) -> bool {
        self.source.add_server.borrow().is_some()
    }

    pub(crate) fn restore_add_server_dialog_after_failure(self: &Rc<Self>) -> bool {
        let context = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .and_then(|handle| handle.context.clone());
        let Some(context) = context else {
            if self.has_retained_add_server_form() {
                if self
                    .source
                    .add_server
                    .borrow()
                    .as_ref()
                    .is_some_and(|handle| handle.onboarding)
                {
                    self.present_onboarding();
                } else {
                    crate::preferences::present_add_server_preferences_dialog(self);
                }
                return true;
            }
            return false;
        };
        let Some(dialog) = context.preferences_dialog.clone() else {
            return false;
        };
        if context.onboarding {
            dialog.present(Some(&self.chrome.window));
        } else {
            self.preferences.set_active_dialog(&dialog);
            present_light_dismiss_dialog(&dialog, &self.chrome.window);
        }
        update_setup_surface(self, &context);
        true
    }

    fn setup_view_context(
        self: &Rc<Self>,
        preferences_dialog: Option<adw::Dialog>,
        flow: Option<Rc<RefCell<Rc<dyn SourceSetupFlow>>>>,
        onboarding: bool,
    ) -> SetupViewContext {
        let resource = crate::ui_resource::CONNECTION_PROGRESS_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            content: gtk::Box,
            surface: gtk::Stack,
            form: gtk::Box,
            status: gtk::Label,
        });
        status.set_label(&self.source_connection_progress_text());
        let flow = flow.unwrap_or_else(|| Rc::new(RefCell::new(self.default_source_setup_flow())));
        SetupViewContext {
            onboarding,
            content,
            surface,
            form,
            progress_status: status,
            flow,
            actions: Rc::new(RefCell::new(None)),
            discovery: Rc::new(RefCell::new(None)),
            retry_focus: Rc::new(RefCell::new(None)),
            preferences_dialog,
        }
    }

    fn default_source_setup_flow(self: &Rc<Self>) -> Rc<dyn SourceSetupFlow> {
        Rc::new(SourceChoiceFlow)
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
    _shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    Rc::new(JellyfinSetupFlow {
        presentation,
        draft: Rc::new(RefCell::new(credential_draft(None))),
        use_instant_mix: Rc::new(Cell::new(false)),
        submit: Rc::new(move |source, credentials, use_instant_mix| {
            source.configure_source(SourceSetup::Jellyfin {
                credentials,
                use_instant_mix,
            });
        }),
    })
}

fn subsonic_setup_flow_for(
    _shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
    kind: OpenSubsonicKind,
) -> Rc<dyn SourceSetupFlow> {
    let preset = None;
    let authentication = OpenSubsonicAuthentication::Password;
    let offer_authentication = kind == OpenSubsonicKind::OpenSubsonic;
    Rc::new(CredentialSetupFlow {
        presentation,
        draft: Rc::new(RefCell::new(credential_draft(preset))),
        authentication: Rc::new(Cell::new(authentication)),
        offer_authentication,
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

fn credential_source_settings_group(
    shell: &Rc<Shell>,
    preset: CredentialPreset,
    source_title: &'static str,
    authentication: Option<OpenSubsonicAuthentication>,
    extra: Option<adw::SwitchRow>,
    submit: impl Fn(&SourceHandle, CredentialInput, Option<OpenSubsonicAuthentication>) + 'static,
) -> gtk::Widget {
    let snapshot = credential_draft(Some(preset));
    let authentication = authentication.map(|value| Rc::new(Cell::new(value)));
    let host = credential_host_view(&snapshot, true, authentication);
    host.fields_group.set_title(&tr("Server Settings"));
    host.fields_group.set_description(Some(&tr(source_title)));
    host.cert_verify
        .set_subtitle(&tr("Off only for a server you control"));
    if let Some(extra) = extra.as_ref() {
        host.rows.add(extra);
    }

    let save = adw::ButtonRow::builder()
        .title(tr("Save Server Settings"))
        .start_icon_name("rufin-document-save-symbolic")
        .end_icon_name("rufin-go-next-symbolic")
        .build();
    save.add_css_class("manage-server-action-row");
    save.add_css_class("suggested-action");
    host.rows.add(&save);

    let source = shell.products.source.clone();
    let name = host.name.clone();
    let url = host.url.clone();
    let username = host.username.clone();
    let password = host.password.clone();
    let cert_verify = host.cert_verify.clone();
    let authentication = host.authentication.clone();
    save.connect_activated(move |_| {
        let authentication = authentication
            .as_ref()
            .map(|authentication| authentication.get());
        submit(
            &source,
            CredentialInput {
                source_name: Some(name.text().trim().to_string()),
                server_url: url.text().trim().to_string(),
                username: username.text().trim().to_string(),
                secret: password.text().to_string(),
                trust_invalid_cert: !cert_verify.is_active(),
            },
            authentication,
        );
    });

    host.widget.upcast()
}

impl SourceSetupFlow for CredentialSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        let (scroller, content, actions, status) =
            setup_scaffold(shell, context, self.presentation);
        let host = credential_host(
            &self.draft,
            true,
            self.offer_authentication
                .then(|| Rc::clone(&self.authentication)),
        );
        content.append(&host.widget);
        let submit = Rc::clone(&self.submit);
        let authentication = Rc::clone(&self.authentication);
        append_credential_connect(
            shell,
            context,
            &content,
            &actions,
            status,
            host,
            move |source, input| {
                submit(source, input, authentication.get());
            },
        );
        scroller.upcast()
    }
}

impl SourceSetupFlow for JellyfinSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        shell.start_server_discovery_once();
        let (scroller, content, actions, status) =
            setup_scaffold(shell, context, self.presentation);
        let host = credential_host(&self.draft, true, None);
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
            &actions,
            status,
            host,
            move |source, credentials| {
                submit(source, credentials, use_instant_mix.get());
            },
        );
        scroller.upcast()
    }
}

impl SourceSetupFlow for LocalSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
        if shell
            .source
            .configured
            .borrow()
            .sources
            .iter()
            .any(|source| source.kind == LOCAL_SOURCE_KIND)
        {
            if let Some(dialog) = &context.preferences_dialog {
                return crate::preferences::library::local_sources_page(shell, dialog).upcast();
            }
        }
        let (scroller, content, actions, status) =
            setup_scaffold(shell, context, self.presentation);
        let resource = crate::ui_resource::LOCAL_SETUP_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            group: adw::PreferencesGroup,
            summary: adw::ActionRow,
            add: gtk::Button,
            connect: gtk::Button,
        });
        summary.set_subtitle(&local_folders_subtitle(&self.folders.borrow()));
        summary.set_activatable_widget(Some(&add));
        content.append(&group);

        let login = connect;
        actions.append(&login);
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
        let status_for_login = status.clone();
        let submit = Rc::clone(&self.submit);
        login.connect_clicked(move |login| {
            let roots = folders.borrow().clone();
            if roots.is_empty() {
                status_for_login.set_text(&tr("Choose at least one local music folder"));
                status_for_login.set_visible(true);
                return;
            }
            let message = tr("Caching local library...");
            begin_connect_attempt(&status_for_login, login, &message);
            submit(&source, roots);
        });
        source_enter_controller(&login);

        scroller.upcast()
    }
}

fn setup_scaffold(
    shell: &Rc<Shell>,
    _context: &SetupViewContext,
    registration: &'static SourcePresentation,
) -> (gtk::ScrolledWindow, gtk::Box, gtk::Box, gtk::Label) {
    let compact = true;
    let resource = crate::ui_resource::SETUP_SCAFFOLD_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        scroller: gtk::ScrolledWindow,
        clamp: adw::Clamp,
        content: gtk::Box,
        actions: gtk::Box,
        status: gtk::Label,
        source_summary: gtk::Box, source_icon: gtk::Image, source_title: gtk::Label, change_source: gtk::Button,
    });
    clamp.set_maximum_size(large_popup_content_width(ADD_SERVER_CLAMP_WIDTH));
    clamp.set_margin_top(if compact { 8 } else { 36 });
    clamp.set_margin_bottom(if compact { 20 } else { 36 });
    content.set_spacing(if compact { 10 } else { 18 });
    if compact {
        content.add_css_class("add-server-compact-content");
    }
    source_icon.set_icon_name(Some(registration.icon_name));
    source_title.set_label(&tr(registration.title));
    let weak = Rc::downgrade(shell);
    change_source.connect_clicked(move |_| {
        let Some(shell) = weak.upgrade() else { return };
        let Some(context) = shell.mounted_add_server_context() else {
            return;
        };
        *context.flow.borrow_mut() = Rc::new(SourceChoiceFlow);
        mount_setup_flow(&shell, &context);
    });
    content.append(&source_summary);
    if let SourceOperation::Failed {
        message,
        add_form: true,
        ..
    } = &*shell.source.operation.borrow()
    {
        status.set_text(message);
        status.add_css_class("error-text");
        status.set_visible(true);
    }
    (scroller, content, actions, status)
}

fn source_choice_selector(shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget {
    let resource = crate::ui_resource::SOURCE_CHOICE_SELECTOR_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        wrapper: gtk::ScrolledWindow, device_sources: adw::PreferencesGroup,
        server_sources: adw::PreferencesGroup, import_backup: gtk::Button,
        privacy: adw::PreferencesGroup, external_metadata: adw::SwitchRow,
        external_lyrics: adw::SwitchRow, discord_presence: adw::SwitchRow,
    });
    for &presentation in source_presentations() {
        let resource = crate::ui_resource::SOURCE_CHOICE_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            row: adw::ActionRow, icon: gtk::Image,
        });
        row.set_title(&tr(presentation.title));
        icon.set_icon_name(Some(presentation.icon_name));
        row.update_property(&[gtk::accessible::Property::Label(&tr(presentation.title))]);
        let weak = Rc::downgrade(shell);
        row.connect_activated(move |_| {
            let Some(shell) = weak.upgrade() else { return };
            let Some(context) = shell.mounted_add_server_context() else {
                return;
            };
            *context.flow.borrow_mut() = (presentation.setup_flow)(&shell, presentation);
            mount_setup_flow(&shell, &context);
        });
        if presentation.kind == LOCAL_SOURCE_KIND {
            device_sources.add(&row);
        } else {
            server_sources.add(&row);
        }
    }
    privacy.set_visible(context.onboarding);
    {
        let settings = shell.settings.current.borrow();
        external_metadata.set_active(settings.external_metadata_enabled);
        external_lyrics.set_active(settings.lyrics.external_lyrics_enabled);
        discord_presence.set_active(settings.rich_presence.enabled);
    }
    let weak = Rc::downgrade(shell);
    external_metadata.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_external_metadata_enabled(row.is_active());
        }
    });
    let weak = Rc::downgrade(shell);
    external_lyrics.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_external_lyrics_enabled(row.is_active());
        }
    });
    let weak = Rc::downgrade(shell);
    discord_presence.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_app_setting("Discord presence setting", row.is_active(), |settings| {
                &mut settings.rich_presence.enabled
            });
        }
    });

    import_backup.set_visible(context.onboarding);

    let weak = Rc::downgrade(shell);
    import_backup.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            crate::preferences::backup::import_dialog(&shell);
        }
    });
    wrapper.upcast()
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
    let host = credential_host_view(&snapshot, compact, authentication);
    bind_credential_draft(
        draft,
        &host.name,
        &host.url,
        &host.username,
        &host.password,
        &host.cert_verify,
    );
    host
}

fn credential_host_view(
    snapshot: &CredentialHostDraft,
    compact: bool,
    authentication: Option<Rc<Cell<OpenSubsonicAuthentication>>>,
) -> CredentialHost {
    let resource = crate::ui_resource::CREDENTIAL_HOST_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        section: gtk::Box,
        fields_group: adw::PreferencesGroup,
        rows: adw::PreferencesGroup,
        name: adw::EntryRow,
        url: adw::EntryRow,
        username: adw::EntryRow,
        password: adw::PasswordEntryRow,
        cert_verify: adw::SwitchRow,
        api_key: adw::SwitchRow, legacy_password: adw::SwitchRow,
    });
    name.set_text(&snapshot.name);
    style_compact_field_row(&name);
    url.set_text(&snapshot.url);
    style_compact_field_row(&url);
    let fields = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    fields.set_homogeneous(true);
    fields.set_hexpand(true);
    fields.append(&compact_field_row_group(&name));
    fields.append(&compact_field_row_group(&url));
    let fields = install_compact_field_row_responsiveness_at(&fields, 440);
    if !compact {
        fields_group.set_title(&tr("Server"));
    }
    fields_group.add(&fields);

    username.set_text(&snapshot.username);
    style_compact_field_row(&username);
    password.set_text(&snapshot.password);
    style_compact_field_row(&password);
    cert_verify.set_active(snapshot.cert_verify);
    rows.add(&username);
    rows.add(&password);
    let authentication_toggles = authentication.as_ref().map(|authentication| {
        bind_open_subsonic_authentication(
            &api_key,
            &legacy_password,
            Rc::clone(authentication),
            &username,
            &password,
        );
        rows.add(&api_key);
        rows.add(&legacy_password);
        [api_key, legacy_password]
    });
    rows.add(&cert_verify);

    CredentialHost {
        widget: section,
        fields_group,
        rows,
        name,
        url,
        username,
        password,
        cert_verify,
        authentication,
        authentication_toggles,
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

fn bind_open_subsonic_authentication(
    api_key: &adw::SwitchRow,
    legacy_password: &adw::SwitchRow,
    authentication: Rc<Cell<OpenSubsonicAuthentication>>,
    username: &adw::EntryRow,
    secret: &adw::PasswordEntryRow,
) {
    update_open_subsonic_authentication_fields(authentication.get(), username, secret);
    api_key.set_active(authentication.get() == OpenSubsonicAuthentication::ApiKey);
    legacy_password.set_active(authentication.get() == OpenSubsonicAuthentication::LegacyPassword);
    legacy_password.set_visible(!api_key.is_active());
    for (row, other, mode) in [
        (api_key, legacy_password, OpenSubsonicAuthentication::ApiKey),
        (
            legacy_password,
            api_key,
            OpenSubsonicAuthentication::LegacyPassword,
        ),
    ] {
        let other = other.downgrade();
        let authentication = Rc::clone(&authentication);
        let username = username.downgrade();
        let secret = secret.downgrade();
        row.connect_active_notify(move |row| {
            if mode == OpenSubsonicAuthentication::ApiKey {
                if let Some(legacy) = other.upgrade() {
                    legacy.set_visible(!row.is_active());
                }
            }
            let next = if row.is_active() {
                if let Some(other) = other.upgrade() {
                    other.set_active(false);
                }
                mode
            } else if other.upgrade().is_some_and(|other| other.is_active()) {
                return;
            } else {
                OpenSubsonicAuthentication::Password
            };
            authentication.set(next);
            if let (Some(username), Some(secret)) = (username.upgrade(), secret.upgrade()) {
                update_open_subsonic_authentication_fields(next, &username, &secret);
            }
        });
    }
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
    actions: &gtk::Box,
    status: gtk::Label,
    host: CredentialHost,
    submit: impl Fn(&SourceHandle, CredentialInput) + 'static,
) {
    let login = text_button("rufin-network-server-symbolic", "Connect");
    login.add_css_class("suggested-action");
    login.set_sensitive(host.ready());
    connect_entry_row_activation(&host.name, &login);
    connect_entry_row_activation(&host.url, &login);
    connect_entry_row_activation(&host.username, &login);
    connect_password_entry_row_activation(&host.password, &login);
    let ready: Rc<dyn Fn() -> bool> = {
        let url = host.url.downgrade();
        let username = host.username.downgrade();
        let password = host.password.downgrade();
        let authentication = host.authentication.clone();
        Rc::new(move || {
            let (Some(url), Some(username), Some(password)) =
                (url.upgrade(), username.upgrade(), password.upgrade())
            else {
                return false;
            };
            remote_login_ready(
                &url,
                &username,
                &password,
                authentication.as_ref().is_none_or(|authentication| {
                    authentication.get() != OpenSubsonicAuthentication::ApiKey
                }),
            )
        })
    };
    let update_ready: Rc<dyn Fn()> = {
        let login = login.downgrade();
        let ready = Rc::clone(&ready);
        Rc::new(move || {
            if let Some(login) = login.upgrade() {
                login.set_sensitive(ready());
            }
        })
    };
    {
        let update_ready = Rc::clone(&update_ready);
        host.url.connect_text_notify(move |_| update_ready());
    }
    {
        let update_ready = Rc::clone(&update_ready);
        host.username.connect_text_notify(move |_| update_ready());
    }
    {
        let update_ready = Rc::clone(&update_ready);
        host.password.connect_text_notify(move |_| update_ready());
    }
    if let Some(toggles) = host.authentication_toggles.as_ref() {
        for toggle in toggles {
            let update_ready = Rc::clone(&update_ready);
            toggle.connect_active_notify(move |_| update_ready());
        }
    }
    *context.actions.borrow_mut() = Some(SetupActions {
        status: status.clone(),
        connect: login.clone(),
        ready,
    });

    let source = shell.products.source.clone();
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
        let message = tr("Connecting to music server...");
        begin_connect_attempt(&status_for_login, login, &message);
        submit(&source, host_for_click.input());
    });
    actions.append(&login);
    content.append(actions);
    content.append(&status);
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
