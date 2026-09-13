import { api, state } from "./connection.js";
import { el, icon } from "./ui.js";

const mediaType = "application/x-rufin-media";
const pinType = "application/x-rufin-collection";
let current = null;
let preview = null;

function mediaSelection(kind, row) {
  return {
    kind,
    row,
    source: row.source ?? state.source,
    folder:
      row.source && row.source !== state.source ? null : state.selectedLibrary,
    ...(kind === "track" ? { uris: [row.uri] } : {}),
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
    cover.append(artwork ? artwork.cloneNode(true) : icon("tracks"));
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

function queueMedia(media, mode = "append", position = {}) {
  return api(media.uris ? "/queue" : `/queue/${media.kind}`, "POST", {
    source: media.source || null,
    folder: media.folder,
    ...(media.uris
      ? { uris: media.uris }
      : { id: media.row.id, album_artists: !!media.row.album_artists }),
    mode,
    ...position,
  });
}

function playlistMedia(id, media) {
  return api("/playlists/entries", "POST", {
    id,
    source: media.source || null,
    folder: media.folder,
    ...(media.uris
      ? { uris: media.uris }
      : {
          selection: {
            kind: media.kind.replaceAll("-", "_"),
            id: media.row.id,
            album_artists: !!media.row.album_artists,
          },
        }),
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
