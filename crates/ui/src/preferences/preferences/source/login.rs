mod files;
pub use files::{bind_integrations, choose_integration};
mod jellyfin_emby;
mod plex;

use rufin_core::source::authorization::{
    SourceAuthorization, SourceAuthorizationEvent, SourceAuthorizationInput,
    SourceAuthorizationMethod, SourceChoice,
};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use rufin_core::runtime::source::{
    DiscoveryStatus, EditableSource, FoundServer, SourceHandle, SourceOperation, music_folder,
};

use sources::{
    CredentialInput, CredentialPreset, SourceSetup, SubsonicAuthentication, SubsonicFlavor,
};

use super::folder_selected_text;
use crate::shell::shell::Shell;
use crate::widgets::controls::text_button;
use crate::widgets::layout::large_popup_content_width;
use crate::widgets::popup::present_light_dismiss_dialog;
use localization::{tr, tr_with};
use rufin_core::runtime::source::source_operation_text;

const ADD_SERVER_CLAMP_WIDTH: i32 = 560;
const SETUP_FORM_PAGE: &str = "form";
const SETUP_PROGRESS_PAGE: &str = "progress";
const SETUP_LOADING_PAGE: &str = "loading";
const JELLYFIN_SOURCE_KIND: &str = "jellyfin";
const LOCAL_SOURCE_KIND: &str = "local";

type SetupFlowFactory = fn(&Rc<Shell>, &'static SourcePresentation) -> Rc<dyn SourceSetupFlow>;
type SettingsGroupFactory =
    fn(&Rc<Shell>, &EditableSource, &'static SourcePresentation) -> Result<gtk::Widget, String>;

#[derive(Clone, Copy)]
struct SourcePresentation {
    kind: &'static str,
    setup_flow: SetupFlowFactory,
    settings_group: Option<SettingsGroupFactory>,
}

static JELLYFIN: SourcePresentation = SourcePresentation {
    kind: JELLYFIN_SOURCE_KIND,
    setup_flow: jellyfin_emby::setup_flow,
    settings_group: Some(jellyfin_emby::settings_group),
};
static EMBY: SourcePresentation = SourcePresentation {
    kind: "emby",
    setup_flow: jellyfin_emby::setup_flow,
    settings_group: Some(jellyfin_emby::settings_group),
};
static NAVIDROME: SourcePresentation = SourcePresentation {
    kind: "navidrome",
    setup_flow: navidrome_setup_flow,
    settings_group: Some(navidrome_settings_group),
};
static SUBSONIC: SourcePresentation = SourcePresentation {
    kind: "subsonic",
    setup_flow: subsonic_setup_flow,
    settings_group: Some(subsonic_settings_group),
};
static LOCAL: SourcePresentation = SourcePresentation {
    kind: LOCAL_SOURCE_KIND,
    setup_flow: local_setup_flow,
    settings_group: None,
};
static WEBDAV: SourcePresentation = SourcePresentation {
    kind: "webdav",
    setup_flow: files::setup_flow,
    settings_group: Some(files::settings_group),
};
static SMB: SourcePresentation = SourcePresentation {
    kind: "smb",
    setup_flow: files::setup_flow,
    settings_group: Some(files::settings_group),
};
static PLEX: SourcePresentation = SourcePresentation {
    kind: "plex",
    setup_flow: plex::setup_flow,
    settings_group: Some(plex::settings_group),
};
static SOURCE_PRESENTATIONS: [&SourcePresentation; 8] = [
    &LOCAL, &JELLYFIN, &EMBY, &PLEX, &NAVIDROME, &SUBSONIC, &WEBDAV, &SMB,
];

fn source_presentations() -> &'static [&'static SourcePresentation] {
    &SOURCE_PRESENTATIONS
}

fn source_presentation(kind: &str) -> Option<&'static SourcePresentation> {
    source_presentations()
        .iter()
        .copied()
        .find(|presentation| presentation.kind == kind)
}

pub fn source_settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
) -> Option<Result<gtk::Widget, String>> {
    if saved.source.kind == "peer" {
        return Some(Ok(peer_settings_group(shell, saved)));
    }
    let presentation = source_presentation(&saved.source.kind)?;
    presentation
        .settings_group
        .map(|factory| factory(shell, saved, presentation))
}

pub trait SourceSetupFlow {
    fn view(&self, shell: &Rc<Shell>, context: &SetupViewContext) -> gtk::Widget;
    /// Starts the form from a server found on the network.
    fn prefill(&self, _server: &FoundServer) {}
}

