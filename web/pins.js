import { api, session } from "./connection.js";
import { navigate, opened, bindCards, cardCover } from "./library.js";
import { sourceIcon } from "./sources.js";
import { showMenu } from "./menus.js";
import { play as playStation } from "./radio.js";
import {
  $,
  button,
  cover,
  el,
  icon,
  run,
  tr,
  coverGroup,
  releaseCovers,
} from "./ui.js";
import { bindDrag, draggedMedia, mediaSelection, playlistMedia, playlistMember } from "./drag.js";

let pins = [];
let items = [];
let visible = true;
let previous = "";
let request;
let covers;

// The action that adds `pin` to the pins, or removes it when it is pinned. The pins show what
// changed when the events stream says they changed, in every tab alike.
function pinAction(pin) {
  const pinned = pins.some((shown) => JSON.stringify(shown) === JSON.stringify(pin));
  return [
    pinned ? tr("Remove from Saved") : tr("Add to Saved"),
    () => api("/pins", "POST", { pin, pinned: !pinned }),
    pinned ? "rufin-list-remove-symbolic" : "rufin-list-add-symbolic",
  ];
}

function move(moved, target) {
  return api("/pins", "PATCH", { moved, target });
}

function samePin(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

// Whether the pinned `item` takes what this page drags: any pin moves among the pins, a playlist
// core says takes tracks takes them, and a playlist folder takes playlists.
function takes(item, media) {
  if (media.pin) return true;
  if (item.kind === "playlist") return Boolean(item.takes_tracks && (media.page || media.uris));
  if (item.kind === "playlist-folder") return Boolean(playlistMember(media));
  return false;
}

// A station has no page: it plays.
function open(item) {
  if (item.kind === "station") return playStation(item, item.channel);
  return navigate(item.page);
}

function render() {
  const list = $("pin-list");
  const focused = document.activeElement.closest(".pin-row")?.dataset.pin;
  list.replaceChildren();
  covers?.abort();
  covers = new AbortController();
  const signal = AbortSignal.any([session.signal, covers.signal]);
  $("pins").hidden = !visible;
  for (const item of items) {
    const row = el("div", "pin-row");
    row.dataset.pin = JSON.stringify(item.pin);
    const station = item.kind === "station";
    row.dataset.kind = item.kind;
    // A station is no row of the library: its menu is its pin alone.
    if (station)
      row.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        if (item.menu.pin) showMenu([pinAction(item.menu.pin)], row, event);
      });
    else row.dataset.row = JSON.stringify(item);
    const title = item.title || item.name;
    const openButton = button(title, () => open(item), null, "pin-open");
    const artwork = el("span", "pin-cover");
    artwork.append(icon(station ? "rufin-radio-station-symbolic" : "rufin-symbolic"));
    const label = el("span", "pin-label");
    const metadata = el("span", "pin-metadata");
    if (station) {
      // Its genres stand where a collection's tracks and length do.
      const genres = item.genres.join(", ");
      const genresLabel = el("span", "pin-duration", genres);
      genresLabel.title = genres;
      if (genres) metadata.append(icon("rufin-tag-outline-symbolic"), genresLabel);
    } else {
      const durationLabel = el("span", "pin-duration", item.duration_text);
      durationLabel.title = item.duration_text;
      metadata.append(
        icon("rufin-tracks-symbolic"),
        document.createTextNode(String(item.track_count)),
        icon("rufin-preferences-system-time-symbolic"),
        durationLabel,
      );
    }
    if (item.badge) {
      const badge = item.badge.kind
        ? sourceIcon(item.badge.kind)
        : el("img", "source-icon");
      if (!item.badge.kind) badge.src = "/icons/rufin.svg";
      badge.title = item.badge.name;
      badge.setAttribute("aria-label", badge.title);
      metadata.append(badge);
    }
    label.append(el("span", "pin-title", title), metadata);
    openButton.replaceChildren(artwork, label);
    const controls = el("div", "pin-controls");
    // A pin plays what core says its row plays: a playlist folder plays nothing.
    for (const [label, name, action] of item.play ? [
      [tr("Play"), "rufin-media-playback-start-symbolic", "replace"],
      [tr("Play Next"), "rufin-mail-forward-symbolic", "next"],
      [tr("Play Later"), "rufin-go-last-symbolic", "append"],
    ] : []) {
      const control = el("button", "icon-button");
      control.type = "button";
      control.title = label;
      control.setAttribute("aria-label", label);
      control.dataset.action = action;
      control.append(icon(name));
      controls.append(control);
    }
    row.append(openButton, controls);
    bindDrag(row, () => ({
      ...mediaSelection(item.kind, item),
      pin: item.pin,
    }));
    row.addEventListener("dragover", (event) => {
      const media = draggedMedia();
      if (!media || !takes(item, media)) return;
      event.preventDefault();
      row.classList.add("pin-drop");
    });
    row.addEventListener("dragleave", () => row.classList.remove("pin-drop"));
    row.addEventListener("dragend", () => {
      list
        .querySelectorAll(".pin-drop")
        .forEach((node) => node.classList.remove("pin-drop"));
    });
    row.addEventListener("drop", (event) => {
      const media = draggedMedia();
      if (!media || !takes(item, media)) return;
      event.preventDefault();
      event.stopPropagation();
      row.classList.remove("pin-drop");
      run(async () => {
        if (media.pin) await move(media.pin, item.pin);
        else if (item.kind === "playlist") await playlistMedia(item.id, media);
        else
          await api("/playlist-folders/members", "POST", {
            folder: item.id,
            members: [playlistMember(media)],
          });
      });
    });
    list.append(row);
    const query = cardCover(item.kind, item);
    if (station) {
      if (item.picture) run(() => cover(artwork, { picture: item.picture, animated: true }, signal, item.artwork_previews?.[0]));
    } else if (query) run(() => coverGroup(artwork, query, item.artwork_count || 1, signal, item.artwork_previews, true));
    // A playlist folder's picture is its preview.
    else if (item.artwork_previews?.[0]) artwork.replaceChildren(Object.assign(el("img"), { alt: "", src: item.artwork_previews[0] }));
  }
  bindCards(list);
  releaseCovers();
  if (focused)
    [...list.children]
      .find((row) => row.dataset.pin === focused)
      ?.querySelector("button")
      ?.focus();
  markPins();
}

