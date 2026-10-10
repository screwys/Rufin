import { tr } from "./ui.js";
import { api, state } from "./connection.js";

import { setFavorite, goToMedia } from "./library.js";

import { playlistMenu, radioMenu, showMenu, goToMenu } from "./menus.js";

import { $, button, el, run } from "./ui.js";
import {
  bindDrag,
  acceptsMedia,
  draggedMedia,
  mediaSelection,
  queueMedia,
} from "./drag.js";

let queueSelection = null;

let latestQueue;
let rendered = "";

const savedQueueLimit = Number(localStorage.getItem("rufin-queue-limit"));

function showQueue(value) {
  const previous = latestQueue?.window.find((row) => row.position === queueSelection);
  const selected = value?.window.find((row) => row.position === queueSelection);
  if (previous?.track.uri !== selected?.track.uri) queueSelection = null;
  latestQueue = value;
  if (!value) {
    rendered = "";
    $("queue-loading").hidden = true;
    return;
  }
  const limit = Number($("queue-limit").value);
  const start = Math.max(
    0,
    Math.min(
      value.window.length - limit,
      (value.current_index ?? 0) - value.offset - Math.floor(limit / 2),
    ),
  );
  const result = {
    ...value,
    offset: value.offset + start,
    window: value.window.slice(start, start + limit),
  };
  $("queue-loading").hidden = !result.loading;
  const existing = [...$("queue").children];
  const hideRows =
    result.loading &&
    !existing.some(
      (node) => Number(node.dataset.position) === result.current_index,
    );
  $("queue").style.visibility = hideRows ? "hidden" : "";
  $("queue-count").style.visibility = result.loading ? "hidden" : "";
  if (result.loading) {
    for (const node of existing)
      node.classList.toggle(
        "current",
        Number(node.dataset.position) === result.current_index,
      );
    return;
  }
  $("queue-count").textContent = result.total;
  const key = JSON.stringify([result.offset, result.window]);
  if (rendered === key) {
    for (const [index, node] of existing.entries()) {
      const current = Number(node.dataset.position) === result.current_index;
      node.classList.toggle("current", current);
      const selected = Number(node.dataset.position) === queueSelection;
      node.classList.toggle("selected", selected);
      node.querySelector(".queue-select")?.setAttribute("aria-pressed", String(selected));
      const number = node.querySelector(".number");
      if (number)
        number.textContent = current ? "▶" : String(result.offset + index + 1);
    }
    return;
  }
  rendered = key;
  $("queue").replaceChildren();
  if (!result.window.length) {
    $("queue").append(el("li", "empty-note", tr("Your queue is empty.")));
    return;
  }
  for (const [index, row] of result.window.entries()) {
    const li = el("li", "queue-row");
    li.dataset.position = row.position;
    li.draggable = true;
    li.classList.toggle("current", row.position === result.current_index);
    const open = button(
      tr("Select {title}", { title: row.track.title }),
      () => {
        queueSelection = row.position;
        document.querySelectorAll(".queue-row").forEach((item) => {
          item.classList.toggle(
            "selected",
            Number(item.dataset.position) === row.position,
          );
          item
            .querySelector(".queue-select")
            .setAttribute(
              "aria-pressed",
              String(Number(item.dataset.position) === row.position),
            );
        });
      },
      null,
      "queue-select",
    );
    open.replaceChildren(el("strong", "", row.track.title || tr("Untitled")));
    const metadata = el("div", "queue-track");
    const artist = button(
      row.track.artist || tr("Unknown artist"),
      () => goToMedia(row.track, "artists", artist),
      null,
      "metadata-link queue-artist",
    );
    metadata.append(open, artist);
    open.setAttribute("aria-pressed", String(queueSelection === row.position));
    li.classList.toggle("selected", queueSelection === row.position);
    open.addEventListener("dblclick", () =>
      run(() => api("/queue/activate", "POST", { position: row.position })),
    );
    open.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        event.stopPropagation();
        run(() => api("/queue/activate", "POST", { position: row.position }));
      }
    });
    const showActions = (anchor, event) => showQueueMenu(row, anchor, event);

    li.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      open.click();
      run(() => showActions(li, event));
    });
    const menu = button(tr("More actions"), () => showActions(menu), "more");
    menu.setAttribute("aria-haspopup", "menu");
    li.append(
      el(
        "span",
        "number",
        row.position === result.current_index
          ? "▶"
          : String((result.offset || 0) + index + 1),
      ),
      metadata,
      menu,
    );
    bindDrag(
      li,
      () => ({
        ...mediaSelection("track", row.track),
        queuePositions: [row.position],
      }),
      false,
    );
    li.addEventListener("dragover", (event) => {
      if (!acceptsMedia(event)) return;
      event.preventDefault();
      li.classList.add("drag-over");
    });
    li.addEventListener("dragleave", () => li.classList.remove("drag-over"));
    li.addEventListener("drop", (event) => {
      if (!acceptsMedia(event)) return;
      event.preventDefault();
      event.stopPropagation();
      li.classList.remove("drag-over");
      const rect = li.getBoundingClientRect();
      dropMedia(
        event.clientY < rect.top + rect.height / 2
          ? { before: row.position }
          : { after: row.position },
      );
    });
    $("queue").append(li);
  }
}

async function showQueueMenu(row, anchor, event) {
  const metadata = await api(
    `/media?${new URLSearchParams({ uri: row.track.uri })}`,
  );
  const track = { ...row.track, favorite: metadata.favorite };
  const destination = await goToMenu(track, "track", metadata);
  showMenu(
    [
      [tr("Play"), () => api("/queue/activate", "POST", { position: row.position })],
      [tr("Play Next"), () => api("/queue/next", "POST", { position: row.position })],
      [
        tr("Play Later"),
        () =>
          api("/queue/reorder", "POST", { positions: [row.position], before: null }),
      ],
      radioMenu(track),
      null,
      playlistMenu({ uris: [track.uri] }),
      [
        track.favorite
          ? tr("Remove from Favorites")
          : tr("Add to Favorites"),
        () => setFavorite([track]),
      ],

      [
        tr("Remove from Queue"),
        () =>
          api(
            `/queue/item?${new URLSearchParams({ position: row.position })}`,
            "DELETE",
          ),
      ],
      ...(destination ? [null, destination] : []),
    ],
    anchor,
    event,
  );
}

function init() {
  $("queue").addEventListener("dragover", (event) => {
    if (acceptsMedia(event)) event.preventDefault();
  });
  $("queue").addEventListener("drop", (event) => {
    if (!acceptsMedia(event)) return;
    event.preventDefault();
    dropMedia({});
  });
  $("queue-limit").value = [5, 10, 25, 50, 100].includes(savedQueueLimit)
    ? String(savedQueueLimit)
    : "5";
  $("clear-queue").addEventListener("click", () =>
    run(() => api("/queue", "DELETE")),
  );
  $("queue-limit").addEventListener("change", () => {
    localStorage.setItem("rufin-queue-limit", $("queue-limit").value);
    showQueue(latestQueue);
  });
}

function dropMedia(position) {
  const payload = draggedMedia();
  run(async () => {
    const media = await payload;
    if (media.queuePositions) {
      if (!media.queuePositions.includes(position.before ?? position.after)) {
        queueSelection = null;
        await api("/queue/reorder", "POST", {
          positions: media.queuePositions,
          ...position,
        });
      }
    } else await queueMedia(media, "insert", position);
  });
}

export { showQueue, showQueueMenu, init };