pub struct SourceSetupViewHandle {
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
pub struct SetupViewContext {
    onboarding: bool,
    content: gtk::Box,
    surface: gtk::Stack,
    form: gtk::Box,
    progress_status: gtk::Label,
    flow: Rc<RefCell<Rc<dyn SourceSetupFlow>>>,
    actions: Rc<RefCell<Option<SetupActions>>>,
    discovery: Rc<RefCell<Option<DiscoveredServersView>>>,
    found: Rc<RefCell<Option<FoundView>>>,
    /// Whether the dialog is too narrow for four tiles side by side.
    narrow: Rc<Cell<bool>>,
    loading: Option<LoadingPage>,
    retry_focus: Rc<RefCell<Option<gtk::Widget>>>,
    preferences_dialog: Option<adw::Dialog>,
}

/// The source list's Found group: the music folder, servers and Rufin devices on the network.
struct FoundView {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    kinds: gtk::FlowBox,
    searching: gtk::Spinner,
    status: gtk::Label,
    error: gtk::Label,
    actions: Rc<RefCell<Option<SetupActions>>>,
    dialog: Option<adw::Dialog>,
    onboarding: bool,
}

/// The first run's last page: the sync's progress over the lookup switches.
#[derive(Clone)]
struct LoadingPage {
    title: gtk::Label,
    bar: gtk::ProgressBar,
    status: gtk::Label,
    ready: gtk::Image,
}

impl LoadingPage {
    /// Shows the sync's progress, or that the library is ready once nothing loads it.
    fn show(&self, operation: &SourceOperation) {
        let progress = match operation {
            SourceOperation::Adding { progress }
            | SourceOperation::Switching { progress, .. }
            | SourceOperation::Refreshing { progress, .. } => Some(progress),
            _ => None,
        };
        self.title.set_label(&if progress.is_some() {
            tr("While your library loads")
        } else {
            tr("Your library is ready")
        });
        self.bar.set_visible(progress.is_some());
        self.status.set_visible(progress.is_some());
        self.ready.set_visible(progress.is_none());
        let Some(progress) = progress else { return };
        self.status
            .set_text(&source_operation_text(operation).unwrap_or_default());
        match progress.total {
            Some(total) if total > 0 => self
                .bar
                .set_fraction(progress.completed as f64 / total as f64),
            _ => self.bar.pulse(),
        }
    }
}

struct SetupActions {
    status: gtk::Label,
    connect: gtk::Button,
    ready: Rc<dyn Fn() -> bool>,
}

struct DiscoveredServersView {
    group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<gtk::Widget>>>,
    kind: &'static str,
    select: Rc<dyn Fn(sources::DiscoveredServer)>,
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
    local_url: String,
    username: String,
    password: String,
    cert_verify: bool,
}

struct CredentialSetupFlow {
    presentation: &'static SourcePresentation,
    draft: Rc<RefCell<CredentialHostDraft>>,
    authentication: Rc<Cell<SubsonicAuthentication>>,
    offer_authentication: bool,
    submit: Rc<dyn Fn(&SourceHandle, CredentialInput, SubsonicAuthentication)>,
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
    local_url: adw::EntryRow,
    username: adw::EntryRow,
    password: adw::PasswordEntryRow,
    cert_verify: adw::SwitchRow,
    authentication: Option<Rc<Cell<SubsonicAuthentication>>>,
    authentication_toggles: Option<[adw::SwitchRow; 2]>,
    save: gtk::Button,
}

impl CredentialHost {
    fn input(&self) -> CredentialInput {
        CredentialInput {
            source_name: trimmed_optional_text(&self.name),
            server_url: self.url.text().to_string(),
            local_server_url: trimmed_optional_text(&self.local_url),
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
            self.authentication.as_ref().is_some_and(|authentication| {
                authentication.get() == SubsonicAuthentication::ApiKey
            }),
        )
    }
}

impl Shell {
    pub fn present_onboarding(self: &Rc<Self>) {
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
        let resource = "preferences/source/onboarding.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
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
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            520.0,
            adw::LengthUnit::Sp,
        ));
        for narrow in [true, false] {
            let weak = Rc::downgrade(self);
            let changed = move |_: &adw::Breakpoint| {
                let Some(context) = weak
                    .upgrade()
                    .and_then(|shell| shell.mounted_add_server_context())
                else {
                    return;
                };
                context.narrow.set(narrow);
                if let Some(found) = context.found.borrow().as_ref() {
                    lay_out_source_tiles(&found.kinds, narrow);
                }
            };
            if narrow {
                breakpoint.connect_apply(changed);
            } else {
                breakpoint.connect_unapply(changed);
            }
        }
        dialog.add_breakpoint(breakpoint);
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

