import { tr } from "./ui.js";
import {
  releaseCovers,
  $,
  button,
  el,
  icon,
  notice,
  run,
  showFavorite,
  cover,
} from "./ui.js";

import { api, fragment, session, state } from "./connection.js";

import {
  closeMenus,
  openName,
  playlistMenu,
  radioMenu,
  showMenu,
  playRadio,
  goToMenu,
} from "./menus.js";

import { renderSources, sources } from "./sources.js";

const titles = {
  home: tr("Home"),
  favorites: tr("Favorites"),
  genres: tr("Genres"),
  tracks: tr("Tracks"),
  albums: tr("Albums"),
  playlists: tr("Playlists"),
  artists: tr("Artists"),
  "smart-playlists": tr("Smart Playlists"),
  sources: tr("Sources"),
};

let viewRequest = null;

let route = "home";

let detail = null;

let offset = 0;

let searchTimer;

const rowsByNode = new WeakMap();
function mediaRow(node) {
  if (!rowsByNode.has(node)) rowsByNode.set(node, JSON.parse(node.dataset.row));
  return rowsByNode.get(node);
}

const pageSize = 48;

let loadMore = async () => {};
let readRange = async () => [];
let selectionRequest;
let selectionPending = Promise.resolve();

async function loadLibrary(path, parameters, signal) {
  const content = $("content"),
    pages = [];
  let busy = false,
    end = false;
  readRange = async (start, end, selectionSignal) => {
    const result = [];
    for (let offset = start; offset <= end; offset += pageSize) {
      const query = new URLSearchParams(parameters);
      query.set("offset", offset);
      query.set("limit", Math.min(pageSize, end - offset + 1));
      const page = await api(
        `${path}?${query}`,
        "GET",
        undefined,
        selectionSignal,
      );
      result.push(...(page.tracks || page.entries));
    }
    return result;
  };
  const fill = () => {
    if (signal.aborted || busy) return;
    if (
      content.scrollHeight - content.scrollTop - content.clientHeight <
        content.clientHeight / 2 &&
      !end
    )
      run(() => loadMore());
    else if (
      content.scrollTop < content.clientHeight / 2 &&
      pages[0]?.start > 0
    )
      run(() => loadMore(-1));
  };
  const syncRows = () => {
    state.visibleItems = [...content.querySelectorAll("[data-row]")].map(
      mediaRow,
    );
    const rows = content.querySelectorAll("tr[data-index]");
    if (rows.length)
      content.style.setProperty(
        "--index-width",
        `${Math.max(3, String(Number(rows[rows.length - 1].dataset.index) + 1).length)}ch`,
      );
  };
  loadMore = async (direction = 1) => {
    if (busy || signal.aborted || (direction > 0 && end)) return;
    const start =
      direction < 0
        ? pages[0].start - pageSize
        : (pages.at(-1)?.start ?? -pageSize) + pageSize;
    if (start < 0) return;
    busy = true;
    const node = el("section", "library-page");
    node.setAttribute("hx-sync", "this:replace");
    node.setAttribute("aria-busy", "true");
    const lifetime = new AbortController();
    const abort = () => lifetime.abort();
    signal.addEventListener("abort", abort, { once: true });
    const remove = () => {
      lifetime.abort();
      signal.removeEventListener("abort", abort);
      node.remove();
      releaseCovers();
    };
    if (direction < 0) pages[0].node.before(node);
    else content.append(node);
    try {
      const query = new URLSearchParams(parameters);
      query.set("offset", start);
      await fragment(`/api${path}?${query}`, node, lifetime.signal);
      const count = Number(node.querySelector("[data-count]").dataset.count);
      const table = node.querySelector(".track-list");
      if (table) {
        content.setAttribute("role", "grid");
        content.setAttribute("aria-label", tr("Tracks"));
        content.setAttribute("aria-multiselectable", "true");
        if (!content.querySelector(".library-header")) {
          const header = table.cloneNode(false);
          header.classList.add("library-header");

          header.append(table.querySelector("thead").cloneNode(true));
          content.prepend(header);
        }
      }
      if (direction > 0) end = count < pageSize;
      if (!count && pages.length) {
        remove();
        return;
      }
      const page = { node, start, remove };
      if (direction < 0) {
        pages.unshift(page);
        content.scrollTop += node.getBoundingClientRect().height;
      } else pages.push(page);
      syncRows();
      if (node.querySelector(".track-list"))
        bindTracks(node, start, lifetime.signal);
      else {
        bindCards(node);
        loadCardCovers(node, lifetime.signal);
      }
      while (pages.length > 3) {
        const candidate = direction > 0 ? pages[0] : pages.at(-1);
        const rect = candidate.node.getBoundingClientRect(),
          viewport = content.getBoundingClientRect();
        if (
          direction > 0
            ? rect.bottom > viewport.top
            : rect.top < viewport.bottom
        )
          break;
        if (candidate.node.contains(document.activeElement))
          content.focus({ preventScroll: true });
        candidate.remove();
        if (direction > 0) {
          pages.shift();
          content.scrollTop -= rect.height;
        } else {
          pages.pop();
          end = false;
        }
      }
      syncRows();
      node.setAttribute("aria-busy", "false");
      requestAnimationFrame(fill);
    } catch (error) {
      remove();
      throw error;
    } finally {
      busy = false;
    }
  };
  await loadMore();
  content.addEventListener("scroll", fill, { signal, passive: true });
  const resize = new ResizeObserver(fill);
  resize.observe(content);
  signal.addEventListener("abort", () => resize.disconnect(), { once: true });
  fill();
}

