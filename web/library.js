import { openMetadata } from "./metadata.js";
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
  slideText,
} from "./ui.js";

import { api, browserHeaders, fragment, session, state } from "./connection.js";

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

import { sources, updateSources } from "./sources.js";
import { pinAction, markPins } from "./pins.js";
import { bindDrag, mediaSelection, queueMedia } from "./drag.js";

let viewRequest = null;

// The page shown: its token as the server resolved it.
let opened = null;
// `token` without the read it names, so its page reads anew.
const anew = (token) => ({ ...token, options: { ...token.options, read: undefined } });

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
// as a new order, apply to the first window, which reads the page anew and names it as it
// opened.
async function loadLibrary(token, extra, signal, initialPage, at) {
  signal.throwIfAborted();
  token = anew(token);
  const content = $("content"), scroller = $("main"), pages = new Map();
  const before = el("div"), after = el("div");
  before.setAttribute("aria-hidden", "true");
  after.setAttribute("aria-hidden", "true");
  content.prepend(before);
  content.append(after);
  let total = null, pageHeight = 1, lastHeight = null, pending = null;
  libraryTotal = total;
  let header = null, showcase = null, timer, resize;
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
  const layout = () => {
    content.style.minHeight = `${extent() + (header?.getBoundingClientRect().height || 0) + (showcase?.getBoundingClientRect().height || 0)}px`;
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
    content.style.setProperty("--index-width", `${Math.max(3, String(total).length)}ch`);
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
        header = table.cloneNode(false);
        header.classList.add("library-header");
        header.append(table.querySelector("thead").cloneNode(true));
        content.prepend(header);
      }
      if (header) content.setAttribute("aria-rowcount", total + 1);
      // The page's header comes with its first rows and stays above the list.
      const lifted = node.querySelector(".detail-showcase");
      if (lifted) {
        showcase?.remove();
        showcase = lifted;
        content.prepend(showcase);
        bindCard(showcase);
        loadCardCovers(content, signal);
      }
      node.hidden = false;
      const page = { start, node, remove, count };
      pages.set(start, page);
      // Changes committed while it read show once its rows do.
      if (data.dataset.missed) showChanged(JSON.parse(data.dataset.missed));
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
  const kept = new Set([before, after, header, showcase, pages.get(start).node]);
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
  document.title = `${data.name || data.title} – Rufin`;
  showHistory();
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

// The pages Back and Forward open, as GTK's window keeps them.
const backStack = [], forwardStack = [];

function showHistory() {
  const back = backStack.length > 0 || !!shown.back;
  $("back").setAttribute("aria-disabled", String(!back));
  $("forward").disabled = forwardStack.length === 0;
}

// Opens `target`: a page token or a navigation entry such as "Albums". A page
// opened from history leaves the history as it is.
function navigate(target, record = true) {
  if (record && opened) {
    backStack.push(opened);
    if (backStack.length > 50) backStack.shift();
    forwardStack.length = 0;
  }
  opened = typeof target === "string"
    ? { route: target, source: state.source || undefined, folder: state.selectedLibrary || undefined }
    : target;
  shown = {};
  $("search").value = opened.options?.filter || "";
  showOrder($("sort").value, false);
  $("navigation").classList.remove("open");
  showHistory();
  run(() => loadView());
}

// The page opened again in the selected source and music folder, searching what it searched,
// as GTK opens its page again.
function reopen() {
  navigate({ ...opened, source: state.source || undefined, folder: state.selectedLibrary || undefined }, false);
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
    icon("rufin-home-symbolic"),
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
async function loadView(initial = false, extra = {}, at = null) {
  const offsets = new Map(at === null ? [] : [...$("content").querySelectorAll(".home-section")]
    .map((node) => [node.dataset.block, Number(node.dataset.offset)]));
  for (const name of ["role", "aria-label", "aria-multiselectable", "aria-rowcount"])
    $("content").removeAttribute(name);
  revealEntry = async () => {};
  listPosition = () => at ?? 0;
  libraryTotal = null;
  selection = [];
  selectionAnchor = 0;
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
  try {
    if (!initial && at === null) {
      $("content").replaceChildren();
      $("main").scrollTop = 0;
    }
    if (opened.route === "Home") {
      if (!initial)
        await fragment(`/api/page?${new URLSearchParams({ page: JSON.stringify(anew(opened)) })}`, $("content"), signal);
      const home = $("content").querySelector("[data-page]").dataset;
      showPage(home);
      bindHome(signal, offsets);
      if (home.missed) showChanged(JSON.parse(home.missed));
    } else await loadLibrary(opened, extra, signal, initial ? $("content").querySelector("[data-page]") : null, at ?? 0);
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
// and hearts showing them, and ratings on the cells showing them, then the page read again when
// the change asks.
function showChanged(value) {
  for (const changed of value.states || []) {
    showState(changed.uri, changed.favorite);
    for (const node of document.querySelectorAll("[data-rating-uri]"))
      if (node.dataset.ratingUri === changed.uri) node.textContent = changed.rating_text;
  }
  if (value.page_refresh) run(() => refreshPage(value.page_refresh));
}

// Shows a media's favorite on every row that holds it, pins included, and every heart.
function showState(uri, favorite) {
  for (const node of document.querySelectorAll("[data-row]"))
    if (mediaRow(node).uri === uri) mediaRow(node).favorite = favorite;
  for (const node of document.querySelectorAll("[data-favorite-uri]"))
    if (node.dataset.favoriteUri === uri) showFavorite(node, favorite);
}

// Reads the page again as a committed change asks: where it is, or opened again in the
// selected source and music folder.
async function refreshPage(verdict) {
  if (verdict === "reopen") {
    const configured = await api("/sources");
    state.source = configured.selected || state.source;
    updateSources(configured);
    return reopen();
  }
  return loadView(false, {}, listPosition());
}

// Turns a row's favorite over, and shows it on every row and heart that shows its media.
async function setFavorite(row, kind = "track") {
  const result = await api("/favorite", "POST", { kind, uri: row.uri, favorite: !row.favorite });
  row.favorite = result.favorite;
  showState(row.uri, result.favorite);
}

// Opens what a row's linked field names: its one page, or a menu of them.
function openLinks(row, field, anchor) {
  const links = row.links?.[field] || [];
  if (links.length === 1) navigate(links[0].page);
  else if (links.length) showMenu(links.map((link) => [link.name, () => navigate(link.page)]), anchor);
  else notice(field === "album" ? tr("Album unavailable") : tr("Artist unavailable"));
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
  if (menu.edit_metadata)
    actions.push([tr("Edit metadata"), () => openMetadata(many ? media : { kind: "track", uri: row.uri }), "rufin-document-edit-symbolic"]);
  const destination = menu.go_to && goToMenu(row);
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
            return openLinks(row, button.dataset.field, button);
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
    if (backStack.length) {
      forwardStack.push(opened);
      navigate(backStack.pop(), false);
    } else if (shown.back) navigate(JSON.parse(shown.back), false);
  });
  $("forward").addEventListener("click", () => {
    if (!forwardStack.length) return;
    backStack.push(opened);
    navigate(forwardStack.pop(), false);
  });
  // A search and a chosen order read the page with the search and the order the toolbar
  // shows; the order goes with the first window, and the page names it from then on.
  const order = () => {
    clearTimeout(searchTimer);
    // Home has no list to search; its search lists the tracks, as the Tracks page does.
    if (opened.route === "Home") {
      const filter = $("search").value;
      if (!filter) return;
      navigate({ route: "Tracks", source: state.source || undefined, folder: state.selectedLibrary || undefined, options: { filter } });
      $("search").value = filter;
      return;
    }
    opened.options = { ...opened.options, filter: $("search").value };
    const sort = $("sort").value, descending = $("descending").getAttribute("aria-pressed");
    run(() => loadView(false, sort ? { sort, descending } : {}));
  };
  $("search").addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(order, 200);
  });
  $("sort").addEventListener("change", order);
  $("descending").addEventListener("click", () => {
    showOrder($("sort").value, $("descending").getAttribute("aria-pressed") !== "true");
    order();
  });
  $("refresh").addEventListener("click", () =>
    run(async () => {
      $("main-menu").hidePopover();
      if (state.source)
        await api("/sources/refresh", "POST", { id: state.source });
      await loadView();
    }),
  );
  $("play-collection").addEventListener("click", () => run(playCollection));
  $("new-playlist").addEventListener("click", () => run(() => openName()));
}

