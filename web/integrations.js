// Integrations, as GTK's page: the list of services beside the open one's page on wide
// windows; narrow windows open each page from the list, with Back.
import { api, browserHeaders } from "./connection.js";
import { listRows, set } from "./preferences.js";
import { openSource } from "./sources.js";
import { $, el, run, tr } from "./ui.js";

let values = {};
let providers = {};
let scrobbling = null;
let folders = [];
let folderStatus = {};
let selected = "lastfm";
// The server whose page is open.
let server = null;

const page = (id) => document.querySelector(`[data-integration-page="${id}"]`);
const hero = (id) => page(id).querySelector(".integration-hero");
const narrow = () => getComputedStyle(document.querySelector(".integrations")).getPropertyValue("--narrow").trim() === "1";

function integrationOpen() {
  return $("preferences-dialog").classList.contains("integration-open");
}

function closeIntegration() {
  if (!integrationOpen()) return;
  $("preferences-dialog").classList.remove("integration-open");
  $("preferences-page-title").textContent = tr("Integrations");
}

function rowOf(id) {
  return document.querySelector(`.integration-row[data-integration="${CSS.escape(id)}"]`);
}

function open(id) {
  selected = id;
  server = scrobbling?.servers.find((item) => item.id === id) || null;
  const shownPage = server ? "server" : id;
  for (const node of document.querySelectorAll("[data-integration-page]")) node.hidden = node.dataset.integrationPage !== shownPage;
  for (const row of document.querySelectorAll(".integration-row"))
    row.setAttribute("aria-current", String(row.dataset.integration === id));
  if (server) showServer();
  if (narrow()) {
    $("preferences-dialog").classList.add("integration-open");
    $("preferences-page-title").textContent = rowOf(id).querySelector(".pref-label > span").textContent;
  }
}

function showRow(id, status, active) {
  const row = rowOf(id);
  if (!row) return;
  row.querySelector("[data-integration-status]").textContent = status;
  row.querySelector(".integration-active").hidden = !active;
}

const accountStatus = (connected, username) =>
  !connected ? tr("Not connected") : username.trim() ? username.trim() : tr("Connected");
const connectionSubtitle = (connected, username) =>
  !connected ? tr("Not connected") : username.trim() ? tr("Connected as {username}", { username: username.trim() }) : tr("Connected");
const protocolTitle = (protocol) => (protocol === "audioscrobbler" ? tr("Audioscrobbler API") : tr("ListenBrainz API"));

// An account's hero: Connect is the primary action until it is connected.
function showAccount(node, connected, status) {
  const action = node.querySelector("[data-hero-action]");
  if (action.disabled) return;
  node.querySelector("[data-hero-status]").textContent = status;
  action.textContent = connected ? tr("Reconnect") : tr("Connect");
  action.classList.toggle("primary", !connected);
}

function showSwitches(container, account) {
  for (const input of container.querySelectorAll("[data-scrobbling-switch]"))
    input.checked = input.dataset.scrobblingSwitch === "enabled" ? account.enabled : account.now_playing;
}

// Every row's status and the pages that read saved state.
function showStatus() {
  if (scrobbling) {
    for (const id of ["lastfm", "listenbrainz", "librefm"]) {
      const account = scrobbling[id];
      showAccount(hero(id), account.connected, connectionSubtitle(account.connected, account.username));
      showRow(id, accountStatus(account.connected, account.username), account.connected && account.enabled);
      showSwitches(page(id), account);
    }
    showServers();
  }
  showRow("add_server", tr("ListenBrainz or Audioscrobbler API"), false);
  const metadata = values["/external_metadata_enabled"] !== false;
  showRow("metadata", metadata ? tr("Covers, artist images, tags") : tr("Off"), metadata);
  const lyrics = values["/external_lyrics_enabled"] !== false;
  const lyricsProviders = (values["/external_lyrics_providers"] || [])
    .map((key) => providers["/external_lyrics_providers"]?.find((provider) => provider.key === key)?.title)
    .filter(Boolean);
  showRow("lyrics", lyrics ? lyricsProviders.join(", ") : tr("Off"), lyrics);
  const presence = values["/discord_presence_enabled"] === true;
  showRow("discord", presence ? tr("Rich Presence on") : tr("Off"), presence);
  const links = values["/external_site_links/enabled"] !== false;
  const services = [["/external_site_links/lastfm", "Last.fm"], ["/external_site_links/musicbrainz", "MusicBrainz"], ["/external_site_links/server", tr("Server")]]
    .filter(([path]) => values[path]).map(([, name]) => name);
  showRow("links", links ? services.join(", ") : tr("Off"), links);
  for (const kind of ["webdav", "smb"])
    showRow(kind, folderStatus[kind] || "", folders.some((item) => (item.kind === "smb") === (kind === "smb")));
}