    pub fn add_server_navigation_page(
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

    pub fn update_add_server_dialog(self: &Rc<Self>) {
        let Some(context) = self.mounted_add_server_context() else {
            return;
        };
        update_setup_surface(self, &context);
    }

    pub fn update_add_server_discovery(self: &Rc<Self>) {
        let Some(context) = self.mounted_add_server_context() else {
            return;
        };
        if let Some(discovery) = context.discovery.borrow().as_ref() {
            refresh_discovered_servers_view(self, discovery);
        }
    }

    pub fn complete_add_server_dialog(&self) {
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

    /// Whether the first run's dialog is open. It stays on its last page until the user
    /// leaves it, where every other add closes once the source is in.
    pub fn shows_first_run(&self) -> bool {
        self.mounted_add_server_context()
            .is_some_and(|context| context.loading.is_some())
    }

    pub fn update_found_sources(self: &Rc<Self>) {
        let Some(context) = self.mounted_add_server_context() else {
            return;
        };
        if let Some(found) = context.found.borrow().as_ref() {
            render_found(self, found);
        }
    }

    /// Searches the network for servers and, on the first run, for Rufin devices to join.
    fn find_servers(self: &Rc<Self>, devices: bool) {
        if self.source.finding_servers.replace(true) {
            return;
        }
        if devices {
            // Nearby devices show once Connect's network runs; no profile is needed for that.
            let connect = self.products.connect.clone();
            self.products.runtime.spawn(async move {
                let _ = connect.execute(rufin_core::connect::Action::Discover).await;
            });
        }
        let source = self.products.source.clone();
        let weak = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let servers = source.find_servers().await;
            let Some(shell) = weak.upgrade() else { return };
            *shell.source.found_servers.borrow_mut() = Some(servers);
            shell.source.finding_servers.set(false);
            shell.update_found_sources();
        });
        self.update_found_sources();
    }

    pub fn clear_retained_add_server_form(&self) {
        self.source.add_server.borrow_mut().take();
    }

    fn mounted_add_server_context(&self) -> Option<SetupViewContext> {
        let context = self.source.add_server.borrow().as_ref()?.context.clone()?;
        context.is_mounted().then_some(context)
    }

    pub fn release_inactive_add_server_form(&self) {
        let mut handle = self.source.add_server.borrow_mut();
        if !matches!(
            *self.source.operation.borrow(),
            SourceOperation::Adding { .. }
        ) {
            handle.take();
            return;
        }
        let Some(handle) = handle.as_mut() else {
            return;
        };
        handle.context.take();
    }

    pub fn has_retained_add_server_form(&self) -> bool {
        self.source.add_server.borrow().is_some()
    }

    pub fn restore_add_server_dialog_after_failure(self: &Rc<Self>) -> bool {
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
                    crate::preferences::preferences::present_add_server_preferences_dialog(self);
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
        let resource = "preferences/source/connection_progress.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            content: gtk::Box,
            surface: gtk::Stack,
            form: gtk::Box,
            status: gtk::Label,
        });
        status.set_label(&self.source_connection_progress_text());
        let flow = flow.unwrap_or_else(|| Rc::new(RefCell::new(self.default_source_setup_flow())));
        let loading = onboarding.then(|| {
            let (page, loading) = loading_page(self, preferences_dialog.as_ref());
            surface.add_named(&page, Some(SETUP_LOADING_PAGE));
            loading
        });
        SetupViewContext {
            onboarding,
            content,
            surface,
            form,
            progress_status: status,
            flow,
            actions: Rc::new(RefCell::new(None)),
            discovery: Rc::new(RefCell::new(None)),
            found: Rc::new(RefCell::new(None)),
            narrow: Rc::new(Cell::new(false)),
            loading,
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
        self.products
            .source
            .discover_servers(self.source.discovery_provider.get());
    }

    fn select_discovery_provider(&self, provider: sources::DiscoveryProvider) {
        if self.source.discovery_provider.replace(provider) != provider {
            self.source.discovery_started.set(false);
            self.source.discovery_running.set(false);
            self.source.discovered_servers.borrow_mut().clear();
            *self.source.discovery_status.borrow_mut() = DiscoveryStatus::Idle;
        }
    }

    fn refresh_server_discovery(self: &Rc<Self>) {
        if self.source.discovery_running.get() {
            return;
        }
        self.source.discovery_running.set(true);
        *self.source.discovered_servers.borrow_mut() = Vec::new();
        *self.source.discovery_status.borrow_mut() = DiscoveryStatus::Searching;
        self.products
            .source
            .discover_servers(self.source.discovery_provider.get());
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

fn subsonic_setup_flow_for(
    _shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
    kind: SubsonicFlavor,
) -> Rc<dyn SourceSetupFlow> {
    let preset = None;
    let authentication = SubsonicAuthentication::Password;
    let offer_authentication = kind == SubsonicFlavor::Subsonic;
    Rc::new(CredentialSetupFlow {
        presentation,
        draft: Rc::new(RefCell::new(credential_draft(preset))),
        authentication: Rc::new(Cell::new(authentication)),
        offer_authentication,
        submit: Rc::new(move |source, input, authentication| {
            let source = source.clone();
            gtk::glib::spawn_future_local(async move {
                let _ = source
                    .save_subsonic(None, kind, authentication, input)
                    .await;
            });
        }),
    })
}

fn navidrome_setup_flow(
    shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    subsonic_setup_flow_for(shell, presentation, SubsonicFlavor::Navidrome)
}

fn subsonic_setup_flow(
    shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    subsonic_setup_flow_for(shell, presentation, SubsonicFlavor::Subsonic)
}

fn subsonic_settings_group_for(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
    kind: SubsonicFlavor,
) -> Result<gtk::Widget, String> {
    let source_id = saved.source.id.clone();
    let authentication = (kind == SubsonicFlavor::Subsonic).then_some(
        saved
            .credentials
            .open_subsonic_authentication
            .unwrap_or(SubsonicAuthentication::Password),
    );
    let snapshot = credential_draft(Some(saved.credentials.clone()));
    let authentication = authentication.map(|value| Rc::new(Cell::new(value)));
    let host = credential_host_view(&snapshot, true, authentication);
    host.fields_group.set_title(&tr("Server Settings"));
    host.fields_group
        .set_description(Some(&tr(rufin_core::runtime::source::source_kind_title(
            presentation.kind,
        )
        .expect("registered source presentation"))));
    host.cert_verify
        .set_subtitle(&tr("Off only for a server you control"));

    let save = &host.save;
    save.set_visible(true);

    let source = shell.products.source.clone();
    let name = host.name.clone();
    let url = host.url.clone();
    let local_url = host.local_url.clone();
    let username = host.username.clone();
    let password = host.password.clone();
    let cert_verify = host.cert_verify.clone();
    let authentication = host.authentication.clone();
    let weak_shell = Rc::downgrade(shell);
    save.connect_clicked(move |button| {
        let authentication = authentication
            .as_ref()
            .map(|authentication| authentication.get());
        let source = source.clone();
        let source_id = source_id.clone();
        let credentials = CredentialInput {
            source_name: Some(name.text().trim().to_string()),
            server_url: url.text().trim().to_string(),
            local_server_url: trimmed_optional_text(&local_url),
            username: username.text().trim().to_string(),
            secret: password.text().to_string(),
            trust_invalid_cert: !cert_verify.is_active(),
        };
        if let Some(shell) = weak_shell.upgrade() {
            complete_source_save(&shell, button, async move {
                source
                    .save_subsonic(
                        Some(source_id),
                        kind,
                        authentication.unwrap_or(SubsonicAuthentication::Password),
                        credentials,
                    )
                    .await
                    .map_err(|error| error.to_string())
            });
        }
    });

    Ok(host.widget.upcast())
}

fn navidrome_settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    subsonic_settings_group_for(shell, saved, presentation, SubsonicFlavor::Navidrome)
}

