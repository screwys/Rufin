import { api, browserHeaders, state } from "./connection.js";
import { qualityControls } from "./download-quality.js";
import { openSource, sourceIcon, updateSources } from "./sources.js";
import { $, el, icon, notice, openPopup, run, tr, setLayoutSettings } from "./ui.js";

let values = {};

// Every control naming a stored setting, in Preferences or the bar's settings popover, shows
// it and changes it. Choices hold their value as JSON.
function show(preferences) {
  values = preferences.values;
  for (const node of document.querySelectorAll("[data-setting]")) {
    const value = values[node.dataset.setting];
    if (value === undefined) continue;
    if (node.type === "checkbox") node.checked = !!value;
    else if (node.type === "radio") node.checked = node.value === JSON.stringify(value);
    else if (node.tagName === "SELECT") node.value = JSON.stringify(value);
    else node.value = value;
    if (node.type === "range") node.nextElementSibling.textContent = node.value;
  }
  for (const node of document.querySelectorAll("[data-when]")) {
    const [, path, not, expected] = node.dataset.when.match(/^(.*?)(!?)=(.*)$/);
    node.hidden = (values[path] === expected) === !!not;
  }
  for (const node of document.querySelectorAll("[data-menu-item]"))
    node.checked = values["/context_menu/items"]?.find((entry) => entry.item === node.dataset.menuItem)?.visible ?? true;
  listRows($("pref-routes"), preferences.routes, (entry) => entry.item, (entry, change) => ({ operation: "route", item: entry.item, ...change }));
  listRows($("pref-home-blocks"), preferences.home_blocks, (entry) => entry.block, (entry, change) => ({ operation: "home_block", block: entry.block, ...change }));
  // The sidebar follows its entries' order and visibility, as GTK's does.
  const nav = document.querySelector("#navigation nav");
  for (const entry of preferences.routes) {
    const link = nav.querySelector(`[data-route="${entry.item}"]`);
    if (!link) continue;
    link.hidden = !entry.visible;
    nav.append(link);
  }
  if (preferences.themes) renderThemes(preferences.themes, preferences.theme_errors);
  $("private-mode").lastElementChild.textContent = tr(values["/private_mode"] ? "Turn off private mode" : "Turn on private mode");
  setLayoutSettings({
    default_profile: { left_sidebar: values["/layout/default_profile/left_sidebar"], right_sidebar: values["/layout/default_profile/right_sidebar"] },
    narrow_enabled: values["/layout/narrow_enabled"],
    narrow_threshold: values["/layout/narrow_threshold"],
    narrow_profile: { left_sidebar: values["/layout/narrow_profile/left_sidebar"], right_sidebar: values["/layout/narrow_profile/right_sidebar"] },
    fullscreen_lyrics: values["/fullscreen_lyrics_visible"],
  });
  $("fullscreen-player").dataset.background = values["/fullscreen_dynamic_background"] === false ? "" : values["/fullscreen_background_image"] === false ? "color" : "image";
  $("right-panel").classList.toggle("without-queue", values["/right_panel/queue_visible"] === false);
  $("right-panel").classList.toggle("without-lyrics", values["/lyrics_panel_visible"] === false);
}

// Theme: every theme as a preview tile, as GTK's Theme page draws them; tiles with a choice of
// accents carry its swatch.
const svg = (tag, attributes) => {
  const node = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [name, value] of Object.entries(attributes)) node.setAttribute(name, value);
  return node;
};
function themePreview(colors) {
  const preview = svg("svg", { viewBox: "0 0 124 96", "aria-hidden": "true" });
  const accent = colors.accent || "var(--accent)";
  const rect = (fill, opacity, x, y, width, height) =>
    preview.append(svg("rect", { x, y, width, height, style: `fill:${fill};fill-opacity:${opacity}` }));
  rect(colors.window, 1, 0, 0, 124, 96);
  rect(colors.sidebar, 1, 3, 12, 23, 69);
  rect(colors.view, 1, 29, 12, 62, 69);
  rect(colors.secondary, 1, 94, 12, 27, 69);
  rect(accent, 0.2, 32, 16, 56, 17);
  rect(colors.text, 0.3, 48, 20, 33, 2);
  rect(colors.text, 0.3, 48, 26, 24, 2);
  rect(colors.text, 0.15, 35, 19, 10, 10);
  rect(colors.text, 0.3, 32, 38, 20, 2);
  for (let row = 0; row < 2; row++)
    for (let column = 0; column < 3; column++) rect(colors.text, 0.15, 32 + column * 19, 45 + row * 18, 16, 14);
  rect(colors.text, 0.15, 42, 4, 40, 4);
  rect(accent, 1, 52, 86, 20, 4);
  return preview;
}

