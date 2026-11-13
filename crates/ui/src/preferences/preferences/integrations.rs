//! Integrations: where Rufin scrobbles, looks covers and lyrics up, shows what plays and reads
//! remote folders. Wide windows list them beside the open one's page; narrow windows open each
//! page from the list.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use localization::{tr, tr_with};
use rufin_core::presence::DisplayType;
use rufin_core::runtime::{
    ScrobblingConnection, ScrobblingConnectionEvent, ScrobblingPreferences, ScrobblingProtocol,
    ScrobblingServerPreferences, ScrobblingServerRequest,
};
use rufin_core::settings::{
    ApplicationPreferences, ArtistImageProvider, CoverProvider, ExternalLyricsProvider, Settings,
};

use super::reorder_row::{ProviderList, ReorderRowPresentation, reorder_row};
use super::{PreferencesNavigationControls, ScrobblingCredentialDrafts, show_discord_flatpak_help};
use crate::shell::shell::Shell;

const LASTFM_API_CREATE_URL: &str = "https://www.last.fm/api/account/create";
const LISTENBRAINZ_TOKEN_URL: &str = "https://listenbrainz.org/settings/";
const FANART_KEY_URL: &str = "https://fanart.tv/get-an-api-key/";
const LIST_WIDTH: i32 = 290;

/// The top of an integration's page.
#[derive(Clone)]
struct Hero {
    status: gtk::Label,
    action: gtk::Button,
}

impl Hero {
    fn new(group: &adw::PreferencesGroup, name: &str, status: &str) -> Self {
        let resource = "preferences/integration_hero.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            hero: gtk::Box,
            hero_name: gtk::Label,
            hero_status: gtk::Label,
            hero_action: gtk::Button,
        });
        hero_name.set_label(name);
        hero_status.set_label(status);
        group.add(&hero);
        Self {
            status: hero_status,
            action: hero_action,
        }
    }

    fn with_action(self, label: &str) -> Self {
        self.action.set_label(label);
        self.action.set_visible(true);
        self
    }

    /// A scrobbling account's state: Connect is the primary action until it is connected.
    fn show_account(&self, connected: bool, status: &str) {
        if !self.action.is_sensitive() {
            return;
        }
        self.status.set_label(status);
        self.action.set_label(&if connected {
            tr("Reconnect")
        } else {
            tr("Connect")
        });
        if connected {
            self.action.remove_css_class("suggested-action");
        } else {
            self.action.add_css_class("suggested-action");
        }
    }
}

/// An integration in the list.
struct ListItem {
    id: String,
    row: gtk::ListBoxRow,
    status: gtk::Label,
    active: gtk::Box,
    chevron: gtk::Image,
}

impl ListItem {
    fn new(id: &str, name: &str) -> Self {
        let resource = "preferences/integration_row.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            row: gtk::ListBoxRow,
            row_name: gtk::Label,
            row_status: gtk::Label,
            row_active: gtk::Box,
            row_chevron: gtk::Image,
        });
        row_name.set_label(name);
        Self {
            id: id.to_owned(),
            row,
            status: row_status,
            active: row_active,
            chevron: row_chevron,
        }
    }

    fn show(&self, status: &str, active: bool) {
        self.status.set_label(status);
        self.active.set_visible(active);
    }
}

/// A scrobbling server the user added, with its page.
struct ServerItem {
    id: String,
    item: ListItem,
    page: adw::PreferencesPage,
    hero: Hero,
    url: adw::ActionRow,
    /// An Audioscrobbler server's username and password, shown while it is signed out.
    sign_in: [adw::EntryRow; 2],
    enabled: adw::SwitchRow,
    now_playing: adw::SwitchRow,
}

struct Integrations {
    shell: Rc<Shell>,
    controls: PreferencesNavigationControls,
    navigation: gtk::glib::WeakRef<adw::NavigationView>,
    detail_page: adw::NavigationPage,
    split: gtk::Box,
    list_scroller: gtk::ScrolledWindow,
    separator: gtk::Separator,
    details: gtk::Stack,
    lists: [gtk::ListBox; 4],
    titles: [gtk::Label; 4],
    items: RefCell<Vec<ListItem>>,
    servers: RefCell<Vec<ServerItem>>,
    add_server_row: gtk::ListBoxRow,
    accounts: [(&'static str, Hero); 3],
    /// Remote folder rows by connection, with the index of their kind's group.
    folders: RefCell<Vec<(sources::SourceId, usize, adw::ActionRow)>>,
    folder_groups: [(adw::PreferencesGroup, adw::ActionRow); 2],
    lyrics: Rc<ProviderGroup<ExternalLyricsProvider>>,
    narrow: Cell<Option<bool>>,
}

pub(super) fn page(
    shell: &Rc<Shell>,
    drafts: &ScrobblingCredentialDrafts,
    controls: &PreferencesNavigationControls,
) -> gtk::Widget {
    let resource = "preferences/integrations.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        navigation: adw::NavigationView,
        detail_page: adw::NavigationPage,
        split: gtk::Box,
        list_scroller: gtk::ScrolledWindow,
        split_separator: gtk::Separator,
        details: gtk::Stack,
        scrobbling_list: gtk::ListBox,
        lookups_list: gtk::ListBox,
        presence_list: gtk::ListBox,
        folders_list: gtk::ListBox,
        scrobbling_title: gtk::Label,
        lookups_title: gtk::Label,
        presence_title: gtk::Label,
        folders_title: gtk::Label,
        lastfm_hero: adw::PreferencesGroup,
        listenbrainz_hero: adw::PreferencesGroup,
        librefm_hero: adw::PreferencesGroup,
        add_server_hero: adw::PreferencesGroup,
        metadata_hero: adw::PreferencesGroup,
        lyrics_hero: adw::PreferencesGroup,
        discord_hero: adw::PreferencesGroup,
        links_hero: adw::PreferencesGroup,
        webdav_hero: adw::PreferencesGroup,
        smb_hero: adw::PreferencesGroup,
        webdav_group: adw::PreferencesGroup,
        smb_group: adw::PreferencesGroup,
        add_webdav: adw::ActionRow,
        add_smb: adw::ActionRow,
        lyrics_providers: adw::PreferencesGroup,
    });
    let not_connected = tr("Not connected");
    let lastfm = Hero::new(&lastfm_hero, "Last.fm", &not_connected).with_action(&tr("Connect"));
    let listenbrainz =
        Hero::new(&listenbrainz_hero, "ListenBrainz", &not_connected).with_action(&tr("Connect"));
    let librefm = Hero::new(&librefm_hero, "Libre.fm", &not_connected).with_action(&tr("Connect"));
    let add_server = Hero::new(
        &add_server_hero,
        &tr("Add server"),
        &tr("Self-hosted scrobbling"),
    );
    Hero::new(
        &metadata_hero,
        &tr("Metadata"),
        &tr("Covers and artist images your library doesn't have"),
    );
    Hero::new(
        &lyrics_hero,
        &tr("Lyrics"),
        &tr("Lyrics your library doesn't have"),
    );
    Hero::new(
        &discord_hero,
        "Discord",
        &tr("Shows what you play on your Discord profile"),
    );
    Hero::new(
        &links_hero,
        &tr("Page links"),
        &tr("Service icons on album and artist pages"),
    );
    Hero::new(
        &webdav_hero,
        &tr("WebDAV / Nextcloud"),
        &tr("Remote folders over WebDAV"),
    );
    Hero::new(
        &smb_hero,
        &tr("SMB / Samba"),
        &tr("Remote folders on a Windows share or NAS"),
    );

