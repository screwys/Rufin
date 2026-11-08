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

function notice(message) {
  $("toast").textContent = message;
  $("toast").hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    $("toast").hidden = true;
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
  if (!fullReady && preview && node.querySelector("img")?.getAttribute("src") !== preview) {
    const image = el("img");
    image.alt = "";
    image.src = preview;
    node.replaceChildren(image);
  } else if (!fullReady && !preview && node.dataset.coverIdentity && node.dataset.coverIdentity !== identity) {
    node.replaceChildren(icon("rufin-symbolic"));
  }
  node.dataset.coverIdentity = identity;
  node.dataset.coverFull = String(fullReady);
  let request, pixels = 0;
  const entry = { url: null, dispose };
  const resize = new ResizeObserver(() => {
    const size = physicalSize();
    if (!query.original && size > pixels) run(() => load(size));
  });
  function physicalSize() {
    const bounds = node.getBoundingClientRect();
    return Math.max(1, Math.ceil(Math.max(bounds.width, bounds.height) * devicePixelRatio));
  }
  function dispose() {
    request?.abort();
    resize.disconnect();
    signal.removeEventListener("abort", dispose);
    if (entry.url) URL.revokeObjectURL(entry.url);
    if (covers.get(node) === entry) covers.delete(node);
  }
  async function load(size) {
    pixels = size;
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
  signal.addEventListener("abort", dispose, { once: true });
  resize.observe(node);
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
  MAX_RIGHT = 500, NARROW_BAR = 642, TRANSPORT = 308, MINI_WIDTH = 360, MINI_HEIGHT = 240;
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

// What the bottom bar shows at this width: its layout, the actions beside the volume and the
// seek bar's width.
function barLayout(width) {
  const side = (width - 16 - TRANSPORT) / 2;
  const action = (count) => 158 + 34 * count;
  return {
    bar: width >= NARROW_BAR ? "desktop" : "controls",
    // Auto DJ and random play keep their places while all seven buttons fit.
    transport: width - 16 >= TRANSPORT ? "all" : "five",
    actions: side >= action(2) ? "queue" : side >= action(1) ? "favorite" : "volume",
    progress: Math.max(140, Math.min(140 + ((width - NARROW_BAR) * 95) / 300, width - 16 - 2 * action(0) - 72)),
  };
}

function applyLayout() {
  const app = $("app");
  const width = app.clientWidth - 2 * GAP;
  const mini = innerHeight < MINI_HEIGHT ? "short" : innerWidth < MINI_WIDTH ? "tall" : null;
  const layout = resolveLayout(width);
  const bar = barLayout(app.clientWidth);
  if (mini) app.dataset.mini = mini;
  else delete app.dataset.mini;
  $("mini-title").hidden = !mini;
  app.dataset.left = layout.left;
  app.dataset.right = layout.right ? "visible" : "hidden";
  app.dataset.bar = mini ? "mini" : bar.bar;
  app.dataset.actions = bar.actions;
  app.dataset.transport = mini === "short" ? "five" : bar.transport;
  app.style.setProperty("--left-w", `${layout.leftWidth || Math.min(appLayout.preferred_left_sidebar_width || 230, MAX_LEFT)}px`);
  app.style.setProperty("--right-w", `${layout.rightWidth}px`);
  app.style.setProperty("--progress-width", `${bar.progress}px`);
  app.style.setProperty("--player-height", `${document.querySelector(".player").offsetHeight + GAP}px`);
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
  for (const [anchor, popup] of [["main-menu-button", "main-menu"], ["playback-settings-button", "playback-settings"]]) {
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