function navigate(next, selected = null) {
  route = next;
  detail = selected;
  offset = 0;
  $("search").value = "";
  $("descending").setAttribute("aria-pressed", "false");
  $("descending").firstElementChild.dataset.icon = "sort-ascending";
  document.querySelectorAll("[data-route]").forEach((node) => {
    if (node.dataset.route === route) node.setAttribute("aria-current", "page");
    else node.removeAttribute("aria-current");
  });
  $("navigation").classList.remove("open");
  setSort();
  run(loadView);
}

function setSort() {
  const tracks = [
    ["title", tr("Title")],
    ["artist", tr("Artist")],
    ["album", tr("Album")],
    ["track_number", tr("Track number")],
    ["year", tr("Year")],
    ["duration", tr("Duration")],
  ];
  const lists = {
    albums: [
      ["title", tr("Title")],
      ["album_artist", tr("Artist")],
      ["year", tr("Year")],
      ["date_added", tr("Date added")],
    ],
    artists: [
      ["title", tr("Name")],
      ["album_count", tr("Albums")],
      ["track_count", tr("Tracks")],
      ["last_played", tr("Last played")],
    ],
    playlists: [
      ["position", tr("Playlist order")],
      ["title", tr("Name")],
      ["track_count", tr("Tracks")],
      ["duration", tr("Duration")],
    ],
    "smart-playlists": [
      ["position", tr("Playlist order")],
      ["title", tr("Name")],
      ["track_count", tr("Tracks")],
      ["duration", tr("Duration")],
    ],
  };
  const options = detail
    ? route === "smart-playlists"
      ? [["definition", tr("Playlist order")]]
      : route === "playlists"
        ? [
            ["position", tr("Playlist order")],
            ["title", tr("Title")],
            ["artist", tr("Artist")],
            ["album", tr("Album")],
          ]
        : tracks
    : lists[route] || tracks;
  $("sort").replaceChildren(
    ...options.map(([value, label]) => new Option(label, value)),
  );
  $("sort").hidden = $("descending").hidden =
    route === "smart-playlists" && !!detail;
  if (route === "albums" && detail) $("sort").value = "track_number";
}