    let entries: [(&gtk::ListBox, &str, String); 10] = [
        (&scrobbling_list, "lastfm", "Last.fm".into()),
        (&scrobbling_list, "listenbrainz", "ListenBrainz".into()),
        (&scrobbling_list, "librefm", "Libre.fm".into()),
        (&scrobbling_list, "add_server", tr("Add server")),
        (&lookups_list, "metadata", tr("Metadata")),
        (&lookups_list, "lyrics", tr("Lyrics")),
        (&presence_list, "discord", "Discord".into()),
        (&presence_list, "links", tr("Page links")),
        (&folders_list, "webdav", tr("WebDAV / Nextcloud")),
        (&folders_list, "smb", tr("SMB / Samba")),
    ];
    let mut items = Vec::new();
    for (list, id, name) in entries {
        let item = ListItem::new(id, &name);
        list.append(&item.row);
        items.push(item);
    }
    let add_server_row = items[3].row.clone();
    items[3].show(&tr("ListenBrainz or Audioscrobbler API"), false);

    let lyrics = lyrics_provider_group(shell, &lyrics_providers);
    lyrics.populate();
    let state = Rc::new(Integrations {
        shell: Rc::clone(shell),
        controls: controls.clone(),
        navigation: navigation.downgrade(),
        detail_page,
        split,
        list_scroller,
        separator: split_separator,
        details,
        lists: [scrobbling_list, lookups_list, presence_list, folders_list],
        titles: [
            scrobbling_title,
            lookups_title,
            presence_title,
            folders_title,
        ],
        items: RefCell::new(items),
        servers: RefCell::new(Vec::new()),
        add_server_row,
        accounts: [
            ("lastfm", lastfm.clone()),
            ("listenbrainz", listenbrainz.clone()),
            ("librefm", librefm.clone()),
        ],
        folders: RefCell::new(Vec::new()),
        folder_groups: [(webdav_group, add_webdav), (smb_group, add_smb)],
        lyrics,
        narrow: Cell::new(None),
    });

    bind_scrobbling(&state, &builder, drafts, &lastfm, &listenbrainz, &librefm);
    bind_add_server(&state, &builder, &add_server);
    bind_metadata(shell, &builder);
    bind_lyrics(&state, &builder);
    bind_presence(shell, &builder);
    bind_folders(&state);

    for list in &state.lists {
        let weak = Rc::downgrade(&state);
        list.connect_row_activated(move |_, row| {
            if let Some(state) = weak.upgrade() {
                state.open(row);
            }
        });
    }
    let weak = Rc::downgrade(&state);
    state.details.connect_visible_child_name_notify(move |_| {
        if let Some(state) = weak.upgrade() {
            state.show_selection();
        }
    });
    controls.set_navigation(&navigation);
    controls.set_nested_page_visible(false);
    let popped_controls = controls.clone();
    navigation.connect_popped(move |_, _| popped_controls.set_nested_page_visible(false));
    if let Some(split) = controls.categories_split() {
        let weak = Rc::downgrade(&state);
        split.connect_collapsed_notify(move |split| {
            if let Some(state) = weak.upgrade() {
                state.set_narrow(split.is_collapsed());
            }
        });
        state.set_narrow(split.is_collapsed());
    } else {
        state.set_narrow(false);
    }
    state.details.set_visible_child_name("lastfm");
    state.show_selection();

    // The state lives while the page does: its refresh is the page's.
    let refresh_state = Rc::clone(&state);
    super::refresh_while_mapped(
        shell,
        &navigation,
        Rc::new(move |settings: &Settings| refresh_state.refresh(settings)),
    );
    navigation.upcast()
}

impl Integrations {
    fn item_id(&self, row: &gtk::ListBoxRow) -> Option<String> {
        self.items
            .borrow()
            .iter()
            .chain(self.servers.borrow().iter().map(|server| &server.item))
            .find(|item| &item.row == row)
            .map(|item| item.id.clone())
    }

    fn open(&self, row: &gtk::ListBoxRow) {
        let Some(id) = self.item_id(row) else {
            return;
        };
        self.details.set_visible_child_name(&id);
        if self.narrow.get() == Some(true)
            && let Some(navigation) = self.navigation.upgrade()
        {
            navigation.push(&self.detail_page);
            self.controls.set_nested_page_visible(true);
        }
    }

    /// The list marks the open page on wide windows, and the narrow page takes its title.
    fn show_selection(&self) {
        let Some(child) = self.details.visible_child() else {
            return;
        };
        self.detail_page
            .set_title(&self.details.page(&child).title().unwrap_or_default());
        let Some(id) = self.details.visible_child_name() else {
            return;
        };
        let items = self.items.borrow();
        let servers = self.servers.borrow();
        let row = items
            .iter()
            .chain(servers.iter().map(|server| &server.item))
            .find(|item| item.id == id.as_str())
            .map(|item| item.row.clone());
        for list in &self.lists {
            match &row {
                Some(row)
                    if self.narrow.get() != Some(true)
                        && row
                            .parent()
                            .is_some_and(|parent| &parent == list.upcast_ref::<gtk::Widget>()) =>
                {
                    list.select_row(Some(row))
                }
                _ => list.unselect_all(),
            }
        }
    }