function renderThemes(themes, errors) {
  const current = JSON.stringify(values["/theme_preference"]);
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  $("pref-themes").replaceChildren(
    ...themes.map((theme) => {
      const tile = el("div", "theme-tile");
      const select = el("button", "theme-select");
      select.type = "button";
      select.role = "radio";
      select.title = theme.name;
      select.setAttribute("aria-label", theme.name);
      select.setAttribute("aria-checked", String(JSON.stringify(theme.preference) === current));
      select.append(themePreview(theme.preview || (dark ? theme.dark : theme.light)));
      select.addEventListener("click", () => save({ operation: "theme", theme: theme.preference }));
      const footer = el("div", "theme-footer");
      footer.append(el("span", "theme-name", theme.name));
      if (theme.accents.length) {
        const chosen = theme.accents.find((accent) => accent.name === theme.accent) || theme.accents[0];
        const button = el("button", "theme-accent");
        button.type = "button";
        button.title = chosen.title;
        button.setAttribute("aria-label", `${tr("Accent color")}: ${chosen.title}`);
        const swatch = el("span", "theme-swatch");
        swatch.style.background = chosen.color;
        button.append(swatch, icon("rufin-pan-down-symbolic"));
        button.addEventListener("click", () => accentMenu(theme, button));
        footer.append(button);
      }
      tile.append(select, footer);
      return tile;
    }),
  );
  $("pref-theme-errors").textContent = errors.join("\n");
  $("pref-theme-errors").hidden = !errors.length;
}

// The standard themes share the app's accent; a theme with its own accents keeps its choice
// and becomes the theme, as GTK's tile dropdown does.
function accentMenu(theme, anchor) {
  const menu = $("theme-accent-menu");
  menu.replaceChildren(
    ...theme.accents.map((accent) => {
      const item = el("button", "menu-item");
      item.type = "button";
      item.role = "menuitemradio";
      item.setAttribute("aria-checked", String(accent.name === theme.accent));
      const swatch = el("span", "theme-swatch");
      swatch.style.background = accent.color;
      item.append(swatch, el("span", "", accent.title));
      item.addEventListener("click", () => {
        menu.hidePopover();
        save(theme.preference.Named
          ? { operation: "theme", theme: theme.preference, accent: accent.name }
          : { operation: "set", path: "/accent_preference", value: accent.name });
      });
      return item;
    }),
  );
  openPopup(menu, anchor, "end");
}

// The sidebar entries and Home blocks: shown or not, and moved up and down, as GTK's lists.
function listRows(host, entries, key, edit) {
  host.replaceChildren(
    ...entries.map((entry, index) => {
      const row = el("div", "pref-row pref-list-row");
      const visible = el("input");
      visible.type = "checkbox";
      visible.checked = entry.visible;
      visible.disabled = entry.can_toggle === false;
      visible.setAttribute("aria-label", entry.title);
      visible.onchange = () => save(edit(entry, { visible: visible.checked }));
      const label = el("span", "pref-label");
      label.append(el("span", "", entry.title));
      row.append(visible, label);
      for (const [delta, name, title, disabled] of [
        [-1, "rufin-go-up-symbolic", tr("Move up"), index === 0],
        [1, "rufin-go-down-symbolic", tr("Move down"), index === entries.length - 1],
      ]) {
        const move = el("button", "icon-button");
        move.type = "button";
        move.title = title;
        move.setAttribute("aria-label", `${title}: ${entry.title}`);
        move.disabled = disabled;
        move.append(icon(name));
        move.onclick = () => save(edit(entry, { delta }));
        row.append(move);
      }
      row.dataset.key = key(entry);
      return row;
    }),
  );
}

function save(edit) {
  return run(async () => {
    try {
      show(await api("/preferences", "POST", edit));
    } catch (error) {
      show(await api("/preferences"));
      throw error;
    }
  });
}

function set(path, value) {
  return save({ operation: "set", path, value });
}

function controlValue(node) {
  if (node.type === "checkbox") return node.checked;
  if (node.type === "number" || node.type === "range") return Number(node.value);
  return JSON.parse(node.value);
}

const sizeText = (bytes) => {
  if (bytes == null) return tr("Unavailable");
  const units = ["B", "KB", "MB", "GB", "TB"];
  let index = 0;
  while (bytes >= 1000 && index < units.length - 1) [bytes, index] = [bytes / 1000, index + 1];
  return `${index ? bytes.toFixed(1) : bytes} ${units[index]}`;
};