// One row per added server, before Add server.
function showServers() {
  const list = $("integrations-scrobbling");
  for (const row of list.querySelectorAll("[data-server]")) if (!scrobbling.servers.some((item) => item.id === row.dataset.integration)) row.remove();
  for (const item of scrobbling.servers) {
    let row = rowOf(item.id);
    if (!row) {
      row = rowOf("add_server").cloneNode(true);
      row.dataset.integration = item.id;
      row.dataset.server = "";
      row.addEventListener("click", () => open(item.id));
      list.insertBefore(row, rowOf("add_server"));
    }
    row.querySelector(".pref-label > span").textContent = item.name;
    showRow(item.id, `${accountStatus(item.connected, item.username)} · ${protocolTitle(item.protocol)}`, item.connected && item.enabled);
  }
  // A removed server's page gives way to Last.fm's.
  if (server) {
    server = scrobbling.servers.find((item) => item.id === server.id) || null;
    if (server) showServer();
    else open("lastfm");
  }
}

function showServer() {
  const node = page("server");
  node.dataset.account = server.id;
  node.querySelector("[data-hero-name]").textContent = server.name;
  const action = hero("server").querySelector("[data-hero-action]");
  showAccount(hero("server"), server.connected, `${connectionSubtitle(server.connected, server.username)} · ${protocolTitle(server.protocol)}`);
  $("server-protocol").textContent = protocolTitle(server.protocol);
  $("server-url").textContent = server.url;
  $("server-token-row").hidden = server.protocol !== "listenbrainz";
  // An Audioscrobbler server signs in again with its username and password once its session stops working.
  const audioscrobbler = server.protocol === "audioscrobbler";
  if (audioscrobbler && !action.disabled) {
    action.textContent = tr("Sign in again");
    action.hidden = server.connected;
  } else action.hidden = false;
  for (const row of node.querySelectorAll("[data-sign-in]")) row.hidden = !audioscrobbler || server.connected;
  if (document.activeElement !== $("server-username")) $("server-username").value = server.username;
  showSwitches(node, server);
}

// The providers of an ordered list: the shown ones in their order, then the hidden ones.
function showProviders() {
  for (const host of document.querySelectorAll("[data-providers]")) {
    const path = host.dataset.providers;
    const all = providers[path] || [];
    const shown = (values[path] || []).filter((key) => all.some((provider) => provider.key === key));
    const rows = [...shown.map((key) => all.find((provider) => provider.key === key)), ...all.filter((provider) => !shown.includes(provider.key))];
    const write = (next) => set(path, next);
    listRows(host, rows.map((provider, index) => {
      const visible = index < shown.length;
      return { ...provider, visible, up: visible && index > 0, down: visible && index + 1 < shown.length };
    }), (entry, change) => {
      const next = shown.filter((key) => key !== (change.source || entry.key));
      if ("visible" in change) write(change.visible ? [...next, entry.key] : next);
      else if (change.delta) {
        const index = shown.indexOf(entry.key);
        if (index < 0) return;
        next.splice(Math.min(Math.max(index + change.delta, 0), shown.length - 1), 0, entry.key);
        write(next);
      } else if (shown.includes(change.source) && shown.includes(entry.key)) {
        next.splice(next.indexOf(entry.key) + (change.after ? 1 : 0), 0, change.source);
        write(next);
      }
    });
    const sensitive = values[host.dataset.enabledBy] !== false;
    for (const row of host.children) {
      row.classList.toggle("insensitive", !sensitive);
      for (const input of row.querySelectorAll("input, button")) if (!sensitive) input.disabled = true;
      row.draggable = sensitive;
    }
  }
}

