import { api, session, state } from "./connection.js";
import { navigate, route, detail, bindCards } from "./library.js";
import { updateSources, sourceIcon } from "./sources.js";
import {
  $,
  button,
  el,
  icon,
  run,
  tr,
  coverGroup,
  releaseCovers,
} from "./ui.js";
import {
  bindDrag,
  acceptsMedia,
  acceptsPin,
  draggedMedia,
  mediaSelection,
  playlistMedia,
} from "./drag.js";

let pins = [];
let items = [];
let visible = true;
let previous = "";
let request;
let covers;

function identity(pin) {
  const [kind, value] = Object.entries(pin)[0];
  return JSON.stringify([
    kind,
    value.source_id ?? null,
    value.album_id ?? value.artist_id ?? value.genre_id ?? value.playlist_id,
    !!value.album_artist,
  ]);
}

function pinFor(kind, row, source = state.source) {
  if (row.pin) return row.pin;
  switch (kind) {
    case "album":
      return { Album: { source_id: source, album_id: row.object_id } };
    case "artist":
      return {
        Artist: {
          source_id: source,
          artist_id: row.object_id,
          album_artist: !!row.album_artists,
        },
      };
    case "genre":
      return { Genre: { source_id: source, genre_id: row.object_id } };
    case "playlist":
      return {
        Playlist: { source_id: row.source ?? null, playlist_id: row.object_id },
      };
    case "smart-playlist":
      return { SmartPlaylist: { playlist_id: row.object_id } };
    default:
      return null;
  }
}

function pinAction(kind, row, source = state.source) {
  const pin = pinFor(kind, row, source);
  if (!pin || !visible) return null;
  const pinned = pins.some((stored) => identity(stored) === identity(pin));
  return [
    pinned ? tr("Remove from Pins") : tr("Add to Pins"),
    async () => {
      await api("/pins", "POST", { pin, pinned: !pinned });
      await refreshPins();
    },
    pinned ? "remove" : "add",
  ];
}

async function move(moved, target) {
  await api("/pins", "PATCH", { moved, target });
  await refreshPins();
}

function pinRoute(item) {
  return {
    album: "albums",
    artist: "artists",
    genre: "genres",
    playlist: "playlists",
    "smart-playlist": "smart-playlists",
  }[item.kind];
}

async function open(item) {
  if (item.source && item.source !== state.source) {
    await api("/sources/select", "POST", { id: item.source });
    state.source = item.source;
    updateSources(await api("/sources"));
  }
  navigate(pinRoute(item), item);
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
    row.dataset.pin = identity(item.pin);
    row.dataset.kind = item.kind;
    row.dataset.row = JSON.stringify(item);
    const title = item.title || item.name;
    const openButton = button(title, () => open(item), null, "pin-open");
    const artwork = el("span", "pin-cover");
    artwork.append(icon("cover-fallback"));
    const label = el("span", "pin-label");
    const metadata = el("span", "pin-metadata");
    const seconds = Math.floor(Math.max(0, item.duration_ms) / 1000);
    const duration = `${seconds >= 3600 ? `${Math.floor(seconds / 3600)}h ` : ""}${seconds >= 60 ? `${Math.floor(seconds / 60) % 60}m ` : ""}${seconds % 60}s`;
    const durationLabel = el("span", "pin-duration", duration);
    durationLabel.title = duration;
    metadata.append(
      icon("tracks"),
      document.createTextNode(String(item.track_count)),
      icon("duration"),
      durationLabel,
    );
    if (["playlist", "smart-playlist"].includes(item.kind)) {
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
    for (const [label, name, action] of [
      [tr("Play"), "play", "replace"],
      [tr("Play Next"), "play-next", "next"],
      [tr("Play Later"), "play-last", "append"],
    ]) {
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
      if (
        !acceptsPin(event) &&
        !(item.kind === "playlist" && acceptsMedia(event))
      )
        return;
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
      if (
        !acceptsPin(event) &&
        !(item.kind === "playlist" && acceptsMedia(event))
      )
        return;
      event.preventDefault();
      event.stopPropagation();
      row.classList.remove("pin-drop");
      const payload = draggedMedia();
      run(async () => {
        const media = await payload;
        if (media.pin) await move(media.pin, item.pin);
        else if (item.kind === "playlist") {
          await playlistMedia(item.id, media);
          await refreshPins();
        } else await addPin(media, item.pin);
      });
    });
    list.append(row);
    const query =
      item.kind === "playlist"
        ? { playlist: item.id }
        : item.kind === "smart-playlist"
          ? {
              smart_playlist: item.id,
              ...(state.source ? { source: state.source } : {}),
              ...(state.selectedLibrary
                ? { folder: state.selectedLibrary }
                : {}),
            }
          : item.kind === "genre"
            ? { genre: item.id, source: item.source }
            : { uri: item.uri };
    run(() => coverGroup(artwork, query, item.artwork_count || 1, signal));
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

async function addPin(media, target) {
  const pin = pinFor(media.kind, media.row, media.source);
  if (!pin) return;
  await api("/pins", "POST", { pin, pinned: true });
  if (target) await move(pin, target);
  else await refreshPins();
}

function initPins() {
  const section = $("pins");
  section.addEventListener("dragover", (event) => {
    if (!acceptsPin(event)) return;
    event.preventDefault();
  });
  section.addEventListener("drop", (event) => {
    if (!acceptsPin(event)) return;
    event.preventDefault();
    const payload = draggedMedia();
    run(async () => {
      const media = await payload;
      if (media.pin && pins.length)
        await move(media.pin, pins[pins.length - 1]);
      else await addPin(media);
    });
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
      pinRoute(item) === route &&
      detail?.id === item.id &&
      !!detail?.album_artists === !!item.album_artists;
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
