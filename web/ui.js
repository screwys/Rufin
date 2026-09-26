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

const icon = (name) => {
  const node = el("span", "icon");
  node.dataset.icon = name;
  node.ariaHidden = "true";
  return node;
};

function setIcon(node, name) {
  if (node.dataset.icon !== name) node.dataset.icon = name;
}

function showFavorite(node, favorite) {
  if (node.getAttribute("aria-pressed") === String(favorite)) return;
  node.setAttribute("aria-pressed", String(favorite));
  setIcon(node.firstElementChild, favorite ? "favorite-filled" : "favorite");
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

function openPopup(popup, anchor) {
  popup.showPopover();
  const rect = anchor.getBoundingClientRect();
  popup.style.left = `${Math.max(8, Math.min(rect.left, innerWidth - popup.offsetWidth - 8))}px`;
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

const coverUrls = new Map();

function finishCoverReveal(node) {
  const image = node.querySelector("img");
  for (const animation of image?.getAnimations() || []) {
    if (animation.playState === "finished") {
      animation.cancel();
      image.animate(
        { filter: ["blur(1px)", "blur(0)"] },
        { duration: 80, easing: "ease-out" },
      );
    } else {
      animation.effect.setKeyframes({ filter: ["blur(4px)", "blur(0)"] });
      animation.onfinish = () => animation.cancel();
    }
  }
}

async function cover(node, uri, signal, reveal = "none") {
  const query = typeof uri === "string" ? { uri } : uri;
  const { original, ...still } = query;
  const preview = original ? cover(node, still, signal, "preview") : false;
  try {
    const [response, previewShown] = await Promise.all([
      fetch(`/api/artwork?${new URLSearchParams(query)}`, { signal }),
      preview,
    ]);
    if (original) reveal = previewShown ? "none" : "ready";
    if (!response.ok) return previewShown;
    const blob = await response.blob();
    if (signal.aborted || !node.isConnected) return false;
    const url = URL.createObjectURL(blob);
    const image = el("img");
    image.alt = "";
    image.src = url;
    try {
      await image.decode();
    } catch {
      URL.revokeObjectURL(url);
      return previewShown;
    }
    if (signal.aborted || !node.isConnected) {
      URL.revokeObjectURL(url);
      return false;
    }
    const previous = coverUrls.get(node);
    if (previous) URL.revokeObjectURL(previous);
    coverUrls.set(node, url);
    const current = node.querySelector("img");
    if (previewShown && current) {
      current.src = url;
      return true;
    }
    for (const animation of node.getAnimations({ subtree: true }))
      animation.cancel();
    node.replaceChildren(image);
    if (
      reveal !== "none" &&
      !matchMedia("(prefers-reduced-motion: reduce)").matches
    ) {
      image.animate(
        { filter: ["blur(4px)", reveal === "preview" ? "blur(1px)" : "blur(0)"] },
        {
          duration: 200,
          easing: "ease-in-out",
          fill: reveal === "preview" ? "forwards" : "none",
        },
      );
    }
    return true;
  } finally {
    if (original && (await preview) && !signal.aborted && node.isConnected)
      finishCoverReveal(node);
  }
}

function releaseCovers() {
  for (const [node, url] of coverUrls)
    if (!node.isConnected) {
      URL.revokeObjectURL(url);
      coverUrls.delete(node);
    }
}

async function coverGroup(node, query, count, signal) {
  node.classList.toggle("cover-mosaic", count > 1);
  if (count <= 1) return cover(node, query, signal);
  node.replaceChildren();
  await Promise.all(
    Array.from({ length: 4 }, (_, index) => {
      const tile = el("span", "cover-quadrant");
      tile.append(icon("cover-fallback"));
      node.append(tile);
      return cover(tile, { ...query, part: index % count }, signal);
    }),
  );
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

  $("toggle-right").addEventListener("click", (event) => {
    event.preventDefault();
    const hidden = $("app").classList.toggle("right-hidden");
    $("toggle-right").setAttribute("aria-expanded", String(!hidden));
    $("toggle-right").firstElementChild.dataset.icon = hidden
      ? "expand-right"
      : "collapse-right";
  });
  $("close-right").addEventListener("click", (event) => {
    event.preventDefault();
    $("app").classList.add("right-hidden");
    $("toggle-right").setAttribute("aria-expanded", "false");
    $("toggle-right").firstElementChild.dataset.icon = "expand-right";
  });
  $("open-navigation").addEventListener("click", (event) => {
    event.preventDefault();
    $("navigation").classList.add("open");
  });
  $("close-navigation").addEventListener("click", (event) => {
    event.preventDefault();
    $("navigation").classList.remove("open");
  });
  document.addEventListener("pointerdown", (event) => {
    if (event.target.closest("dialog, [popover]")) return;
    if (
      !$("app").classList.contains("right-hidden") &&
      getComputedStyle($("right-panel")).position === "absolute" &&
      !event.target.closest("#right-panel, #toggle-right")
    )
      $("close-right").click();
    if (
      $("navigation").classList.contains("open") &&
      getComputedStyle($("navigation")).position === "absolute" &&
      !event.target.closest("#navigation, #open-navigation")
    )
      $("close-navigation").click();
  });
  if (matchMedia("(max-width:860px)").matches) {
    $("app").classList.add("right-hidden");
    $("toggle-right").setAttribute("aria-expanded", "false");
    $("toggle-right").firstElementChild.dataset.icon = "expand-right";
  }
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
  coverUrls,
  releaseCovers,
  bindHoverControls,
  init,
};
