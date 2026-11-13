import { messages } from "/translations.js";

export function tr(message, values = {}) {
  let text = messages[message] || message;
  for (const [name, value] of Object.entries(values))
    text = text.replaceAll(`{${name}}`, () => String(value));
  return text;
}

const $ = (id) => document.getElementById(id);

const el = (tag, className = "", text = "") => {
  const node = document.createElement(tag);
  node.className = className;
  node.textContent = text;
  return node;
};

// An icon from /icons by its file name, as GTK names it.
const icon = (name) => {
  const node = el("span", "icon");
  setIcon(node, name);
  node.ariaHidden = "true";
  return node;
};

function setIcon(node, name) {
  if (node.dataset.icon === name) return;
  node.dataset.icon = name;
  node.style.setProperty("--icon", `url("/icons/${name}.svg")`);
}

function showFavorite(node, favorite) {
  if (node.getAttribute("aria-pressed") !== String(favorite)) {
    node.setAttribute("aria-pressed", String(favorite));
  }
  setIcon(node.firstElementChild, favorite ? "rufin-heart-filled-symbolic" : "rufin-heart-outline-symbolic");
}

const button = (label, action, name, className = "icon-button") => {
  const node = el("button", className, name ? "" : label);
  node.type = "button";
  node.title = label;
  node.setAttribute("aria-label", label);
  if (name) node.append(icon(name));
  node.addEventListener("click", () => run(action));
  return node;
};

