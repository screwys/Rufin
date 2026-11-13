import { api } from "./connection.js";
import { $, button, el, icon, notice, run, tr } from "./ui.js";

// The editor core keeps open for this dialog, as GTK's metadata dialog holds it.
let editor = null;
let lyrics = null;
let artworkUrl = null;

const query = () => `?id=${editor.id}`;

// Opens "Edit metadata" for {kind, uri}, a page's selected rows {page, ranges} or {uris}.
function openMetadata(item) {
  return run(async () => {
    const opened = await api("/metadata", "POST", item);
    close();
    editor = opened;
    lyrics = null;
    $("metadata-status").textContent = "";
    $("metadata-save").disabled = false;
    $("metadata-save").textContent = tr("Save");
    $("metadata-cancel").textContent = tr("Cancel");
    $("metadata-lyrics-tab").hidden = editor.selected_count !== 1 || editor.kind !== "Track";
    // As GTK: artwork for one item, lyrics for one track, and no switcher for Tags alone.
    $("metadata-artwork-tab").hidden = editor.selected_count !== 1;
    $("metadata-pages").hidden = editor.selected_count !== 1;
    document.querySelector('[name="metadata-page"][value="tags"]').checked = true;
    showSection("tags");
    show(editor);
    $("metadata-dialog").showModal();
  });
}

// Any number of a field's options, picked in a menu. The field is set when the menu closes.
function choices(field, set) {
  const chosen = new Set(field.value.split(";").map((value) => value.trim()).filter(Boolean));
  const value = () => field.options.filter((option) => chosen.has(option)).join("; ");
  const picker = el("button", "metadata-choices");
  picker.type = "button";
  picker.setAttribute("aria-haspopup", "listbox");
  const name = el("span", "", field.value);
  picker.append(name, icon("rufin-pan-down-symbolic"));
  const menu = el("div", "smart-suggestions");
  menu.popover = "auto";
  menu.setAttribute("role", "listbox");
  menu.setAttribute("aria-multiselectable", "true");
  for (const option of field.options) {
    const item = el("button", "menu-item");
    item.type = "button";
    item.setAttribute("role", "option");
    item.append(icon("rufin-object-select-symbolic"), el("span", "", option));
    const mark = () => {
      item.setAttribute("aria-selected", String(chosen.has(option)));
      item.classList.toggle("active", chosen.has(option));
    };
    mark();
    item.addEventListener("click", () => {
      if (!chosen.delete(option)) chosen.add(option);
      mark();
      name.textContent = value();
    });
    menu.append(item);
  }
  picker.addEventListener("click", () => {
    menu.showPopover();
    const rect = picker.getBoundingClientRect();
    menu.style.minWidth = `${rect.width}px`;
    menu.style.left = `${Math.max(8, Math.min(rect.left, innerWidth - menu.offsetWidth - 8))}px`;
    menu.style.top = `${rect.bottom + menu.offsetHeight + 12 <= innerHeight ? rect.bottom + 4 : Math.max(8, rect.top - menu.offsetHeight - 4)}px`;
  });
  menu.addEventListener("toggle", (event) => {
    if (event.newState === "closed" && value() !== field.value) set(value());
  });
  picker.menu = menu;
  return picker;
}

// A list's values as chips, each removed on its own, and an input that adds one.
function values(field, value, set) {
  const chips = value.split(";").map((chip) => chip.trim()).filter(Boolean);
  const host = el("span", "metadata-values");
  chips.forEach((chip, index) => {
    const node = el("span", "metadata-chip", chip);
    if (field.writable)
      node.append(button(tr("Remove"), () => set(chips.filter((_, other) => other !== index).join("; ")), "rufin-window-close-symbolic"));
    host.append(node);
  });
  if (!field.writable) return host;
  const add = el("input");
  add.setAttribute("aria-label", field.label);
  add.dataset.key = field.key;
  add.addEventListener("change", () => {
    if (add.value.trim()) set([...chips, add.value].join("; "));
  });
  // Enter adds the value; it does not save the dialog.
  add.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    add.dispatchEvent(new Event("change"));
  });
  host.append(add);
  return host;
}

// A date is typed, in whole or in part, or picked from the browser's calendar.
function calendar(input) {
  const date = el("input", "metadata-date");
  date.type = "date";
  date.tabIndex = -1;
  date.addEventListener("change", () => {
    if (!date.value) return;
    input.value = date.value;
    input.dispatchEvent(new Event("change"));
  });
  const open = button(tr("Choose"), () => {
    // A year or a month opens on its first day.
    const [year, month = "01", day = "01"] = input.value.split(/\D+/);
    date.value = /^\d{4}$/.test(year) ? `${year}-${month}-${day}` : "";
    date.showPicker();
  }, "rufin-x-office-calendar-symbolic");
  const host = el("span", "metadata-calendar");
  host.append(open, date);
  return host;
}

