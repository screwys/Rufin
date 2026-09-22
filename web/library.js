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
  coverGroup,
  bindHoverControls,
} from "./ui.js";

import { api, fragment, session, state } from "./connection.js";

import {
  closeMenus,
  openName,
  openPlaylistTransfer,
  playlistMenu,
  radioMenu,
  showMenu,
  playRadio,
  goToMenu,
} from "./menus.js";

import { renderSources, sources } from "./sources.js";
import { pinAction, refreshPins, markPins } from "./pins.js";
import { bindDrag, mediaSelection, queueMedia } from "./drag.js";

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

let revealEntry = async () => {};
let libraryTotal = null;
let readRange = async () => [];
let selectionRequest;
let selectionPending = Promise.resolve();

async function loadLibrary(path, parameters, signal, initialPage = null, knownTotal = null) {
  signal.throwIfAborted();
  const content = $("content"), scroller = $("main"), pages = new Map();
  const before = el("div"), after = el("div");
  before.setAttribute("aria-hidden", "true");
  after.setAttribute("aria-hidden", "true");
  content.prepend(before);
  content.append(after);
  let total = knownTotal, pageHeight = 1, lastHeight = null, pending = null;
  libraryTotal = total;
  let header = null, timer, resize;
  signal.addEventListener("abort", () => {
    clearTimeout(timer);
    resize?.disconnect();
    pages.clear();
    content.style.minHeight = "";
  }, { once: true });
  const pageCount = () => Math.ceil(total / pageSize);
  const fullHeight = () => Math.max(0, pageCount() - 1) * pageHeight +
    (lastHeight ?? pageHeight * Math.min(1, (total % pageSize || pageSize) / pageSize));
  // Browsers cap element height. Map long scrollbars to logical positions while
  // keeping the actual rows at their normal size.
  const extent = () => Math.min(fullHeight(), 8_000_000);
  const origin = () => before.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
  const scrollPosition = () => Math.max(0, scroller.scrollTop - origin());
  const ratio = () => Math.max(1, (fullHeight() - scroller.clientHeight) /
    Math.max(1, extent() - scroller.clientHeight));
  const logicalPosition = () => scrollPosition() * ratio();
  const targetPage = () => Math.min(Math.max(0, pageCount() - 1), Math.floor(logicalPosition() / pageHeight));
  const syncRows = () => {
    state.visibleItems = [...content.querySelectorAll("[data-row]")].map(mediaRow);
    content.style.setProperty("--index-width", `${Math.max(3, String(total).length)}ch`);
  };
  const layout = () => {
    content.style.minHeight = `${extent() + (header?.getBoundingClientRect().height || 0)}px`;
    const ordered = [...pages.values()].sort((a, b) => a.start - b.start);
    let previous = before;
    for (const page of ordered) {
      if (previous.nextElementSibling !== page.node) previous.after(page.node);
      previous = page.node;
    }
    const height = ordered.reduce((sum, page) => sum + page.node.getBoundingClientRect().height, 0);
    const shift = logicalPosition() - scrollPosition();
    const top = Math.max(0, (ordered[0]?.start || 0) / pageSize * pageHeight - shift);
    before.style.height = `${top}px`;
    after.style.height = `${Math.max(0, extent() - top - height)}px`;
    syncRows();
  };
  const loadPage = async (start, existing = null) => {
    const lifetime = new AbortController();
    const abort = () => lifetime.abort();
    signal.addEventListener("abort", abort, { once: true });
    const node = existing || el("section", "library-page");
    node.classList.add("library-page");
    node.setAttribute("hx-sync", "this:replace");
    node.hidden = true;
    after.before(node);
    const remove = () => {
      lifetime.abort();
      signal.removeEventListener("abort", abort);
      if (node.contains(document.activeElement)) content.focus({ preventScroll: true });
      node.remove();
      releaseCovers();
    };
    try {
      const query = new URLSearchParams(parameters);
      query.set("offset", start);
      query.set("total", String(total === null));
      if (!existing) await fragment(`/api${path}?${query}`, node, lifetime.signal);
      signal.throwIfAborted();
      const data = node.matches("[data-page]") ? node : node.querySelector("[data-page]");
      if (data.dataset.total !== "") {
        total = Number(data.dataset.total);
        libraryTotal = total;
      }
      const count = Number(data.dataset.count);
      const table = node.querySelector(".track-list");
      if (table && !header) {
        content.setAttribute("role", "grid");
        content.setAttribute("aria-label", tr("Tracks"));
        content.setAttribute("aria-multiselectable", "true");
        content.setAttribute("aria-rowcount", total + 1);
        header = table.cloneNode(false);
        header.classList.add("library-header");
        header.append(table.querySelector("thead").cloneNode(true));
        content.prepend(header);
      }
      node.hidden = false;
      const page = { start, node, remove, count };
      pages.set(start, page);
      if (table) bindTracks(node, start, lifetime.signal);
      else {
        bindCards(node);
        loadCardCovers(node, lifetime.signal);
      }
      return page;
    } catch (error) {
      remove();
      throw error;
    }
  };
  const measure = () => {
    const page = [...pages.values()].find((page) => page.count === pageSize) || pages.values().next().value;
    if (!page) return;
    pageHeight = Math.max(1, page.node.getBoundingClientRect().height);
    if (page.count < pageSize && total > pageSize) {
      const grid = page.node.querySelector(".album-grid");
      const columns = grid ? getComputedStyle(grid).gridTemplateColumns.split(" ").length : 1;
      pageHeight *= Math.ceil(pageSize / columns) / Math.max(1, Math.ceil(page.count / columns));
    }
    const last = pages.get((pageCount() - 1) * pageSize);
    lastHeight = last ? last.node.getBoundingClientRect().height : null;
  };
  const fill = () => {
    if (pending) return pending;
    if (signal.aborted || !total) return Promise.resolve();
    pending = (async () => {
      let target;
      do {
        target = targetPage();
        const first = Math.max(0, target - 1);
        const last = Math.min(pageCount() - 1, target + Math.ceil(scroller.clientHeight / pageHeight));
        for (const [start, page] of pages) {
          if (start / pageSize < first || start / pageSize > last) {
            page.remove();
            pages.delete(start);
          }
        }
        layout();
        for (let index = first; index <= last; index++) {
          if (!pages.has(index * pageSize)) await loadPage(index * pageSize);
          if (signal.aborted) return;
          const end = pages.get((pageCount() - 1) * pageSize);
          if (end) lastHeight = end.node.getBoundingClientRect().height;
          layout();
          if (target !== targetPage()) break;
        }
      } while (target !== targetPage());
    })().finally(() => { pending = null; });
    return pending;
  };
  readRange = async (start, end, selectionSignal) => {
    const result = [];
    for (let offset = start; offset <= end; offset += pageSize) {
      const query = new URLSearchParams(parameters);
      query.set("offset", offset);
      query.set("limit", Math.min(pageSize, end - offset + 1));
      query.delete("total");
      const page = await api(`${path}?${query}`, "GET", undefined, selectionSignal);
      result.push(...(page.tracks || page.entries));
    }
    return result;
  };
  const start = Math.floor(offset / pageSize) * pageSize;
  if (offset !== start) { initialPage?.remove(); initialPage = null; }
  await loadPage(start, initialPage);
  measure();
  layout();
  scroller.scrollTop = start ? origin() + start / pageSize * pageHeight / ratio() : 0;
  await fill();
  signal.throwIfAborted();
  revealEntry = async (index) => {
    if (index < 0 || index >= total) return;
    const start = Math.floor(index / pageSize) * pageSize;
    scroller.scrollTop = start ? origin() + start / pageSize * pageHeight / ratio() : 0;
    await fill();
    signal.throwIfAborted();
    return content.querySelector(`tr[data-index="${index}"]`);
  };
  scroller.addEventListener("scroll", () => {
    // Reposition the window immediately when the logical scroll range is compressed.
    if (ratio() > 1) layout();
    clearTimeout(timer);
    timer = setTimeout(() => run(fill), 40);
  }, { signal, passive: true });
  let width = scroller.clientWidth, height = scroller.clientHeight;
  resize = new ResizeObserver(() => {
    if (width === scroller.clientWidth && height === scroller.clientHeight) return;
    const atTop = scroller.scrollTop === 0;
    const position = logicalPosition() / pageHeight;
    width = scroller.clientWidth;
    height = scroller.clientHeight;
    measure();
    layout();
    scroller.scrollTop = atTop ? 0 : origin() + position * pageHeight / ratio();
    run(fill);
  });
  resize.observe(scroller);
}

