import { api } from "./connection.js";
import { el, icon } from "./ui.js";

const mediaType = "application/x-rufin-media";
const pinType = "application/x-rufin-collection";
// Playlists and smart playlists, which a folder or another playlist takes.
const playlistType = "application/x-rufin-playlists";
let current = null;
let preview = null;

// What dragging or playing a row carries: the page core says it plays, else its track's URI;
// a playlist folder carries neither.
function mediaSelection(kind, row) {
  return { kind, row, ...(row.play ? { page: row.play } : row.uri ? { uris: [row.uri] } : {}) };
}

function bindDrag(node, read, pinnable = true) {
  node.draggable = true;
  node.addEventListener("dragstart", (event) => {
    const media = read();
    current = Promise.resolve(media);
    if (media.page || media.uris) event.dataTransfer.setData(mediaType, "");
    if (pinnable) event.dataTransfer.setData(pinType, "");
    if (playlistMember(media)) event.dataTransfer.setData(playlistType, "");
    event.dataTransfer.effectAllowed = "copyMove";
    preview?.remove();
    preview = el("div", "drag-preview");
    preview.setAttribute("aria-hidden", "true");
    preview.style.left = `${event.clientX - 18}px`;
    preview.style.top = `${event.clientY - 18}px`;
    const cover = el("span", "drag-preview-cover");
    const artwork =
      node.querySelector(".cover, .pin-cover, .detail-cover img");
    cover.append(artwork ? artwork.cloneNode(true) : icon("rufin-tracks-symbolic"));
    const row = media.row || JSON.parse(node.dataset.row);
    preview.append(
      cover,
      el("span", "drag-preview-title", row.title || row.name),
    );
    document.body.append(preview);
    event.dataTransfer.setDragImage(preview, 18, 18);
    const image = preview;
    requestAnimationFrame(() => image.remove());
  });
  node.addEventListener("dragend", () => {
    current = null;
    preview?.remove();
    preview = null;
  });
}

function acceptsMedia(event) {
  return event.dataTransfer.types.includes(mediaType);
}

function draggedMedia() {
  return current;
}
function acceptsPin(event) {
  return event.dataTransfer.types.includes(pinType);
}

function acceptsPlaylists(event) {
  return event.dataTransfer.types.includes(playlistType);
}

// The folder member a dragged playlist or smart playlist is, as core names it.
function playlistMember(media) {
  if (media.kind === "playlist") return { Playlist: { key: media.row.id } };
  if (media.kind === "smart-playlist") return { SmartPlaylist: { key: media.row.id } };
  return null;
}

// Plays or queues a collection's page whole or the selected rows of a page, or tracks by URI.
function queueMedia(media, mode = "append", position = {}) {
  return api("/queue", "POST", {
    ...(media.uris ? { uris: media.uris } : { page: media.page, ranges: media.ranges }),
    mode,
    ...position,
  });
}

function playlistMedia(id, media) {
  return api("/playlists/entries", "POST", {
    id,
    ...(media.uris ? { uris: media.uris } : { page: media.page, ranges: media.ranges }),
  });
}

export {
  bindDrag,
  acceptsMedia,
  acceptsPin,
  acceptsPlaylists,
  playlistMember,
  draggedMedia,
  mediaSelection,
  queueMedia,
  playlistMedia,
};