    fn set_narrow(&self, narrow: bool) {
        if self.narrow.replace(Some(narrow)) == Some(narrow) {
            return;
        }
        let details = self.details.clone().upcast::<gtk::Widget>();
        if narrow {
            if details.parent().is_some() {
                self.split.remove(&details);
            }
            self.detail_page.set_child(Some(&details));
        } else {
            if let Some(navigation) = self.navigation.upgrade() {
                while navigation.pop() {}
            }
            self.controls.set_nested_page_visible(false);
            self.detail_page.set_child(None::<&gtk::Widget>);
            self.split.append(&details);
        }
        self.separator.set_visible(!narrow);
        self.list_scroller.set_hexpand(narrow);
        self.list_scroller
            .set_width_request(if narrow { -1 } else { LIST_WIDTH });
        for list in &self.lists {
            if narrow {
                list.remove_css_class("navigation-sidebar");
                list.add_css_class("boxed-list");
                list.set_selection_mode(gtk::SelectionMode::None);
            } else {
                list.remove_css_class("boxed-list");
                list.add_css_class("navigation-sidebar");
                list.set_selection_mode(gtk::SelectionMode::Single);
            }
        }
        for title in &self.titles {
            if narrow {
                title.remove_css_class("dim-label");
                title.remove_css_class("caption-heading");
                title.add_css_class("heading");
            } else {
                title.remove_css_class("heading");
                title.add_css_class("dim-label");
                title.add_css_class("caption-heading");
            }
        }
        for item in self.items.borrow().iter() {
            item.chevron.set_visible(narrow);
        }
        for server in self.servers.borrow().iter() {
            server.item.chevron.set_visible(narrow);
        }
        self.show_selection();
    }

