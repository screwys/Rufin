import { $, bindGenres, button, cover, el, icon, releaseCovers, run, slideText, tr } from "./ui.js";
import { api, fragment, session } from "./connection.js";
import { readFile } from "./metadata.js";
import { pinAction } from "./pins.js";
import { showMenu } from "./menus.js";

// The stations filter while it is open, which a page read again keeps with its caret.
let filter = null;
let caret = 0;

// The server the stations are saved on; empty when Rufin keeps them.
let savedOn = "";

const stationOf = (node) => JSON.parse(node.closest("[data-station]").dataset.station);

// The station on air shows its logo playing when it is an animated image.
function logo(node, station, signal) {
  if (!station.picture) return;
  const query = node.closest(".radio-on-air")
    ? { picture: station.picture, animated: true }
    : { picture: station.picture };
  run(() => cover(node, query, signal, station.artwork_previews?.[0]));
}

function clock(seconds) {
  const at = new Date(seconds * 1000);
  return `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
}

// An added station plays by its key, a suggested one by its place in Rufin's list.
function play(station, channel) {
  return api("/queue/station", "POST", {
    ...(station.key == null ? { listed: station.listed } : { key: station.key }),
    channel,
  });
}

// A drag moves a card among the added stations.
let moving = false;

// Moves `card` with the pointer of `down`: it takes the place of the card the pointer is over,
// the others making room, and when the pointer lifts the stations are ordered as the cards stand.
function startMove(card, down) {
  const stop = new AbortController();
  const cards = () => [...card.parentElement.children];
  const keys = () => cards().map((node) => stationOf(node).key);
  const before = keys().join();
  moving = true;
  card.classList.add("station-moving");
  document.addEventListener(
    "pointermove",
    (event) => {
      if (event.pointerId !== down.pointerId) return;
      const over = document.elementFromPoint(event.clientX, event.clientY)?.closest(".station-card");
      if (!over || over === card || over.parentElement !== card.parentElement) return;
      if (cards().indexOf(card) < cards().indexOf(over)) over.after(card);
      else over.before(card);
    },
    { signal: stop.signal },
  );
  // A finger that drags a card does not scroll the page.
  document.addEventListener("touchmove", (event) => event.preventDefault(), { signal: stop.signal, passive: false });
  const end = (event) => {
    if (event.pointerId !== down.pointerId) return;
    card.classList.remove("station-moving");
    stop.abort();
    moving = false;
    // The click that ends the drag plays nothing.
    const swallow = new AbortController();
    document.addEventListener(
      "click",
      (click) => {
        click.stopPropagation();
        click.preventDefault();
      },
      { capture: true, once: true, signal: swallow.signal },
    );
    setTimeout(() => swallow.abort(), 50);
    if (card.isConnected && keys().join() !== before) run(() => api("/stations/order", "POST", { keys: keys() }));
  };
  document.addEventListener("pointerup", end, { signal: stop.signal });
  document.addEventListener("pointercancel", end, { signal: stop.signal });
}

// Starts a move from an added station's handle, or from a finger held on its card.
function bindMove(page) {
  page.addEventListener("pointerdown", (event) => {
    const card = event.target.closest(".station-card");
    if (moving || !card?.querySelector(".station-tools")) return;
    if (event.target.closest('[data-action="station-move"]')) {
      event.preventDefault();
      startMove(card, event);
      return;
    }
    if (event.pointerType !== "touch" || event.target.closest(".station-end, .station-channel")) return;
    const held = new AbortController();
    const cancel = () => {
      clearTimeout(timer);
      held.abort();
    };
    const timer = setTimeout(() => {
      held.abort();
      if (card.isConnected) startMove(card, event);
    }, 500);
    for (const name of ["pointerup", "pointercancel"]) document.addEventListener(name, cancel, { signal: held.signal });
    document.addEventListener(
      "pointermove",
      (move) => {
        if (Math.hypot(move.clientX - event.clientX, move.clientY - event.clientY) > 8) cancel();
      },
      { signal: held.signal },
    );
  });
  // A held finger moves the card rather than opening the browser's menu.
  page.addEventListener("contextmenu", (event) => {
    if (matchMedia("(hover: none)").matches && event.target.closest(".station-card")?.querySelector(".station-tools"))
      event.preventDefault();
  });
}

// Shows the cards whose names hold the filter; Suggested goes with its last card.
function showFilter(page) {
  const input = page.querySelector("#station-filter");
  input.hidden = filter === null;
  input.value = filter ?? "";
  page.querySelector('[data-action="station-search"]').setAttribute("aria-expanded", String(filter !== null));
  const text = (filter ?? "").trim().toLocaleLowerCase();
  for (const section of page.querySelectorAll(".radio-section")) {
    const cards = [...section.querySelectorAll(".station-card")];
    for (const card of cards)
      card.hidden = !card.querySelector(".station-name").textContent.toLocaleLowerCase().includes(text);
    if (!section.contains(input)) section.hidden = cards.every((card) => card.hidden);
  }
}

// Reads the Radio page from `path` into the content, or binds the one the server drew, and
// answers what it says of itself. A page that opens starts without the filter.
async function loadRadio(path, signal, opening) {
  if (opening) filter = null;
  const typing = document.activeElement?.id === "station-filter";
  if (path) await fragment(path, $("content"), signal);
  const page = $("content").querySelector(".radio-page");
  savedOn = page.dataset.savedOn;
  for (const node of page.querySelectorAll("time[data-at]")) node.textContent = clock(Number(node.dataset.at));
  for (const node of page.querySelectorAll(".detail-meta")) {
    slideText(node.querySelector(".detail-facts"));
    bindGenres(node);
  }
  for (const node of page.querySelectorAll("[data-station]"))
    logo(node.querySelector(".cover"), stationOf(node), signal);
  // The logos of the page this one replaced go once each of these had the chance to take its own.
  releaseCovers();
  const input = page.querySelector("#station-filter");
  showFilter(page);
  if (typing && filter !== null) {
    input.focus({ preventScroll: true });
    input.setSelectionRange(caret, caret);
  }
  for (const name of ["input", "keyup", "click"])
    input.addEventListener(name, () => {
      filter = input.value;
      caret = input.selectionStart;
      if (name === "input") showFilter(page);
    });
  const toggle = page.querySelector('[data-action="station-search"]');
  input.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    filter = null;
    showFilter(page);
    toggle.focus();
  });
  bindMove(page);
  page.addEventListener("click", (event) => {
    const control = event.target.closest("[data-action]");
    if (!control) return;
    event.preventDefault();
    const action = control.dataset.action;
    run(() => {
      if (action === "play") return play(stationOf(control));
      if (action === "channel") return play(stationOf(control), control.dataset.channel);
      if (action === "add") return api("/stations/listed", "POST", { id: stationOf(control).listed });
      if (action === "station-pin") return pin(stationOf(control), control);
      if (action === "station-edit") return openEdit(stationOf(control));
      if (action === "station-delete") return remove(stationOf(control));
      // The header's button says what it does: its form posts the player's play or pause.
      if (action === "toggle")
        return api(control.closest("form").getAttribute("action").replace("/api", ""), "POST", {});
      if (action === "station-add") return openAdd();
      if (action === "station-search") {
        filter = filter === null ? "" : null;
        caret = 0;
        showFilter(page);
        if (filter !== null) input.focus();
      }
    });
  });
  return page.dataset;
}

// Adds a station to Saved or removes it. One with channels lists them, each saved by itself.
function pin(station, anchor) {
  if (!station.channels.length) return pinAction(station.pin)[1]();
  return showMenu(
    station.channels.map((channel) => [
      channel.name,
      pinAction(channel.pin)[1],
      channel.pinned ? "rufin-object-select-symbolic" : undefined,
    ]),
    anchor,
  );
}

// A station of Rufin's list can be added again from Suggested; any other is asked about first.
function remove(station) {
  if (!station.listed && !confirm(tr("Delete this station?"))) return undefined;
  return api(`/stations?${new URLSearchParams({ key: station.key })}`, "DELETE");
}

// The Add dialog's lookup: a search of the names typed, or the reading of a pasted address.
let lookup = null;
let lookupTimer;
let probed = null;

function showStatus(status, error = false) {
  $("station-add-status").textContent = status;
  $("station-add-status").classList.toggle("failed", error);
  $("station-add-status").hidden = !status;
}

// Shows what a lookup answered: the stations found, the stream read, or the status alone.
function showAdd({ status = "", error = false, found = null, probe = null } = {}) {
  probed = probe;
  showStatus(status, error);
  $("station-results").replaceChildren(...(found || []));
  $("station-results").hidden = $("station-results-note").hidden = !found?.length;
  $("station-probe").hidden = $("station-add-actions").hidden = !probe;
  if (probe) {
    $("station-probe-name").textContent = probe.name;
    $("station-probe-detail").textContent = [probe.format, probe.host].filter(Boolean).join(" · ");
    $("station-add-name").value = probe.name;
    $("station-add-homepage").value = "";
  }
  releaseCovers();
}

function foundRow(station, signal) {
  const row = el("div", "pref-row");
  const picture = el("div", "cover");
  picture.append(icon("rufin-radio-station-symbolic"));
  const label = el("span", "pref-label");
  label.append(el("span", "", station.name), el("small", "", station.detail));
  const added = () => {
    const mark = el("span", "station-added");
    mark.append(icon("rufin-object-select-symbolic"), document.createTextNode(tr("Added")));
    return mark;
  };
  const add = button(
    tr("Add"),
    async () => {
      await api("/stations", "POST", {
        name: station.name,
        stream_url: station.stream_url,
        homepage: station.homepage,
        logo: station.logo,
      });
      add.replaceWith(added());
    },
    null,
    "station-result-add",
  );
  row.append(picture, label, station.added ? added() : add);
  logo(picture, station, signal);
  return row;
}

function look() {
  clearTimeout(lookupTimer);
  lookup?.abort();
  const text = $("station-address").value.trim();
  if (!text) return showAdd();
  lookup = new AbortController();
  const signal = AbortSignal.any([session.signal, lookup.signal]);
  // What the last lookup answered stays until this one answers.
  lookupTimer = setTimeout(async () => {
    showStatus(tr("Loading..."));
    try {
      if (/^https?:\/\//i.test(text)) {
        const probe = await api("/stations/probe", "POST", { address: text }, signal);
        if (!signal.aborted) showAdd({ probe });
      } else {
        const { stations } = await api(`/stations/search?${new URLSearchParams({ q: text })}`, "GET", undefined, signal);
        if (signal.aborted) return;
        if (stations.length) showAdd({ found: stations.map((station) => foundRow(station, signal)) });
        else showAdd({ status: tr("No results") });
      }
    } catch (error) {
      // What failed shows where the results would be.
      if (!signal.aborted) showAdd({ status: error.message, error: true });
    }
  }, 300);
}

function openAdd() {
  $("station-add-form").reset();
  showAdd();
  $("station-saved-on").textContent = tr("Stations are saved on {saved_on}.", { saved_on: savedOn });
  $("station-saved-on").hidden = !savedOn;
  $("station-add-dialog").showModal();
  $("station-address").focus();
}

let editing = null;
let editLogo = null;
// The pictures Edit shows: the one chosen for the station and the one it has without.
let pictures = { chosen: null, automatic: null };
// What Apply does with the chosen picture: sets the image picked here, takes it away, or
// neither.
let picked = null;
let pickedUrl = null;
let unchosen = false;

function showPicture() {
  const picture = el("div", "cover");
  picture.append(icon("rufin-radio-station-symbolic"));
  $("station-edit-picture").replaceChildren(picture);
  if (pickedUrl) URL.revokeObjectURL(pickedUrl);
  pickedUrl = picked ? URL.createObjectURL(picked) : null;
  const shown = unchosen ? pictures.automatic : pictures.chosen || pictures.automatic;
  if (pickedUrl) {
    const image = el("img");
    image.alt = "";
    image.src = pickedUrl;
    picture.replaceChildren(image);
  } else if (shown) logo(picture, shown, AbortSignal.any([session.signal, editLogo.signal]));
  releaseCovers();
  $("station-edit-picture-remove").disabled = !picked && (unchosen || !pictures.chosen);
}

async function openEdit(station) {
  editLogo?.abort();
  editLogo = new AbortController();
  pictures = await api(`/stations/picture?${new URLSearchParams({ key: station.key })}`);
  editing = station;
  picked = null;
  unchosen = false;
  $("station-edit-file").value = "";
  showPicture();
  $("station-edit-name").value = station.name;
  $("station-edit-homepage").value = station.homepage;
  // Only a station added by its address can change its stream.
  $("station-edit-stream-row").hidden = !!station.listed;
  $("station-edit-stream").value = station.stream_url;
  $("station-edit-channel-row").hidden = !station.channels.length;
  $("station-edit-channel").replaceChildren(
    ...station.channels.map((channel) => {
      const option = el("option", "", channel.name);
      option.selected = channel.own;
      return option;
    }),
  );
  $("station-edit-dialog").showModal();
  $("station-edit-name").focus();
}

function init() {
  $("station-address").addEventListener("input", look);
  $("station-add-dialog").addEventListener("close", () => {
    clearTimeout(lookupTimer);
    lookup?.abort();
    showAdd();
  });
  $("station-add-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const name = $("station-add-name").value.trim();
    if (!probed || !name) return;
    run(async () => {
      await api("/stations", "POST", { name, stream_url: probed.stream_url, homepage: $("station-add-homepage").value.trim() });
      $("station-add-dialog").close();
    });
  });
  $("station-edit-file").addEventListener("change", (event) => {
    const [file] = event.target.files;
    event.target.value = "";
    if (!file) return;
    picked = file;
    unchosen = false;
    showPicture();
  });
  // Without the image picked here the station shows its chosen picture again, so both go.
  $("station-edit-picture-remove").addEventListener("click", () => {
    picked = null;
    unchosen = !!pictures.chosen;
    showPicture();
  });
  $("station-edit-dialog").addEventListener("close", () => {
    editLogo?.abort();
    picked = null;
    if (pickedUrl) URL.revokeObjectURL(pickedUrl);
    pickedUrl = null;
    $("station-edit-picture").replaceChildren();
    releaseCovers();
  });
  $("station-edit-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const name = $("station-edit-name").value.trim();
    if (!name) return;
    run(async () => {
      await api("/stations", "PATCH", {
        key: editing.key,
        name,
        homepage: $("station-edit-homepage").value.trim(),
        stream_url: editing.listed ? "" : $("station-edit-stream").value.trim(),
        channel: editing.channels.length ? $("station-edit-channel").value : null,
        picture: picked
          ? { operation: "upload", data: await readFile(picked) }
          : unchosen
            ? { operation: "remove" }
            : undefined,
      });
      $("station-edit-dialog").close();
    });
  });
}

export { init, loadRadio, play };