const time = (millis) => {
  const seconds = Math.max(0, Math.floor((millis || 0) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
};

let toastTimer;
let toastParent;

// A modal dialog draws over the page, so the toast shows inside the topmost one, in the same
// place. Work in progress stays until its result replaces it.
function notice(message, { working = false } = {}) {
  const toast = $("toast");
  toastParent ??= toast.parentElement;
  const dialogs = document.querySelectorAll("dialog:modal");
  (dialogs[dialogs.length - 1] || toastParent).append(toast);
  toast.textContent = message;
  toast.hidden = false;
  clearTimeout(toastTimer);
  if (!working) toastTimer = setTimeout(() => {
    toast.hidden = true;
  }, 5000);
}

async function run(action) {
  try {
    return await action();
  } catch (error) {
    if (error.name !== "AbortError") notice(error.message);
  }
}

function noticeError(message) {
  if (message) notice(message);
}

function openPopup(popup, anchor, align = "start") {
  popup.showPopover();
  const rect = anchor.getBoundingClientRect();
  const left = align === "end" ? rect.right - popup.offsetWidth : align === "center" ? rect.left + rect.width / 2 - popup.offsetWidth / 2 : rect.left;
  popup.style.left = `${Math.max(8, Math.min(left, innerWidth - popup.offsetWidth - 8))}px`;
  popup.style.top = `${Math.max(8, rect.top >= popup.offsetHeight + 8 ? rect.top - popup.offsetHeight - 8 : Math.min(rect.bottom + 8, innerHeight - popup.offsetHeight - 8))}px`;
}

function initDialogs() {
  document.querySelectorAll("[data-close]").forEach((node) =>
    node.addEventListener("click", () => {
      const target = $(node.dataset.close);
      if (target.matches("[popover]")) target.hidePopover();
      else target.close();
    }),
  );
  for (const dialog of document.querySelectorAll("dialog:not(#login-dialog)")) {
    dialog.addEventListener("click", (event) => {
      if (event.target !== dialog) return;
      const rect = dialog.getBoundingClientRect();
      if (
        event.clientX < rect.left ||
        event.clientX > rect.right ||
        event.clientY < rect.top ||
        event.clientY > rect.bottom
      )
        dialog.close();
    });
  }
}

const initialAppearance = document.documentElement.dataset.appearance
  ? JSON.parse(document.documentElement.dataset.appearance)
  : { theme: "System", accent: "System", colors: {} };
let savedAppearance = initialAppearance;

function applyColors(colors) {
  if (!Object.keys(colors).length) return;
  savedAppearance.colors = colors;
  if (colors["color-scheme"])
    document.documentElement.dataset.theme = colors["color-scheme"];
  for (const [name, value] of Object.entries(colors))
    document.documentElement.style.setProperty(name, value);
}

function applyAppearance(appearance) {
  savedAppearance = appearance;
  document.documentElement.dataset.theme =
    appearance.theme === "System"
      ? matchMedia("(prefers-color-scheme: dark)").matches
        ? "dark"
        : "light"
      : appearance.theme.toLowerCase();
  const accents = {
    Blue: "#3584e4",
    Teal: "#2190a4",
    Green: "#3a944a",
    Yellow: "#c88800",
    Orange: "#ed5b00",
    Red: "#e62d42",
    Pink: "#d56199",
    Purple: "#9141ac",
    Slate: "#6f8396",
  };
  const accent = accents[appearance.accent];
  if (accent) {
    document.documentElement.style.setProperty("--blue", accent);
    document.documentElement.style.setProperty("--accent", accent);
  }
  applyColors(appearance.colors || {});
}

function initAppearance() {
  applyAppearance(savedAppearance);
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () =>
    applyAppearance(savedAppearance),
  );
}

const covers = new Map();

async function cover(node, uri, signal, preview = null) {
  const query = typeof uri === "string" ? { uri } : uri;
  const identity = JSON.stringify(query);
  let fullReady = node.dataset.coverIdentity === identity && node.dataset.coverFull === "true";
  covers.get(node)?.dispose();
  let request, pixels = 0;
  const entry = { url: null, identity, pixels: 0, dispose };
  // A page read again shows the same covers on new nodes: each takes the picture the node it
  // replaced loaded, rather than showing it again from the preview.
  const replaced = !fullReady && [...covers].find(([other, shown]) => !other.isConnected && shown.url && shown.identity === identity)?.[1];
  if (replaced) {
    [entry.url, entry.pixels, pixels, fullReady] = [replaced.url, replaced.pixels, replaced.pixels, true];
    replaced.url = null;
    replaced.dispose();
    const image = el("img");
    image.alt = "";
    image.src = entry.url;
    node.replaceChildren(image);
  } else if (!fullReady && preview && node.querySelector("img")?.getAttribute("src") !== preview) {
    const image = el("img");
    image.alt = "";
    image.src = preview;
    node.replaceChildren(image);
  } else if (!fullReady && !preview && node.dataset.coverIdentity && node.dataset.coverIdentity !== identity) {
    node.replaceChildren(icon("rufin-symbolic"));
  }
  node.dataset.coverIdentity = identity;
  node.dataset.coverFull = String(fullReady);
  const resize = new ResizeObserver(() => {
    const size = physicalSize();
    if (!query.original && size > pixels) run(() => load(size));
  });
  function physicalSize() {
    const bounds = node.getBoundingClientRect();
    return Math.max(1, Math.ceil(Math.max(bounds.width, bounds.height) * devicePixelRatio));
  }
  // An aborted page stops loading its covers but keeps their pictures, which the page read
  // again in its place takes over; releaseCovers lets go of the ones nothing took.
  function stop() {
    request?.abort();
    resize.disconnect();
  }
  function dispose() {
    stop();
    signal.removeEventListener("abort", stop);
    if (entry.url) URL.revokeObjectURL(entry.url);
    if (covers.get(node) === entry) covers.delete(node);
  }
  async function load(size) {
    pixels = entry.pixels = size;
    request?.abort();
    request = new AbortController();
    const requestSignal = AbortSignal.any([signal, request.signal]);
    try {
      const response = await fetch(`/api/artwork?${new URLSearchParams({ ...query, size })}`, { signal: requestSignal });
      if (!response.ok) return fullReady || !!preview;
      const image = el("img");
      const url = URL.createObjectURL(await response.blob());
      image.alt = "";
      image.src = url;
      try { await image.decode(); }
      catch { URL.revokeObjectURL(url); return fullReady || !!preview; }
      if (requestSignal.aborted || !node.isConnected) {
        URL.revokeObjectURL(url);
        return false;
      }
      if (entry.url) URL.revokeObjectURL(entry.url);
      entry.url = url;
      fullReady = true;
      node.dataset.coverFull = "true";
      node.replaceChildren(image);
      return true;
    } catch (error) {
      if (requestSignal.aborted) throw error;
      return fullReady || !!preview;
    }
  }
  covers.set(node, entry);
  signal.addEventListener("abort", stop, { once: true });
  resize.observe(node);
  if (replaced && (query.original || physicalSize() <= pixels)) return true;
  return load(physicalSize());
}

function releaseCovers() {
  for (const [node, entry] of covers)
    if (!node.isConnected) entry.dispose();
}

async function coverGroup(node, query, count, signal, previews = []) {
  node.classList.toggle("cover-mosaic", count > 1);
  if (count <= 1) return cover(node, query, signal, previews[0]);
  let tiles = [...node.children].filter((tile) => tile.classList.contains("cover-quadrant"));
  if (tiles.length !== 4) {
    node.replaceChildren();
    tiles = Array.from({ length: 4 }, () => {
      const tile = el("span", "cover-quadrant");
      tile.append(icon("rufin-symbolic"));
      node.append(tile);
      return tile;
    });
  }
  await Promise.all(tiles.map((tile, index) =>
    cover(tile, { ...query, part: index % count }, signal, previews[index % count])));
}

// GTK's shell sizes (crates/ui/src/shell/shell/layout.rs, player/bottom.rs and player/mod.rs).
const RAIL = 72, GAP = 8, MIN_MAIN = 450, MIN_LEFT = 210, MAX_LEFT = 400, MIN_RIGHT = 250,
  MAX_RIGHT = 500, NARROW_BAR = 708, TRANSPORT = 308, MINI_WIDTH = 360, MINI_HEIGHT = 240;
const appLayout = (() => {
  try { return JSON.parse($("app").dataset.layout); } catch { return {}; }
})();
// The sidebar and queue toggles of this browser, over the profile GTK's settings choose.
let toggled = {};
try { toggled = JSON.parse(localStorage.getItem("rufin-layout")) || {}; } catch {}
const saveToggled = () => { try { localStorage.setItem("rufin-layout", JSON.stringify(toggled)); } catch {} };

// GTK's resolve_layout: the profile's sidebars, narrowed until the page keeps 450px.
function resolveLayout(width) {
  width = Math.max(width, MIN_MAIN);
  const narrow = appLayout.narrow_enabled && width < (appLayout.narrow_threshold ?? 1300);
  const profile = {
    ...((narrow ? appLayout.narrow_profile : appLayout.default_profile) || { left_sidebar: "Full", right_sidebar: "Visible" }),
    ...toggled,
  };
  const preferredLeft = Math.min(MAX_LEFT, Math.max(MIN_LEFT, appLayout.preferred_left_sidebar_width || 230));
  const preferredRight = appLayout.preferred_right_sidebar_width || 300;
  let left = profile.left_sidebar;
  let leftWidth = left === "Full" ? preferredLeft : left === "Compact" ? RAIL : 0;
  let right = profile.right_sidebar === "Visible";
  let rightWidth = 0;
  const fitRight = (mode, size) => {
    const available = width - size - (mode === "Hidden" ? 0 : GAP) - MIN_MAIN - GAP;
    return available >= MIN_RIGHT ? Math.min(available, Math.min(MAX_RIGHT, Math.max(MIN_RIGHT, preferredRight))) : null;
  };
  if (right) {
    const fitted = fitRight(left, leftWidth);
    const compact = left === "Full" ? fitRight("Compact", RAIL) : null;
    if (fitted != null) rightWidth = fitted;
    else if (compact != null) [left, leftWidth, rightWidth] = ["Compact", RAIL, compact];
    else right = false;
  }
  if (!right && left === "Full") {
    const available = width - MIN_MAIN - GAP;
    if (available >= MIN_LEFT) leftWidth = Math.min(preferredLeft, available);
    else [left, leftWidth] = ["Compact", RAIL];
  }
  if (left === "Compact" && width - leftWidth - GAP < MIN_MAIN) [left, leftWidth, right] = ["Hidden", 0, false];
  return { left: left.toLowerCase(), leftWidth, right, rightWidth };
}

// What the desktop row shows at this width: the actions beside the volume and the seek bar's width.
function barLayout(width) {
  const side = (width - 16 - TRANSPORT) / 2;
  const action = (count) => 192 + 34 * count;
  return {
    actions: side >= action(2) ? "queue" : side >= action(1) ? "favorite" : "volume",
    // A running sleep timer shows its time left where the pill fits beside every action.
    sleepLabel: side >= action(2) + 64,
    progress: Math.max(140, Math.min(140 + ((width - NARROW_BAR) * 95) / 300, width - 16 - 2 * action(0) - 72)),
  };
}

// GTK's controls_row: how many extras join the controls layout's five on its button row, a pair
// at a time, and the gap there, narrower where the row is tight.
function controlsRow(width) {
  const gap = (extras) => Math.min(8, (width - 36 * (4 + extras) - 44) / (4 + extras));
  const up = [6, 4, 2].find((extras) => gap(extras) >= 0) ?? 0;
  return { up, gap: Math.max(0, gap(up)) };
}

// Volume, favorite and the extras sit where each layout keeps them, as GTK's
// place_bottom_player_seek_buttons moves them: in the desktop row's actions, beside previous and
// next in the compact layout, volume in shuffle's place on the strip, and in the controls layout
// the first `up` on the button row and the rest at the ends of the seek bar's row.
let placed = "";
function placeBarButtons(bar, up) {
  if (placed === `${bar}:${up}`) return;
  placed = `${bar}:${up}`;
  const row = document.querySelector(".transport-buttons");
  const seekRow = document.querySelector(".seek-row");
  const volumeBox = document.querySelector(".volume");
  const extras = ["auto-dj", "random-buttons", "sleep-timer-button", "playback-settings-button", "mute", "current-favorite"].map($);
  const [dj, random, sleep, settings, volume, favorite] = extras;
  row.prepend(dj);
  row.append(random);
  document.querySelector(".player-actions").prepend(sleep, settings);
  volumeBox.prepend(favorite);
  volumeBox.insertBefore(volume, $("volume"));
  if (bar === "compact") {
    row.prepend(volume);
    row.append(favorite);
  } else if (bar === "strip") row.prepend(volume);
  else if (bar === "controls") {
    const raised = (button) => extras.indexOf(button) < up;
    const start = [sleep, volume, dj], end = [random, favorite, settings];
    row.prepend(...start.filter(raised));
    row.append(...end.filter(raised));
    seekRow.prepend(...start.filter((button) => !raised(button)));
    seekRow.append(...end.filter((button) => !raised(button)));
  }
}

// The fullscreen player's panes, as GTK's PaneLayout: both side by side or one of them, and one at
// a time under 800px.
let paneLayout = "both";
const queueHeader = document.querySelector("#right-panel > .panel-header");
function panes(collapsed) {
  return { both: [true, !collapsed], first: [true, false], second: [false, true] }[paneLayout];
}

function switchPane() {
  const collapsed = $("app").clientWidth < 800;
  paneLayout = { both: collapsed ? "second" : "first", first: "second", second: collapsed ? "first" : "both" }[paneLayout];
  applyLayout();
}

// GTK's apply_fullscreen_responsive_layout_for_size: the panes, whether the lyrics fit under the
// details, and the cover's size.
function applyFullscreen(width, height) {
  const section = $("fullscreen-player");
  const collapsed = width < 800;
  const single = height - 90 < 320;
  const [first, second] = panes(collapsed);
  const lyrics = appLayout.fullscreen_lyrics !== false && !single && first;
  section.dataset.panes = first && second ? "both" : first ? "first" : "second";
  section.toggleAttribute("data-lyrics", lyrics);
  section.toggleAttribute("data-beside", lyrics || single);
  setIcon(
    $("fullscreen-pane").firstElementChild,
    paneLayout === "second" ? "rufin-pane-right-focus-symbolic" : paneLayout === "first" || collapsed ? "rufin-pane-left-focus-symbolic" : "rufin-view-dual-symbolic",
  );
  const paneWidth = first && second ? (width - 24) / 2 : width - 16;
  const paneHeight = height - 90 - 48;
  section.toggleAttribute("data-without-album", single && paneHeight < 112);
  section.toggleAttribute("data-without-meta", single && paneHeight < 140);
  const clamp = (value, low, high) => Math.max(low, Math.min(high, Math.floor(value)));
  let size, hero = null;
  if (single) size = clamp(Math.min((paneWidth - 48) * 0.4, paneHeight - 32), 48, 340);
  else if (lyrics) {
    hero = clamp((height - 90) * 0.42, 96, 544);
    size = clamp(Math.min((paneWidth - 48) * 0.4, hero - 32), 48, 340);
  } else size = clamp(Math.min(height - 90 - 228, paneWidth - 48, height * 0.5), 96, 480);
  section.style.setProperty("--cover-size", `${size}px`);
  section.style.setProperty("--hero-height", hero ? `${hero}px` : "auto");
}

// The queue and the lyrics move into the fullscreen player while it shows, as GTK's do.
function openFullscreen() {
  const section = $("fullscreen-player");
  if (!section.hidden) return;
  $("fullscreen-lyrics").append($("lyrics-panel"));
  $("fullscreen-second").append(queueHeader, $("queue"));
  $("right-panel").classList.remove("open");
  section.hidden = false;
  applyLayout();
}

function closeFullscreen() {
  const section = $("fullscreen-player");
  if (section.hidden) return;
  section.hidden = true;
  $("right-panel").prepend(queueHeader, $("queue"));
  $("right-panel").append($("lyrics-panel"));
  applyLayout();
}

function applyLayout() {
  const app = $("app");
  const width = app.clientWidth - 2 * GAP;
  const mini = innerHeight < MINI_HEIGHT ? "short" : innerWidth < MINI_WIDTH ? "tall" : null;
  const layout = resolveLayout(width);
  const bar = barLayout(app.clientWidth);
  const fullscreen = !mini && !$("fullscreen-player").hidden;
  const narrow = !mini && app.clientWidth < NARROW_BAR;
  // GTK's bottom_player_layout.
  const kind = mini === "short" ? "strip" : mini || (narrow && fullscreen) ? "controls" : narrow ? "compact" : "desktop";
  const transport = Math.max(app.clientWidth - 16, 220);
  const row = controlsRow(transport);
  placeBarButtons(kind, row.up);
  if (mini) app.dataset.mini = mini;
  else delete app.dataset.mini;
  $("mini-title").hidden = !mini;
  app.dataset.left = layout.left;
  app.dataset.right = layout.right ? "visible" : "hidden";
  app.dataset.bar = kind;
  app.toggleAttribute("data-narrow", narrow);
  app.dataset.actions = bar.actions;
  app.toggleAttribute("data-sleep-label", kind === "desktop" && bar.sleepLabel);
  // The controls layout's seek bar gives way to the buttons beside it.
  app.toggleAttribute("data-seek-hidden", kind === "controls" && transport < (6 - row.up) * 34 + 132);
  app.style.setProperty("--controls-gap", `${row.gap}px`);
  app.style.setProperty("--left-w", `${layout.leftWidth || Math.min(appLayout.preferred_left_sidebar_width || 230, MAX_LEFT)}px`);
  app.style.setProperty("--right-w", `${layout.rightWidth}px`);
  app.style.setProperty("--progress-width", `${bar.progress}px`);
  app.style.setProperty("--player-height", `${document.querySelector(".player").offsetHeight + GAP}px`);
  if (fullscreen) applyFullscreen(app.clientWidth, app.clientHeight);
  if (layout.left !== "hidden") $("navigation").classList.remove("open");
  if (layout.right) $("right-panel").classList.remove("open");
  const sidebarLabel = tr({ full: "Collapse sidebar", compact: "Expand sidebar", hidden: "Show sidebar" }[layout.left]);
  for (const node of [$("sidebar-toggle")]) {
    node.title = sidebarLabel;
    node.setAttribute("aria-label", sidebarLabel);
  }
  $("menu-sidebar").lastElementChild.textContent = tr(layout.left === "full" ? "Collapse sidebar" : "Expand sidebar");
  const rightShown = layout.right || $("right-panel").classList.contains("open");
  const rightLabel = tr(rightShown ? "Hide sidebar" : "Show sidebar");
  $("toggle-right").title = rightLabel;
  $("toggle-right").setAttribute("aria-label", rightLabel);
  $("toggle-right").setAttribute("aria-expanded", String(rightShown));
  setIcon($("toggle-right").firstElementChild, rightShown ? "rufin-sidebar-collapse-right-symbolic" : "rufin-sidebar-expand-right-symbolic");
}

// The layout settings Preferences just saved.
function setLayoutSettings(layout) {
  Object.assign(appLayout, layout);
  applyLayout();
}

function toggleLeft() {
  if ($("app").dataset.left === "hidden") $("navigation").classList.toggle("open");
  else {
    toggled.left_sidebar = $("app").dataset.left === "full" ? "Compact" : "Full";
    saveToggled();
  }
  applyLayout();
}

function toggleRight() {
  const panel = $("right-panel");
  if ($("app").dataset.right === "visible") toggled.right_sidebar = "Hidden";
  else if (toggled.right_sidebar === "Hidden") delete toggled.right_sidebar;
  else panel.classList.toggle("open");
  saveToggled();
  applyLayout();
  if ($("app").dataset.right === "hidden" && toggled.right_sidebar !== "Hidden" && !panel.classList.contains("open"))
    panel.classList.add("open");
  applyLayout();
}

function initLayout() {
  const divider = $("queue-lyrics-divider");
  const panel = $("right-panel");
  const resize = (percent) => {
    percent = Math.max(10, Math.min(90, percent));
    panel.style.setProperty("--queue-share", percent);
    panel.style.setProperty("--lyrics-share", 100 - percent);
    divider.setAttribute("aria-valuenow", Math.round(percent));
  };
  divider.addEventListener("pointerdown", (event) => {
    if (event.button === 0) {
      event.preventDefault();
      divider.blur();
      divider.setPointerCapture(event.pointerId);
    }
  });
  divider.addEventListener("pointermove", (event) => {
    if (!divider.hasPointerCapture(event.pointerId)) return;
    const top = $("queue").getBoundingClientRect().top;
    const height =
      panel.getBoundingClientRect().bottom - top - divider.offsetHeight;
    resize(((event.clientY - top) / height) * 100);
  });
  divider.addEventListener("keydown", (event) => {
    const step = { ArrowUp: -5, ArrowDown: 5, Home: -100, End: 100 }[event.key];
    if (step == null) return;
    event.preventDefault();
    resize(Number(divider.getAttribute("aria-valuenow")) + step);
  });

  for (const [id, toggle] of [["sidebar-toggle", toggleLeft], ["menu-sidebar", toggleLeft], ["toggle-right", toggleRight]])
    $(id).addEventListener("click", (event) => {
      event.preventDefault();
      $("main-menu").hidePopover?.();
      toggle();
    });
  $("close-right").addEventListener("click", (event) => {
    event.preventDefault();
    if ($("app").dataset.right === "visible") toggleRight();
    else panel.classList.remove("open");
    applyLayout();
  });
  document.addEventListener("pointerdown", (event) => {
    if (event.target.closest("dialog, [popover]")) return;
    if (panel.classList.contains("open") && !event.target.closest("#right-panel, #toggle-right")) {
      panel.classList.remove("open");
      applyLayout();
    }
    if ($("navigation").classList.contains("open") && !event.target.closest("#navigation, #sidebar-toggle"))
      $("navigation").classList.remove("open");
  });
  $("toggle-fullscreen").addEventListener("click", () => {
    $("main-menu").hidePopover();
    if (document.fullscreenElement) document.exitFullscreen();
    else document.documentElement.requestFullscreen?.();
  });
  $("open-about").addEventListener("click", () => {
    $("main-menu").hidePopover();
    $("about-dialog").showModal();
  });
  for (const [anchor, popup] of [["main-menu-button", "main-menu"], ["sleep-timer-button", "sleep-timer"], ["playback-settings-button", "playback-settings"], ["fullscreen-customize", "fullscreen-settings"]]) {
    $(anchor).removeAttribute("popovertarget");
    $(anchor).addEventListener("click", () => {
      if ($(popup).matches(":popover-open")) $(popup).hidePopover();
      else openPopup($(popup), $(anchor), anchor === "main-menu-button" ? "end" : "center");
    });
  }
  new ResizeObserver(applyLayout).observe($("app"));
  addEventListener("resize", applyLayout);
  applyLayout();
}

function init() {
  document.addEventListener("pointerdown", (event) => {
    for (const row of document.querySelectorAll(".controls-visible")) {
      if (!row.contains(event.target)) row.classList.remove("controls-visible");
    }
  });
  initDialogs();
  initAppearance();
  initLayout();
}

// GTK's text slide: text wider than its room slides to its end and back once, pausing two
// seconds at each end, at 1.1 line heights a second, with its cut edges faded; a pointer over
// it slides it again. Reduced motion keeps the ellipsis.
const SLIDE_PAUSE = 2000;
const slides = new WeakMap();

function slideFade(node) {
  const overflow = node.scrollWidth - node.clientWidth;
  node.style.setProperty("--fade-start", node.scrollLeft > 1 ? "24px" : "0px");
  node.style.setProperty("--fade-end", overflow > 1 && node.scrollLeft < overflow - 1 ? "24px" : "0px");
}

function startSlide(node, state) {
  cancelAnimationFrame(state.frame);
  state.frame = requestAnimationFrame((start) => {
    node.scrollLeft = 0;
    slideFade(node);
    const overflow = node.scrollWidth - node.clientWidth;
    if (overflow <= 1 || !node.isConnected || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const speed = (parseFloat(getComputedStyle(node).fontSize) * 1.2 * 1.1) / 1000;
    const travel = overflow / speed;
    const step = (now) => {
      const elapsed = now - start;
      if (elapsed >= 2 * (SLIDE_PAUSE + travel) || !node.isConnected) {
        node.scrollLeft = 0;
        slideFade(node);
        state.completed = true;
        return;
      }
      node.scrollLeft =
        elapsed < SLIDE_PAUSE ? 0
        : elapsed < SLIDE_PAUSE + travel ? (elapsed - SLIDE_PAUSE) * speed
        : elapsed < 2 * SLIDE_PAUSE + travel ? overflow
        : overflow - (elapsed - 2 * SLIDE_PAUSE - travel) * speed;
      slideFade(node);
      state.frame = requestAnimationFrame(step);
    };
    state.frame = requestAnimationFrame(step);
  });
}

// Slides `node`'s text once from the start, as GTK does for new text; a new size starts a slide
// that has not finished again.
function slideText(node) {
  let state = slides.get(node);
  if (!state) {
    state = { frame: 0, completed: false };
    slides.set(node, state);
    node.classList.add("text-slide");
    node.addEventListener("pointerenter", () => {
      if (state.completed) {
        state.completed = false;
        startSlide(node, state);
      }
    });
    new ResizeObserver(() => (state.completed ? slideFade(node) : startSlide(node, state))).observe(node);
  }
  state.completed = false;
  startSlide(node, state);
}

function bindHoverControls(row) {
  if (!row.querySelector(".cover-controls, .pin-controls")) return;
  let revealTouch = false;
  let pointerType = "mouse";
  row.addEventListener("pointerenter", (event) => {
    if (event.pointerType === "mouse") {
      pointerType = "mouse";
      row.classList.add("controls-visible");
    }
  });
  row.addEventListener("pointerleave", (event) => {
    if (event.pointerType === "mouse") row.classList.remove("controls-visible");
  });
  row.addEventListener("pointerdown", (event) => {
    pointerType = event.pointerType;
    if (event.pointerType === "mouse") return;
    revealTouch = !row.classList.contains("controls-visible");
    row.classList.add("controls-visible");
  });
  row.addEventListener("pointercancel", () => {
    revealTouch = false;
    row.classList.remove("controls-visible");
  });
  row.addEventListener(
    "click",
    (event) => {
      if (revealTouch) {
        revealTouch = false;
        event.preventDefault();
        event.stopImmediatePropagation();
        return;
      }
      if (
        event.detail &&
        event.target.closest(".cover-controls button, .pin-controls button")
      ) {
        event.target.closest("button").blur();
        if (pointerType !== "mouse") row.classList.remove("controls-visible");
      }
    },
    true,
  );
  row.addEventListener("focusin", (event) => {
    if (event.target.matches(":focus-visible"))
      row.classList.add("controls-visible");
  });
  row.addEventListener("focusout", (event) => {
    if (
      !row.contains(event.relatedTarget) &&
      (pointerType !== "mouse" || !row.matches(":hover"))
    )
      row.classList.remove("controls-visible");
  });
}

export {
  $,
  setLayoutSettings,
  openFullscreen,
  closeFullscreen,
  switchPane,
  slideText,
  button,
  el,
  icon,
  notice,
  noticeError,
  openPopup,
  run,
  setIcon,
  showFavorite,
  time,
  applyAppearance,
  initialAppearance,
  applyColors,
  cover,
  coverGroup,
  covers,
  releaseCovers,
  bindHoverControls,
  init,
};