    fn item(&self, id: &str) -> std::cell::Ref<'_, ListItem> {
        std::cell::Ref::map(self.items.borrow(), |items| {
            items
                .iter()
                .find(|item| item.id == id)
                .expect("integration")
        })
    }

    /// Every row's status and every page that depends on saved state.
    fn refresh(self: &Rc<Self>, settings: &Settings) {
        let preferences = self.shell.products.scrobbling.preferences();
        let accounts = [
            (
                preferences.lastfm.connected,
                preferences.lastfm.username.as_str(),
                preferences.lastfm.enabled,
            ),
            (
                preferences.listenbrainz.connected,
                preferences.listenbrainz.username.as_str(),
                preferences.listenbrainz.enabled,
            ),
            (
                preferences.librefm.connected,
                preferences.librefm.username.as_str(),
                preferences.librefm.enabled,
            ),
        ];
        for ((id, hero), (connected, username, enabled)) in self.accounts.iter().zip(accounts) {
            hero.show_account(connected, &connection_subtitle(connected, username));
            self.item(id)
                .show(&account_status(connected, username), connected && enabled);
        }
        self.sync_servers(&preferences);

        let metadata = settings.application.external_metadata_enabled;
        self.item("metadata").show(
            &if metadata {
                tr("Covers, artist images, tags")
            } else {
                tr("Off")
            },
            metadata,
        );
        let lyrics = &settings.lyrics.preferences;
        let providers = lyrics
            .external_lyrics_providers
            .iter()
            .map(|provider| tr(provider.title()))
            .collect::<Vec<_>>();
        self.item("lyrics").show(
            &if lyrics.external_lyrics_enabled {
                providers.join(", ")
            } else {
                tr("Off")
            },
            lyrics.external_lyrics_enabled,
        );
        let presence = settings.rich_presence.enabled;
        self.item("discord").show(
            &if presence {
                tr("Rich Presence on")
            } else {
                tr("Off")
            },
            presence,
        );
        let links = &settings.application.external_site_links;
        let services = [
            (links.lastfm, "Last.fm".to_owned()),
            (links.musicbrainz, "MusicBrainz".to_owned()),
            (links.server, tr("Server")),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect::<Vec<_>>();
        self.item("links").show(
            &if links.enabled {
                services.join(", ")
            } else {
                tr("Off")
            },
            links.enabled,
        );
        self.sync_folders();
    }

    /// One page per added server, in the list before Add server.
    fn sync_servers(self: &Rc<Self>, preferences: &ScrobblingPreferences) {
        let gone = {
            let mut servers = self.servers.borrow_mut();
            let (kept, gone): (Vec<_>, Vec<_>) = servers.drain(..).partition(|server| {
                preferences
                    .servers
                    .iter()
                    .any(|candidate| candidate.id == server.id)
            });
            *servers = kept;
            gone
        };
        for server in gone {
            if self.details.visible_child_name().as_deref() == Some(server.id.as_str()) {
                self.details.set_visible_child_name("lastfm");
            }
            self.lists[0].remove(&server.item.row);
            self.details.remove(&server.page);
        }
        for server in &preferences.servers {
            if !self
                .servers
                .borrow()
                .iter()
                .any(|item| item.id == server.id)
            {
                let item = self.add_server_page(server);
                self.servers.borrow_mut().push(item);
            }
            let servers = self.servers.borrow();
            let item = servers
                .iter()
                .find(|item| item.id == server.id)
                .expect("server page");
            let protocol = protocol_title(server.protocol);
            item.hero.show_account(
                server.connected,
                &format!(
                    "{} · {protocol}",
                    connection_subtitle(server.connected, &server.username)
                ),
            );
            // An Audioscrobbler server signs in again with its username and password once its
            // session stops working.
            let audioscrobbler = server.protocol == ScrobblingProtocol::Audioscrobbler;
            if audioscrobbler {
                item.hero.action.set_label(&tr("Sign in again"));
            }
            item.hero
                .action
                .set_visible(!audioscrobbler || !server.connected);
            for row in &item.sign_in {
                row.set_visible(audioscrobbler && !server.connected);
            }
            item.item.show(
                &format!(
                    "{} · {protocol}",
                    account_status(server.connected, &server.username)
                ),
                server.connected && server.enabled,
            );
            item.url.set_subtitle(&server.url);
            item.enabled.set_active(server.enabled);
            item.now_playing.set_active(server.now_playing_enabled);
        }
    }

    fn add_server_page(self: &Rc<Self>, server: &ScrobblingServerPreferences) -> ServerItem {
        let resource = "preferences/scrobbling_server.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            server_page: adw::PreferencesPage,
            server_hero: adw::PreferencesGroup,
            server_protocol: gtk::Label,
            server_url: adw::ActionRow,
            server_token: adw::PasswordEntryRow,
            server_username: adw::EntryRow,
            server_password: adw::PasswordEntryRow,
            server_enabled: adw::SwitchRow,
            server_now_playing: adw::SwitchRow,
            server_remove: adw::ButtonRow,
        });
        server_page.set_title(&server.name);
        let hero = Hero::new(&server_hero, &server.name, "").with_action(&tr("Connect"));
        server_protocol.set_label(&protocol_title(server.protocol));
        let listenbrainz = server.protocol == ScrobblingProtocol::ListenBrainz;
        server_token.set_visible(listenbrainz);
        server_username.set_text(&server.username);
        let item = ListItem::new(&server.id, &server.name);
        item.chevron.set_visible(self.narrow.get() == Some(true));
        let position = self.add_server_row.index();
        self.lists[0].insert(&item.row, position);
        self.details
            .add_titled(&server_page, Some(&server.id), &server.name);

        let id = server.id.clone();
        for (row, field) in [
            (
                &server_enabled,
                (|server: &mut ScrobblingServerPreferences| &mut server.enabled)
                    as fn(&mut ScrobblingServerPreferences) -> &mut bool,
            ),
            (&server_now_playing, |server| {
                &mut server.now_playing_enabled
            }),
        ] {
            let shell = Rc::clone(&self.shell);
            let id = id.clone();
            row.connect_active_notify(move |row| {
                shell.update_scrobbling_settings("scrobbling server setting", |preferences| {
                    preferences
                        .servers
                        .iter_mut()
                        .find(|server| server.id == id)
                        .is_some_and(|server| replace_if_changed(field(server), row.is_active()))
                });
            });
        }
        let connect: Rc<dyn Fn()> = {
            let weak = Rc::downgrade(self);
            let hero = hero.clone();
            let rows = (
                server_token.downgrade(),
                server_username.downgrade(),
                server_password.downgrade(),
            );
            let id = id.clone();
            Rc::new(move || {
                let (Some(state), Some(token), Some(username), Some(password)) = (
                    weak.upgrade(),
                    rows.0.upgrade(),
                    rows.1.upgrade(),
                    rows.2.upgrade(),
                ) else {
                    return;
                };
                let (username, secret) = if listenbrainz {
                    (String::new(), token.text())
                } else {
                    (username.text().trim().to_string(), password.text())
                };
                state.sign_in_server(&hero, id.clone(), username, secret.to_string(), &password);
            })
        };
        let hero_connect = Rc::clone(&connect);
        hero.action.connect_clicked(move |_| hero_connect());
        let apply_connect = Rc::clone(&connect);
        server_token.connect_apply(move |_| apply_connect());
        server_password.connect_entry_activated(move |_| connect());
        let weak = Rc::downgrade(self);
        server_remove.connect_activated(move |row| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            row.set_sensitive(false);
            let id = id.clone();
            let row = row.downgrade();
            gtk::glib::spawn_future_local(async move {
                match state.shell.products.scrobbling.remove_server(id).await {
                    Ok(preferences) => state.sync_servers(&preferences),
                    Err(error) => {
                        state
                            .shell
                            .control_feedback
                            .show_feedback_toast(error.to_string());
                        if let Some(row) = row.upgrade() {
                            row.set_sensitive(true);
                        }
                    }
                }
            });
        });
        ServerItem {
            id: server.id.clone(),
            item,
            page: server_page,
            hero,
            url: server_url,
            sign_in: [server_username, server_password.upcast()],
            enabled: server_enabled,
            now_playing: server_now_playing,
        }
    }

    /// The account opens its sign-in page in the browser and waits for it.
    fn connect_browser(
        self: &Rc<Self>,
        hero: &Hero,
        request: ScrobblingConnection,
        opening: String,
    ) {
        let lastfm = matches!(request, ScrobblingConnection::LastFm { .. });
        hero.action.set_sensitive(false);
        hero.status.set_label(&opening);
        let state = Rc::clone(self);
        let hero = hero.clone();
        gtk::glib::spawn_future_local(async move {
            let result = connect_scrobbling(&state.shell, request, lastfm).await;
            hero.action.set_sensitive(true);
            match result {
                Ok(username) => hero.show_account(true, &connection_subtitle(true, &username)),
                Err(error) => hero.status.set_label(&error),
            }
            let settings = state.shell.settings.current.borrow().clone();
            state.refresh(&settings);
        });
    }

    fn connect_listenbrainz(self: &Rc<Self>, hero: &Hero, token: String) {
        hero.action.set_sensitive(false);
        hero.status.set_label(&tr("Checking the user token..."));
        let state = Rc::clone(self);
        let hero = hero.clone();
        gtk::glib::spawn_future_local(async move {
            let result = state
                .shell
                .products
                .scrobbling
                .connect_listenbrainz(token)
                .await;
            hero.action.set_sensitive(true);
            if let Err(error) = result {
                hero.status.set_label(&error.to_string());
                return;
            }
            let settings = state.shell.settings.current.borrow().clone();
            state.refresh(&settings);
        });
    }

    /// Signs in to an added server again; the password row is emptied once it is used.
    fn sign_in_server(
        self: &Rc<Self>,
        hero: &Hero,
        id: String,
        username: String,
        secret: String,
        password: &adw::PasswordEntryRow,
    ) {
        hero.action.set_sensitive(false);
        hero.status.set_label(&tr("Signing in..."));
        let state = Rc::clone(self);
        let hero = hero.clone();
        let password = password.downgrade();
        gtk::glib::spawn_future_local(async move {
            let result = state
                .shell
                .products
                .scrobbling
                .sign_in_server(id, username, secret)
                .await;
            hero.action.set_sensitive(true);
            if let Some(password) = password.upgrade() {
                password.set_text("");
            }
            match result {
                Ok(preferences) => state.sync_servers(&preferences),
                Err(error) => hero.status.set_label(&error.to_string()),
            }
        });
    }

    /// The remote folder connections of each kind, before its Add row.
    fn sync_folders(&self) {
        let connections = self.shell.products.source.file_integrations();
        self.folders.borrow_mut().retain(|(id, kind, row)| {
            let keep = connections.iter().any(|connection| &connection.id == id);
            if !keep {
                self.folder_groups[*kind].0.remove(row);
            }
            keep
        });
        for (index, kind) in ["webdav", "smb"].into_iter().enumerate() {
            let (group, add) = &self.folder_groups[index];
            let ours = connections
                .iter()
                .filter(|connection| (connection.kind == "smb") == (kind == "smb"))
                .collect::<Vec<_>>();
            self.item(kind).show(
                &localization::folder_count_text(ours.len() as u64),
                !ours.is_empty(),
            );
            for connection in ours {
                if let Some((_, _, row)) = self
                    .folders
                    .borrow()
                    .iter()
                    .find(|(id, _, _)| id == &connection.id)
                {
                    if row.title() != connection.name {
                        row.set_title(&connection.name);
                    }
                    continue;
                }
                let resource = "preferences/source/file_integration.ui";
                let builder = crate::ui_resource::builder(resource);
                let row: adw::ActionRow =
                    crate::ui_resource::object(&builder, resource, "connection");
                row.set_title(&connection.name);
                let shell = Rc::downgrade(&self.shell);
                let id = connection.id.clone();
                row.connect_activated(move |row| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    match shell.products.source.editable(id.clone()) {
                        Ok(Some(saved)) => super::source::login::present_integration(
                            &shell,
                            row,
                            kind,
                            Some(saved),
                            Rc::new(|_| {}),
                        ),
                        Ok(None) => {}
                        Err(error) => shell
                            .control_feedback
                            .show_feedback_toast(error.to_string()),
                    }
                });
                group.remove(add);
                group.add(&row);
                group.add(add);
                self.folders
                    .borrow_mut()
                    .push((connection.id.clone(), index, row));
            }
        }
    }
}

