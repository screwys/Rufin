import { messages } from "/translations.js";
import { token } from "./connection.js";

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

let savedAppearance = { theme: "System", accent: "System", colors: {} };

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

async function cover(node, uri, signal) {
  const response = await fetch(
    `/api/artwork?${new URLSearchParams(typeof uri === "string" ? { uri } : uri)}`,
    {
      signal,
      headers: { Authorization: `Bearer ${token}` },
    },
  );
  if (!response.ok) return;
  const blob = await response.blob();
  if (signal.aborted || !node.isConnected) return;
  const url = URL.createObjectURL(blob);
  const previous = coverUrls.get(node);
  if (previous) URL.revokeObjectURL(previous);
  coverUrls.set(node, url);
  const image = el("img");
  image.alt = "";
  image.src = url;
  node.replaceChildren(image);
}

function releaseCovers() {
  for (const [node, url] of coverUrls)
    if (!node.isConnected) {
      URL.revokeObjectURL(url);
      coverUrls.delete(node);
    }
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

  $("toggle-right").addEventListener("click", () => {
    const hidden = $("app").classList.toggle("right-hidden");
    $("toggle-right").setAttribute("aria-expanded", String(!hidden));
    $("toggle-right").firstElementChild.dataset.icon = hidden
      ? "expand-right"
      : "collapse-right";
  });
  $("close-right").addEventListener("click", () => {
    $("app").classList.add("right-hidden");
    $("toggle-right").setAttribute("aria-expanded", "false");
    $("toggle-right").firstElementChild.dataset.icon = "expand-right";
  });
  $("open-navigation").addEventListener("click", () => {
    $("navigation").classList.add("open");
  });
  $("close-navigation").addEventListener("click", () => {
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
  initDialogs();
  initAppearance();
  initLayout();
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
  applyColors,
  cover,
  coverUrls,
  releaseCovers,
  init,
};