fn subsonic_settings_group(
    shell: &Rc<Shell>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    subsonic_settings_group_for(shell, saved, presentation, SubsonicFlavor::Subsonic)
}

/// Another device's Local source has only its name, kept on this device.
fn peer_settings_group(shell: &Rc<Shell>, saved: &EditableSource) -> gtk::Widget {
    let resource = "preferences/source/credential_host.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        section: gtk::Box,
        fields_group: adw::PreferencesGroup,
        rows: adw::PreferencesGroup,
        name: adw::EntryRow,
        save: gtk::Button,
    });
    rows.set_visible(false);
    name.set_text(&saved.source.name);
    fields_group.add(&name);
    save.set_visible(true);
    connect_entry_row_activation(&name, &save);
    let source = shell.products.source.clone();
    let source_id = saved.source.id.clone();
    let weak_shell = Rc::downgrade(shell);
    save.connect_clicked(move |button| {
        let source = source.clone();
        let source_id = source_id.clone();
        let name = name.text().trim().to_string();
        if let Some(shell) = weak_shell.upgrade() {
            complete_source_save(&shell, button, async move {
                source
                    .save_peer(source_id, name)
                    .await
                    .map_err(|error| error.to_string())
            });
        }
    });
    section.upcast()
}

fn complete_source_save(
    shell: &Rc<Shell>,
    button: &gtk::Button,
    result: impl std::future::Future<Output = Result<(), String>> + 'static,
) {
    button.set_sensitive(false);
    let button = button.downgrade();
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let result = result.await;
        match result {
            Ok(()) => {
                if let Some(button) = button.upgrade() {
                    button.set_label(&tr("Saved..."));
                }
                gtk::glib::timeout_future(std::time::Duration::from_secs(2)).await;
            }
            Err(error) => {
                if let Some(shell) = shell.upgrade() {
                    shell.control_feedback.show_feedback_toast(error);
                }
            }
        }
        if let Some(button) = button.upgrade() {
            button.set_label(&tr("Save"));
            button.set_sensitive(true);
        }
    });
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
                return crate::preferences::preferences::library::local_sources_page(shell, dialog)
                    .upcast();
            }
        }
        let (scroller, content, actions, status) =
            setup_scaffold(shell, context, self.presentation);
        let resource = "preferences/source/local_setup.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
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
    let resource = "preferences/source/setup_scaffold.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
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
    source_icon.set_icon_name(Some(
        rufin_core::runtime::source::source_kind_icon_name(registration.kind)
            .expect("registered source presentation"),
    ));
    source_title.set_label(&tr(rufin_core::runtime::source::source_kind_title(
        registration.kind,
    )
    .expect("registered source presentation")));
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
    let resource = "preferences/source/choice_selector.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        wrapper: gtk::ScrolledWindow, welcome: gtk::Box, app_icon: gtk::Image, title: gtk::Label,
        found: adw::PreferencesGroup, searching: gtk::Spinner, found_status: gtk::Label,
        status: gtk::Label, kinds: gtk::FlowBox, import_backup: gtk::Button,
    });
    welcome.set_visible(context.onboarding);
    title.set_visible(!context.onboarding);
    // The bundled icon carries the stable name in every build.
    app_icon.set_icon_name(Some(app_identity::STABLE_APP_ID));
    for &presentation in source_presentations() {
        let resource = "preferences/source/choice.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            tile: gtk::FlowBoxChild, icon: gtk::Image, label: gtk::Label,
        });
        label.set_label(&tr(rufin_core::runtime::source::source_kind_title(
            presentation.kind,
        )
        .expect("registered source presentation")));
        icon.set_icon_name(Some(
            rufin_core::runtime::source::source_kind_icon_name(presentation.kind)
                .expect("registered source presentation"),
        ));
        kinds.append(&tile);
    }
    let weak = Rc::downgrade(shell);
    kinds.connect_child_activated(move |_, tile| {
        let Some(shell) = weak.upgrade() else { return };
        let presentation = source_presentations()[tile.index() as usize];
        open_setup_flow(&shell, presentation, None);
    });

    let view = FoundView {
        group: found,
        rows: RefCell::new(Vec::new()),
        kinds: kinds.clone(),
        searching,
        status: found_status,
        error: status,
        actions: Rc::clone(&context.actions),
        dialog: context.preferences_dialog.clone(),
        onboarding: context.onboarding,
    };
    render_found(shell, &view);
    lay_out_source_tiles(&view.kinds, context.narrow.get());
    *context.found.borrow_mut() = Some(view);
    if shell.source.found_servers.borrow().is_none() {
        shell.find_servers(context.onboarding);
    }
    // Rufin devices come and go on Connect's own watch.
    let mut devices = shell.products.connect.subscribe();
    let weak = Rc::downgrade(shell);
    let watch = gtk::glib::spawn_future_local(async move {
        while devices.changed().await.is_ok() {
            let Some(shell) = weak.upgrade() else { return };
            shell.update_found_sources();
        }
    });
    wrapper.connect_destroy(move |_| watch.abort());

    import_backup.set_visible(context.onboarding);
    let weak = Rc::downgrade(shell);
    import_backup.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            crate::preferences::preferences::backup::import_dialog(&shell);
        }
    });
    wrapper.upcast()
}

