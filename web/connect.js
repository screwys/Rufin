import { chooseFileIntegration } from "./sources.js";
import { api } from "./connection.js";
import { icon, setIcon, tr, notice } from "./ui.js";

const element = (id) => document.getElementById(`connect-${id}`);
const progressInteger = new Intl.NumberFormat();
const confirmation = () =>
  window.confirm(
    tr(
      "Connecting an existing profile will remove all data in this device, do you really want to continue?",
    ),
  );
let state;
let offered;
let offeredContinuation;
let previousDevices;
let mode = "invite";
let storageConnection = null;
let storageKind = "";

function updateView() {
  if (!state) return;
  const working = state.connecting || !!state.collection_progress;
  const busy = !!state.pairing || working;
  const progressVisible = working && !state.pairing && !state.error;
  const hasProfile = !!state.settings.profile;
  const joining = mode === "join" && !hasProfile;
  const profileControls = hasProfile && !busy;
  const visible = {
    start: !progressVisible,
    join: joining && !busy,
    invite: !joining && !busy,
    verify: !!state.pairing,
    connecting: progressVisible,
    devices: !busy && state.devices.some(device => device.enrolled),
    nearby: !busy && state.devices.some(device => !device.enrolled),
    network: !busy && (profileControls || joining),
    folder: profileControls,
  };
  for (const section of document.querySelectorAll("[data-connect-section]")) {
    if (section.dataset.connectSection in visible)
      section.hidden = !visible[section.dataset.connectSection];
  }
  for (const button of document.querySelectorAll("[data-connect-mode]"))
    button.setAttribute("aria-pressed", String(button.dataset.connectMode === mode));
  element("dialog").classList.toggle("connect-working", progressVisible);
  element("cancel-pairing").hidden = !state.connecting;
  element("choices").hidden = hasProfile || busy;
  element("retry").hidden = !state.error || !state.settings.profile || state.connecting || !!state.pairing;
  element("leave").hidden = !profileControls || !state.devices.some((device) => device.enrolled);
  element("approve").hidden = !!state.pairing?.approved;
  element("approval-status").hidden = !state.pairing?.approved;
  element("approval-status").textContent = state.pairing?.verified
    ? tr("Connecting devices") : tr("Waiting for the other device to approve");
}

function showCollectionProgress(value) {
  element("connecting-status").textContent = value.profile_status;
  const progress = value.collection_progress;
  const bar = element("collection-progress");
  const total = progress?.total > 0 ? progress.total : null;
  const fraction = total === null ? null : Math.min(1, Math.max(0, progress.completed / total));
  if (fraction === null) bar.removeAttribute("value");
  else bar.value = fraction;
  let counter = "";
  if (progress && total !== null) {
    const current = progressInteger.format(progress.completed);
    const maximum = progressInteger.format(total);
    const percent = progressInteger.format(Math.floor(Math.min(progress.completed, total) * 100 / total));
    counter = `${current} / ${maximum} (${percent}%)`;
  } else if (progress?.completed > 0) {
    counter = progressInteger.format(progress.completed);
  }
  element("collection-counter").textContent = counter;
  element("collection-counter").hidden = !counter;
  return counter;
}

export async function refreshContinuation() {
  const continuation = state?.continuation;
  offeredContinuation = continuation;
  const control = element("continue");
  control.hidden = !continuation;
  const offer = continuation && `${continuation.device}:${continuation.at}`;
  if (continuation) {
    const title = tr("Continue from {device}", { device: continuation.name });
    control.textContent = title;
    if (offer !== offered) notice(title);
  }
  offered = offer;
  return continuation;
}

async function action(value) {
  try {
    show(await api("/connect", "POST", value));
  } catch (error) {
    if (value.action === "pair" && !value.replace && error.confirmation === "replace_profile") {
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
  const node = button.querySelector(".icon");
  setIcon(node, "rufin-object-select-symbolic");
  setTimeout(() => setIcon(node, original), 1500);
}
function textNode(tag, text, className = "") {
  const node = document.createElement(tag);
  node.textContent = text;
  node.className = className;
  return node;
}
function show(value) {
  const pairingChanged = value.pairing?.session !== state?.pairing?.session;
  const networkChanged = state && (state.settings.nearby !== value.settings.nearby || state.settings.relay !== value.settings.relay || state.settings.public_relay !== value.settings.public_relay);
  const completed = state && state.completed_pairings !== value.completed_pairings;
  const folderChanged = JSON.stringify(state?.settings.folder) !== JSON.stringify(value.settings.folder);
  state = value;
  if (folderChanged) run(() => showFolder(value.settings.folder));
  const counter = showCollectionProgress(value);
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
    element("progress-text").textContent = value.error || [value.profile_status, counter].filter(Boolean).join(" · ");
    if (!value.receiving_collection) {
      element("progress").hidden = true;
      notice(value.error || tr(value.profile_status));
    }
  }
  element("enabled").checked = value.settings.enabled;
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
  if (value.pairing && pairingChanged && !element("dialog").open) present(value);
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
      const actions = document.createElement("div");
      actions.className = "connect-device-controls";
      actions.append(
        button(tr("Join"), () => join(device.id)),
        button(tr("Invite"), () => action({ action: "invite", peer: device.id })),
      );
      heading.append(actions);
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
        flashSuccess(test, "rufin-view-refresh-symbolic");
      } catch {
        notice(tr("Connection test has failed"));
      }
    }, "icon-button");
    test.title = tr("Test connection");
    test.setAttribute("aria-label", test.title);
    test.replaceChildren(icon("rufin-view-refresh-symbolic"));
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
    element("folder").value = "";
    await showStorageConnection();
  });
}

