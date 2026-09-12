import { tr } from "./ui.js";
import { api, state } from "./connection.js";

import { loadView, openMediaLink } from "./library.js";

import { $, button, el, icon, run } from "./ui.js";

function playRadio(row, kind = "track", mode = "replace") {
  return api("/queue/radio", "POST", { kind, id: row.id, uri: row.uri, mode });
}

let menuAnchor = null;

let submenuAnchor = null;

let submenuRequest = null;

function closeMenus() {
  menuAnchor?.closest(".album-card")?.classList.remove("menu-open");
  submenuRequest?.abort();
  for (const id of ["context-submenu", "track-menu"])
    if ($(id).matches(":popover-open")) $(id).hidePopover();
}

function menuItems(menu, actions) {
  menu.replaceChildren();
  for (const entry of actions) {
    if (!entry) {
      menu.append(el("hr"));
      continue;
    }
    const [label, execute, itemIcon, children] = entry;
    const name =
      itemIcon ||
      {
        [tr("Play")]: "play",
        [tr("Play Next")]: "play-next",
        [tr("Play Later")]: "play-last",
        [tr("Remove from Queue")]: "remove",
        [tr("Remove from Playlist")]: "remove",
        [tr("Rename Playlist")]: "edit",
        [tr("Delete Playlist")]: "delete",
        [tr("Favorite")]: "favorite",
        [tr("Add to Favorites")]: "favorite",
        [tr("Remove from Favorites")]: "favorite-filled",
      }[label];
    const item = button(
      label,
      async () => {
        if (children) {
          await openSubmenu(children, item, true);
          return;
        }
        closeMenus();
        await execute();
      },
      name,
      "menu-item",
    );
    if (name) item.append(document.createTextNode(label));
    item.setAttribute("role", "menuitem");
    if (children) {
      item.setAttribute("aria-haspopup", "menu");
      item.append(icon("forward"));
      item.addEventListener("pointerenter", () =>
        run(() => openSubmenu(children, item, false)),
      );
      item.addEventListener("keydown", (event) => {
        if (event.key === "ArrowRight") {
          event.preventDefault();
          run(() => openSubmenu(children, item, true));
        }
      });
    } else if (menu === $("track-menu"))
      item.addEventListener("pointerenter", () => {
        if ($("context-submenu").matches(":popover-open"))
          $("context-submenu").hidePopover();
      });
    menu.append(item);
  }
}

function showMenu(actions, anchor, event) {
  closeMenus();
  menuAnchor = anchor;
  anchor.closest(".album-card")?.classList.add("menu-open");
  submenuAnchor = null;
  const menu = $("track-menu");
  menuItems(menu, actions);
  menu.showPopover();
  const rect = anchor.getBoundingClientRect();
  menu.style.left = `${Math.max(8, Math.min(event?.clientX || rect.right, innerWidth - menu.offsetWidth - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(event?.clientY || rect.bottom, innerHeight - menu.offsetHeight - 8))}px`;
  menu.querySelector("button")?.focus();
}

async function openSubmenu(children, anchor, focus) {
  const menu = $("context-submenu");
  if (submenuAnchor === anchor && menu.matches(":popover-open")) {
    if (focus) menu.querySelector("input,button")?.focus();
    return;
  }
  submenuAnchor = anchor;
  submenuRequest?.abort();
  submenuRequest = new AbortController();
  if (children.playlistTarget)
    renderPlaylistSubmenu(menu, children.playlistTarget, submenuRequest.signal);
  else menuItems(menu, children);
  menu.showPopover();
  positionSubmenu(menu, anchor);
  if (focus) menu.querySelector("input,button")?.focus();
}

function positionSubmenu(menu, anchor) {
  const rect = anchor.getBoundingClientRect();
  menu.style.left = `${Math.max(8, rect.right + menu.offsetWidth + 4 <= innerWidth ? rect.right + 4 : rect.left - menu.offsetWidth - 4)}px`;
  menu.style.top = `${Math.max(8, Math.min(rect.top, innerHeight - menu.offsetHeight - 8))}px`;
}

function radioMenu(row, kind = "track") {
  const title = {
    track: tr("Track radio"),
    album: tr("Album radio"),
    artist: tr("Artist radio"),
    album_artist: tr("Artist radio"),
    playlist: tr("Playlist radio"),
  }[kind];
  return [
    title,
    null,
    "radio",
    [
      [tr("Play"), () => playRadio(row, kind, "replace")],
      [tr("Play Next"), () => playRadio(row, kind, "next")],
      [tr("Play Later"), () => playRadio(row, kind, "append")],
    ],
  ];
}

function playlistMenu(target) {
  return [
    tr("Add to Playlist"),
    null,
    "add-playlist",
    { playlistTarget: target },
  ];
}

