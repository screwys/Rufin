import { tr } from "./ui.js";
import { api, state } from "./connection.js";

import { $, button, icon, notice, openPopup, run } from "./ui.js";

let genreOffset = 0;
let genreTotal = 0;

let genreTimer;

let genreRequest = 0;

let selectedGenre = null;

const genreRowHeight = 36;

/** The counts of tracks and of albums; the field shows the one of the unit chosen. */
let limits = [100, 10];

/** The kind chosen, a `played` value. */
let played = "All";

/** The kind tiles a page holds, in three columns. */
const kindsPerPage = 6;
const kindColumns = 3;

/** The page of kinds shown. */
let kindPage = 0;

const kindTiles = () => [...$("mix-played").querySelectorAll("button")];

/** Favorites only as chosen, which a kind that fixes it on keeps. */
let chosenFavorites = false;

/** Favorites only follows the chosen kind: picking tracks of a kind whose rules pick favorites
 *  alone it shows on and can't change; otherwise it is as chosen. */
function showFavorites() {
  const tile = kindTiles().find((tile) => tile.value === played);
  const fixed = units().value !== "1" && tile?.dataset.favorites === "true";
  $("mix-favorites").disabled = fixed;
  $("mix-favorites").checked = fixed || chosenFavorites;
}

/** Builds the pages of kind tiles from core's kinds, once. */
function showKinds(kinds) {
  if ($("mix-played").childElementCount) return;
  for (let first = 0; first < kinds.length; first += kindsPerPage) {
    const page = document.createElement("div");
    page.className = "mix-page";
    for (const kind of kinds.slice(first, first + kindsPerPage)) {
      const tile = document.createElement("button");
      tile.type = "button";
      tile.value = kind.kind;
      tile.dataset.favorites = String(kind.favorites);
      tile.setAttribute("aria-pressed", "false");
      const title = document.createElement("span");
      title.textContent = kind.title;
      tile.append(icon(kind.icon), title);
      page.append(tile);
    }
    $("mix-played").append(page);
    $("mix-dots").append(document.createElement("span"));
  }
}

function showKindPage(page) {
  const pages = [...$("mix-played").children];
  kindPage = Math.max(0, Math.min(page, pages.length - 1));
  pages.forEach((shown, index) => (shown.hidden = index !== kindPage));
  [...$("mix-dots").children].forEach((dot, index) =>
    dot.setAttribute("aria-current", String(index === kindPage)),
  );
  $("mix-previous").disabled = kindPage === 0;
  $("mix-next").disabled = kindPage === pages.length - 1;
}

/** The unit's two choices, tracks and albums; its value is the index of the one chosen. */
const units = () => $("mix-form").elements["mix-unit"];

function showUnit() {
  const albums = units().value === "1";
  $("mix-count").value = limits[Number(albums)];
  // The first text names tracks; the others name albums and how they play.
  [...$("mix-count-label").children].forEach((text, index) => {
    text.hidden = albums === (index === 0);
  });
}

function choosePlayed(value) {
  played = value;
  const tiles = kindTiles();
  for (const tile of tiles)
    tile.setAttribute("aria-pressed", String(tile.value === value));
  const chosen = tiles.findIndex((tile) => tile.value === value);
  showKindPage(Math.floor(Math.max(0, chosen) / kindsPerPage));
  showFavorites();
}

/** A year field's value; an empty or unreadable one leaves that end open. */
function year(id) {
  const text = $(id).value.trim(),
    value = Number(text);
  return /^\d+$/.test(text) && value >= 1850 && value <= 2050 ? value : null;
}

function chooseGenre(row) {
  selectedGenre = row;
  $("mix-genre").firstElementChild.textContent = row?.name || tr("Any");
  if ($("genre-menu").matches(":popover-open")) $("genre-menu").hidePopover();
}

async function loadMix(reset = false) {
  if (!state.source) return;
  const requestedSource = state.source,
    request = ++genreRequest,
    offset = genreOffset;
  const result = await api(
    `/mix?${new URLSearchParams({ source: state.source, ...(state.selectedLibrary ? { folder: state.selectedLibrary } : {}), q: $("genre-search").value, offset, total: offset === 0 })}`,
  );
  if (request !== genreRequest || state.source !== requestedSource) return;
  if (result.total !== null) genreTotal = result.total;
  if (reset || state.mixSource !== state.source) {
    const settings = result.settings;
    limits = [settings.limit, settings.album_limit];
    units().value = Number(settings.albums);
    showUnit();
    $("mix-min-year").value = settings.min_year ?? "";
    $("mix-max-year").value = settings.max_year ?? "";
    chosenFavorites = settings.favorites;
    showKinds(result.kinds);
    choosePlayed(settings.played_filter);
    chooseGenre(result.selected_genre);
    state.mixSource = state.source;
  }
  const window = $("genre-window");
  const focused = document.activeElement?.dataset.genreId;
  window.replaceChildren();
  window.style.paddingTop = `${offset * genreRowHeight}px`;
  window.style.paddingBottom = `${Math.max(0, genreTotal - offset - result.genres.length) * genreRowHeight}px`;
  for (const row of result.genres) {
    const option = button(
      row.name,
      () => chooseGenre(row),
      null,
      "genre-option",
    );
    option.setAttribute("role", "option");
    option.dataset.genreId = String(row.id);
    option.setAttribute("aria-selected", String(row.id === selectedGenre?.id));
    if (row.id === selectedGenre?.id) option.append(icon("rufin-object-select-symbolic"));
    window.append(option);
    if (focused === String(row.id)) option.focus({ preventScroll: true });
  }
}