function query() {
  const params = new URLSearchParams({
    offset,
    limit: pageSize,
    q: $("search").value,
    sort: $("sort").value,
    descending: $("descending").getAttribute("aria-pressed"),
  });
  if (state.source) params.set("source", state.source);
  if (state.selectedLibrary) params.set("folder", state.selectedLibrary);
  if (route === "favorites") params.set("favorites", "true");
  if (detail) params.set("id", detail.id);
  if (detail?.album_artists) params.set("album_artists", "true");
  return params;
}

function empty(title, description, action) {
  const node = el("div", "empty");
  node.append(
    icon(route === "sources" ? "server" : route),
    el("h2", "", title),
    el("p", "", description),
  );
  if (action)
    node.append(
      button(
        tr("Add Source"),
        () => $("source-dialog").showModal(),
        null,
        "primary",
      ),
    );
  $("content").replaceChildren(node);
}

async function loadView() {
  for (const name of ["role", "aria-label", "aria-multiselectable"])
    $("content").removeAttribute(name);
  selectionRequest?.abort();
  selectionPending = Promise.resolve();
  loadMore = async () => {};
  disposeHome();
  closeMenus();
  viewRequest?.abort();
  viewRequest = new AbortController();
  const signal = AbortSignal.any([session.signal, viewRequest.signal]);
  $("content").setAttribute("aria-busy", "true");
  $("content").inert = true;
  $("content").replaceChildren(el("p", "empty-note", tr("Loading...")));
  releaseCovers();
  $("title").textContent = detail?.title || detail?.name || titles[route];
  $("source-name").textContent =
    sources.find((item) => item.id === state.source)?.name || tr("Library");
  $("back").hidden = !detail;
  $("library-toolbar").hidden = ["sources", "home"].includes(route);
  $("play-collection").hidden =
    route === "sources" ||
    (["playlists", "albums", "artists", "smart-playlists"].includes(route)
      ? !detail
      : !state.source);
  $("new-playlist").hidden = route !== "playlists" || !!detail;
  $("search").placeholder = tr("Search");
  try {
    if (route === "sources") {
      renderSources(await api("/sources", "GET", undefined, signal));
      return;
    }
    if (!state.source && !["playlists", "smart-playlists"].includes(route)) {
      empty(
        tr("Your music belongs here"),
        tr("Add a music source to start listening."),
        true,
      );
      return;
    }
    const params = query();
    let path = route === "favorites" ? "/tracks" : `/${route}`;
    if (
      ["albums", "artists", "genres", "smart-playlists"].includes(route) &&
      detail
    )
      path += "/tracks";
    if (route === "playlists" && detail) path += "/entries";
    $("content").replaceChildren();
    $("content").scrollTop = 0;
    selectedTracks.clear();
    selectionAnchor = 0;
    if (route === "home") {
      await fragment(`/api${path}?${params}`, $("content"), signal);
      bindHome(signal);
    } else await loadLibrary(path, params, signal);
  } catch (error) {
    if (!signal.aborted) empty(tr("Could not load this page"), error.message);
    throw error;
  } finally {
    if (!signal.aborted) {
      $("content").setAttribute("aria-busy", "false");
      $("content").inert = false;
    }
  }
}

async function setFavorite(rows, kind = "track") {
  const favorite = !rows.every((row) => row.favorite);
  await Promise.all(
    rows.map(async (row) => {
      const result = await api("/favorite", "POST", {
        kind,
        uri: row.uri,
        favorite,
      });
      row.favorite = result.favorite;
      for (const track of [...state.visibleItems, ...selectedTracks.values()])
        if (track.uri === row.uri) track.favorite = result.favorite;
      for (const node of document.querySelectorAll("[data-favorite-uri]")) {
        if (node.dataset.favoriteUri === row.uri) {
          showFavorite(node, result.favorite);
        }
      }
    }),
  );
  if (route === "favorites") await loadView();
}

