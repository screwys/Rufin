import { tr } from "./ui.js";
import {
  releaseCovers,
  $,
  button,
  el,
  icon,
  setIcon,
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
  mediaLinks,
} from "./menus.js";

import { renderSources, sources, updateSources } from "./sources.js";
import { pinAction, markPins } from "./pins.js";
import { bindDrag, mediaSelection, queueMedia } from "./drag.js";

let viewRequest = null;

// The page shown: its token as the server resolved it, or "sources".
let opened = null;

// What the page's fragment says of itself: its title, Back page, navigation entry, order and
// actions.
let shown = {};

let searchTimer;

const rowsByNode = new WeakMap();
function mediaRow(node) {
  if (!rowsByNode.has(node)) rowsByNode.set(node, JSON.parse(node.dataset.row));
  return rowsByNode.get(node);
}

const pageSize = 48;

let revealEntry = async () => {};
let libraryTotal = null;
// The row the list shows at its top, which a reread opens at again.
let listPosition = () => 0;

// Pages the list of `token` in windows as it scrolls, from row `at`; `extra` parameters, such
// as a new order, apply to the first window, which names the page as it opened.
async function loadLibrary(token, extra, signal, initialPage, knownTotal, at) {
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
      const query = new URLSearchParams({
        ...extra,
        page: JSON.stringify(token),
        offset: start,
        limit: pageSize,
        total: String(total === null),
      });
      if (!existing) await fragment(`/api/page?${query}`, node, lifetime.signal);
      signal.throwIfAborted();
      const data = node.matches("[data-page]") ? node : node.querySelector("[data-page]");
      showPage(data.dataset);
      token = opened;
      extra = {};
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
      // Changes committed while it read show once its rows do.
      if (data.dataset.missed) {
        syncRows();
        showChanged(JSON.parse(data.dataset.missed));
      }
      if (table) bindTracks(node, lifetime.signal);
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
  let start = Math.floor(at / pageSize) * pageSize;
  if (at !== start) { initialPage?.remove(); initialPage = null; }
  // A reread past the end of a list that shrank opens at its end.
  if (!(await loadPage(start, initialPage)).count && total) {
    pages.get(start).remove();
    pages.delete(start);
    at = start = (pageCount() - 1) * pageSize;
    await loadPage(start);
  }
  // What the page showed before stays until these rows replace it.
  const kept = new Set([before, after, header, pages.get(start).node]);
  for (const node of [...content.children]) if (!kept.has(node)) node.remove();
  measure();
  layout();
  scroller.scrollTop = at ? origin() + at / pageSize * pageHeight / ratio() : 0;
  listPosition = () => logicalPosition() / pageHeight * pageSize;
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

// Shows what a page's fragment says of itself.
function showPage(data) {
  opened = JSON.parse(data.page);
  shown = data;
  if (opened.source) {
    state.source = opened.source;
    state.selectedLibrary = opened.folder ?? null;
  }
  $("title").textContent = data.name || data.title;
  $("back").hidden = !data.back;
  $("library-toolbar").hidden = data.section === "Home";
  $("play-collection").hidden = data.play !== "true";
  $("new-playlist").hidden = $("import-playlist").hidden = data.playlists !== "true";
  if (data.sorts) {
    $("sort").replaceChildren(
      ...JSON.parse(data.sorts).map(([name, title]) => new Option(title, name)),
    );
    showOrder(data.sort, data.descending === "true");
  }
  markSection(data.section);
  markPins();
}

function markSection(section) {
  document.querySelectorAll("[data-route]").forEach((node) => {
    if (node.dataset.route === section) node.setAttribute("aria-current", "page");
    else node.removeAttribute("aria-current");
  });
}

// Opens `target`: a page token, a navigation entry such as "Albums", or "sources".
function navigate(target) {
  opened = typeof target === "string" && target !== "sources"
    ? { route: target, source: state.source || undefined, folder: state.selectedLibrary || undefined }
    : target;
  shown = {};
  $("search").value = opened.options?.filter || "";
  showOrder($("sort").value, false);
  $("navigation").classList.remove("open");
  run(() => loadView());
}

// The page opened again in the selected source and music folder, searching what it searched,
// as GTK opens its page again.
function reopen() {
  navigate(
    opened === "sources"
      ? "sources"
      : { ...opened, source: state.source || undefined, folder: state.selectedLibrary || undefined },
  );
}

function showOrder(sort, descending) {
  $("sort").value = sort;
  $("descending").setAttribute("aria-pressed", String(descending));
  setIcon($("descending").firstElementChild, descending ? "rufin-sort-name-descending-symbolic" : "rufin-sort-name-symbolic");
  $("descending").hidden = sort === "definition";
}

function empty(title, description, action) {
  const node = el("div", "empty");
  node.append(
    icon(opened === "sources" ? "rufin-network-server-symbolic" : "rufin-home-symbolic"),
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

// Loads the page shown; a reread (`at`, the row to show at the top) keeps what it shows
// until the new rows replace it, and Home keeps its scroll and carousel positions.
async function loadView(initial = false, knownTotal = null, extra = {}, at = null) {
  const offsets = new Map(at === null ? [] : [...$("content").querySelectorAll(".home-section")]
    .map((node) => [node.dataset.block, Number(node.dataset.offset)]));
  for (const name of ["role", "aria-label", "aria-multiselectable", "aria-rowcount"])
    $("content").removeAttribute(name);
  revealEntry = async () => {};
  listPosition = () => at ?? 0;
  libraryTotal = null;
  selection = [];
  selectionAnchor = 0;
  state.visibleItems = [];
  disposeHome();
  closeMenus();
  viewRequest?.abort();
  viewRequest = new AbortController();
  const signal = AbortSignal.any([session.signal, viewRequest.signal]);
  if (!initial && at === null) {
    $("content").setAttribute("aria-busy", "true");
    $("content").inert = true;
    $("content").replaceChildren(el("p", "empty-note", tr("Loading...")));
    releaseCovers();
  }
  $("source-name").textContent =
    sources.find((item) => item.id === state.source)?.name || tr("Library");
  $("search").placeholder = tr("Search");
  try {
    if (opened === "sources") {
      $("title").textContent = tr("Sources");
      for (const id of ["back", "library-toolbar"]) $(id).hidden = true;
      markSection(null);
      renderSources(await api("/sources", "GET", undefined, signal));
      return;
    }
    if (!initial && at === null) {
      $("content").replaceChildren();
      $("main").scrollTop = 0;
    }
    if (opened.route === "Home") {
      if (!initial)
        await fragment(`/api/page?${new URLSearchParams({ page: JSON.stringify(opened) })}`, $("content"), signal);
      const home = $("content").querySelector("[data-page]").dataset;
      showPage(home);
      bindHome(signal, offsets);
      if (home.missed) showChanged(JSON.parse(home.missed));
    } else await loadLibrary(opened, extra, signal, initial ? $("content").querySelector("[data-page]") : null, knownTotal, at ?? 0);
  } catch (error) {
    if (!signal.aborted) {
      if (state.source) empty(tr("Could not load this page"), error.message);
      else empty(tr("Your music belongs here"), tr("Add a music source to start listening."), true);
    }
    throw error;
  } finally {
    if (!signal.aborted) {
      $("content").setAttribute("aria-busy", "false");
      $("content").inert = false;
    }
  }
}

// Shows what a committed change means for the page: the favorites that changed on the rows
// and hearts showing them, then the page read again when the change asks.
function showChanged(value) {
  for (const changed of value.states || []) {
    for (const row of state.visibleItems)
      if (row.uri === changed.uri) row.favorite = changed.favorite;
    for (const node of document.querySelectorAll("[data-favorite-uri]"))
      if (node.dataset.favoriteUri === changed.uri) showFavorite(node, changed.favorite);
  }
  if (value.page_refresh) run(() => refreshPage(value.page_refresh));
}

// Reads the page again as a committed change asks: where it is, or opened again in the
// selected source and music folder.
async function refreshPage(verdict) {
  if (opened === "sources") return;
  if (verdict === "reopen") {
    const configured = await api("/sources");
    state.source = configured.selected || state.source;
    updateSources(configured);
    return reopen();
  }
  return loadView(false, null, {}, listPosition());
}

// Turns a row's favorite over, and shows it on every row and heart that shows its media.
async function setFavorite(row, kind = "track") {
  const result = await api("/favorite", "POST", { kind, uri: row.uri, favorite: !row.favorite });
  row.favorite = result.favorite;
  for (const track of state.visibleItems) if (track.uri === row.uri) track.favorite = result.favorite;
  for (const node of document.querySelectorAll("[data-favorite-uri]"))
    if (node.dataset.favoriteUri === row.uri) showFavorite(node, result.favorite);
}

async function goToMedia(row, kind, anchor, rowKind = "track") {
  const metadata = await mediaLinks(row, rowKind);
  const links =
    kind === "albums" ? [metadata.album].filter(Boolean) : metadata.artists;
  const open = (link) => openMediaLink(link);
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

// Opens the page of an album or artist link.
function openMediaLink(link) {
  navigate(link.page);
}

// The selected rows as ordered [start, end) ranges, and the row Shift selects from.
let selection = [];
let selectionAnchor = 0;
const selected = (index) => selection.some(({ start, end }) => start <= index && index < end);
const selectedMany = () => selection.reduce((count, { start, end }) => count + end - start, 0) > 1;

// Adds rows [start, end) to the selection, or takes them out of it.
function mark(start, end, add) {
  selection = selection.flatMap((range) => range.end <= start || range.start >= end ? [range] : [
    ...(range.start < start ? [{ start: range.start, end: start }] : []),
    ...(range.end > end ? [{ start: end, end: range.end }] : []),
  ]);
  if (add) selection.push({ start, end });
  selection.sort((a, b) => a.start - b.start);
}

function selectTrack(index, event = {}) {
  if (event.shiftKey) {
    if (!event.ctrlKey && !event.metaKey) selection = [];
    mark(Math.min(index, selectionAnchor), Math.max(index, selectionAnchor) + 1, true);
  } else {
    if (event.ctrlKey || event.metaKey) mark(index, index + 1, !selected(index));
    else selection = [{ start: index, end: index + 1 }];
    selectionAnchor = index;
  }
  for (const node of $("content").querySelectorAll("tr[data-index]")) {
    const on = selected(Number(node.dataset.index));
    node.classList.toggle("selected", on);
    node.setAttribute("aria-selected", String(on));
  }
}

// Plays the page from track `row` at `index`, or the rows selected with it.
function playSelection(index, row) {
  return selectedMany()
    ? api("/queue", "POST", { page: opened, ranges: selection, mode: "replace" })
    : api("/queue", "POST", { page: opened, index, anchor_uri: row.uri, anchor_entry: row.id, mode: "replace" });
}

async function showTrackMenu(index, row, anchor, event) {
  if (!selected(index)) selectTrack(index);
  const media = { page: opened, ranges: selection }, menu = row.menu, many = selectedMany();
  const actions = [];
  if (menu.play) actions.push([tr("Play"), () => playSelection(index, row)]);
  if (menu.play_next) actions.push([tr("Play Next"), () => queueMedia(media, "next")]);
  if (menu.play_later) actions.push([tr("Play Later"), () => queueMedia(media, "append")]);
  if (menu.radio) actions.push(radioMenu(menu.radio));
  actions.push(null);
  if (menu.add_to_playlist) actions.push(playlistMenu(media));
  if (menu.favorite) {
    const query = new URLSearchParams({ page: JSON.stringify(opened), ranges: JSON.stringify(selection) });
    const favorite = many ? (await api(`/favorite?${query}`)).favorite : row.favorite;
    actions.push([
      favorite ? tr("Remove from Favorites") : tr("Add to Favorites"),
      () => many ? api("/favorite", "POST", { kind: "track", ...media, favorite: !favorite }) : setFavorite(row),
    ]);
  }
  if (shown.writable === "true")
    actions.push([
      tr("Remove from Playlist"),
      () => api("/playlists/entries", "DELETE", { id: opened.route.PlaylistDetail, ...media }),
    ]);
  const destination = menu.go_to && await goToMenu(row);
  if (destination) actions.push(null, destination);
  showMenu(actions, anchor, event);
}

function bindTracks(host, signal) {
  const nodes = [...host.querySelectorAll("tbody tr[data-row]")];
  for (const tr of nodes) {
    const index = Number(tr.dataset.index),
      row = mediaRow(tr);
    tr.setAttribute("aria-rowindex", index + 2);
    bindDrag(tr, () => selected(index)
      ? { kind: "track", row, page: opened, ranges: selection }
      : mediaSelection("track", row), false);
    tr.classList.toggle("selected", selected(index));
    tr.setAttribute("aria-selected", String(selected(index)));
    tr.addEventListener(
      "click",
      (event) => {
        const button = event.target.closest("[data-action]");
        if (button) {
          event.preventDefault();
          run(() => {
            const action = button.dataset.action;
            if (action === "replace") return playSelection(index, row);
            if (action === "next" || action === "append") return playUris([row.uri], action);
            if (action === "favorite") return setFavorite(row);
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
        if (!event.target.closest("button")) run(() => playSelection(index, row));
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
          if (event.key === "Enter") run(() => playSelection(index, row));
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
          selectionAnchor = 0;
          selectTrack(libraryTotal - 1, { shiftKey: true });
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

// Plays the page whole, as its header Play does.
function playCollection() {
  return api("/queue", "POST", { page: opened, mode: "replace" });
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
    navigate(JSON.parse(shown.back));
  });
  $("search").addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      opened.options = { ...opened.options, filter: $("search").value };
      run(loadView);
    }, 200);
  });
  // A chosen order goes with the first window; the page names it from then on.
  const order = () => {
    run(() => loadView(false, libraryTotal, {
      sort: $("sort").value,
      descending: $("descending").getAttribute("aria-pressed"),
    }));
  };
  $("sort").addEventListener("change", order);
  $("descending").addEventListener("click", () => {
    showOrder($("sort").value, $("descending").getAttribute("aria-pressed") !== "true");
    order();
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
}

function bindCards(host) {
  for (const card of host.querySelectorAll("[data-row][data-kind]")) {
    bindHoverControls(card);
    const row = mediaRow(card),
      kind = card.dataset.kind;
    if (!card.classList.contains("pin-row"))
      bindDrag(card, () => mediaSelection(kind, row), kind !== "track");
    const open = () => {
      if (row.page) return navigate(row.page);
      host
        .querySelectorAll(".album-card.selected")
        .forEach((node) => node.classList.remove("selected"));
      card.classList.add("selected");
    };
    const play = (mode) => queueMedia(mediaSelection(kind, row), mode);
    const menu = async (anchor, event) => {
      const actions = [], options = row.menu;
      if (options.play) actions.push([tr("Play"), () => play("replace")]);
      if (options.play_next) actions.push([tr("Play Next"), () => play("next")]);
      if (options.play_later) actions.push([tr("Play Later"), () => play("append")]);
      if (options.radio) actions.push(radioMenu(options.radio));
      actions.push(null);
      if (options.add_to_playlist)
        actions.push(
          playlistMenu(row.play ? { page: row.play } : { uris: [row.uri] }),
        );
      if (options.favorite)
        actions.push([
          row.favorite ? tr("Remove from Favorites") : tr("Add to Favorites"),
          () => setFavorite(row, kind),
        ]);
      if (options.pin) actions.push(pinAction(options.pin, options.pinned));
      // The web's playlist dialogs edit, export and delete playlists, not smart ones or folders.
      if (options.export && kind === "playlist")
        actions.push([tr("Export Playlist"), () => openPlaylistTransfer(row), "rufin-document-save-symbolic"]);
      if (options.remove_unavailable)
        actions.push([
          tr("Remove unavailable tracks"),
          () => api(`/playlists/remove-unavailable?id=${row.id}`, "POST"),
          "rufin-user-trash-symbolic",
        ]);
      if (options.edit && kind === "playlist") actions.push([tr("Edit"), () => openName(row)]);
      if (options.delete && kind === "playlist")
        actions.push([
          tr("Delete Playlist"),
          async () => {
            if (confirm(tr('Delete "{name}"?', { name: row.title || row.name })))
              await api(`/playlists?id=${row.id}`, "DELETE");
          },
        ]);
      if (options.go_to) {
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
          return goToMedia(row, action, button, kind);
        if (action === "favorite") return setFavorite(row, kind);
        if (action === "radio") return playRadio(row.menu.radio);
        if (action === "menu") return menu(button);
        return play(action);
      });
      if (event.detail && button.closest(".cover-controls")) {
        button.blur();
      }
    });
    if (kind === "track" || card.classList.contains("pin-row")) {
      card.addEventListener("dblclick", (event) => {
        if (event.target.closest(".cover-open, .collection-title, .pin-open") && (row.play || kind === "track"))
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
    card.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      run(() => menu(card, event));
    });
  }
}

const requestedCovers = new WeakSet();

// What a card's picture is read by; None for a playlist folder, whose preview is its picture.
function cardCover(kind, row) {
  if (kind === "playlist") return { playlist: row.id };
  if (kind === "smart-playlist")
    return {
      smart_playlist: row.id,
      ...(state.source ? { source: state.source } : {}),
      ...(state.selectedLibrary ? { folder: state.selectedLibrary } : {}),
    };
  if (kind === "genre" || kind === "mood") return { genre: row.id, source: row.source };
  return row.uri;
}

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
        const query = cardCover(kind, row);
        if (query) await coverGroup(
          card.querySelector(".cover"),
          query,
          row.artwork_count || 1,
          signal,
          row.artwork_previews,
        );
      }
    });
  releaseCovers();
}

let homeObserver;

function bindHome(signal, offsets) {
  const layouts = [];
  bindCards($("content"));
  state.visibleItems = [...$("content").querySelectorAll("[data-row]")].map(mediaRow);
  for (const section of $("content").querySelectorAll(".home-section")) {
    const body = section.querySelector(".home-row");
    if (!body) {
      loadCardCovers(section, signal);
      continue;
    }
    let offset = offsets.get(section.dataset.block) || 0;
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
      section.dataset.offset = offset;
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
          // The change it commits reads Home again, this block from its start.
          section.dataset.offset = offset = 0;
          try {
            await api("/home", "POST", { source: state.source, block: section.dataset.block }, signal);
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
  cardCover,
  openMediaLink,
  init,
  goToMedia,
  loadView,
  markCurrent,
  navigate,
  opened,
  playUris,
  reopen,
  showChanged,
  setFavorite,
  showPage,
  viewRequest,
  disposeHome,
  bindCards,
};
