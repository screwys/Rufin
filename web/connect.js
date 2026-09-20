import { chooseFileIntegration } from "./sources.js";
import { api } from "./connection.js";
import { tr, notice } from "./ui.js";

const element = (id) => document.getElementById(`connect-${id}`);
const replacementMessage =
  "Connecting an existing profile will remove all data in this device, do you really want to continue?";
const confirmation = () =>
  window.confirm(
    tr(
      "Connecting an existing profile will remove all data in this device, do you really want to continue?",
    ),
  );
let state;
let offeredDevice;
let toastDevice;
let previousDevices;
let mode = "invite";
let applyingStorage = false;
let storageConnection = null;
let storageKind = "";

function updateView() {
  if (!state) return;
  const busy = !!state.pairing || state.connecting;
  const established = !!state.settings.established;
  const setup = !!state.settings.setup_pending;
  const joining = mode === "join" && !established;
  const importing = joining && element("join-method").value === "file";
  const profileControls = established && !busy;
  const visible = {
    start: true,
    join: joining && !busy,
    invite: !joining && !busy && !setup,
    file: (profileControls || importing) && !busy,
    verify: !!state.pairing,
    connecting: state.connecting,
    devices: !busy && !setup && state.devices.some(device => device.enrolled),
    nearby: joining && !busy && !importing && state.devices.some(device => !device.enrolled),
    network: !busy && (profileControls || (joining && !importing)),
    media: profileControls,
  };
  for (const section of document.querySelectorAll("[data-connect-section]")) {
    if (section.dataset.connectSection in visible)
      section.hidden = !visible[section.dataset.connectSection];
  }
  for (const button of document.querySelectorAll("[data-connect-mode]"))
    button.setAttribute("aria-pressed", String(button.dataset.connectMode === mode));
  element("peer-controls").hidden = importing;
  element("choices").hidden = established || busy;
  element("finish-setup").hidden = !setup || busy;
  element("setup-title").hidden = !setup || busy;
  element("import").hidden = !importing;
  element("save-storage").hidden = importing;
  element("file").placeholder = importing ? tr("Connect file") : tr("Connect folder");
  element("file").setAttribute("aria-label", element("file").placeholder);
  element("leave").hidden = !profileControls;
  element("approve").hidden = !!state.pairing?.approved;
  element("approval-status").hidden = !state.pairing?.approved;
  element("approval-status").textContent = state.pairing?.verified
    ? tr("Connecting devices") : tr("Waiting for the other device to approve");
}

function selectMethod() {
  if (mode === "join" && element("join-method").value === "file") element("file").value = "";
  else run(showDestination);
  updateView();
}

export async function offerContinuation() {
  const last = Number(sessionStorage.getItem("rufin-connect-active") || 0);
  sessionStorage.setItem("rufin-connect-active", String(Date.now()));
  if (Date.now() - last < 300000) return;
  try {
    const device = await refreshContinuation();
    const offer = element("offer");
    if (device && !sessionStorage.getItem("rufin-connect-offered")) {
      sessionStorage.setItem("rufin-connect-offered", "true");
      offer.hidden = false;
      toastDevice = device;
      element("offer-text").textContent = tr("Continue from where {device} left off?", {
        device: device.name,
      });
    }
  } catch {
    element("offer").hidden = true;
  }
}

async function refreshContinuation() {
  const device = await api("/connect/continuation");
  offeredDevice = device;
  const control = element("continue");
  control.hidden = !device;
  if (device) {
    const title = tr("Continue from where {device} left off", { device: device.name });
    control.title = title;
    control.setAttribute("aria-label", title);
  }
  return device;
}