fn protocol_title(protocol: ScrobblingProtocol) -> String {
    match protocol {
        ScrobblingProtocol::ListenBrainz => tr("ListenBrainz API"),
        ScrobblingProtocol::Audioscrobbler => tr("Audioscrobbler API"),
    }
}

fn account_status(connected: bool, username: &str) -> String {
    match (connected, username.trim()) {
        (false, _) => tr("Not connected"),
        (true, "") => tr("Connected"),
        (true, username) => username.to_owned(),
    }
}

pub fn connection_subtitle(connected: bool, username: &str) -> String {
    match (connected, username.trim()) {
        (false, _) => tr("Not connected"),
        (true, "") => tr("Connected"),
        (true, username) => tr_with("Connected as {username}", &[("username", username)]),
    }
}

fn replace_if_changed(current: &mut bool, next: bool) -> bool {
    if *current == next {
        return false;
    }
    *current = next;
    true
}

fn open_link(row: &adw::ActionRow, shell: &Rc<Shell>, url: &'static str) {
    let shell = Rc::downgrade(shell);
    row.connect_activated(move |_| {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        gtk::UriLauncher::new(url).launch(
            Some(&shell.chrome.window),
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    });
}

fn bind_switch(
    row: &adw::SwitchRow,
    shell: &Rc<Shell>,
    warning_action: &'static str,
    field: fn(&mut ScrobblingPreferences) -> &mut bool,
    initial: &ScrobblingPreferences,
) {
    row.set_active(*field(&mut initial.clone()));
    let shell = Rc::clone(shell);
    row.connect_active_notify(move |row| {
        shell.update_scrobbling_settings(warning_action, |preferences| {
            replace_if_changed(field(preferences), row.is_active())
        });
    });
}

fn bind_scrobbling(
    state: &Rc<Integrations>,
    builder: &gtk::Builder,
    drafts: &ScrobblingCredentialDrafts,
    lastfm: &Hero,
    listenbrainz: &Hero,
    librefm: &Hero,
) {
    let resource = "preferences/integrations.ui";
    crate::objects!(builder, resource, {
        lastfm_enabled: adw::SwitchRow,
        lastfm_now_playing: adw::SwitchRow,
        lastfm_api_key: adw::PasswordEntryRow,
        lastfm_api_secret: adw::PasswordEntryRow,
        lastfm_api_link: adw::ActionRow,
        listenbrainz_token: adw::PasswordEntryRow,
        listenbrainz_token_link: adw::ActionRow,
        listenbrainz_enabled: adw::SwitchRow,
        listenbrainz_now_playing: adw::SwitchRow,
        librefm_enabled: adw::SwitchRow,
        librefm_now_playing: adw::SwitchRow,
    });
    let shell = &state.shell;
    let preferences = shell.products.scrobbling.preferences();
    bind_switch(
        &lastfm_enabled,
        shell,
        "Last.fm scrobbling setting",
        |p| &mut p.lastfm.enabled,
        &preferences,
    );
    bind_switch(
        &lastfm_now_playing,
        shell,
        "Last.fm now playing setting",
        |p| &mut p.lastfm.now_playing_enabled,
        &preferences,
    );
    bind_switch(
        &listenbrainz_enabled,
        shell,
        "ListenBrainz scrobbling setting",
        |p| &mut p.listenbrainz.enabled,
        &preferences,
    );
    bind_switch(
        &listenbrainz_now_playing,
        shell,
        "ListenBrainz now playing setting",
        |p| &mut p.listenbrainz.now_playing_enabled,
        &preferences,
    );
    bind_switch(
        &librefm_enabled,
        shell,
        "Libre.fm scrobbling setting",
        |p| &mut p.librefm.enabled,
        &preferences,
    );
    bind_switch(
        &librefm_now_playing,
        shell,
        "Libre.fm now playing setting",
        |p| &mut p.librefm.now_playing_enabled,
        &preferences,
    );
    open_link(&lastfm_api_link, shell, LASTFM_API_CREATE_URL);
    open_link(&listenbrainz_token_link, shell, LISTENBRAINZ_TOKEN_URL);

    // Typed keys survive the page being built again until they are saved.
    lastfm_api_key.set_text(&drafts.borrow().lastfm.api_key);
    let key_drafts = drafts.clone();
    lastfm_api_key.connect_text_notify(move |row| {
        key_drafts.borrow_mut().lastfm.api_key = row.text().to_string();
    });
    let key_shell = Rc::clone(shell);
    lastfm_api_key.connect_apply(move |row| {
        key_shell.save_scrobbling_credential(
            "Last.fm API key setting",
            rufin_core::scrobbling::ScrobblingCredentialEdit::LastFm {
                api_key: Some(row.text().trim().to_string()),
                api_secret: None,
            },
        );
    });
    lastfm_api_secret.set_text(&drafts.borrow().lastfm.api_secret);
    let secret_drafts = drafts.clone();
    lastfm_api_secret.connect_text_notify(move |row| {
        secret_drafts.borrow_mut().lastfm.api_secret = row.text().to_string();
    });
    let secret_shell = Rc::clone(shell);
    lastfm_api_secret.connect_apply(move |row| {
        secret_shell.save_scrobbling_credential(
            "Last.fm shared secret setting",
            rufin_core::scrobbling::ScrobblingCredentialEdit::LastFm {
                api_key: None,
                api_secret: Some(row.text().trim().to_string()),
            },
        );
    });
    listenbrainz_token.set_text(&drafts.borrow().listenbrainz.user_token);
    let token_drafts = drafts.clone();
    listenbrainz_token.connect_text_notify(move |row| {
        token_drafts.borrow_mut().listenbrainz.user_token = row.text().to_string();
    });

    let weak = Rc::downgrade(state);
    let (key, secret) = (lastfm_api_key.downgrade(), lastfm_api_secret.downgrade());
    let hero = lastfm.clone();
    lastfm.action.connect_clicked(move |_| {
        let (Some(state), Some(key), Some(secret)) =
            (weak.upgrade(), key.upgrade(), secret.upgrade())
        else {
            return;
        };
        state.connect_browser(
            &hero,
            ScrobblingConnection::LastFm {
                api_key: key.text().trim().to_string(),
                api_secret: secret.text().trim().to_string(),
            },
            tr("Opening Last.fm authorization..."),
        );
    });
    let weak = Rc::downgrade(state);
    let hero = librefm.clone();
    librefm.action.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            state.connect_browser(
                &hero,
                ScrobblingConnection::LibreFm,
                tr("Opening Libre.fm authorization..."),
            );
        }
    });
    let weak = Rc::downgrade(state);
    let token = listenbrainz_token.downgrade();
    let hero = listenbrainz.clone();
    listenbrainz.action.connect_clicked(move |_| {
        let (Some(state), Some(token)) = (weak.upgrade(), token.upgrade()) else {
            return;
        };
        let token = token.text().trim().to_string();
        if token.is_empty() {
            hero.status.set_label(&tr("Enter your user token first"));
            return;
        }
        state.connect_listenbrainz(&hero, token);
    });

    // Saved keys arrive after the page shows; typed ones stay.
    let owner = shell.products.scrobbling.clone();
    let rows = [
        (lastfm_api_key.downgrade(), lastfm_api_key.text()),
        (lastfm_api_secret.downgrade(), lastfm_api_secret.text()),
        (listenbrainz_token.downgrade(), listenbrainz_token.text()),
    ];
    let weak = Rc::downgrade(state);
    gtk::glib::spawn_future_local(async move {
        let Ok(loaded) = owner.load_preferences().await else {
            return;
        };
        let values = [
            loaded.lastfm.api_key,
            loaded.lastfm.api_secret,
            loaded.listenbrainz.user_token,
        ];
        for ((row, initial), value) in rows.into_iter().zip(values) {
            if let Some(row) = row.upgrade()
                && row.text() == initial
            {
                row.set_text(&value);
            }
        }
        if let Some(state) = weak.upgrade() {
            let settings = state.shell.settings.current.borrow().clone();
            state.refresh(&settings);
        }
    });
}