function showStorage(summary) {
  $("pref-storage-total").textContent = sizeText(summary.total_bytes);
  for (const cache of summary.caches) {
    const row = document.querySelector(`[data-cache="${cache.kind}"]`);
    if (row) row.querySelector("small").textContent = sizeText(cache.bytes);
  }
}

// A row that opens something, as GTK's activatable rows with their chevron.
function linkRow(title, subtitle, action, leading = null) {
  const row = el("button", "pref-row pref-link");
  row.type = "button";
  const label = el("span", "pref-label");
  label.append(el("span", "", title));
  if (subtitle) label.append(el("small", "", subtitle));
  if (leading) row.append(leading);
  row.append(label, icon("rufin-go-next-symbolic"));
  row.addEventListener("click", () => run(action));
  return row;
}

// Library: the sources, each opening its actions, as GTK's Library page.
async function renderLibrary() {
  const configured = await api("/sources");
  updateSources(configured);
  $("pref-sources").replaceChildren(
    ...configured.sources.map((item) =>
      linkRow(item.name, item.id === state.source ? tr("Selected") : "", () => showSource(item), sourceIcon(item.kind))),
  );
  if (!configured.sources.length) $("pref-sources").append(el("div", "pref-row", tr("No sources configured")));
  const scope = $("pref-activity-source");
  const chosen = scope.value;
  scope.replaceChildren(new Option(tr("All"), ""), ...configured.sources.map((item) => new Option(item.name, item.id)));
  scope.value = configured.sources.some((item) => item.id === chosen) ? chosen : "";
}

// Activity: the history downloads as the chosen file, and a picked file imports, as in GTK.
async function exportActivity() {
  const format = $("pref-activity-format").value;
  const response = await fetch(`/api/activity/export?${new URLSearchParams({ format, source: $("pref-activity-source").value })}`,
    { headers: browserHeaders() });
  if (!response.ok) throw new Error((await response.json()).error || tr("Request failed ({status})", { status: response.status }));
  const url = URL.createObjectURL(await response.blob());
  const link = el("a");
  link.href = url;
  link.download = format === "jsonl" ? "activity.jsonl" : "activity.csv";
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

async function importActivity(file) {
  notice(tr("Matching tracks…"), { working: true });
  const response = await fetch("/api/activity/import",
    { method: "POST", headers: { ...browserHeaders(), "Content-Type": "application/octet-stream" }, body: file });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || tr("Request failed ({status})", { status: response.status }));
  notice(result.message);
}

function showSource(item) {
  showPage("source", item.name);
  const action = (title, run, className = "") => {
    const row = el("button", `pref-row pref-action ${className}`, title);
    row.type = "button";
    row.addEventListener("click", () => runAction(run));
    return row;
  };
  const runAction = (task) => run(async () => {
    await task();
    await renderLibrary();
  });
  $("pref-source-actions").replaceChildren(
    action(tr("Use This Source"), async () => {
      await api("/sources/select", "POST", { id: item.id });
      state.source = item.id;
    }),
    action(tr("Resync Library"), () => api("/sources/refresh", "POST", { id: item.id })),
    action(tr("Server Settings"), () => openSource(item)),
    action(tr("Forget Source"), async () => {
      if (!confirm(tr("Remove everything Rufin keeps about {name}, including its favorites, ratings and lyrics?", { name: item.name }))) return;
      await api(`/sources?${new URLSearchParams({ id: item.id })}`, "DELETE");
      showPage("library");
    }, "destructive"),
  );
}

// Downloads: one source's download quality, as GTK's Downloads page picks a source.
let downloadSources = [];
let downloadQuality = null;
async function renderDownloads() {
  downloadSources = (await api("/sources")).sources.filter((item) => item.download_quality);
  const select = $("pref-download-source");
  const chosen = select.value;
  select.replaceChildren(...downloadSources.map((item) => new Option(item.name, item.id)));
  select.value = downloadSources.some((item) => item.id === chosen) ? chosen : downloadSources[0]?.id || "";
  const item = downloadSources.find((source) => source.id === select.value);
  $("pref-download-quality").hidden = !item;
  if (!item) return;
  if (!downloadQuality)
    downloadQuality = qualityControls($("pref-download-quality"), item.download_quality, async (edit) => {
      const source = downloadSources.find((entry) => entry.id === select.value);
      const result = await api("/downloads/quality", "POST", { source: source.id, edit });
      source.download_quality = result.download_quality;
      return result.download_quality;
    });
  else downloadQuality.set(item.download_quality);
}