async function goToMedia(row, kind, anchor) {
  const metadata = await api(`/media?${new URLSearchParams({ uri: row.uri })}`);
  const links =
    kind === "albums" ? [metadata.album].filter(Boolean) : metadata.artists;
  const open = (link) => openMediaLink(kind, link);
  if (links.length === 1) open(links[0]);
  else if (links.length)
    showMenu(
      links.map((link) => [link.name || link.title, () => open(link)]),
      anchor,
    );
  else
    notice(
      kind === "albums" ? tr("Album unavailable") : tr("Artist unavailable"),
    );
}

function openMediaLink(kind, link) {
  if (link.source && link.source !== state.source) {
    state.source = link.source;
    state.selectedLibrary = null;
  }
  navigate(kind, link);
}

let selectedTracks = new Map();

let selectionAnchor = 0;

function selectTrack(index, event = {}) {
  selectionRequest?.abort();
  selectionRequest = new AbortController();
  const signal = AbortSignal.any([viewRequest.signal, selectionRequest.signal]);
  selectionPending = updateSelection(index, event, signal);
  run(() => selectionPending);
  return selectionPending;
}

async function updateSelection(index, event, signal) {
  const rows = [...$("content").querySelectorAll("tr[data-index]")];
  if (event.shiftKey) {
    const start = Math.min(index, selectionAnchor),
      end = Math.max(index, selectionAnchor);
    const mounted = rows.filter(
      (node) =>
        Number(node.dataset.index) >= start &&
        Number(node.dataset.index) <= end,
    );
    const range =
      mounted.length === end - start + 1
        ? mounted.map(mediaRow)
        : await readRange(start, end, signal);
    signal.throwIfAborted();
    if (!event.ctrlKey && !event.metaKey) selectedTracks.clear();
    range.forEach((row, offset) => selectedTracks.set(start + offset, row));
  } else {
    const row = mediaRow(
      rows.find((node) => Number(node.dataset.index) === index),
    );
    if (event.ctrlKey || event.metaKey) {
      if (selectedTracks.has(index)) selectedTracks.delete(index);
      else selectedTracks.set(index, row);
    } else selectedTracks = new Map([[index, row]]);
    selectionAnchor = index;
  }
  for (const node of rows) {
    const selected = selectedTracks.has(Number(node.dataset.index));
    node.classList.toggle("selected", selected);
    node.setAttribute("aria-selected", String(selected));
  }
}

function selectedRows() {
  return [...selectedTracks].sort(([a], [b]) => a - b).map(([, row]) => row);
}

async function playSelection(row) {
  await selectionPending;
  const selected = selectedRows();
  return selected.length > 1
    ? playUris(selected.map((track) => track.uri))
    : playCollection(row);
}

async function showTrackMenu(index, row, anchor, event) {
  if (!selectedTracks.has(index)) await selectTrack(index);
  else await selectionPending;
  const rows = selectedRows(),
    uris = rows.map((row) => row.uri);
  const actions = [
    [tr("Play"), () => playSelection(row)],
    [tr("Play Next"), () => playUris(uris, "next")],
    [tr("Play Later"), () => playUris(uris, "append")],
    radioMenu(row),
    null,
    playlistMenu({ uris }),
    [
      rows.every((row) => row.favorite)
        ? tr("Remove from Favorites")
        : tr("Add to Favorites"),
      () => setFavorite(rows),
    ],
  ];
  if (route === "playlists" && detail?.writable)
    actions.push([
      tr("Remove from Playlist"),
      async () => {
        await api("/playlists/entries", "DELETE", {
          id: detail.id,
          entries: rows.map((row) => row.id),
        });
        await loadView();
      },
    ]);
  const destination = await goToMenu(row);
  if (destination) actions.push(null, destination);
  showMenu(actions, anchor, event);
}