async function goToMenu(row, kind = "track", metadata) {
  if (kind === "artist")
    return [
      tr("Go to"),
      null,
      "go-to",
      [
        [
          tr("Go to {artist}", { artist: row.name }),
          () => openMediaLink("artists", row),
          "artists",
        ],
      ],
    ];
  metadata ??= await api(`/media?${new URLSearchParams({ uri: row.uri })}`);
  const items = metadata.artists.map((artist) => [
    tr("Go to {artist}", { artist: artist.name }),
    () => openMediaLink("artists", artist),
    "artists",
  ]);
  const album = kind === "album" ? row : metadata.album;
  if (album)
    items.push([
      tr("Go to Album"),
      () => openMediaLink("albums", album),
      "albums",
    ]);
  return items.length ? [tr("Go to"), null, "go-to", items] : null;
}

function renderPlaylistSubmenu(menu, target, signal) {
  menu.replaceChildren();
  const search = el("input");
  search.type = "search";
  search.placeholder = tr("Search...");
  search.setAttribute("aria-label", tr("Search playlists"));
  const scroll = el("div", "playlist-menu-scroll"),
    window = el("div");
  scroll.append(window);
  menu.append(search, scroll);
  menu.append(
    button(
      tr("New Playlist"),
      () => {
        closeMenus();
        openName(null, target);
      },
      "add",
      "menu-item",
    ),
  );
  menu.lastChild.append(document.createTextNode(tr("New Playlist")));
  let offset = 0,
    request = 0,
    timer;
  const load = async () => {
    const turn = ++request,
      start = offset;
    const params = new URLSearchParams({
      offset: start,
      limit: 64,
      sort: "title",
      q: search.value,
    });
    if (state.source) params.set("source", state.source);
    const result = await api(`/playlists?${params}`, "GET", undefined, signal);
    if (signal.aborted || turn !== request) return;
    const focused = document.activeElement?.dataset.playlistId;
    window.replaceChildren();
    window.style.paddingTop = `${start * 32}px`;
    window.style.paddingBottom =
      result.playlists.length === 64 ? "1024px" : "0px";
    for (const row of result.playlists) {
      const item = button(
        row.name,
        async () => {
          await api("/playlists/entries", "POST", {
            id: row.id,
            ...target,
            source: state.source,
            folder: state.selectedLibrary,
          });
          closeMenus();
        },
        null,
        "menu-item",
      );
      item.disabled = !row.writable;
      item.dataset.playlistId = String(row.id);
      item.setAttribute("role", "menuitem");
      window.append(item);
      if (focused === String(row.id)) item.focus({ preventScroll: true });
    }
    if (submenuAnchor) positionSubmenu(menu, submenuAnchor);
  };
  search.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      offset = 0;
      scroll.scrollTop = 0;
      run(load);
    }, 150);
  });
  scroll.addEventListener("scroll", () => {
    const next = Math.max(0, Math.floor(scroll.scrollTop / 32 / 32) * 32);
    if (next !== offset) {
      offset = next;
      clearTimeout(timer);
      timer = setTimeout(() => run(load), 60);
    }
  });
  run(load);
}

let renamePlaylist = null;

let newPlaylistUris = { uris: [] };

function openName(playlist = null, uris = []) {
  renamePlaylist = playlist;
  newPlaylistUris = Array.isArray(uris) ? { uris } : uris;
  $("name-title").textContent = playlist
    ? tr("Rename Playlist")
    : tr("New Playlist");
  $("playlist-name").value = playlist?.name || "";
  $("name-dialog").showModal();
  $("playlist-name").focus();
}

function init() {
  for (const id of ["track-menu", "context-submenu"]) {
    $(id).addEventListener("keydown", (event) => {
      if (event.key === "ArrowLeft" && id === "context-submenu") {
        event.preventDefault();
        $(id).hidePopover();
        submenuAnchor?.focus();
        return;
      }
      if (!["ArrowDown", "ArrowUp"].includes(event.key)) return;
      event.preventDefault();
      const items = [...$(id).querySelectorAll("button:not(:disabled)")],
        index = items.indexOf(document.activeElement);
      items[
        (index + (event.key === "ArrowDown" ? 1 : -1) + items.length) %
          items.length
      ]?.focus();
    });
  }
  document.addEventListener("pointerdown", (event) => {
    if (
      $("track-menu").matches(":popover-open") &&
      !event.target.closest("#track-menu,#context-submenu")
    )
      closeMenus();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && $("track-menu").matches(":popover-open")) {
      event.preventDefault();
      closeMenus();
      menuAnchor?.focus();
    }
  });
  $("name-form").addEventListener("submit", (event) => {
    event.preventDefault();
    run(async () => {
      const name = $("playlist-name").value.trim();
      if (!name) return;
      await api(
        "/playlists",
        renamePlaylist ? "PATCH" : "POST",
        renamePlaylist
          ? { id: renamePlaylist.id, name }
          : { name, ...newPlaylistUris },
      );
      $("name-dialog").close();
      await loadView();
    });
  });
}

export {
  goToMenu,
  closeMenus,
  openName,
  playRadio,
  playlistMenu,
  radioMenu,
  showMenu,
  init,
};
