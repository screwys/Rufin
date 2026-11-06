import { tr } from "./ui.js";
import { api, state } from "./connection.js";

import { loadView, openMediaLink } from "./library.js";

import { $, button, el, icon, run, notice } from "./ui.js";
import { sourceIcon } from "./sources.js";

// Plays the radio a row's menu offers.
function playRadio(seed, mode = "replace") {
  return api("/queue/radio", "POST", { ...seed, mode });
}

let menuAnchor = null;

let submenuAnchor = null;

let submenuRequest = null;

function closeMenus() {
  menuAnchor
    ?.closest(".album-card, .pin-row")
    ?.classList.remove("menu-open", "controls-visible");
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
        [tr("Play")]: "rufin-media-playback-start-symbolic",
        [tr("Play Next")]: "rufin-mail-forward-symbolic",
        [tr("Play Later")]: "rufin-go-last-symbolic",
        [tr("Remove from Queue")]: "rufin-list-remove-symbolic",
        [tr("Remove from Playlist")]: "rufin-list-remove-symbolic",
        [tr("Edit")]: "rufin-document-edit-symbolic",
        [tr("Delete Playlist")]: "rufin-process-stop-symbolic",
        [tr("Favorite")]: "rufin-heart-outline-symbolic",
        [tr("Add to Favorites")]: "rufin-heart-outline-symbolic",
        [tr("Remove from Favorites")]: "rufin-heart-filled-symbolic",
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
      item.append(icon("rufin-go-next-symbolic"));
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
  anchor.closest(".album-card, .pin-row")?.classList.add("menu-open");
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

function radioMenu(seed) {
  const title = {
    track: tr("Track radio"),
    album: tr("Album radio"),
    artist: tr("Artist radio"),
    album_artist: tr("Artist radio"),
    genre: tr("Genre radio"),
    playlist: tr("Playlist radio"),
  }[seed.kind];
  return [
    title,
    null,
    "rufin-audio-only-symbolic",
    [
      [tr("Play"), () => playRadio(seed, "replace")],
      [tr("Play Next"), () => playRadio(seed, "next")],
      [tr("Play Later"), () => playRadio(seed, "append")],
    ],
  ];
}

function playlistMenu(target) {
  return [
    tr("Add to Playlist"),
    null,
    "rufin-playlists-compact-symbolic",
    { playlistTarget: target },
  ];
}

// The album and artists a row links to: track rows carry them, albums ask the server in the
// source and music folder of the page the album opens.
async function mediaLinks(row, kind = "track") {
  if (kind !== "track")
    return api(`/media?${new URLSearchParams({ uri: row.uri, page: JSON.stringify(row.page) })}`);
  return row.media;
}

async function goToMenu(row, kind = "track") {
  if (kind === "artist")
    return [
      tr("Go to"),
      null,
      "rufin-external-link-compact-symbolic",
      [
        [
          tr("Go to {artist}", { artist: row.name }),
          () => openMediaLink(row),
          "rufin-artists-symbolic",
        ],
      ],
    ];
  const metadata = await mediaLinks(row, kind);
  const items = metadata.artists.map((artist) => [
    tr("Go to {artist}", { artist: artist.name }),
    () => openMediaLink(artist),
    "rufin-artists-symbolic",
  ]);
  const album = kind === "album" ? row : metadata.album;
  if (album)
    items.push([
      tr("Go to Album"),
      () => openMediaLink(album),
      "rufin-albums-symbolic",
    ]);
  return items.length ? [tr("Go to"), null, "rufin-external-link-compact-symbolic", items] : null;
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
        return openName(null, target);
      },
      "rufin-list-add-symbolic",
      "menu-item",
    ),
  );
  menu.lastChild.append(document.createTextNode(tr("New Playlist")));
  let playlists = [],
    timer;
  const show = () => {
    const focused = document.activeElement?.dataset.playlistId;
    const query = search.value.toLocaleLowerCase();
    window.replaceChildren();
    for (const row of playlists.filter((row) => row.name.toLocaleLowerCase().includes(query))) {
      const item = button(
        row.name,
        async () => {
          await api("/playlists/entries", "POST", {
            id: row.id,
            source: state.source,
            folder: state.selectedLibrary,
            ...target,
          });
          closeMenus();
        },
        null,
        "menu-item",
      );
      item.dataset.playlistId = String(row.id);
      item.setAttribute("role", "menuitem");
      window.append(item);
      if (focused === String(row.id)) item.focus({ preventScroll: true });
    }
    if (submenuAnchor) positionSubmenu(menu, submenuAnchor);
  };
  search.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(show, 150);
  });
  run(async () => {
    const params = new URLSearchParams(state.source ? { source: state.source } : {});
    const result = await api(`/playlists/destinations?${params}`, "GET", undefined, signal);
    if (signal.aborted) return;
    playlists = result.playlists;
    show();
  });
}