function bindTracks(host, start, signal) {
  const nodes = [...host.querySelectorAll("tbody tr")];
  for (const [position, tr] of nodes.entries()) {
    const index = start + position,
      row = mediaRow(tr);
    tr.dataset.index = index;
    if (selectedTracks.has(index)) selectedTracks.set(index, row);
    const selected = selectedTracks.has(index);
    tr.classList.toggle("selected", selected);
    tr.setAttribute("aria-selected", String(selected));
    tr.addEventListener(
      "click",
      (event) => {
        const button = event.target.closest("[data-action]");
        if (button)
          run(() => {
            const action = button.dataset.action;
            if (action === "favorite") return setFavorite([row]);
            if (action === "menu") return showTrackMenu(index, row, button);
            return goToMedia(row, action, button);
          });
        else if (event.detail < 2) selectTrack(index, event);
      },
      { signal },
    );
    tr.addEventListener(
      "dblclick",
      (event) => {
        if (!event.target.closest("button")) run(() => playSelection(row));
      },
      { signal },
    );
    tr.addEventListener(
      "contextmenu",
      (event) => {
        event.preventDefault();
        run(() => showTrackMenu(index, row, tr, event));
      },
      { signal },
    );
    tr.addEventListener(
      "keydown",
      (event) => {
        if (event.target !== tr) return;
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          if (event.key === "Enter") run(() => playSelection(row));
          else selectTrack(index, event);
        } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          run(async () => {
            const direction = event.key === "ArrowDown" ? 1 : -1;
            let next = $("content").querySelector(
              `tr[data-index="${index + direction}"]`,
            );
            if (!next) {
              await loadMore(direction);
              if (signal.aborted) return;
              next = $("content").querySelector(
                `tr[data-index="${index + direction}"]`,
              );
            }
            if (next) {
              selectTrack(index + direction, event);
              next.focus();
            }
          });
        } else if ((event.ctrlKey || event.metaKey) && event.key === "a") {
          event.preventDefault();
          const visible = $("content").querySelectorAll("tr[data-index]");
          selectionAnchor = Number(visible[0].dataset.index);
          selectTrack(Number(visible[visible.length - 1].dataset.index), {
            shiftKey: true,
          });
        } else if (event.key === "F10" && event.shiftKey) {
          event.preventDefault();
          run(() => showTrackMenu(index, row, tr));
        }
      },
      { signal },
    );
  }
  markCurrent();
}

function playUris(uris, mode = "replace") {
  return api("/queue", "POST", { uris, mode });
}

function playCollection(track = null) {
  if (route === "smart-playlists" && detail)
    return api("/queue/smart-playlist", "POST", {
      id: detail.id,
      source: state.source || null,
      folder: state.selectedLibrary,
      q: $("search").value,
      mode: "replace",
      anchor_uri: track?.uri || null,
    });
  const options = {
    source: state.source,
    q: $("search").value,
    sort: $("sort").value,
    descending: $("descending").getAttribute("aria-pressed") === "true",
    mode: "replace",
    anchor_uri: track?.uri || null,
    favorites: route === "favorites",
    folder: state.selectedLibrary,
  };
  if (detail && ["albums", "artists", "playlists", "genres"].includes(route))
    return api(
      `/queue/${{ albums: "album", artists: "artist", playlists: "playlist", genres: "genre" }[route]}`,
      "POST",
      {
        ...options,
        id: detail.id,
        anchor_entry: route === "playlists" ? track?.id : null,
        album_artists: !!detail.album_artists,
      },
    );
  return api("/queue/source", "POST", { ...options, source: state.source });
}

function markCurrent() {
  document
    .querySelectorAll("tr[data-uri]")
    .forEach((row) =>
      row.classList.toggle(
        "current",
        row.dataset.uri === state.playback?.current?.track?.uri,
      ),
    );
}