async function openMix() {
  if (!state.source) return;
  genreOffset = 0;
  $("genre-search").value = "";
  await loadMix(state.mixSource !== state.source);
  openPopup($("mix-dialog"), $("mix"), "center");
  units()[Number(units().value)].focus();
}

async function playMix(mode = "replace") {
  const result = await api("/queue/mix", "POST", {
    source: state.source,
    folder: state.selectedLibrary,
    albums: units().value === "1",
    count: Number($("mix-count").value),
    min_year: year("mix-min-year"),
    max_year: year("mix-max-year"),
    genre: selectedGenre?.id ?? null,
    played,
    favorites: chosenFavorites,
    mode,
  });
  if (result.empty) notice(tr("No matching tracks found"));
}

function init() {
  $("mix-form").addEventListener("click", (event) => {
    const step = event.target.closest("[data-step]");
    if (!step) return;
    $("mix-count").stepUp(Number(step.dataset.step));
  });
  // Each unit keeps its own count.
  for (const unit of units())
    unit.addEventListener("change", () => {
      limits[1 - Number(units().value)] = Number($("mix-count").value);
      showUnit();
      showFavorites();
    });
  $("mix-favorites").addEventListener("change", () => {
    chosenFavorites = $("mix-favorites").checked;
  });
  $("mix-played").addEventListener("click", (event) => {
    const tile = event.target.closest("button");
    if (tile) choosePlayed(tile.value);
  });
  $("mix-previous").addEventListener("click", () => showKindPage(kindPage - 1));
  $("mix-next").addEventListener("click", () => showKindPage(kindPage + 1));
  // Left and Right past the edge tile of a page move to the next page's tile in the same row.
  $("mix-played").addEventListener("keydown", (event) => {
    const forward = event.key === "ArrowRight";
    if (!forward && event.key !== "ArrowLeft") return;
    const tiles = kindTiles(),
      index = tiles.indexOf(document.activeElement);
    if (index < 0) return;
    const place = index % kindsPerPage,
      row = Math.floor(place / kindColumns),
      column = place % kindColumns,
      page = Math.floor(index / kindsPerPage);
    const edge = forward ? column === kindColumns - 1 || index === tiles.length - 1 : column === 0;
    const target = forward ? page + 1 : page - 1;
    if (!edge || target < 0 || target * kindsPerPage >= tiles.length) return;
    event.preventDefault();
    const first = target * kindsPerPage,
      last = Math.min(first + kindsPerPage, tiles.length) - 1;
    showKindPage(target);
    tiles[Math.min(first + row * kindColumns + (forward ? 0 : kindColumns - 1), last)].focus();
  });
  $("mix").addEventListener("click", () => run(openMix));
  $("mix").addEventListener("contextmenu", (event) => {
    event.preventDefault();
    run(openMix);
  });
  $("mix-form").addEventListener("submit", (event) => {
    event.preventDefault();
    run(async () => {
      await playMix(event.submitter.value);
      $("mix-dialog").hidePopover();
    });
  });
  $("mix-genre").addEventListener("click", () =>
    run(async () => {
      genreOffset = 0;
      $("genre-search").value = "";
      $("genre-options").scrollTop = 0;
      await loadMix();
      $("any-genre").replaceChildren(document.createTextNode(tr("Any")));
      $("any-genre").setAttribute("aria-selected", String(!selectedGenre));
      if (!selectedGenre) $("any-genre").append(icon("rufin-object-select-symbolic"));
      openPopup($("genre-menu"), $("mix-genre"));
      $("genre-search").focus();
    }),
  );
  $("genre-menu").addEventListener("toggle", (event) =>
    $("mix-genre").setAttribute(
      "aria-expanded",
      String(event.newState === "open"),
    ),
  );
  $("any-genre").addEventListener("click", () => chooseGenre(null));
  $("genre-search").addEventListener("input", () => {
    clearTimeout(genreTimer);
    genreTimer = setTimeout(() => {
      genreOffset = 0;
      $("genre-options").scrollTop = 0;
      run(() => loadMix());
    }, 150);
  });
  $("genre-options").addEventListener("scroll", () => {
    const offset = Math.min(Math.max(0, genreTotal - 64), Math.max(
      0,
      Math.floor($("genre-options").scrollTop / genreRowHeight / 32) * 32,
    ));
    if (offset === genreOffset) return;
    genreOffset = offset;
    clearTimeout(genreTimer);
    genreTimer = setTimeout(() => run(() => loadMix()), 60);
  });
}

export { init };