async function renderFileConnections() {
  const saved = () => run(renderFileConnections);
  const connections = await api("/integrations/files");
  $("pref-file-connections").replaceChildren(
    ...connections.map((item) => linkRow(item.name, "", () => openSource(item, { kind: item.kind, saved }), sourceIcon(item.kind))),
  );
  $("pref-file-connections").hidden = !connections.length;
}

function showPage(page, title = null) {
  for (const tab of document.querySelectorAll(".pref-tab[data-page]"))
    tab.setAttribute("aria-selected", String(tab.dataset.page === page));
  for (const panel of document.querySelectorAll(".pref-content > section"))
    panel.hidden = panel.id !== `pref-${page}`;
  const tab = document.querySelector(`.pref-tab[data-page="${page}"]`);
  $("preferences-page-title").textContent = title || tab?.textContent.trim() || "";
  $("preferences-dialog").classList.add("showing-page");
  $("preferences-dialog").classList.toggle("in-subpage", page === "source");
  if (page === "storage") run(async () => showStorage(await api("/preferences/storage")));
  if (page === "library") run(renderLibrary);
  if (page === "downloads") run(renderDownloads);
  if (page === "integrations") run(renderFileConnections);
}

// Opens Preferences at `page`, as the source menu's Manage opens GTK's Library page.
function openPreferences(page = "general") {
  const dialog = $("preferences-dialog");
  showPage(page);
  if (page === "general" && matchMedia("(max-width: 640px)").matches) dialog.classList.remove("showing-page");
  if (!dialog.open) dialog.showModal();
  run(async () => show(await api("/preferences")));
}

function init() {
  const dialog = $("preferences-dialog");
  document.addEventListener("change", (event) => {
    const node = event.target.closest("[data-setting]");
    if (node) {
      const path = node.dataset.setting;
      const value = controlValue(node);
      if (path === "/playback/loudness_normalization" && value === "EbuR128" && values[path] !== "EbuR128" &&
        !confirm(tr("Enable EBU R128 Analysis?"))) {
        show({ values, routes: [], home_blocks: [] });
        return;
      }
      set(path, value);
    }
    const item = event.target.closest("[data-menu-item]");
    if (item)
      set("/context_menu/items", (values["/context_menu/items"] || []).map((entry) =>
        entry.item === item.dataset.menuItem ? { ...entry, visible: item.checked } : entry));
  });
  document.addEventListener("input", (event) => {
    if (event.target.matches(".pref-range input")) event.target.nextElementSibling.textContent = event.target.value;
  });
  $("open-preferences").addEventListener("click", () => {
    $("main-menu").hidePopover();
    openPreferences();
  });
  $("pref-add-source").addEventListener("click", () => openSource());
  for (const node of document.querySelectorAll("[data-file-connection]"))
    node.addEventListener("click", () => openSource(null, { kind: node.dataset.fileConnection, saved: () => run(renderFileConnections) }));
  $("pref-download-source").addEventListener("change", () => run(renderDownloads));
  $("pref-activity-export").addEventListener("click", () => run(exportActivity));
  $("pref-activity-import").addEventListener("click", () => $("pref-activity-file").click());
  $("pref-activity-file").addEventListener("change", (event) => {
    const [file] = event.target.files;
    event.target.value = "";
    if (file) run(() => importActivity(file));
  });
  // The source dialog saved a source: Library shows it.
  document.addEventListener("rufin-sources", () => {
    if (dialog.open && !$("pref-library").hidden) run(renderLibrary);
  });
  $("playback-settings-button").addEventListener("click", () => run(async () => show(await api("/preferences"))));
  $("main-menu-button").addEventListener("click", () => run(async () => show(await api("/preferences"))));
  $("private-mode").addEventListener("click", () => {
    $("main-menu").hidePopover();
    set("/private_mode", !values["/private_mode"]);
  });
  $("preferences-back").addEventListener("click", () => {
    if (dialog.classList.contains("in-subpage")) showPage("library");
    else dialog.classList.remove("showing-page");
  });
  for (const tab of document.querySelectorAll(".pref-tab[data-page]"))
    tab.addEventListener("click", () => showPage(tab.dataset.page));
  for (const link of dialog.querySelectorAll("[data-open]"))
    link.addEventListener("click", () => {
      dialog.close();
      $(link.dataset.open).click();
    });
  for (const row of document.querySelectorAll("[data-cache]"))
    row.querySelector("button").addEventListener("click", () => {
      if (confirm(tr("Clear this cache?")))
        run(async () => showStorage(await api("/preferences/storage", "POST", { kind: row.dataset.cache })));
    });
}

// Shows the stored settings on every control that names one, outside Preferences too.
const showSettings = async () => show(await api("/preferences"));

export { init, openPreferences, showSettings };