async function action(value) {
  try {
    show(await api("/connect", "POST", value));
  } catch (error) {
    if (
      value.action === "import" &&
      !value.replace &&
      error.message === replacementMessage
    ) {
      if (confirmation()) await action({ ...value, replace: true });
      return;
    }
    throw error;
  }
}
function run(callback) {
  return Promise.resolve().then(callback).catch((error) => notice(error.message));
}
function click(id, callback) {
  element(id).addEventListener("click", () => run(callback));
}
function button(title, callback, className = "") {
  const node = document.createElement("button");
  node.textContent = title;
  node.className = className;
  node.addEventListener("click", () => run(callback));
  return node;
}
function flashSuccess(button, original) {
  const icon = button.querySelector(".icon");
  icon.dataset.icon = "check";
  setTimeout(() => {
    icon.dataset.icon = original;
  }, 1500);
}
function textNode(tag, text, className = "") {
  const node = document.createElement(tag);
  node.textContent = text;
  node.className = className;
  return node;
}
function show(value) {
  const pairingChanged = value.pairing?.session !== state?.pairing?.session;
  const profileChanged = state && state.settings.profile !== value.settings.profile;
  const destinationChanged = state && JSON.stringify(state.settings.destination) !== JSON.stringify(value.settings.destination);
  const networkChanged = state && (state.settings.nearby !== value.settings.nearby || state.settings.relay !== value.settings.relay || state.settings.public_relay !== value.settings.public_relay);
  const completed = state && state.completed_pairings !== value.completed_pairings;
  const setupReady = value.settings.setup_pending && !value.connecting &&
    (!state?.settings.setup_pending || state.connecting);
  state = value;
  if (networkChanged) {
    element("nearby").checked = value.settings.nearby;
    element("relay").value = value.settings.relay || (value.settings.public_relay ? "https://euc1-1.relay.n0.iroh.link/" : "");
    element("relay-enabled").checked = Boolean(value.settings.relay || value.settings.public_relay);
    showRelay();
  }
  if (completed) {
    mode = "invite";
    element("progress").hidden = !value.receiving_collection;
    element("dialog").close();
    if (!value.receiving_collection) notice(tr("Connected"));
  }
  if (!element("progress").hidden) {
    element("progress-text").textContent = tr(value.profile_status);
    if (!value.receiving_collection) {
      element("progress").hidden = true;
      notice(tr(value.profile_status));
    }
  }
  if (element("dialog").open && element("import").hidden &&
      (destinationChanged || (value.settings.destination?.source_id && element("storage").selectedIndex < 0))) run(showDestination);
  element("connecting-status").textContent = tr(value.profile_status);
  element("enabled").checked = value.settings.enabled;
  element("media-status").textContent = value.media_status ? tr(value.media_status) : "";
  element("media-status").hidden = !value.media_status;
  element("error").textContent = value.error || "";
  element("error").hidden = !value.error;
  element("invitation").value = value.invitation || "";
  element("share-invitation").hidden = !value.invitation;
  element("copy").hidden = !value.invitation;
  element("leave").hidden = !value.settings.profile;
  if (value.pairing && pairingChanged) {
    element("verification-name").textContent = value.pairing.name;
    element("emoji").replaceChildren(
      ...value.pairing.emoji.map((emoji) => textNode("span", emoji)),
    );
  }
  updateView();
  updateStorage();
  if (value.pairing && pairingChanged && !element("dialog").open) present(value);
  if ((profileChanged || setupReady) && element("dialog").open) run(loadFolders);
  const signature = JSON.stringify(value.devices.map(device => [device.id, device.enrolled]));
  for (const card of element("dialog").querySelectorAll("[data-device-id]")) {
    const device = value.devices.find(device => device.id === card.dataset.deviceId);
    if (!device) continue;
    card.querySelector("h4").textContent = device.name;
    const route = card.querySelector("p");
    if (route) {
      route.textContent = tr(device.connection);
      route.classList.toggle("reachable", device.reachable);
    }
  }
  if (signature === previousDevices) return;
  previousDevices = signature;
  const devices = element("devices");
  const nearby = element("nearby-devices");
  devices.replaceChildren();
  nearby.replaceChildren();
  for (const device of value.devices) {
    const card = document.createElement("article");
    card.className = "connect-card";
    card.dataset.deviceId = device.id;
    const heading = document.createElement("div");
    heading.className = "connect-section-heading";
    heading.append(textNode("h4", device.name));
    card.append(heading);
    if (!device.enrolled) {
      heading.append(
        button(tr("Join profile"), () => join(device.id), "primary"),
      );
      nearby.append(card);
      continue;
    }
    const route = textNode("p", tr(device.connection), "connect-device-status");
    route.classList.toggle("reachable", device.reachable);
    card.append(route);
    const controls = document.createElement("div");
    controls.className = "connect-device-controls";
    for (const [label, command] of [
      [tr("Play"), "play"],
      [tr("Pause"), "pause"],
      [tr("Next"), "next"],
      [tr("Stop"), "stop"],
    ]) {
      controls.append(button(label, () =>
        action({ action: "control", peer: device.id, command: { command } }),
      ));
    }
    card.append(controls);
    const options = document.createElement("div");
    options.className = "connect-actions";
    const test = button(tr("Test connection"), async () => {
      try {
        await action({ action: "test_connection", peer: device.id });
        flashSuccess(test, "refresh");
      } catch {
        notice(tr("Connection test has failed"));
      }
    }, "icon-button");
    test.title = tr("Test connection");
    test.setAttribute("aria-label", test.title);
    const testIcon = document.createElement("span");
    testIcon.className = "icon";
    testIcon.dataset.icon = "refresh";
    testIcon.setAttribute("aria-hidden", "true");
    test.replaceChildren(testIcon);
    options.append(
      test,
      button(tr("Remove device"), () => action({ action: "remove", peer: device.id }), "connect-destructive"),
    );
    card.append(options);
    devices.append(card);
  }
}