function showFolders() {
  for (const host of document.querySelectorAll("[data-folders]")) {
    const kind = host.dataset.folders;
    host.replaceChildren(...folders.filter((item) => (item.kind === "smb") === (kind === "smb")).map((item) => {
      const row = el("button", "pref-row pref-link");
      row.type = "button";
      const label = el("span", "pref-label");
      label.append(el("span", "", item.name));
      row.append(label);
      row.addEventListener("click", () => openSource(item, { kind: item.kind, saved: () => run(loadFolders) }));
      return row;
    }));
  }
}

async function loadFolders() {
  [folders, folderStatus] = await Promise.all([api("/integrations/files"), api("/integrations/files/status")]);
  showFolders();
  showStatus();
}

async function loadScrobbling() {
  showScrobbling(await api("/scrobbling"));
}

function showScrobbling(preferences) {
  const first = !scrobbling;
  scrobbling = preferences;
  // Saved keys fill the fields; typed ones stay until they are saved.
  for (const [id, value] of [["lastfm-api-key", preferences.lastfm.api_key], ["lastfm-api-secret", preferences.lastfm.api_secret], ["listenbrainz-token", preferences.listenbrainz.user_token]])
    if (first || document.activeElement !== $(id)) $(id).value = value;
  showStatus();
}

// The Preferences values reached the page: what Integrations shows from them.
function showIntegrations(preferences) {
  values = preferences.values;
  providers = preferences.providers;
  showProviders();
  showStatus();
}

const scrobblingEdit = (edit) => api("/scrobbling", "POST", edit).then(showScrobbling);