/// Four tiles with the name under the icon, or two columns with the name beside it.
fn lay_out_source_tiles(kinds: &gtk::FlowBox, narrow: bool) {
    kinds.set_max_children_per_line(if narrow { 2 } else { 4 });
    let mut tile = kinds.first_child();
    while let Some(child) = tile {
        if let Some(content) = child
            .downcast_ref::<gtk::FlowBoxChild>()
            .and_then(|tile| tile.child())
            .and_downcast::<gtk::Box>()
        {
            content.set_orientation(if narrow {
                gtk::Orientation::Horizontal
            } else {
                gtk::Orientation::Vertical
            });
        }
        tile = child.next_sibling();
    }
}

fn open_setup_flow(
    shell: &Rc<Shell>,
    presentation: &'static SourcePresentation,
    server: Option<&FoundServer>,
) {
    let Some(context) = shell.mounted_add_server_context() else {
        return;
    };
    let flow = (presentation.setup_flow)(shell, presentation);
    if let Some(server) = server {
        flow.prefill(server);
    }
    *context.flow.borrow_mut() = flow;
    mount_setup_flow(shell, &context);
}

fn found_row(title: &str, subtitle: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .use_markup(false)
        .build();
    let icon = gtk::Image::from_icon_name(icon);
    icon.set_pixel_size(32);
    row.add_prefix(&icon);
    row
}