async function join(invitation) {
  if (!confirmation()) return;
  await action({ action: "join", invitation, replace: true });
}

async function showDestination() {
  const destination = state.settings.destination;
  storageConnection = destination?.source_id || null;
  const connections = await api("/integrations/files");
  storageKind = connections.find((item) => item.id === storageConnection)?.kind || (storageConnection ? "webdav" : "");
  element("storage").value = storageKind;
  element("file").value = storagePath(storageConnection);
  await showStorageConnection(connections);
  updateStorage();
}

async function showStorageConnection(connections = null) {
  connections ||= await api("/integrations/files");
  const selected = connections.find((item) => item.id === storageConnection);
  element("storage-connection").textContent = selected?.name || tr("Choose connection");
  element("storage-connection").hidden = !element("storage").value;
}

function chooseStorageConnection(kind = storageKind) {
  return chooseFileIntegration(kind, async (id) => {
    storageKind = kind;
    element("storage").value = kind;
    storageConnection = id;
    element("file").value = storagePath(id);
    await showStorageConnection();
    updateStorage();
  });
}

function showRelay() {
  const enabled = element("relay-enabled").checked;
  element("relay").hidden = !enabled;
}

function storagePath(source) {
  if (!element("import").hidden) return "";
  const saved = state.settings.destination;
  if ((saved?.source_id || null) === source && saved?.path) return saved.path;
  return source ? "Rufin Connect" : state.local_folder || "";
}

function updateStorage() {
  const source = fileSource();
  const path = element("file").value;
  const saved = state?.settings.destination;
  element("storage-help").hidden = !source || !element("import").hidden;
  element("local-storage-help").hidden = !!source || !element("import").hidden;
  element("save-storage").disabled = applyingStorage || element("storage").selectedIndex < 0 || (element("storage").value && !source) || !path ||
    ((saved?.source_id || null) === source && saved?.path === path);
}

async function loadFolders() {
  const profile = state.settings.profile;
  const configured = await api("/sources");
  const folders = [];
  for (const source of configured.sources || []) {
    const roots = await api(`/connect/roots?source=${encodeURIComponent(source.id)}`);
    if (state.settings.profile !== profile) return;
    for (const root of roots) folders.push({ source, root });
  }
  element("folder-section").hidden = folders.length === 0;
  const list = element("folders");
  list.replaceChildren();
  for (const { source, root } of folders) {
    const card = document.createElement("div");
    card.className = "connect-inline";
    const label = document.createElement("label");
    label.textContent = `${source.name} · ${root.label}`;
    const input = document.createElement("input");
    input.value = state.settings.folders[`${source.id}/${root.id}`] || state.settings.folders[source.id] || "";
    input.spellcheck = false;
    const inputId = `connect-folder-${list.childElementCount}`;
    input.id = inputId;
    label.htmlFor = inputId;
    card.append(label, input);
    card.append(button(tr("Use folder"), () => action({
      action: "folder", source: source.id, root_id: root.id, path: input.value,
    })));
    list.append(card);
  }
}

function fileSource() {
  return storageConnection;
}

function present(value) {
  element("name").value = value.settings.name;
  element("nearby").checked = value.settings.nearby;
  element("relay").value = value.settings.relay || (value.settings.public_relay ? "https://euc1-1.relay.n0.iroh.link/" : "");
  element("relay-enabled").checked = Boolean(value.settings.relay || value.settings.public_relay);
  showRelay();
  element("encoding").value = value.settings.encoding;
  updateView();
  element("dialog").showModal();
  if (!value.pairing && !value.connecting) {
    run(showDestination);
    run(loadFolders);
    run(refreshContinuation);
  }
}

