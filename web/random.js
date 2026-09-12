import { tr } from "./ui.js";
import { api, state } from "./connection.js";

import { $, button, icon, notice, openPopup, run } from "./ui.js";

let genreOffset = 0;

let genreTimer;

let genreRequest = 0;

let selectedGenre = null;

const genreRowHeight = 36;

function chooseGenre(row) {
  selectedGenre = row;
  $("random-genre").textContent = row?.name || tr("Any genre");
  if ($("genre-menu").matches(":popover-open")) $("genre-menu").hidePopover();
}

async function loadRandom(reset = false) {
  if (!state.source) return;
  const requestedSource = state.source,
    request = ++genreRequest,
    offset = genreOffset;
  const result = await api(
    `/random?${new URLSearchParams({ source: state.source, ...(state.selectedLibrary ? { folder: state.selectedLibrary } : {}), q: $("genre-search").value, offset })}`,
  );
  if (request !== genreRequest || state.source !== requestedSource) return;
  if (reset || state.randomSource !== state.source) {
    const settings = result.settings;
    $("random-count").value = settings.limit;
    $("use-min-year").checked = settings.min_year != null;
    $("use-max-year").checked = settings.max_year != null;
    $("random-min-year").disabled = settings.min_year == null;
    $("random-max-year").disabled = settings.max_year == null;
    for (const id of ["random-min-year", "random-max-year"]) {
      for (const button of $(id).parentElement.querySelectorAll("button"))
        button.disabled = $(id).disabled;
    }
    $("random-min-year").value = settings.min_year ?? 1850;
    $("random-max-year").value = settings.max_year ?? 2050;
    $("random-played").value = settings.played_filter;
    chooseGenre(result.selected_genre);
    state.randomSource = state.source;
  }
  const window = $("genre-window");
  const focused = document.activeElement?.dataset.genreId;
  window.replaceChildren();
  window.style.paddingTop = `${offset * genreRowHeight}px`;
  window.style.paddingBottom =
    result.genres.length === 64 ? `${32 * genreRowHeight}px` : "0px";
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
    if (row.id === selectedGenre?.id) option.append(icon("check"));
    window.append(option);
    if (focused === String(row.id)) option.focus({ preventScroll: true });
  }
}

async function openRandom() {
  if (!state.source) return;
  genreOffset = 0;
  $("genre-search").value = "";
  await loadRandom(state.randomSource !== state.source);
  $("random-dialog").showPopover();
  $("random-count").focus();
}

async function playRandom(mode = "replace") {
  const result = await api("/queue/random", "POST", {
    source: state.source,
    folder: state.selectedLibrary,
    count: Number($("random-count").value),
    min_year: $("use-min-year").checked
      ? Number($("random-min-year").value)
      : null,
    max_year: $("use-max-year").checked
      ? Number($("random-max-year").value)
      : null,
    genre: selectedGenre?.id ?? null,
    played: $("random-played").value,
    mode,
  });
  if (result.empty) notice(tr("No matching tracks found"));
}

function init() {
  $("random-form").addEventListener("click", (event) => {
    const step = event.target.closest("[data-step]");
    if (!step) return;
    const input = step.parentElement.querySelector("input");
    if (!input.disabled) input.stepUp(Number(step.dataset.step));
  });
  $("random").addEventListener("click", () => run(openRandom));
  $("random").addEventListener("contextmenu", (event) => {
    event.preventDefault();
    run(openRandom);
  });
  $("random-form").addEventListener("submit", (event) => {
    event.preventDefault();
    run(async () => {
      await playRandom(event.submitter.value);
      $("random-dialog").hidePopover();
    });
  });
  for (const [toggle, field] of [
    ["use-min-year", "random-min-year"],
    ["use-max-year", "random-max-year"],
  ]) {
    $(toggle).addEventListener("change", () => {
      $(field).disabled = !$(toggle).checked;
      for (const button of $(field).parentElement.querySelectorAll("button"))
        button.disabled = $(field).disabled;
    });
  }
  $("random-genre").addEventListener("click", () =>
    run(async () => {
      genreOffset = 0;
      $("genre-search").value = "";
      $("genre-options").scrollTop = 0;
      await loadRandom();
      $("any-genre").replaceChildren(document.createTextNode(tr("Any genre")));
      $("any-genre").setAttribute("aria-selected", String(!selectedGenre));
      if (!selectedGenre) $("any-genre").append(icon("check"));
      openPopup($("genre-menu"), $("random-genre"));
      $("genre-search").focus();
    }),
  );
  $("genre-menu").addEventListener("toggle", (event) =>
    $("random-genre").setAttribute(
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
      run(() => loadRandom());
    }, 150);
  });
  $("genre-options").addEventListener("scroll", () => {
    const offset = Math.max(
      0,
      Math.floor($("genre-options").scrollTop / genreRowHeight / 32) * 32,
    );
    if (offset === genreOffset) return;
    genreOffset = offset;
    clearTimeout(genreTimer);
    genreTimer = setTimeout(() => run(() => loadRandom()), 60);
  });
}

export { init };