fn render_found(shell: &Rc<Shell>, view: &FoundView) {
    for row in view.rows.borrow_mut().drain(..) {
        view.group.remove(&row);
    }
    let mut rows = Vec::new();
    if let Some((folder, readable)) = music_folder() {
        let row = found_row(
            &tr("Music folder"),
            &path_subtitle(&folder),
            "rufin-folder-music-symbolic",
        );
        // A sandbox knows this path without having the folder; the system's dialog grants it.
        let add = gtk::Button::builder()
            .label(if readable { tr("Add") } else { tr("Choose...") })
            .valign(gtk::Align::Center)
            .css_classes(["suggested-action"])
            .build();
        row.add_suffix(&add);
        row.set_activatable_widget(Some(&add));
        let weak = Rc::downgrade(shell);
        add.connect_clicked(move |add| {
            let Some(shell) = weak.upgrade() else { return };
            let (add, folder) = (add.clone(), folder.clone());
            gtk::glib::spawn_future_local(async move {
                let folder = if readable {
                    folder
                } else {
                    match choose_music_folder(&shell.chrome.window, Some(folder)).await {
                        Some(folder) => folder,
                        None => return,
                    }
                };
                add.set_sensitive(false);
                shell.products.source.configure_source(SourceSetup::Local {
                    roots: vec![folder],
                });
            });
        });
        *view.actions.borrow_mut() = Some(SetupActions {
            status: view.error.clone(),
            connect: add,
            ready: Rc::new(|| true),
        });
        rows.push(row.upcast::<gtk::Widget>());
    }
    let servers = shell.source.found_servers.borrow().clone();
    for server in servers.iter().flatten() {
        let Some(presentation) = source_presentation(&server.kind) else {
            continue;
        };
        let kind = tr(
            rufin_core::runtime::source::source_kind_title(presentation.kind)
                .expect("registered source presentation"),
        );
        let row = found_row(
            &server.name,
            &format!("{kind}, {}", server.address),
            rufin_core::runtime::source::source_kind_icon_name(presentation.kind)
                .expect("registered source presentation"),
        );
        row.add_suffix(&gtk::Image::from_icon_name("rufin-go-next-symbolic"));
        row.set_activatable(true);
        let weak = Rc::downgrade(shell);
        let server = server.clone();
        row.connect_activated(move |_| {
            if let Some(shell) = weak.upgrade() {
                open_setup_flow(&shell, presentation, Some(&server));
            }
        });
        rows.push(row.upcast());
    }
    let connect = shell.products.connect.status();
    for device in connect
        .devices
        .iter()
        .filter(|device| view.onboarding && !device.enrolled)
    {
        let row = found_row(
            &device.name,
            &tr("Join to copy its sources and sign-ins"),
            "rufin-phonelink-symbolic",
        );
        row.add_suffix(&gtk::Image::from_icon_name("rufin-go-next-symbolic"));
        row.set_activatable(true);
        let weak = Rc::downgrade(shell);
        let invitation = device.id.clone();
        let dialog = view.dialog.as_ref().map(|dialog| dialog.downgrade());
        row.connect_activated(move |_| {
            let (Some(shell), Some(dialog)) = (
                weak.upgrade(),
                dialog.as_ref().and_then(|dialog| dialog.upgrade()),
            ) else {
                return;
            };
            let invitation = invitation.clone();
            gtk::glib::spawn_future_local(async move {
                let joined = crate::preferences::preferences::connect::join_after_confirmation(
                    &shell, &dialog, invitation,
                )
                .await;
                // Connect's own page shows the pairing from here.
                if joined {
                    dialog.close();
                }
            });
        });
        rows.push(row.upcast());
    }
    let finding = shell.source.finding_servers.get();
    let search = adw::ButtonRow::builder()
        .title(if finding {
            tr("Searching...")
        } else {
            tr("Search Again")
        })
        .start_icon_name("rufin-view-refresh-symbolic")
        .sensitive(!finding)
        .build();
    let weak = Rc::downgrade(shell);
    let devices = view.onboarding;
    search.connect_activated(move |_| {
        if let Some(shell) = weak.upgrade() {
            shell.find_servers(devices);
        }
    });
    rows.push(search.upcast());
    for row in &rows {
        view.group.add(row);
    }
    *view.rows.borrow_mut() = rows;
    view.searching.set_visible(finding);
    view.searching.set_spinning(finding);
    let none_found = servers.as_ref().is_some_and(Vec::is_empty);
    view.status.set_visible(finding || none_found);
    view.status.set_label(&if finding {
        tr("Searching Local Network")
    } else {
        tr("No Servers Found")
    });
}