let editingPlaylist = null;
let transferPlaylist = null;

async function openPlaylistTransfer(playlist = null) {
  transferPlaylist = playlist;
  $("playlist-transfer-title").textContent = playlist ? tr("Export Playlist") : tr("Import Playlist");
  $("playlist-transfer-path").value = playlist ? `${playlist.name}.m3u8` : "";
  for (const name of ["link", "mode", "format"]) $("playlist-transfer-" + name + "-row").hidden = !playlist;
  $("playlist-transfer-link").checked = false;
  if (playlist) $("playlist-transfer-link-row").hidden = !(await api(`/playlists/file?id=${playlist.id}`)).can_link;
  const sources = await api("/sources");
  $("playlist-transfer-source").replaceChildren(new Option(tr("Rufin host"), ""), ...sources.sources.filter(source => ["local", "smb", "webdav"].includes(source.kind)).map(source => new Option(source.name, source.id)));
  $("playlist-transfer-source").value = state.source || "";
  $("playlist-transfer-dialog").showModal();
}

let newPlaylistUris = { uris: [] };
let playlistSource = null;
let playlistPublic = null;

async function openName(playlist = null, uris = []) {
  editingPlaylist = playlist;
  $("name-dialog").classList.toggle("playlist-edit", !!playlist);
  $("playlist-apply").textContent = playlist ? tr("Apply") : tr("Save");
  newPlaylistUris = Array.isArray(uris) ? { uris } : uris;
  $("name-title").textContent = playlist
    ? tr("Edit Playlist")
    : tr("New Playlist");
  $("playlist-name").value = playlist?.name || "";
  $("playlist-owner").hidden = !!playlist;
  $("playlist-file-options").hidden = !playlist;
  $("playlist-public-row").hidden = true;
  $("playlist-public").checked = false;
  playlistPublic = null;
  if (playlist) {
    try {
      const visibility = await api(`/playlists/public?id=${playlist.id}`);
      playlistPublic = visibility.public;
      $("playlist-public-row").hidden = playlistPublic == null;
      $("playlist-public").checked = playlistPublic === true;
    } catch (error) {
      notice(error.message);
    }
  }
  if (playlist) await loadPlaylistFile();
  if (!playlist) {
    const configured = await api("/sources");
    // A playlist made for a source that cannot own one is a Rufin playlist, with no choice.
    playlistSource =
      configured.sources.find(
        (source) => source.id === state.source && source.owns_playlists,
      ) || null;
    $("playlist-owner").hidden = !playlistSource;
    $("playlist-owner").setAttribute(
      "aria-pressed",
      String(!!playlistSource),
    );
    if (playlistSource) showPlaylistOwner();
  }
  $("name-dialog").showModal();
  $("playlist-name").focus();
}

async function loadPlaylistFile() {
  const settings = await api(`/playlists/file?id=${editingPlaylist.id}`);
  const link = settings.link;
  $("playlist-file-options").hidden = !settings.can_link;
  $("playlist-file-path").value = link?.path || "";
  $("playlist-file-name").value = (link?.path || "").split(/[\\/]/).pop();
  $("playlist-file-refresh").checked = link?.auto_refresh ?? true;
  $("playlist-file-save").value = link?.auto_save == null ? "inherit" : link.auto_save ? "on" : "off";
  $("playlist-file-mode").value = link?.path_mode || "automatic";
  $("playlist-file-error").textContent = link?.error || "";
  const sources = await api("/sources");
  $("playlist-file-source").replaceChildren(new Option(tr("Rufin host"), ""), ...sources.sources.filter(source => ["local", "smb", "webdav"].includes(source.kind)).map(source => new Option(source.name, source.id)));
  $("playlist-file-source").value = link?.source_id || "";
  for (const button of document.querySelectorAll("[data-playlist-file]")) button.disabled = button.dataset.playlistFile !== "link" && !link;
}