fn bind_add_server(state: &Rc<Integrations>, builder: &gtk::Builder, hero: &Hero) {
    let resource = "preferences/integrations.ui";
    crate::objects!(builder, resource, {
        add_protocol_group: adw::PreferencesGroup,
        add_protocol: adw::ToggleGroup,
        add_name: adw::EntryRow,
        add_url: adw::EntryRow,
        add_token: adw::PasswordEntryRow,
        add_username: adw::EntryRow,
        add_password: adw::PasswordEntryRow,
        add_connect: gtk::Button,
    });
    let show_protocol = {
        let group = add_protocol_group.downgrade();
        let rows = (
            add_token.downgrade(),
            add_username.downgrade(),
            add_password.downgrade(),
        );
        move |toggles: &adw::ToggleGroup| {
            let listenbrainz = toggles.active_name().as_deref() != Some("audioscrobbler");
            if let Some(group) = group.upgrade() {
                group.set_description(Some(&if listenbrainz {
                    tr("Koito, Maloja and other ListenBrainz servers.")
                } else {
                    tr("GNU FM, Maloja and other Last.fm-style servers. On Maloja, the password is an API key.")
                }));
            }
            if let (Some(token), Some(username), Some(password)) =
                (rows.0.upgrade(), rows.1.upgrade(), rows.2.upgrade())
            {
                token.set_visible(listenbrainz);
                username.set_visible(!listenbrainz);
                password.set_visible(!listenbrainz);
            }
        }
    };
    add_protocol.set_active_name(Some("listenbrainz"));
    show_protocol(&add_protocol);
    add_protocol.connect_active_name_notify(show_protocol);

    let weak = Rc::downgrade(state);
    let hero_for_add = hero.clone();
    let protocol_toggles = add_protocol.downgrade();
    let fields: [adw::EntryRow; 5] = [
        add_name,
        add_url,
        add_token.upcast(),
        add_username,
        add_password.upcast(),
    ];
    add_connect.connect_clicked(move |button| {
        let (Some(state), Some(protocol)) = (weak.upgrade(), protocol_toggles.upgrade()) else {
            return;
        };
        let protocol = if protocol.active_name().as_deref() == Some("audioscrobbler") {
            ScrobblingProtocol::Audioscrobbler
        } else {
            ScrobblingProtocol::ListenBrainz
        };
        let [name, url, token, username, password] = &fields;
        let listenbrainz = protocol == ScrobblingProtocol::ListenBrainz;
        let request = ScrobblingServerRequest {
            name: name.text().trim().to_string(),
            protocol,
            url: url.text().trim().to_string(),
            username: if listenbrainz {
                String::new()
            } else {
                username.text().trim().to_string()
            },
            secret: if listenbrainz {
                token.text().trim().to_string()
            } else {
                password.text().to_string()
            },
        };
        if request.name.is_empty() || request.url.is_empty() {
            hero_for_add.status.set_label(&tr("Enter a name and URL."));
            return;
        }
        button.set_sensitive(false);
        hero_for_add.status.set_label(&tr("Signing in..."));
        let button = button.downgrade();
        let hero = hero_for_add.clone();
        let fields = fields.clone();
        gtk::glib::spawn_future_local(async move {
            let result = state.shell.products.scrobbling.add_server(request).await;
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
            fields[4].set_text("");
            let preferences = match result {
                Ok(preferences) => preferences,
                Err(error) => {
                    hero.status.set_label(&error.to_string());
                    return;
                }
            };
            hero.status.set_label(&tr("Self-hosted scrobbling"));
            for field in &fields {
                field.set_text("");
            }
            state.sync_servers(&preferences);
            if let Some(server) = preferences.servers.last() {
                state.details.set_visible_child_name(&server.id);
            }
        });
    });
}

/// Opens an account's sign-in in the browser and waits for it to finish.
async fn connect_scrobbling(
    shell: &Rc<Shell>,
    request: ScrobblingConnection,
    refresh_external_artwork: bool,
) -> Result<String, String> {
    let events = shell.products.scrobbling.connect(request);
    while let Ok(event) = events.next().await {
        match event {
            ScrobblingConnectionEvent::OpenUrl { url, opened } => {
                let launcher = gtk::UriLauncher::new(&url);
                match launcher.launch_future(Some(&shell.chrome.window)).await {
                    Ok(()) => opened.complete(None).await,
                    Err(error) => {
                        let error = tr_with(
                            "Couldn't open the sign-in page: {error}",
                            &[("error", &error.to_string())],
                        );
                        opened.complete(Some(error.clone())).await;
                        return Err(error);
                    }
                }
            }
            ScrobblingConnectionEvent::Connected { username } => {
                if refresh_external_artwork {
                    shell.retry_external_artwork("Last.fm connection setting");
                }
                return Ok(username);
            }
            ScrobblingConnectionEvent::TimedOut => break,
            ScrobblingConnectionEvent::Failed(error) => return Err(error),
        }
    }
    Err(tr("Couldn't connect"))
}