// Reads the sign-in events: the page to open, then the account or why it failed.
async function connectionEvents(body, receive) {
  const response = await fetch("/api/scrobbling/connect", {
    method: "POST",
    headers: { ...browserHeaders(), "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error((await response.json()).error || tr("Request failed ({status})", { status: response.status }));
  const reader = response.body.getReader(), decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) return;
    buffer += decoder.decode(value, { stream: true });
    let end;
    while ((end = buffer.indexOf("\n\n")) >= 0) {
      const data = buffer.slice(0, end).split("\n").find((line) => line.startsWith("data:"));
      buffer = buffer.slice(end + 2);
      if (data) receive(JSON.parse(data.slice(5)));
    }
  }
}

// The account opens its sign-in page in a new tab and waits for it. The tab opens with the
// click, so the browser allows it, and takes the page once the server names it.
async function connectBrowser(node, body, opening) {
  const action = node.querySelector("[data-hero-action]");
  const status = node.querySelector("[data-hero-status]");
  const tab = window.open("", "_blank");
  if (tab) tab.opener = null;
  action.disabled = true;
  status.textContent = opening;
  let connected = false;
  try {
    await connectionEvents(body, (event) => {
      if (event.open) {
        if (tab) tab.location = event.open;
        else status.textContent = tr("Couldn't open the sign-in page: {error}", { error: event.open });
      } else if (event.connected !== undefined) {
        connected = true;
      } else if (event.error) {
        tab?.close();
        status.textContent = event.error;
      }
    });
  } catch (error) {
    tab?.close();
    status.textContent = error.message;
  } finally {
    action.disabled = false;
  }
  // A failure stays on the hero until the next attempt.
  if (connected) await loadScrobbling();
}

async function connectListenbrainz(node, token) {
  const action = node.querySelector("[data-hero-action]");
  const status = node.querySelector("[data-hero-status]");
  if (!token.trim()) {
    status.textContent = tr("Enter your user token first");
    return;
  }
  action.disabled = true;
  status.textContent = tr("Checking the user token...");
  try {
    await scrobblingEdit({ operation: "connect_listenbrainz", user_token: token.trim() });
    action.disabled = false;
    showStatus();
  } catch (error) {
    status.textContent = error.message;
  } finally {
    action.disabled = false;
  }
}

// An added server signs in again: with its user token, or its username and password.
async function signInServer() {
  const node = hero("server");
  const action = node.querySelector("[data-hero-action]");
  const status = node.querySelector("[data-hero-status]");
  const listenbrainz = server.protocol === "listenbrainz";
  const request = {
    operation: "sign_in_server",
    id: server.id,
    username: listenbrainz ? "" : $("server-username").value.trim(),
    secret: listenbrainz ? $("server-token").value.trim() : $("server-password").value,
  };
  action.disabled = true;
  status.textContent = tr("Signing in...");
  try {
    await scrobblingEdit(request);
  } catch (error) {
    status.textContent = error.message;
  } finally {
    action.disabled = false;
    $("server-password").value = "";
  }
  if (server) showServer();
}

function showProtocol() {
  const protocol = document.querySelector('[name="add-server-protocol"]:checked').value;
  for (const node of page("add_server").querySelectorAll("[data-protocol]")) node.hidden = node.dataset.protocol !== protocol;
}

async function addServer() {
  const node = hero("add_server");
  const status = node.querySelector("[data-hero-status]");
  const action = $("add-server-connect");
  const protocol = document.querySelector('[name="add-server-protocol"]:checked').value;
  const request = {
    operation: "add_server",
    protocol,
    name: $("add-server-name").value.trim(),
    url: $("add-server-url").value.trim(),
    username: protocol === "listenbrainz" ? "" : $("add-server-username").value.trim(),
    secret: protocol === "listenbrainz" ? $("add-server-token").value.trim() : $("add-server-password").value,
  };
  if (!request.name || !request.url) {
    status.textContent = tr("Enter a name and URL.");
    return;
  }
  action.disabled = true;
  status.textContent = tr("Signing in...");
  try {
    await scrobblingEdit(request);
  } catch (error) {
    status.textContent = error.message;
    return;
  } finally {
    action.disabled = false;
    $("add-server-password").value = "";
  }
  status.textContent = tr("Self-hosted scrobbling");
  for (const id of ["add-server-name", "add-server-url", "add-server-token", "add-server-username"]) $(id).value = "";
  const added = scrobbling.servers.at(-1);
  if (added) open(added.id);
}

function init() {
  for (const row of document.querySelectorAll(".integration-row")) row.addEventListener("click", () => open(row.dataset.integration));
  for (const input of document.querySelectorAll("[data-scrobbling-switch]"))
    input.addEventListener("change", () => run(() => scrobblingEdit({
      operation: "switch",
      account: input.closest("[data-account]").dataset.account,
      switch: input.dataset.scrobblingSwitch,
      value: input.checked,
    })));
  for (const [id, field] of [["lastfm-api-key", "api_key"], ["lastfm-api-secret", "api_secret"]])
    $(id).addEventListener("change", () => run(() => scrobblingEdit({ operation: "credential", [field]: $(id).value.trim() })));
  hero("lastfm").querySelector("[data-hero-action]").addEventListener("click", () => {
    const api_key = $("lastfm-api-key").value.trim(), api_secret = $("lastfm-api-secret").value.trim();
    run(() => connectBrowser(hero("lastfm"), { service: "lastfm", api_key, api_secret }, tr("Opening Last.fm authorization...")));
  });
  hero("librefm").querySelector("[data-hero-action]").addEventListener("click", () =>
    run(() => connectBrowser(hero("librefm"), { service: "librefm" }, tr("Opening Libre.fm authorization..."))));
  const listenbrainz = () => run(() => connectListenbrainz(hero("listenbrainz"), $("listenbrainz-token").value));
  hero("listenbrainz").querySelector("[data-hero-action]").addEventListener("click", listenbrainz);
  $("listenbrainz-token").addEventListener("keydown", (event) => event.key === "Enter" && listenbrainz());
  const connectServer = () => server && run(signInServer);
  hero("server").querySelector("[data-hero-action]").addEventListener("click", connectServer);
  for (const id of ["server-token", "server-password"]) $(id).addEventListener("keydown", (event) => event.key === "Enter" && connectServer());
  $("server-remove").addEventListener("click", (event) => {
    const button = event.currentTarget;
    button.disabled = true;
    run(async () => {
      try {
        await scrobblingEdit({ operation: "remove_server", id: server.id });
      } finally {
        button.disabled = false;
      }
    });
  });
  $("add-server-connect").addEventListener("click", () => run(addServer));
  for (const input of document.querySelectorAll('[name="add-server-protocol"]')) input.addEventListener("change", showProtocol);
  for (const node of document.querySelectorAll("[data-file-connection]"))
    node.addEventListener("click", () => openSource(null, { kind: node.dataset.fileConnection, saved: () => run(loadFolders) }));
  // Opening Integrations reads the accounts and connections again.
  document.querySelector('.pref-tab[data-page="integrations"]').addEventListener("click", () => {
    open(selected);
    closeIntegration();
    run(loadScrobbling);
    run(loadFolders);
  });
}

export { closeIntegration, init, integrationOpen, showIntegrations };