fn loading_page(shell: &Rc<Shell>, dialog: Option<&adw::Dialog>) -> (gtk::Widget, LoadingPage) {
    let resource = "preferences/source/onboarding_progress.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        page: gtk::ScrolledWindow, title: gtk::Label, bar: gtk::ProgressBar, status: gtk::Label,
        ready: gtk::Image, external_metadata: adw::SwitchRow, external_lyrics: adw::SwitchRow,
        discord_presence: adw::SwitchRow, private_mode: adw::SwitchRow, start: gtk::Button,
    });
    {
        let settings = shell.settings.current.borrow();
        external_metadata.set_active(settings.application.external_metadata_enabled);
        external_lyrics.set_active(settings.lyrics.preferences.external_lyrics_enabled);
        discord_presence.set_active(settings.rich_presence.enabled);
        private_mode.set_active(settings.application.private_mode);
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
            shell.player_ui.set_external_lyrics_enabled(row.is_active());
        }
    });
    let weak = Rc::downgrade(shell);
    discord_presence.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.settings.set_app_setting(
                "Discord presence setting",
                row.is_active(),
                |settings| &mut settings.rich_presence.enabled,
            );
            crate::preferences::preferences::show_discord_flatpak_help(row);
        }
    });
    let weak = Rc::downgrade(shell);
    private_mode.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_private_mode(row.is_active());
        }
    });
    let dialog = dialog.map(|dialog| dialog.downgrade());
    start.connect_clicked(move |_| {
        if let Some(dialog) = dialog.as_ref().and_then(|dialog| dialog.upgrade()) {
            dialog.close();
        }
    });
    (
        page.upcast(),
        LoadingPage {
            title,
            bar,
            status,
            ready,
        },
    )
}

fn mount_setup_flow(shell: &Rc<Shell>, context: &SetupViewContext) {
    context.actions.borrow_mut().take();
    context.discovery.borrow_mut().take();
    context.found.borrow_mut().take();
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
            if let Some(loading) = &context.loading {
                loading.show(&operation);
                context.surface.set_visible_child_name(SETUP_LOADING_PAGE);
            } else {
                context.surface.set_visible_child_name(SETUP_PROGRESS_PAGE);
            }
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
        // The first source is in: the last page stays, now without progress.
        SetupSurfaceState::Form
            if context.loading.is_some()
                && !shell.source.configured.borrow().sources.is_empty() =>
        {
            if let Some(loading) = &context.loading {
                loading.show(&operation);
            }
            context.surface.set_visible_child_name(SETUP_LOADING_PAGE);
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
        | SourceOperation::Downloading { .. }
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
            local_url: String::new(),
            username: String::new(),
            password: String::new(),
            cert_verify: true,
        },
        |preset| CredentialHostDraft {
            name: preset.source_name,
            url: preset.server_url,
            local_url: preset.local_server_url.unwrap_or_default(),
            username: preset.username,
            password: String::new(),
            cert_verify: !preset.trust_invalid_cert,
        },
    )
}

fn credential_host(
    draft: &Rc<RefCell<CredentialHostDraft>>,
    compact: bool,
    authentication: Option<Rc<Cell<SubsonicAuthentication>>>,
) -> CredentialHost {
    let snapshot = draft.borrow().clone();
    let host = credential_host_view(&snapshot, compact, authentication);
    bind_credential_draft(
        draft,
        &host.name,
        &host.url,
        &host.local_url,
        &host.username,
        &host.password,
        &host.cert_verify,
    );
    host
}

fn credential_host_view(
    snapshot: &CredentialHostDraft,
    compact: bool,
    authentication: Option<Rc<Cell<SubsonicAuthentication>>>,
) -> CredentialHost {
    let resource = "preferences/source/credential_host.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        section: gtk::Box,
        fields_group: adw::PreferencesGroup,
        rows: adw::PreferencesGroup,
        name: adw::EntryRow,
        url: adw::EntryRow,
        local_url: adw::EntryRow,
        username: adw::EntryRow,
        password: adw::PasswordEntryRow,
        cert_verify: adw::SwitchRow,
        api_key: adw::SwitchRow, legacy_password: adw::SwitchRow,
        save: gtk::Button,
    });
    crate::widgets::forms::attach_address_suggestions(&url);
    name.set_text(&snapshot.name);
    style_compact_field_row(&name);
    url.set_text(&snapshot.url);
    style_compact_field_row(&url);
    crate::widgets::forms::attach_address_suggestions(&local_url);
    local_url.set_text(&snapshot.local_url);
    style_compact_field_row(&local_url);
    if !compact {
        fields_group.set_title(&tr("Server"));
    }
    fields_group.add(&name);
    fields_group.add(&url);
    fields_group.add(&local_url);

    set_required(&url, &tr("Server Address"), true);
    set_required(&username, &tr("Username"), true);
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
        local_url,
        username,
        password,
        cert_verify,
        authentication,
        authentication_toggles,
        save,
    }
}

pub(super) fn update_open_subsonic_authentication_fields(
    authentication: SubsonicAuthentication,
    username: &adw::EntryRow,
    secret: &adw::PasswordEntryRow,
) {
    let api_key = authentication == SubsonicAuthentication::ApiKey;
    username.set_visible(!api_key);
    if api_key {
        set_required(secret, &tr("API key"), true);
    } else {
        secret.set_title(&tr("Password"));
    }
}

/// Titles a field, with an asterisk when the form cannot be sent without it.
pub(super) fn set_required(row: &impl IsA<adw::PreferencesRow>, title: &str, required: bool) {
    row.set_title(&if required {
        format!("{title} *")
    } else {
        title.to_owned()
    });
}