// While this page's drag is over the sidebar, as on GTK: the drop area for a collection no pin
// keeps yet, and the pins that cannot take the drag dimmed.
function showDropFeedback(media) {
  const pin = media.row?.pin;
  $("pin-new").classList.toggle(
    "open",
    Boolean(visible && !media.pin && pin && !pins.some((shown) => samePin(shown, pin))),
  );
  for (const [index, row] of [...$("pin-list").children].entries())
    row.classList.toggle("drop-unavailable", !takes(items[index], media));
}

function clearDropFeedback() {
  $("pin-new").classList.remove("open", "pin-drop");
  for (const row of $("pin-list").children) row.classList.remove("drop-unavailable", "pin-drop");
}

function initPins() {
  const navigation = $("navigation");
  navigation.addEventListener("dragenter", () => {
    const media = draggedMedia();
    if (media) showDropFeedback(media);
  });
  navigation.addEventListener("dragleave", (event) => {
    if (!navigation.contains(event.relatedTarget)) clearDropFeedback();
  });
  document.addEventListener("dragend", clearDropFeedback);
  document.addEventListener("drop", clearDropFeedback);
  const section = $("pins");
  section.addEventListener("dragover", (event) => {
    if (draggedMedia()?.pin) event.preventDefault();
  });
  section.addEventListener("drop", (event) => {
    const moved = draggedMedia()?.pin;
    if (!moved) return;
    event.preventDefault();
    if (pins.length) run(() => move(moved, pins[pins.length - 1]));
  });
  const area = $("pin-new");
  area.addEventListener("dragover", (event) => {
    if (!area.classList.contains("open")) return;
    event.preventDefault();
    event.stopPropagation();
    area.classList.add("pin-drop");
  });
  area.addEventListener("dragleave", () => area.classList.remove("pin-drop"));
  area.addEventListener("drop", (event) => {
    if (!area.classList.contains("open")) return;
    const pin = draggedMedia()?.row?.pin;
    event.preventDefault();
    event.stopPropagation();
    clearDropFeedback();
    if (pin) run(() => api("/pins", "POST", { pin, pinned: true, first: true }));
  });
}

function resetPins() {
  request?.abort();
  covers?.abort();
  previous = "";
  pins = [];
  items = [];
  $("pin-list").replaceChildren();
  $("pins").hidden = true;
}

function markPins() {
  for (const [index, row] of [...$("pin-list").children].entries()) {
    const item = items[index];
    const selected =
      opened && JSON.stringify(item.page?.route) === JSON.stringify(opened.route);
    if (selected)
      row.querySelector(".pin-open").setAttribute("aria-current", "page");
    else row.querySelector(".pin-open").removeAttribute("aria-current");
  }
}

async function refreshPins() {
  request?.abort();
  request = new AbortController();
  const signal = AbortSignal.any([session.signal, request.signal]);
  const next = await api("/pins", "GET", undefined, signal);
  const serialized = JSON.stringify(next);
  if (serialized === previous) return;
  previous = serialized;
  pins = next.pins;
  items = next.items;
  visible = next.visible;
  render();
}

export { pinAction, refreshPins, markPins, initPins, resetPins };