function show(value) {
  editor = { ...editor, ...value };
  const host = $("metadata-fields");
  const focused = document.activeElement?.dataset.key;
  host.replaceChildren();
  for (const field of editor.fields) {
    if (!field.writable && !field.value) continue;
    const several = field.options.length && field.kind === "List";
    const list = !field.options.length && field.kind === "List";
    const shown = field.mixed ? "" : field.value;
    // A menu's items and a list's chips are not the row's control, so its row is not a label.
    const row = el(several || list ? "div" : "label", "pref-row metadata-field");
    const label = el("span", "pref-label");
    label.append(el("span", "", field.mixed ? field.mixed_label : field.label));
    const set = (value) =>
      run(async () => show(await api(`/metadata${query()}`, "PATCH", { operation: "set", key: field.key, value })));
    let input;
    if (several) {
      input = choices(field, set);
    } else if (field.options.length) {
      input = el("select");
      for (const option of ["", ...field.options]) input.append(new Option(option, option));
    } else if (list) {
      input = values(field, shown, set);
    } else {
      input = el("input");
      if (field.kind === "Number") Object.assign(input, { type: "number", min: "0", step: "any" });
    }
    if (!several && !list) {
      // The number a text starts with: tags hold `8; 8` and `120.5` too.
      input.value = field.kind === "Number" ? parseInt(shown) || "" : shown;
      input.addEventListener("change", () => set(input.value));
    }
    if (!list) input.dataset.key = field.key;
    input.disabled = !field.writable;
    if (!field.writable) row.title = tr("This source cannot edit this field");
    row.append(label, input);
    if (several) row.append(input.menu);
    if (field.writable) {
      if (field.kind === "Date") row.append(calendar(input));
      row.append(button(tr("Clear"), () => set(""), "rufin-edit-clear-symbolic"));
      if (field.changed) {
        const reset = button(tr("Original: {value}", { value: field.original }), () =>
          run(async () => show(await api(`/metadata${query()}`, "PATCH", { operation: "reset", key: field.key }))),
        "rufin-edit-undo-symbolic");
        row.append(reset);
      }
    }
    host.append(row);
  }
  host.querySelector(`[data-key="${focused}"]`)?.focus();
  $("metadata-lookup-open").lastElementChild.textContent = tr(editor.kind === "Track" ? "Search by AcoustID" : "Lookup");
  $("metadata-lookup-open").disabled = !editor.external_lookup_allowed;
  $("metadata-scope").hidden = editor.track_count <= 1;
  $("metadata-scope").textContent = editor.scope_title
    ? `${editor.scope_title} · ${tr("Changes apply to {count} tracks", { count: editor.track_count })}`
    : tr("Changes apply to {count} tracks", { count: editor.track_count });
  $("metadata-lock-group").hidden = !editor.lock_writable;
  $("metadata-lock").checked = !!editor.locked;
  $("metadata-artwork-remove").disabled = !editor.can_remove_artwork;
  $("metadata-artwork-storage").hidden = editor.artwork_storage === "Server" || !editor.can_embed_artwork;
  $("metadata-artwork-storage").value = editor.artwork_storage;
  if (!$("metadata-artwork-artist").value) $("metadata-artwork-artist").value = editor.search_artist || "";
  if (!$("metadata-artwork-album").value) $("metadata-artwork-album").value = editor.search_album || "";
  if (editor.artwork_revision !== showArtwork.revision) showArtwork();
}

async function showArtwork() {
  showArtwork.revision = editor.artwork_revision;
  const preview = $("metadata-artwork-preview");
  if (artworkUrl) URL.revokeObjectURL(artworkUrl);
  artworkUrl = null;
  const response = await fetch(`/api/metadata/artwork${query()}`);
  if (!response.ok) {
    preview.replaceChildren(icon("rufin-symbolic"));
    return;
  }
  artworkUrl = URL.createObjectURL(await response.blob());
  const image = el("img");
  image.alt = "";
  image.src = artworkUrl;
  preview.replaceChildren(image);
}

function showSection(page) {
  for (const name of ["tags", "artwork", "lyrics"]) $(`metadata-${name}`).hidden = name !== page;
  if (page === "artwork") run(showArtwork);
  if (page === "lyrics" && !lyrics)
    run(async () => {
      lyrics = await api(`/metadata/lyrics${query()}`);
      $("metadata-lyrics-artist").value = lyrics.artist;
      $("metadata-lyrics-title").value = lyrics.title;
      $("metadata-lyrics-text").value = lyrics.text || "";
      for (const option of $("metadata-lyrics-storage").options) option.hidden = !lyrics.storages.includes(option.value);
      $("metadata-lyrics-storage").value = lyrics.storage;
      $("metadata-lyrics-search").disabled = !lyrics.can_search;
    });
}