function restoreView() {
  const parameters = new URLSearchParams(location.search);
  const view = parameters.get("view") || "home";
  const collections = {
    album: "albums",
    artist: "artists",
    genre: "genres",
    playlist: "playlists",
    "smart-playlist": "smart-playlists",
  };
  route = Object.hasOwn(collections, view) ? collections[view] : (Object.hasOwn(titles, view) ? view : "home");
  if (Object.hasOwn(collections, view) && parameters.has("id")) {
    const id = parameters.get("id");
    detail = { id: /^-?\d+$/.test(id) ? Number(id) : id, title: parameters.get("title") };
  }
  if (parameters.has("source")) state.source = parameters.get("source");
  if (parameters.has("folder")) state.selectedLibrary = parameters.get("folder");
  offset = Math.max(0, Number(parameters.get("offset")) || 0);
  setSort();
  $("search").value = parameters.get("q") || "";
  if (parameters.has("sort")) $("sort").value = parameters.get("sort");
  $("descending").setAttribute("aria-pressed", String(parameters.get("descending") === "true"));
  document.querySelectorAll("[data-route]").forEach((node) => {
    if (node.dataset.route === route) node.setAttribute("aria-current", "page");
    else node.removeAttribute("aria-current");
  });
}

function navigate(next, selected = null) {
  route = next;
  detail = selected;
  markPins();
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
  run(() => loadView(false, state.selectedLibrary ? null : selected?.track_count ?? null));
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

async function loadView(initial = false, knownTotal = null) {
  run(refreshPins);
  for (const name of ["role", "aria-label", "aria-multiselectable", "aria-rowcount"])
    $("content").removeAttribute(name);
  selectionRequest?.abort();
  selectionPending = Promise.resolve();
  revealEntry = async () => {};
  readRange = async () => [];
  libraryTotal = null;
  selectedTracks.clear();
  selectionAnchor = 0;
  state.visibleItems = [];
  disposeHome();
  closeMenus();
  viewRequest?.abort();
  viewRequest = new AbortController();
  const signal = AbortSignal.any([session.signal, viewRequest.signal]);
  if (!initial) {
    $("content").setAttribute("aria-busy", "true");
    $("content").inert = true;
    $("content").replaceChildren(el("p", "empty-note", tr("Loading...")));
    releaseCovers();
  }
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
  $("import-playlist").hidden = route !== "playlists" || !!detail;
  $("search").placeholder = tr("Search");
  try {
    if (route === "sources") {
      renderSources(await api("/sources", "GET", undefined, signal));
      return;
    }
    if (!state.source && !["playlists", "smart-playlists"].includes(route)) {
      if (initial) {
        $("content").querySelector("[data-open-source]")?.addEventListener("click", () => $("source-dialog").showModal());
        return;
      }
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
    if (!initial) {
      $("content").replaceChildren();
      $("main").scrollTop = 0;
    }
    if (route === "home") {
      if (!initial) await fragment(`/api${path}?${params}`, $("content"), signal);
      bindHome(signal);
    } else await loadLibrary(path, params, signal, initial ? $("content").querySelector("[data-page]") : null, knownTotal);
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
  const nodes = [...host.querySelectorAll("tbody tr[data-row]")];
  for (const [position, tr] of nodes.entries()) {
    const index = start + position,
      row = mediaRow(tr);
    tr.dataset.index = index;
    tr.setAttribute("aria-rowindex", index + 2);
    bindDrag(
      tr,
      async () => {
        const media = mediaSelection("track", row);
        await selectionPending;
        media.uris = selectedTracks.has(index)
          ? selectedRows().map((item) => item.uri)
          : [row.uri];
        return media;
      },
      false,
    );
    if (selectedTracks.has(index)) selectedTracks.set(index, row);
    const selected = selectedTracks.has(index);
    tr.classList.toggle("selected", selected);
    tr.setAttribute("aria-selected", String(selected));
    tr.addEventListener(
      "click",
      (event) => {
        const button = event.target.closest("[data-action]");
        if (button) {
          event.preventDefault();
          run(() => {
            const action = button.dataset.action;
            if (action === "replace") return playSelection(row);
            if (action === "next" || action === "append") return playUris([row.uri], action);
            if (action === "favorite") return setFavorite([row]);
            if (action === "menu") return showTrackMenu(index, row, button);
            return goToMedia(row, action, button);
          });
        } else if (event.detail < 2) selectTrack(index, event);
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
        } else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
          event.preventDefault();
          run(async () => {
            const destination = event.key === "Home" ? 0 : event.key === "End" ? libraryTotal - 1 : index + (event.key === "ArrowDown" ? 1 : -1);
            let next = $("content").querySelector(
              `tr[data-index="${destination}"]`,
            );
            if (!next) {
              next = await revealEntry(destination);
            }
            if (next) {
              selectTrack(destination, event);
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
      node.addEventListener("click", (event) => {
        event.preventDefault();
        navigate(node.dataset.route);
      }),
    );
  $("back").addEventListener("click", (event) => {
    event.preventDefault();
    navigate(route === "genres" ? "home" : route);
  });
  $("search").addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      offset = 0;
      run(loadView);
    }, 200);
  });
  $("sort").addEventListener("change", () => {
    offset = 0;
    run(() => loadView(false, libraryTotal));
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
    run(() => loadView(false, libraryTotal));
  });
  $("refresh").addEventListener("click", () =>
    run(async () => {
      if (state.source)
        await api("/sources/refresh", "POST", { id: state.source });
      await loadView();
    }),
  );
  $("play-collection").addEventListener("click", () => run(playCollection));
  $("new-playlist").addEventListener("click", () => run(() => openName()));
  setSort();
}

function bindCards(host) {
  for (const card of host.querySelectorAll("[data-row][data-kind]")) {
    bindHoverControls(card);
    const row = mediaRow(card),
      kind = card.dataset.kind;
    if (!card.classList.contains("pin-row"))
      bindDrag(card, () => mediaSelection(kind, row), kind !== "track");
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
    const play = (mode) => queueMedia(mediaSelection(kind, row), mode);
    const menu = async (anchor, event) => {
      const actions = [
        [tr("Play"), () => play("replace")],
        [tr("Play Next"), () => play("next")],
        [tr("Play Later"), () => play("append")],
      ];
      if (["track", "album", "artist", "playlist"].includes(kind))
        actions.push(
          radioMenu(
            row,
            kind === "artist" && row.album_artists ? "album_artist" : kind,
          ),
        );
      actions.push(
        null,
        playlistMenu(
          kind === "track"
            ? { uris: [row.uri] }
            : {
                selection: {
                  kind: kind.replaceAll("-", "_"),
                  id: row.id,
                  album_artists: !!row.album_artists,
                },
                source: row.source ?? state.source,
                folder:
                  row.source && row.source !== state.source
                    ? null
                    : state.selectedLibrary,
              },
        ),
      );
      if (["track", "album", "artist"].includes(kind))
        actions.push([
          row.favorite ? tr("Remove from Favorites") : tr("Add to Favorites"),
          () => setFavorite([row], kind),
        ]);
      const pin = pinAction(kind, row, row.source ?? state.source);
      if (pin) actions.push(pin);
      if (kind === "playlist") actions.push([tr("Export Playlist"), () => openPlaylistTransfer(row), "export"]);
      if (row.writable)
        actions.push(
          [tr("Edit"), () => openName(row)],
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
      event.preventDefault();
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
    if (kind === "track" || card.classList.contains("pin-row")) {
      card.addEventListener("dblclick", (event) => {
        if (event.target.closest(".cover-open, .collection-title, .pin-open"))
          run(() => play("replace"));
      });
      card.addEventListener("keydown", (event) => {
        if (
          kind === "track" &&
          event.key === "Enter" &&
          event.target.closest('[data-action="open"]')
        ) {
          event.preventDefault();
          run(() => play("replace"));
        }
      });
    }
    if (kind !== "genre" || card.classList.contains("pin-row"))
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
        await coverGroup(
          card.querySelector(".cover"),
          query,
          row.artwork_count || 1,
          signal,
        );
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
      const columns = getComputedStyle(body.firstElementChild).gridTemplateColumns.split(" ").length;
      const cards = body.querySelectorAll(".album-card");
      offset = Math.min(
        offset,
        Math.max(0, Math.floor((cards.length - 1) / columns) * columns),
      );
      cards.forEach((card, index) => {
        card.hidden = index < offset || index >= offset + columns;
      });
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
  restoreView,
  openMediaLink,
  init,
  goToMedia,
  loadView,
  markCurrent,
  navigate,
  playUris,
  route,
  detail,
  setFavorite,
  viewRequest,
  disposeHome,
  bindCards,
};