function showPlaylistOwner() {
  const owner = $("playlist-owner");
  const current = owner.getAttribute("aria-pressed") === "true";
  $("playlist-public-row").hidden = !(current && playlistSource?.supports_playlist_public);
  const image = current
    ? sourceIcon(playlistSource.kind)
    : el("img", "source-icon");
  if (!current) image.src = "/icons/rufin.svg";
  owner.replaceChildren(image);
  owner.title = current
    ? tr(
        "This is owned by {source} and synced to it.",
        { source: playlistSource.name },
      )
    : tr(
        "This is owned by Rufin. It can include entries from all configured sources but it is not synced to remotes.",
      );
  owner.setAttribute("aria-label", owner.title);
}

function playlistEditInput() {
  return { id: editingPlaylist.id, name: $("playlist-name").value.trim(), auto_refresh: $("playlist-file-refresh").checked, auto_save: $("playlist-file-save").value === "inherit" ? null : $("playlist-file-save").value === "on", path_mode: $("playlist-file-mode").value };
}

function init() {
  $("import-playlist").addEventListener("click", () => run(() => openPlaylistTransfer()));
  $("playlist-transfer-form").addEventListener("submit", event => {
    event.preventDefault();
    run(async () => {
      const source = $("playlist-transfer-source").value || null;
      const path = $("playlist-transfer-path").value.trim();
      if (transferPlaylist) {
        const extension = $("playlist-transfer-format").value;
        await api("/playlists/export", "POST", { id: transferPlaylist.id, source, path: path.replace(/\.(m3u8?|pls|xspf)$/i, "") + "." + extension, path_mode: $("playlist-transfer-mode").value, linked: $("playlist-transfer-link").checked });
      } else {
        await api("/playlists/import", "POST", { source, paths: path.split("\n").map(path => path.trim()).filter(Boolean) });
      }
      $("playlist-transfer-dialog").close();
      await loadView();
    });
  });
  for (const button of document.querySelectorAll("[data-playlist-file]")) {
    button.addEventListener("click", () => run(async () => {
      const action = button.dataset.playlistFile;
      const force = button.dataset.force === "true";
      if ((action === "delete" || force) && !confirm(button.textContent.trim())) return;
      const edit = playlistEditInput();
      if (!edit.name) return;
      await api("/playlists/file", "PATCH", edit);
      const input = { id: editingPlaylist.id, action, force, path: action === "rename" ? $("playlist-file-name").value : $("playlist-file-path").value, source: $("playlist-file-source").value || null };
      try { await api("/playlists/file", "POST", input); }
      catch (error) {
        $("playlist-file-error").textContent = error.message;
        const resolution = action === "save" ? tr("Replace file contents") : tr("Reload from file");
        if (!error.conflict || !["save", "reload"].includes(action) || !confirm(`${error.message}\n${resolution}?`)) return;
        await api("/playlists/file", "POST", { ...input, force: true });
      }
      await loadPlaylistFile();
    }));
  }
  $("track-menu").setAttribute("popover", "manual");
  $("playlist-owner").addEventListener("click", () => {
    const owner = $("playlist-owner");
    owner.setAttribute(
      "aria-pressed",
      String(owner.getAttribute("aria-pressed") !== "true"),
    );
    showPlaylistOwner();
  });
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
      const remoteEdit = editingPlaylist && $("playlist-file-options").hidden;
      const changedName = editingPlaylist && name !== editingPlaylist.name;
      const changedPublic = playlistPublic != null && playlistPublic !== $("playlist-public").checked;
      if (remoteEdit && !changedName && !changedPublic) {
        $("name-dialog").close();
        return;
      }
      const result = await api(
        editingPlaylist && !remoteEdit ? "/playlists/file" : "/playlists",
        editingPlaylist ? "PATCH" : "POST",
        editingPlaylist
          ? remoteEdit
            ? { id: editingPlaylist.id, name: changedName ? name : null, public: changedPublic ? $("playlist-public").checked : null }
            : playlistEditInput()
          : {
              name,
              ...newPlaylistUris,
              selection_source: state.source,
              folder: state.selectedLibrary,
              source:
                $("playlist-owner").getAttribute("aria-pressed") === "true"
                  ? playlistSource.id
                  : null,
              public: $("playlist-public-row").hidden ? null : $("playlist-public").checked,
            },
      );
      $("name-dialog").close();
      if (result.settings_error) notice(result.settings_error);
    });
  });
}

export {
  openPlaylistTransfer,
  goToMenu,
  mediaLinks,
  closeMenus,
  openName,
  playRadio,
  playlistMenu,
  radioMenu,
  showMenu,
  init,
};