/// A list of providers that are asked in order: the shown ones first, then the hidden ones.
struct ProviderGroup<T> {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    read: Box<dyn Fn() -> (ProviderList<T>, bool)>,
    write: Box<dyn Fn(Vec<T>)>,
    key: Box<dyn Fn(T) -> &'static str>,
    /// Title and subtitle.
    row: Box<dyn Fn(T) -> (String, String)>,
}

impl<T: Copy + PartialEq + 'static> ProviderGroup<T> {
    fn populate(self: &Rc<Self>) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
        let (list, sensitive) = (self.read)();
        let shown = list.shown.len();
        for (index, (item, visible)) in list.rows().into_iter().enumerate() {
            let (title, subtitle) = (self.row)(item);
            let edit = {
                let weak = Rc::downgrade(self);
                move |edit: &dyn Fn(ProviderList<T>) -> Vec<T>| {
                    let Some(group) = weak.upgrade() else {
                        return;
                    };
                    let (list, _) = (group.read)();
                    let next = edit(list);
                    (group.write)(next);
                    group.populate();
                }
            };
            let on_visible = {
                let edit = edit.clone();
                move |visible| edit(&|list: ProviderList<T>| list.set_shown(item, visible))
            };
            let on_move = {
                let edit = edit.clone();
                move |delta| edit(&|list: ProviderList<T>| list.moved(item, delta))
            };
            let on_drop = {
                let weak = Rc::downgrade(self);
                move |source: String, after| {
                    let Some(group) = weak.upgrade() else {
                        return false;
                    };
                    let (list, _) = (group.read)();
                    let Some(source) = list
                        .all
                        .iter()
                        .copied()
                        .find(|item| (group.key)(*item) == source)
                    else {
                        return false;
                    };
                    edit(&|list: ProviderList<T>| list.dropped(source, item, after));
                    true
                }
            };
            let row = reorder_row(
                ReorderRowPresentation {
                    title,
                    subtitle,
                    icon: None,
                    visible,
                    visible_sensitive: true,
                    up_sensitive: visible && index > 0,
                    down_sensitive: visible && index + 1 < shown,
                    drag_id: (self.key)(item).to_string(),
                },
                on_visible,
                on_move,
                on_drop,
            );
            row.set_sensitive(sensitive);
            self.group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }
}

fn bind_metadata(shell: &Rc<Shell>, builder: &gtk::Builder) {
    let resource = "preferences/integrations.ui";
    crate::objects!(builder, resource, {
        external_metadata_row: adw::SwitchRow,
        cover_providers: adw::PreferencesGroup,
        artist_image_providers: adw::PreferencesGroup,
        fanart_key: adw::PasswordEntryRow,
        fanart_link: adw::ActionRow,
    });
    let covers = artwork_providers(
        shell,
        cover_providers,
        &CoverProvider::all(),
        |settings| &mut settings.cover_providers,
        |provider| match provider {
            CoverProvider::MusicBrainz => ("musicbrainz", "MusicBrainz"),
            CoverProvider::LastFm => ("lastfm", "Last.fm"),
        },
    );
    let artists = artwork_providers(
        shell,
        artist_image_providers,
        &ArtistImageProvider::all(),
        |settings| &mut settings.artist_image_providers,
        |provider| match provider {
            ArtistImageProvider::FanartTv => ("fanart", "fanart.tv"),
            ArtistImageProvider::WikimediaCommons => ("commons", "Wikimedia Commons"),
        },
    );
    covers.populate();
    artists.populate();

    external_metadata_row.set_active(
        shell
            .settings
            .current
            .borrow()
            .application
            .external_metadata_enabled,
    );
    let metadata_shell = Rc::clone(shell);
    external_metadata_row.connect_active_notify(move |row| {
        metadata_shell.set_external_metadata_enabled(row.is_active());
        covers.populate();
        artists.populate();
    });

    fanart_key.set_text(&shell.settings.current.borrow().application.fanart_api_key);
    let fanart_shell = Rc::clone(shell);
    fanart_key.connect_apply(move |row| {
        if fanart_shell
            .settings
            .set_app_setting(
                "fanart.tv key setting",
                row.text().trim().to_string(),
                |settings| &mut settings.application.fanart_api_key,
            )
            .is_some()
        {
            fanart_shell.retry_external_artwork("fanart.tv key setting");
        }
    });
    open_link(&fanart_link, shell, FANART_KEY_URL);
}

/// An artwork provider list of the application settings: its key, name and what it finds. A
/// change asks again for the artwork still missing.
fn artwork_providers<T: Copy + PartialEq + 'static>(
    shell: &Rc<Shell>,
    group: adw::PreferencesGroup,
    all: &[T],
    field: fn(&mut ApplicationPreferences) -> &mut Vec<T>,
    describe: fn(T) -> (&'static str, &'static str),
) -> Rc<ProviderGroup<T>> {
    let all = all.to_vec();
    let read_shell = Rc::downgrade(shell);
    let write_shell = Rc::downgrade(shell);
    Rc::new(ProviderGroup {
        group,
        rows: RefCell::new(Vec::new()),
        read: Box::new(move || {
            let mut settings = read_shell
                .upgrade()
                .map(|shell| shell.settings.current.borrow().application.clone())
                .unwrap_or_default();
            let shown = field(&mut settings).clone();
            let all = all.clone();
            (
                ProviderList { shown, all },
                settings.external_metadata_enabled,
            )
        }),
        write: Box::new(move |providers| {
            if let Some(shell) = write_shell.upgrade()
                && shell
                    .settings
                    .set_app_setting("artwork provider setting", providers, |settings| {
                        field(&mut settings.application)
                    })
                    .is_some()
            {
                shell.retry_external_artwork("artwork provider setting");
            }
        }),
        key: Box::new(move |provider| describe(provider).0),
        row: Box::new(move |provider| (describe(provider).1.to_owned(), String::new())),
    })
}