function bindCards(host) {
  for (const card of host.querySelectorAll("[data-row][data-kind]")) bindCard(card);
}

// GTK's header: Play, then Radio, give up their labels while the buttons do not fit their row;
// where even the circles do not fit, the insets and the gap beside the cover give way together.
function fitActions(actions) {
  const form = actions.closest("form");
  form.style.removeProperty("--inset");
  form.style.removeProperty("--gap");
  const pills = [...actions.querySelectorAll(".detail-pill")];
  for (const pill of pills) pill.classList.remove("detail-pill-icon");
  for (const pill of pills)
    if (actions.scrollWidth > actions.clientWidth) pill.classList.add("detail-pill-icon");
  const overflow = actions.scrollWidth - actions.clientWidth;
  if (overflow <= 0) return;
  const style = getComputedStyle(form);
  const spacing = Math.max(0, 2 * parseFloat(style.getPropertyValue("--inset")) + parseFloat(style.getPropertyValue("--gap")) - overflow) / 3;
  form.style.setProperty("--inset", `${Math.floor(spacing)}px`);
  form.style.setProperty("--gap", `${Math.floor(spacing)}px`);
}

function bindCard(card) {
  for (const node of card.querySelectorAll(".detail-title, .home-showcase .detail-heading > .detail-facts")) slideText(node);
  // The header's width, not the row's, which the fitting itself changes.
  for (const actions of card.querySelectorAll(".detail-actions")) {
    new ResizeObserver(() => fitActions(actions)).observe(card);
    fitActions(actions);
  }
  {
    const host = card.parentElement;
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
      if (options.edit_metadata && ["album", "artist", "track"].includes(kind))
        actions.push([tr("Edit metadata"), () => openMetadata({ kind, uri: row.uri }), "rufin-document-edit-symbolic"]);
      const destination = options.go_to && goToMenu(row);
      if (destination) actions.push(null, destination);
      // The pin closes the menu, as on GTK and Android.
      if (options.pin) actions.push(pinAction(options.pin));
      showMenu(actions, anchor, event);
    };
    card.addEventListener("click", (event) => {
      const button = event.target.closest("[data-action]");
      if (!button) return;
      event.preventDefault();
      const action = button.dataset.action;
      run(() => {
        if (action === "open") return open();
        if (action === "genre") return navigate(JSON.parse(button.dataset.page));
        if (action === "genres") return genreMenu(card, button);
        if (action === "step-showcase") return stepShowcase(card, button.dataset.forward === "true");
        if (action === "link") return openLinks(row, button.dataset.field, button);
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
    const genres = card.querySelector(".detail-genres");
    if (genres) new ResizeObserver(() => fitGenres(genres)).observe(genres);
  }
}

// Genre chips fill the row; those that do not fit go under the last chip's +n.
function fitGenres(row) {
  const chips = [...row.querySelectorAll('[data-action="genre"]')];
  const more = row.querySelector(".detail-genre-more");
  chips.forEach((chip) => (chip.hidden = false));
  more.hidden = true;
  const fits = () => row.scrollWidth <= row.clientWidth;
  if (fits()) return;
  more.hidden = false;
  for (let shown = chips.length - 1; shown >= 0 && !fits(); shown--) {
    chips[shown].hidden = true;
    more.textContent = `+${chips.length - shown}`;
  }
}

function genreMenu(card, anchor) {
  const hidden = [...card.querySelectorAll('[data-action="genre"][hidden]')];
  showMenu(hidden.map((chip) => [chip.textContent, () => navigate(JSON.parse(chip.dataset.page))]), anchor);
}

// Steps Home's showcase; the header core answers slides in from the side it went to.
async function stepShowcase(head, forward) {
  const response = await fetch("/api/home/showcase", {
    method: "POST",
    headers: { ...browserHeaders(), "Content-Type": "application/json", "HX-Request": "true" },
    body: JSON.stringify({ source: state.source, forward }),
  });
  if (!response.ok) throw new Error(tr("Request failed ({status})", { status: response.status }));
  const template = document.createElement("template");
  template.innerHTML = (await response.text()).trim();
  const next = template.content.querySelector(".detail-showcase");
  if (!next) return head.remove();
  next.classList.add(forward ? "slide-next" : "slide-previous");
  head.replaceWith(next);
  bindCard(next);
  loadCardCovers(next.parentElement, session.signal);
}

const requestedCovers = new WeakSet();

// What a card's picture is read by; None for a playlist folder, whose preview is its picture.
function cardCover(kind, row) {
  if (kind === "playlist") return { playlist: row.id };
  if (kind === "smart-playlist") return { smart_playlist: row.id };
  if (kind === "genre" || kind === "mood") return { genre: row.id, source: row.source };
  return row.uri;
}

function loadCardCovers(host, signal) {
  const cards = [...host.querySelectorAll(".album-card:not([hidden]), .detail-showcase")].filter(
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
  for (const section of $("content").querySelectorAll(".home-section")) {
    const body = section.querySelector(".home-row");
    if (!body) {
      loadCardCovers(section, signal);
      continue;
    }
    // ‹ › scroll the row by about its width and rest at its ends; a reread keeps its place.
    const previous = section.querySelector('[data-action="previous-home"]');
    const next = section.querySelector('[data-action="next-home"]');
    const ends = () => {
      previous.disabled = body.scrollLeft <= 0;
      next.disabled = body.scrollLeft + body.clientWidth >= body.scrollWidth - 1;
      section.dataset.offset = body.scrollLeft;
    };
    body.scrollLeft = offsets.get(section.dataset.block) || 0;
    body.addEventListener("scroll", ends, { passive: true, signal });
    for (const [button, sign] of [[previous, -1], [next, 1]])
      button.onclick = () => body.scrollBy({ left: sign * body.clientWidth * 0.9, behavior: "smooth" });
    loadCardCovers(section, signal);
    for (const [action, shuffled] of [["play-home", false], ["shuffle-home", true]])
      section.querySelector(`[data-action="${action}"]`).onclick = () =>
        run(() =>
          api("/queue", "POST", {
            mode: "replace",
            page: JSON.parse($("content").querySelector("[data-page]").dataset.page),
            section: Number(section.dataset.index),
            shuffled,
          }),
        );
    ends();
    layouts.push(ends);
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
  init,
  openLinks,
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