fn bind_open_subsonic_authentication(
    api_key: &adw::SwitchRow,
    legacy_password: &adw::SwitchRow,
    authentication: Rc<Cell<SubsonicAuthentication>>,
    username: &adw::EntryRow,
    secret: &adw::PasswordEntryRow,
) {
    update_open_subsonic_authentication_fields(authentication.get(), username, secret);
    api_key.set_active(authentication.get() == SubsonicAuthentication::ApiKey);
    legacy_password.set_active(authentication.get() == SubsonicAuthentication::LegacyPassword);
    legacy_password.set_visible(!api_key.is_active());
    for (row, other, mode) in [
        (api_key, legacy_password, SubsonicAuthentication::ApiKey),
        (
            legacy_password,
            api_key,
            SubsonicAuthentication::LegacyPassword,
        ),
    ] {
        let other = other.downgrade();
        let authentication = Rc::clone(&authentication);
        let username = username.downgrade();
        let secret = secret.downgrade();
        row.connect_active_notify(move |row| {
            if mode == SubsonicAuthentication::ApiKey {
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
                SubsonicAuthentication::Password
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
    local_url: &adw::EntryRow,
    username: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
    cert_verify: &adw::SwitchRow,
) {
    let value = Rc::clone(draft);
    name.connect_text_notify(move |row| value.borrow_mut().name = row.text().to_string());
    let value = Rc::clone(draft);
    url.connect_text_notify(move |row| value.borrow_mut().url = row.text().to_string());
    let value = Rc::clone(draft);
    local_url.connect_text_notify(move |row| value.borrow_mut().local_url = row.text().to_string());
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
                authentication.as_ref().is_some_and(|authentication| {
                    authentication.get() == SubsonicAuthentication::ApiKey
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
                    authentication.get() == SubsonicAuthentication::ApiKey
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

fn discovered_servers_view(host: &CredentialHost, kind: &'static str) -> DiscoveredServersView {
    let group = adw::PreferencesGroup::builder()
        .title(tr("Found Servers"))
        .build();
    DiscoveredServersView {
        group,
        rows: Rc::new(RefCell::new(Vec::new())),
        kind,
        select: {
            let name = host.name.downgrade();
            let url = host.url.downgrade();
            Rc::new(move |server| {
                if let Some(name) = name.upgrade() {
                    name.set_text(&server.name);
                }
                if let Some(url) = url.upgrade() {
                    url.set_text(&server.address);
                }
            })
        },
    }
}

fn refresh_discovered_servers_view(shell: &Rc<Shell>, view: &DiscoveredServersView) {
    let status = shell.source.discovery_status.borrow().clone();
    let running = shell.source.discovery_running.get();
    let servers = shell.source.discovered_servers.borrow().clone();
    view.group
        .set_description(Some(&discovery_status_label(&status, view.kind)));
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
                rufin_core::runtime::source::source_kind_icon_name(view.kind)
                    .unwrap_or("rufin-network-server-symbolic"),
            ));
            row.set_activatable(true);
            let select = Rc::clone(&view.select);
            row.connect_activated(move |_| {
                select(server.clone());
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

fn discovery_status_label(status: &DiscoveryStatus, kind: &str) -> String {
    match status {
        DiscoveryStatus::Idle => tr("Searching will start automatically"),
        DiscoveryStatus::Searching => tr_with(
            "Searching for {provider} servers on the local network...",
            &[(
                "provider",
                &tr(rufin_core::runtime::source::source_kind_title(kind).unwrap_or(kind)),
            )],
        ),
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
    api_key: bool,
) -> bool {
    let address = url.text();
    let address = address.trim().trim_end_matches('/');
    let address_without_scheme = address
        .strip_prefix("http://")
        .or_else(|| address.strip_prefix("https://"))
        .unwrap_or(address);
    !address_without_scheme.trim().is_empty()
        && if api_key {
            !password.text().trim().is_empty()
        } else {
            !username.text().trim().is_empty()
        }
}

fn path_subtitle(path: &Path) -> String {
    desktop_integration::display_path(path)
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

async fn choose_music_folder(
    window: &gtk::ApplicationWindow,
    initial: Option<PathBuf>,
) -> Option<PathBuf> {
    let dialog = gtk::FileDialog::builder()
        .title(tr("Select Music Folder"))
        .build();
    if let Some(folder) = initial {
        dialog.set_initial_folder(Some(&gtk::gio::File::for_path(folder)));
    }
    dialog.select_folder_future(Some(window)).await.ok()?.path()
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
            let initial = folders
                .borrow()
                .last()
                .cloned()
                .or_else(|| music_folder().map(|(folder, _)| folder));
            let Some(path) = choose_music_folder(&window, initial).await else {
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

use crate::widgets::field_layout::{
    install_compact_field_row_responsiveness_at, style_compact_field_row,
};