function init() {
  document
    .querySelectorAll("[data-route]")
    .forEach((node) =>
      node.addEventListener("click", () => navigate(node.dataset.route)),
    );
  $("back").addEventListener("click", () =>
    navigate(route === "genres" ? "home" : route),
  );
  $("search").addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      offset = 0;
      run(loadView);
    }, 200);
  });
  $("sort").addEventListener("change", () => {
    offset = 0;
    run(loadView);
  });
  $("descending").addEventListener("click", () => {
    $("descending").setAttribute(
      "aria-pressed",
      String($("descending").getAttribute("aria-pressed") !== "true"),
    );
    $("descending").firstElementChild.dataset.icon =
      $("descending").getAttribute("aria-pressed") === "true"
        ? "sort-descending"
        : "sort-ascending";
    offset = 0;
    run(loadView);
  });
  $("refresh").addEventListener("click", () =>
    run(async () => {
      if (state.source)
        await api("/sources/refresh", "POST", { id: state.source });
      await loadView();
    }),
  );
  $("play-collection").addEventListener("click", () => run(playCollection));
  $("new-playlist").addEventListener("click", () => openName());
  setSort();
}

function bindCards(host) {
  for (const card of host.querySelectorAll("[data-row][data-kind]")) {
    const row = mediaRow(card),
      kind = card.dataset.kind;
    const open = () => {
      if (kind !== "track")
        return navigate(
          {
            album: "albums",
            artist: "artists",
            playlist: "playlists",
            "smart-playlist": "smart-playlists",
            genre: "genres",
          }[kind],
          row,
        );
      host
        .querySelectorAll(".album-card.selected")
        .forEach((node) => node.classList.remove("selected"));
      card.classList.add("selected");
    };
    const play = (mode) =>
      kind === "track"
        ? playUris([row.uri], mode)
        : api(`/queue/${kind}`, "POST", {
            id: row.id,
            source: state.source || null,
            folder: state.selectedLibrary,
            mode,
          });
    const menu = async (anchor, event) => {
      const actions = [
        [tr("Play"), () => play("replace")],
        [tr("Play Next"), () => play("next")],
        [tr("Play Later"), () => play("append")],
      ];
      if (kind !== "smart-playlist") actions.push(radioMenu(row, kind));
      actions.push(
        null,
        playlistMenu(
          kind === "track"
            ? { uris: [row.uri] }
            : { selection: { kind: kind.replaceAll("-", "_"), id: row.id } },
        ),
      );
      if (["track", "album", "artist"].includes(kind))
        actions.push([
          row.favorite ? tr("Remove from Favorites") : tr("Add to Favorites"),
          () => setFavorite([row], kind),
        ]);
      if (row.writable)
        actions.push(
          [tr("Rename Playlist"), () => openName(row)],
          [
            tr("Delete Playlist"),
            async () => {
              if (
                confirm(tr('Delete "{name}"?', { name: row.title || row.name }))
              ) {
                await api(`/playlists?id=${row.id}`, "DELETE");
                await loadView();
              }
            },
          ],
        );
      if (["track", "album", "artist"].includes(kind)) {
        const destination = await goToMenu(row, kind);
        if (destination) actions.push(null, destination);
      }
      showMenu(actions, anchor, event);
    };
    card.addEventListener("click", (event) => {
      const button = event.target.closest("[data-action]");
      if (!button) return;
      const action = button.dataset.action;
      run(() => {
        if (action === "open") return open();
        if (action === "artists" || action === "albums")
          return goToMedia(row, action, button);
        if (action === "favorite") return setFavorite([row], kind);
        if (action === "radio") return playRadio(row, kind);
        if (action === "menu") return menu(button);
        return play(action);
      });
      if (event.detail && button.closest(".cover-controls")) {
        button.blur();
      }
    });
    if (kind === "track") {
      card.addEventListener("dblclick", (event) => {
        if (event.target.closest(".cover-open, .collection-title"))
          run(() => play("replace"));
      });
      card.addEventListener("keydown", (event) => {
        if (
          event.key === "Enter" &&
          event.target.closest('[data-action="open"]')
        ) {
          event.preventDefault();
          run(() => play("replace"));
        }
      });
    }
    if (kind !== "genre")
      card.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        run(() => menu(card, event));
      });
  }
}