export function watch(signal) {
  const events = new EventSource("/api/connect/events");
  events.addEventListener("connect", (event) => show(JSON.parse(event.data)));
  signal.addEventListener("abort", () => events.close(), { once: true });
}

export function init() {
  for (const button of document.querySelectorAll("[data-connect-mode]")) {
    button.addEventListener("click", () => {
      mode = button.dataset.connectMode;
      selectMethod();
      if (mode === "join") run(() => action({ action: "discover" }));
      else if (!state.settings.enabled) run(() => action({ action: "enable", enabled: true }));
    });
  }
  element("join-method").addEventListener("change", selectMethod);
  element("storage").addEventListener("change", () => {
    const kind = element("storage").value;
    if (kind) {
      element("storage").value = storageKind;
      run(() => chooseStorageConnection(kind));
    } else {
      storageKind = "";
      storageConnection = null;
      element("file").value = storagePath(null);
      element("storage-connection").hidden = true;
      updateStorage();
    }
  });
  click("storage-connection", () => chooseStorageConnection());
  element("file").addEventListener("input", updateStorage);
  const continuePlayback = async (device) => {
    element("offer").hidden = true;
    if (device) await action({ action: "continue", peer: device.id });
    await refreshContinuation();
  };
  click("offer-accept", () => continuePlayback(toastDevice));
  click("continue", () => continuePlayback(offeredDevice));
  click("offer-dismiss", () => { element("offer").hidden = true; });
  click("progress-open", () => document.getElementById("open-connect").click());
  click("progress-dismiss", () => {
    element("progress").hidden = true;
  });
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      sessionStorage.setItem("rufin-connect-active", String(Date.now()));
    } else {
      offerContinuation();
    }
  });
  document.getElementById("open-connect").addEventListener("click", () => run(async () => {
    const value = await api("/connect");
    show(value);
    present(value);
  }));
  element("enabled").addEventListener("change", () => run(async () => {
    const enabled = element("enabled").checked;
    try {
      await action({ action: "enable", enabled });
    } catch (error) {
      element("enabled").checked = state.settings.enabled;
      throw error;
    }
  }));
  click("refresh", async () => {
    await action({ action: "refresh" });
    await loadFolders();
  });
  click("copy", async () => {
    const field = element("invitation");
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(field.value);
      flashSuccess(element("copy"), "copy");
    } else {
      field.hidden = false;
      field.focus();
      field.select();
    }
  });
  click("rename", () => action({ action: "rename", name: element("name").value }));
  click("join", () => join(element("peer").value));
  click("cancel-pairing", () => action({ action: "cancel_pairing" }));
  click("finish-setup", () => action({ action: "finish_setup" }));
  click("approve", async () => {
    await action({ action: "pair", session: state.pairing.session, approve: true });
  });
  click("reject", () => action({ action: "pair", session: state.pairing.session, approve: false }));
  element("relay-enabled").addEventListener("change", showRelay);
  click("network", () => action({
    action: "network", nearby: element("nearby").checked,
    relay: element("relay-enabled").checked ? element("relay").value.trim() : null,
    public_relay: false,
  }));
  element("encoding").addEventListener("change", () => run(async () => {
    const encoding = element("encoding").value;
    if (encoding === state.settings.encoding) return;
    const confirm = state.settings.established && !state.settings.setup_pending;
    if (confirm && !window.confirm(tr("Changing this will redownload all local files."))) {
      element("encoding").value = state.settings.encoding;
      return;
    }
    await action({ action: "encoding", encoding, confirm });
    element("encoding").value = state.settings.encoding;
  }));
  click("import", () => action({
    action: "import", path: element("file").value, source_id: fileSource(), replace: false, key: null,
  }));
  click("save-storage", async () => {
    const button = element("save-storage");
    applyingStorage = true;
    button.textContent = tr("Applying...");
    updateStorage();
    try {
      await action({
        action: "destination",
        destination: fileSource()
            ? { kind: "web_dav", source_id: fileSource(), path: element("file").value }
            : { kind: "local", path: element("file").value },
      });
      button.textContent = tr("Applied");
      await new Promise((resolve) => setTimeout(resolve, 2000));
    } finally {
      applyingStorage = false;
      button.textContent = tr("Apply");
      updateStorage();
    }
  });
  click("leave", async () => {
    if (!window.confirm(tr("Disconnect this device?"))) return;
    await action({ action: "leave" });
    updateView();
  });
}