fn lyrics_provider_group(
    shell: &Rc<Shell>,
    group: &adw::PreferencesGroup,
) -> Rc<ProviderGroup<ExternalLyricsProvider>> {
    let weak_shell = Rc::downgrade(shell);
    Rc::new(ProviderGroup {
        group: group.clone(),
        rows: RefCell::new(Vec::new()),
        read: {
            let shell = weak_shell.clone();
            Box::new(move || {
                let settings = shell
                    .upgrade()
                    .map(|shell| shell.settings.current.borrow().lyrics.preferences.clone())
                    .unwrap_or_default();
                (
                    ProviderList {
                        shown: settings.external_lyrics_providers,
                        all: ExternalLyricsProvider::all().to_vec(),
                    },
                    settings.external_lyrics_enabled,
                )
            })
        },
        write: {
            let shell = weak_shell.clone();
            Box::new(move |providers| {
                if let Some(shell) = shell.upgrade() {
                    shell.player_ui.update_lyrics_settings(
                        "lyrics provider setting",
                        false,
                        |settings| {
                            if settings.external_lyrics_providers == providers {
                                return false;
                            }
                            settings.external_lyrics_providers = providers;
                            true
                        },
                    );
                }
            })
        },
        key: Box::new(ExternalLyricsProvider::key),
        row: Box::new(move |provider| {
            let subtitle = match provider {
                ExternalLyricsProvider::Lrclib | ExternalLyricsProvider::SimpMusic => {
                    tr("Synced and plain lyrics")
                }
                ExternalLyricsProvider::Netease => tr("Synced lyrics, some word by word"),
                ExternalLyricsProvider::Genius => tr("Plain lyrics"),
            };
            (tr(provider.title()), subtitle)
        }),
    })
}

fn bind_lyrics(state: &Rc<Integrations>, builder: &gtk::Builder) {
    let resource = "preferences/integrations.ui";
    crate::objects!(builder, resource, {
        external_lyrics_row: adw::SwitchRow,
        prefer_server_row: adw::SwitchRow,
    });
    let shell = &state.shell;
    let settings = shell.settings.current.borrow().lyrics.preferences.clone();
    external_lyrics_row.set_active(settings.external_lyrics_enabled);
    prefer_server_row.set_active(settings.prefer_server_lyrics);
    prefer_server_row.set_sensitive(settings.external_lyrics_enabled);
    let player = Rc::downgrade(&shell.player_ui);
    prefer_server_row.connect_active_notify(move |row| {
        if let Some(player) = player.upgrade() {
            player.set_lyrics_setting("lyrics search setting", false, |settings| {
                settings.prefer_server_lyrics = row.is_active()
            });
        }
    });
    let player = Rc::downgrade(&shell.player_ui);
    let prefer_server = prefer_server_row.downgrade();
    let providers = Rc::downgrade(&state.lyrics);
    external_lyrics_row.connect_active_notify(move |row| {
        if let Some(player) = player.upgrade() {
            player.set_external_lyrics_enabled(row.is_active());
        }
        if let Some(prefer_server) = prefer_server.upgrade() {
            prefer_server.set_sensitive(row.is_active());
        }
        if let Some(providers) = providers.upgrade() {
            providers.populate();
        }
    });
}

fn bind_presence(shell: &Rc<Shell>, builder: &gtk::Builder) {
    let resource = "preferences/integrations.ui";
    crate::objects!(builder, resource, {
        presence_row: adw::SwitchRow,
        display_row: adw::ComboRow,
        discord_links_row: adw::SwitchRow,
        paused_row: adw::SwitchRow,
        listening_row: adw::SwitchRow,
        external_links_row: adw::SwitchRow,
        lastfm_links_row: adw::SwitchRow,
        musicbrainz_links_row: adw::SwitchRow,
        server_links_row: adw::SwitchRow,
    });
    let settings = shell.settings.current.borrow().clone();
    presence_row.set_active(settings.rich_presence.enabled);
    let presence_shell = Rc::clone(shell);
    presence_row.connect_active_notify(move |row| {
        presence_shell.settings.set_app_setting(
            "Discord presence setting",
            row.is_active(),
            |settings| &mut settings.rich_presence.enabled,
        );
        show_discord_flatpak_help(row);
    });
    let displays = [
        (DisplayType::Application, tr("Application name")),
        (DisplayType::Track, tr("Song title")),
        (DisplayType::Artist, tr("Artist name")),
    ];
    display_row.set_model(Some(&gtk::StringList::new(
        &displays
            .iter()
            .map(|(_, title)| title.as_str())
            .collect::<Vec<_>>(),
    )));
    display_row.set_selected(
        displays
            .iter()
            .position(|(display, _)| *display == settings.rich_presence.display_type)
            .unwrap_or(0) as u32,
    );
    let display_shell = Rc::clone(shell);
    display_row.connect_selected_notify(move |row| {
        let Some((display, _)) = displays.get(row.selected() as usize) else {
            return;
        };
        display_shell
            .settings
            .set_app_setting("Discord display setting", *display, |settings| {
                &mut settings.rich_presence.display_type
            });
    });
    shell.settings.bind_switch(
        &discord_links_row,
        "Discord link setting",
        |settings| &mut settings.rich_presence.links,
        |_| {},
    );
    shell.settings.bind_switch(
        &paused_row,
        "Discord paused setting",
        |settings| &mut settings.rich_presence.show_paused,
        |_| {},
    );
    shell.settings.bind_switch(
        &listening_row,
        "Discord activity type setting",
        |settings| &mut settings.rich_presence.show_as_listening,
        |_| {},
    );

    let links = settings.application.external_site_links.clone();
    let services = [lastfm_links_row, musicbrainz_links_row, server_links_row];
    for row in &services {
        row.set_sensitive(links.enabled);
    }
    external_links_row.set_active(links.enabled);
    let links_shell = Rc::clone(shell);
    let service_rows = services.clone().map(|row| row.downgrade());
    external_links_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        for service in &service_rows {
            if let Some(service) = service.upgrade() {
                service.set_sensitive(enabled);
            }
        }
        if links_shell
            .settings
            .set_app_setting("external site links setting", enabled, |settings| {
                &mut settings.application.external_site_links.enabled
            })
            .is_some()
        {
            links_shell.replace_current_route_when_ready();
        }
    });
    let fields: [(&str, fn(&mut Settings) -> &mut bool); 3] = [
        ("Last.fm site links setting", |settings| {
            &mut settings.application.external_site_links.lastfm
        }),
        ("MusicBrainz site links setting", |settings| {
            &mut settings.application.external_site_links.musicbrainz
        }),
        ("server site links setting", |settings| {
            &mut settings.application.external_site_links.server
        }),
    ];
    for (row, (action, field)) in services.iter().zip(fields) {
        let links_shell = Rc::clone(shell);
        shell.settings.bind_switch(row, action, field, move |_| {
            links_shell.replace_current_route_when_ready()
        });
    }
}

fn bind_folders(state: &Rc<Integrations>) {
    for (index, kind) in ["webdav", "smb"].into_iter().enumerate() {
        let (_, add) = &state.folder_groups[index];
        let weak = Rc::downgrade(state);
        add.connect_activated(move |row| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let refresh = Rc::downgrade(&state);
            super::source::login::present_integration(
                &state.shell,
                row,
                kind,
                None,
                Rc::new(move |_| {
                    if let Some(state) = refresh.upgrade() {
                        state.sync_folders();
                    }
                }),
            );
        });
    }
}