async function showFolder(folder) {
  storageConnection = folder?.connection ?? null;
  element("folder").value = folder?.path ?? "";
  const connections = storageConnection ? await api("/integrations/files") : [];
  storageKind = connections.find((item) => item.id === storageConnection)?.kind ?? "";
  element("storage").value = storageKind;
  await showStorageConnection(connections);
}

function showRelay() {
  const enabled = element("relay-enabled").checked;
  element("relay").hidden = !enabled;
}

function showDisableConfirmation() {
  element("disable-dialog").returnValue = "cancel";
  element("disable-dialog").showModal();
}

function present(value) {
  element("name").value = value.settings.name;
  element("nearby").checked = value.settings.nearby;
  element("relay").value = value.settings.relay || (value.settings.public_relay ? "https://euc1-1.relay.n0.iroh.link/" : "");
  element("relay-enabled").checked = Boolean(value.settings.relay || value.settings.public_relay);
  showRelay();
  updateView();
  element("dialog").showModal();
  if (!value.pairing && !value.connecting) run(refreshContinuation);
}

export function watch(signal) {
  const events = new EventSource("/api/connect/events");
  events.addEventListener("connect", (event) => {
    show(JSON.parse(event.data));
    void refreshContinuation();
  });
  signal.addEventListener("abort", () => events.close(), { once: true });
  run(async () => {
    show(await api("/connect"));
    await refreshContinuation();
  });
}

export function init() {
  for (const button of document.querySelectorAll("[data-connect-mode]")) {
    button.addEventListener("click", () => {
      mode = button.dataset.connectMode;
      updateView();
      if (mode === "join") run(() => action({ action: "discover" }));
      else if (!state.settings.enabled) run(() => action({ action: "enable", enabled: true }));
    });
  }
  element("storage").addEventListener("change", () => {
    const kind = element("storage").value;
    if (kind) {
      element("storage").value = storageKind;
      run(() => chooseStorageConnection(kind));
    } else {
      storageKind = "";
      storageConnection = null;
      element("folder").value = "";
      element("storage-connection").hidden = true;
    }
  });
  click("storage-connection", () => chooseStorageConnection());
  const continuePlayback = async (continuation) => {
    if (continuation) await action({ action: "continue", peer: continuation.device });
    await refreshContinuation();
  };
  click("continue", () => continuePlayback(offeredContinuation));
  click("progress-open", () => document.getElementById("open-connect").click());
  click("progress-dismiss", () => {
    element("progress").hidden = true;
  });
  document.addEventListener("visibilitychange", () => {
    void refreshContinuation();
  });
  document.getElementById("open-connect").addEventListener("click", () => run(async () => {
    const value = await api("/connect");
    show(value);
    present(value);
  }));
  element("enabled").addEventListener("change", () => run(async () => {
    const enabled = element("enabled").checked;
    if (!enabled) {
      element("enabled").checked = state.settings.enabled;
      showDisableConfirmation();
      return;
    }
    try {
      await action({ action: "enable", enabled });
    } catch (error) {
      element("enabled").checked = state.settings.enabled;
      throw error;
    }
  }));
  element("disable-dialog").addEventListener("close", () => {
    if (element("disable-dialog").returnValue === "disable") {
      run(() => action({ action: "enable", enabled: false }));
    }
  });
  click("refresh", () => action({ action: "refresh" }));
  click("copy", async () => {
    const field = element("invitation");
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(field.value);
      flashSuccess(element("copy"), "rufin-edit-copy-symbolic");
    } else {
      field.hidden = false;
      field.focus();
      field.select();
    }
  });
  click("rename", () => action({ action: "rename", name: element("name").value }));
  click("join", () => join(element("peer").value));
  click("cancel-pairing", () => action({ action: "cancel_pairing" }));
  click("retry", async () => {
    element("retry").disabled = true;
    try { await action({ action: "retry" }); }
    finally { element("retry").disabled = false; }
  });
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
  click("folder-save", () => {
    const path = element("folder").value.trim();
    const folder = !path ? null : storageConnection ? { connection: storageConnection, path } : { path };
    return action({ action: "folder", folder });
  });
  click("folder-clear", () => action({ action: "folder", folder: null }));
  click("leave", async () => {
    if (!window.confirm(tr("Disconnect this device?"))) return;
    await action({ action: "leave" });
    updateView();
  });
}