function close() {
  if ($("lookup-dialog").open) $("lookup-dialog").close();
  if (editor) {
    const id = editor.id;
    editor = null;
    run(() => api(`/metadata?id=${id}`, "DELETE"));
  }
  if (artworkUrl) URL.revokeObjectURL(artworkUrl);
  artworkUrl = null;
  showArtwork.revision = undefined;
  for (const id of ["metadata-artwork-artist", "metadata-artwork-album"]) $(id).value = "";
  $("metadata-artwork-results").replaceChildren();
  $("metadata-lyrics-results").replaceChildren();
}

async function save(confirmed = false) {
  $("metadata-save").disabled = true;
  try {
    const lyricsChanged = lyrics && $("metadata-lyrics-text").value !== (lyrics.text || "");
    const result = await api(`/metadata/save${query()}`, "POST", {
      confirmed,
      lyrics: lyricsChanged ? [$("metadata-lyrics-text").value, $("metadata-lyrics-storage").value] : null,
    });
    if (result.confirm) {
      if (confirm(result.confirm)) return save(true);
      return;
    }
    show(result);
    if (result.error) {
      $("metadata-status").textContent = result.error;
      // Saved in part: what is saved stays saved, so the dialog only closes now.
      if (result.committed) {
        $("metadata-save").textContent = tr("Saved");
        $("metadata-cancel").textContent = tr("Close");
        return;
      }
    } else if (result.done) {
      $("metadata-dialog").close();
      notice(tr("Metadata saved"));
    }
  } finally {
    if ($("metadata-save").textContent !== tr("Saved")) $("metadata-save").disabled = false;
  }
}

// The lookup GTK's Lookup dialog runs: fingerprints and MusicBrainz matches for each track,
// chosen per track, then taken into the draft.
let lookup = null;
const matchKinds = { Recording: "Recording", Release: "Release", ReleaseGroup: "Release group", Artist: "Artist" };

async function openLookup() {
  const value = await api(`/metadata/lookup${query()}`, "POST");
  lookup = { index: value.tracks[0]?.index ?? 0, accepted: false };
  $("lookup-query").value = "";
  $("lookup-fill-missing").checked = false;
  showLookup(value);
  $("lookup-dialog").showModal();
  run(followLookup);
}

// Follows the lookup while it runs, a change at a time.
async function followLookup() {
  while (lookup && editor) {
    const value = await api(`/metadata/lookup${query()}&${new URLSearchParams({ index: lookup.index, wait: 1 })}`);
    if (!lookup) return;
    showLookup(value);
    if (value.progress.finished) return;
  }
}

function showLookup(value) {
  const { progress, item } = value;
  $("lookup-progress").max = Math.max(1, progress.total);
  $("lookup-progress").value = progress.completed;
  $("lookup-progress").hidden = progress.finished;
  $("lookup-stage").textContent = progress.failure || (progress.finished ? "" : progress.stage || tr("Searching..."));
  $("lookup-stop").hidden = progress.finished;
  $("lookup-tracks").hidden = progress.total <= 1;
  $("lookup-tracks").replaceChildren(
    ...value.tracks.map((track) => {
      const row = button(track.title, () => {
        lookup.index = track.index;
        return api(`/metadata/lookup${query()}&index=${track.index}`).then(showLookup);
      }, null, "menu-item");
      row.replaceChildren(el("span", "", track.title), el("small", "", track.error || track.status || ""));
      row.setAttribute("aria-current", String(track.index === item.index));
      return row;
    }),
  );
  $("lookup-item-title").textContent = item.title;
  $("lookup-status").textContent = item.failure || "";
  const choose = (candidate) => run(async () =>
    showLookup(await api(`/metadata/lookup/item${query()}&index=${item.index}`, "POST", { operation: "choose", candidate })));
  const match = (title, detail, candidate) => {
    const row = el("label", "pref-row");
    const radio = el("input");
    radio.type = "radio";
    radio.name = "lookup-match";
    radio.checked = (candidate?.id ?? null) === (item.selected ?? null);
    radio.addEventListener("change", () => choose(candidate));
    const label = el("span", "pref-label");
    label.append(el("span", "", title));
    if (detail) label.append(el("small", "", detail));
    row.append(radio, label);
    return row;
  };
  $("lookup-matches").replaceChildren(
    match(tr("Original"), "", null),
    ...item.candidates.map((candidate) =>
      match(candidate.title, [tr(matchKinds[candidate.kind] || candidate.kind), candidate.detail].filter(Boolean).join(" · "), candidate)),
  );
  if (!item.ready && !item.candidates.length) $("lookup-matches").append(el("p", "empty-note", tr("Searching...")));
  $("lookup-comparison").tBodies[0].replaceChildren(
    ...item.fields.map((field) => {
      const row = el("tr", field.changed ? "changed" : "");
      row.append(el("th", "", field.label), el("td", "", field.original), el("td", "", field.suggested));
      return row;
    }),
  );
}

