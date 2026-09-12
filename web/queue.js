import { tr } from "./ui.js";
import { api, state } from "./connection.js";

import { setFavorite, goToMedia } from "./library.js";

import { playlistMenu, radioMenu, showMenu, goToMenu } from "./menus.js";

import { $, button, el, run } from "./ui.js";

let queueSelection = null;

let latestQueue;
let rendered = "";

const savedQueueLimit = Number(localStorage.getItem("rufin-queue-limit"));

function showQueue(value) {
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
    !existing.some((node) => node.dataset.id === result.current_id);
  $("queue").style.visibility = hideRows ? "hidden" : "";
  $("queue-count").style.visibility = result.loading ? "hidden" : "";
  if (result.loading) {
    for (const node of existing)
      node.classList.toggle("current", node.dataset.id === result.current_id);
    return;
  }
  $("queue-count").textContent = result.total;
  const key = JSON.stringify([result.offset, result.window]);
  if (rendered === key) {
    for (const [index, node] of existing.entries()) {
      const current = node.dataset.id === result.current_id;
      node.classList.toggle("current", current);
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
    li.dataset.id = row.id;
    li.draggable = true;
    li.classList.toggle("current", row.id === result.current_id);
    const open = button(
      tr("Select {title}", { title: row.track.title }),
      () => {
        queueSelection = row.id;
        document.querySelectorAll(".queue-row").forEach((item) => {
          item.classList.toggle("selected", item.dataset.id === row.id);
          item
            .querySelector(".queue-select")
            .setAttribute("aria-pressed", String(item.dataset.id === row.id));
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
    open.setAttribute("aria-pressed", String(queueSelection === row.id));
    li.classList.toggle("selected", queueSelection === row.id);
    open.addEventListener("dblclick", () =>
      run(() => api("/queue/activate", "POST", { id: row.id })),
    );
    open.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        event.stopPropagation();
        run(() => api("/queue/activate", "POST", { id: row.id }));
      }
    });
    const showActions = async (anchor, event) => {
      const metadata = await api(
        `/media?${new URLSearchParams({ uri: row.track.uri })}`,
      );
      const track = { ...row.track, favorite: metadata.favorite };
      const destination = await goToMenu(track, "track", metadata);
      showMenu(
        [
          [tr("Play"), () => api("/queue/activate", "POST", { id: row.id })],
          [tr("Play Next"), () => api("/queue/next", "POST", { id: row.id })],
          [
            tr("Play Later"),
            () =>
              api("/queue/reorder", "POST", { ids: [row.id], before: null }),
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
                `/queue/item?${new URLSearchParams({ id: row.id })}`,
                "DELETE",
              ),
          ],
          ...(destination ? [null, destination] : []),
        ],
        anchor,
        event,
      );
    };
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
        row.id === result.current_id
          ? "▶"
          : String((result.offset || 0) + index + 1),
      ),
      metadata,
      menu,
    );
    li.addEventListener("dragstart", (event) => {
      event.dataTransfer.setData("text/plain", row.id);
      event.dataTransfer.effectAllowed = "move";
    });
    li.addEventListener("dragover", (event) => {
      event.preventDefault();
      li.classList.add("drag-over");
    });
    li.addEventListener("dragleave", () => li.classList.remove("drag-over"));
    li.addEventListener("drop", (event) => {
      event.preventDefault();
      li.classList.remove("drag-over");
      const id = event.dataTransfer.getData("text/plain");
      if (id && id !== row.id)
        run(() => api("/queue/reorder", "POST", { ids: [id], before: row.id }));
    });
    $("queue").append(li);
  }
}

function init() {
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

export { showQueue, init };
