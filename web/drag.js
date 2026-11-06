import { api } from "./connection.js";
import { el, icon } from "./ui.js";

const mediaType = "application/x-rufin-media";
const pinType = "application/x-rufin-collection";
let current = null;
let preview = null;

// What dragging or playing a row carries: the page core says it plays, else its track's URI.
function mediaSelection(kind, row) {
  return {
    kind,
    row,
    ...(row.play ? { page: row.play } : { uris: [row.uri] }),
  };
}

function bindDrag(node, read, pinnable = true) {
  node.draggable = true;
  node.addEventListener("dragstart", (event) => {
    const media = read();
    current = Promise.resolve(media);
    event.dataTransfer.setData(mediaType, "");
    if (pinnable) event.dataTransfer.setData(pinType, "");
    event.dataTransfer.effectAllowed = "copyMove";
    preview?.remove();
    preview = el("div", "drag-preview");
    preview.setAttribute("aria-hidden", "true");
    preview.style.left = `${event.clientX - 18}px`;
    preview.style.top = `${event.clientY - 18}px`;
    const cover = el("span", "drag-preview-cover");
    const artwork =
      node.querySelector(".cover, .pin-cover") ||
      node.closest(".home-showcase")?.querySelector(".cover");
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

// Plays or queues a collection's page whole, or tracks by URI.
function queueMedia(media, mode = "append", position = {}) {
  return api("/queue", "POST", {
    ...(media.uris ? { uris: media.uris } : { page: media.page }),
    mode,
    ...position,
  });
}

function playlistMedia(id, media) {
  return api("/playlists/entries", "POST", {
    id,
    ...(media.uris ? { uris: media.uris } : { page: media.page }),
  });
}

export {
  bindDrag,
  acceptsMedia,
  acceptsPin,
  draggedMedia,
  mediaSelection,
  queueMedia,
  playlistMedia,
};