function closeLookup() {
  if (!lookup) return;
  const accepted = lookup.accepted;
  lookup = null;
  if (!accepted && editor) run(() => api(`/metadata/lookup${query()}`, "DELETE"));
}

function readFile(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).split(",", 2)[1]);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

function init() {
  const dialog = $("metadata-dialog");
  $("metadata-lookup-open").addEventListener("click", () => run(openLookup));
  $("lookup-dialog").addEventListener("close", closeLookup);
  $("lookup-cancel").addEventListener("click", () => $("lookup-dialog").close());
  $("lookup-stop").addEventListener("click", () => run(async () => {
    await api(`/metadata/lookup${query()}`, "DELETE");
    $("lookup-stop").hidden = true;
    $("lookup-stage").textContent = "";
  }));
  $("lookup-done").addEventListener("click", () => run(async () => {
    const result = await api(`/metadata/lookup/accept${query()}`, "POST", { fill_missing: $("lookup-fill-missing").checked });
    lookup.accepted = true;
    $("lookup-dialog").close();
    show(result);
  }));
  for (const [id, operation] of [["lookup-query", "search"], ["lookup-refresh", "refresh"]])
    $(id).addEventListener(id === "lookup-query" ? "keydown" : "click", (event) => {
      if (id === "lookup-query" && event.key !== "Enter") return;
      event.preventDefault();
      run(async () => showLookup(await api(`/metadata/lookup/item${query()}&index=${lookup.index}`, "POST", { operation, query: $("lookup-query").value })));
    });
  dialog.addEventListener("close", close);
  $("metadata-cancel").addEventListener("click", () => dialog.close());
  $("metadata-form").addEventListener("submit", (event) => {
    event.preventDefault();
    run(() => save());
  });
  for (const tab of document.querySelectorAll('[name="metadata-page"]'))
    tab.addEventListener("change", () => showSection(tab.value));
  $("metadata-lock").addEventListener("change", () =>
    run(async () => show(await api(`/metadata${query()}`, "PATCH", { operation: "lock", locked: $("metadata-lock").checked }))),
  );
  const artwork = (edit) => run(async () => {
    const result = await api(`/metadata/artwork${query()}`, "POST", edit);
    show(result);
    return result;
  });
  $("metadata-artwork-file").addEventListener("change", (event) => {
    const [file] = event.target.files;
    event.target.value = "";
    if (file) run(async () => artwork({ operation: "upload", data: await readFile(file) }));
  });
  $("metadata-artwork-remove").addEventListener("click", () => artwork({ operation: "remove" }));
  $("metadata-artwork-storage").addEventListener("change", () =>
    artwork({ operation: "storage", storage: $("metadata-artwork-storage").value }));
  $("metadata-artwork-search").addEventListener("click", () => run(async () => {
    $("metadata-artwork-results").replaceChildren(el("p", "empty-note", tr("Loading...")));
    const result = await artwork({ operation: "search", artist: $("metadata-artwork-artist").value, album: $("metadata-artwork-album").value });
    $("metadata-artwork-results").replaceChildren(
      ...(result?.results || []).map((entry, index) => {
        const choice = el("button", "metadata-artwork-result");
        choice.type = "button";
        const image = el("img");
        image.alt = "";
        image.loading = "lazy";
        image.src = entry.thumbnail;
        choice.append(image, el("span", "", entry.title), el("small", "", entry.detail));
        choice.addEventListener("click", () => artwork({ operation: "choose", index }));
        return choice;
      }),
    );
    if (!result?.results?.length) $("metadata-artwork-results").replaceChildren(el("p", "empty-note", tr("No results")));
  }));
  $("metadata-lyrics-search").addEventListener("click", () => run(async () => {
    $("metadata-lyrics-results").replaceChildren(el("p", "empty-note", tr("Loading...")));
    const result = await api(`/metadata/lyrics${query()}`, "POST", {
      operation: "search", artist: $("metadata-lyrics-artist").value, title: $("metadata-lyrics-title").value,
    });
    $("metadata-lyrics-results").replaceChildren(
      ...result.results.map((entry, index) => {
        const choice = button(entry.title, async () => {
          const chosen = await api(`/metadata/lyrics${query()}`, "POST", { operation: "choose", index });
          if (chosen.text != null) $("metadata-lyrics-text").value = chosen.text;
        }, null, "menu-item");
        choice.replaceChildren(el("span", "", `${entry.title} · ${entry.artist}`), el("small", "", entry.provider));
        return choice;
      }),
    );
    if (!result.results.length) $("metadata-lyrics-results").replaceChildren(el("p", "empty-note", tr("No results")));
  }));
}

export { init, openMetadata };