const requestedCovers = new WeakSet();

function loadCardCovers(host, signal) {
  const cards = [...host.querySelectorAll(".album-card:not([hidden])")].filter(
    (card) => !requestedCovers.has(card),
  );
  cards.forEach((card) => requestedCovers.add(card));
  let next = 0;
  for (let worker = 0; worker < 4; worker++)
    run(async () => {
      while (next < cards.length && !signal.aborted) {
        const card = cards[next++],
          row = JSON.parse(card.dataset.row),
          kind = card.dataset.kind;
        const query =
          kind === "playlist"
            ? { playlist: row.id }
            : kind === "smart-playlist"
              ? {
                  smart_playlist: row.id,
                  ...(state.source ? { source: state.source } : {}),
                  ...(state.selectedLibrary
                    ? { folder: state.selectedLibrary }
                    : {}),
                }
              : row.uri;
        await cover(card.querySelector(".cover"), query, signal);
      }
    });
  releaseCovers();
}

let homeObserver;

function bindHome(signal) {
  const layouts = [];
  bindCards($("content"));
  const syncRows = () => {
    state.visibleItems = [...$("content").querySelectorAll("[data-row]")].map(
      mediaRow,
    );
  };
  syncRows();
  for (const section of $("content").querySelectorAll(".home-section")) {
    const body = section.querySelector(".home-row");
    if (!body) {
      loadCardCovers(section, signal);
      continue;
    }
    let offset = 0;
    const layout = () => {
      const columns = Math.max(
        1,
        Math.min(
          Math.ceil(body.clientWidth / 210),
          Math.max(1, Math.floor(body.clientWidth / 138)),
        ),
      );
      const cards = body.querySelectorAll(".album-card");
      offset = Math.min(
        offset,
        Math.max(0, Math.floor((cards.length - 1) / columns) * columns),
      );
      cards.forEach((card, index) => {
        card.hidden = index < offset || index >= offset + columns;
      });
      body.firstElementChild.style.gridTemplateColumns = `repeat(${columns},minmax(0,1fr))`;
      section.querySelector('[data-action="previous-home"]').disabled =
        offset === 0;
      section.querySelector('[data-action="next-home"]').disabled =
        offset + columns >= cards.length;
      loadCardCovers(section, signal);
      return columns;
    };
    section.querySelector('[data-action="previous-home"]').onclick = () => {
      offset = Math.max(0, offset - layout());
      layout();
    };
    section.querySelector('[data-action="next-home"]').onclick = () => {
      offset += layout();
      layout();
    };
    const refresh = section.querySelector('[data-action="refresh-home"]');
    if (refresh)
      refresh.onclick = () =>
        run(async () => {
          refresh.disabled = true;
          try {
            await api(
              "/home",
              "POST",
              { source: state.source, block: section.dataset.block },
              signal,
            );
            const params = new URLSearchParams({
              source: state.source,
              block: section.dataset.block,
            });
            if (state.selectedLibrary)
              params.set("folder", state.selectedLibrary);
            await fragment(
              `/api/home?${params}`,
              body,
              signal,
              ".home-row > .album-grid",
            );
            bindCards(body);
            syncRows();
            offset = 0;
            layout();
          } finally {
            if (!signal.aborted) refresh.disabled = false;
          }
        });
    layout();
    layouts.push(layout);
  }
  homeObserver = new ResizeObserver(() =>
    layouts.forEach((layout) => layout()),
  );
  homeObserver.observe($("content"));
}

function disposeHome() {
  homeObserver?.disconnect();
  homeObserver = null;
}

export {
  openMediaLink,
  init,
  goToMedia,
  loadView,
  markCurrent,
  navigate,
  playUris,
  route,
  setFavorite,
  viewRequest,
  disposeHome,
};
