import { api, session } from "./connection.js";
import { navigate, opened, bindCards, cardCover } from "./library.js";
import { sourceIcon } from "./sources.js";
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

// The action that adds `pin` to the pins, or removes it when `pinned`.
function pinAction(pin, pinned) {
  return [
    pinned ? tr("Remove from Pins") : tr("Add to Pins"),
    async () => {
      await api("/pins", "POST", { pin, pinned: !pinned });
      await refreshPins();
    },
    pinned ? "rufin-list-remove-symbolic" : "rufin-list-add-symbolic",
  ];
}

async function move(moved, target) {
  await api("/pins", "PATCH", { moved, target });
  await refreshPins();
}

function open(item) {
  navigate(item.page);
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
    row.dataset.kind = item.kind;
    row.dataset.row = JSON.stringify(item);
    const title = item.title || item.name;
    const openButton = button(title, () => open(item), null, "pin-open");
    const artwork = el("span", "pin-cover");
    artwork.append(icon("rufin-symbolic"));
    const label = el("span", "pin-label");
    const metadata = el("span", "pin-metadata");
    const durationLabel = el("span", "pin-duration", item.duration_text);
    durationLabel.title = item.duration_text;
    metadata.append(
      icon("rufin-tracks-symbolic"),
      document.createTextNode(String(item.track_count)),
      icon("rufin-preferences-system-time-symbolic"),
      durationLabel,
    );
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
        else if (item.kind === "playlist" && (media.page || media.uris)) {
          await playlistMedia(item.id, media);
          await refreshPins();
        } else await addPin(media, item.pin);
      });
    });
    list.append(row);
    const query = cardCover(item.kind, item);
    if (query) run(() => coverGroup(artwork, query, item.artwork_count || 1, signal, item.artwork_previews));
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
  const pin = media.row.pin;
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
      opened && opened !== "sources" && JSON.stringify(item.page?.route) === JSON.stringify(opened.route);
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
