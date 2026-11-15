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
  for (const chip of $("mix-played").children)
    chip.setAttribute("aria-pressed", String(chip.value === value));
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
    });
  $("mix-played").addEventListener("click", (event) => {
    const chip = event.target.closest("button");
    if (chip) choosePlayed(chip.value);
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
