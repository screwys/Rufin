import { tr } from "./ui.js";
import { api, state } from "./connection.js";

import { loadView, navigate, route } from "./library.js";

import { $, button, el, icon, notice, openPopup, run } from "./ui.js";

let sources = [];

let libraries = [];

function updateSources(configured) {
  sources = configured.sources;
  if (!sources.some((item) => item.id === state.source))
    state.source = configured.selected || sources[0]?.id || "";
  libraries = configured.libraries || [];
  state.selectedLibrary = configured.library || null;
  $("source-name").textContent =
    sources.find((item) => item.id === state.source)?.name || tr("Library");
}

function sourceIcon(kind) {
  if (kind === "local") {
    const node = icon("cover-fallback");
    node.classList.add("source-icon");
    return node;
  }
  if (
    ![
      "jellyfin",
      "emby",
      "plex",
      "navidrome",
      "subsonic",
      "webdav",
      "smb",
    ].includes(kind)
  )
    return icon("server");
  const image = el("img", "source-icon");
  image.alt = "";
  image.src = `/source-icons/${kind}`;
  return image;
}

function renderSources(configured) {
  updateSources(configured);
  $("content").replaceChildren();
  const intro = el("div", "source-intro");
  intro.append(
    el("p", "", tr("Connect your music servers or local folders.")),
    button(tr("Add Source"), () => openSource(), null, "primary"),
  );
  $("content").append(intro);
  for (const item of sources) {
    const card = el("div", "list-card"),
      label = el("div", "label");
    label.append(el("strong", "", item.name), el("p", "", item.kind));
    const edit = button(
      tr("Edit"),
      () => openSource(item),
      "edit",
      "source-edit",
    );
    edit.append(el("span", "", tr("Edit")));
    const actions = el("div", "source-actions");
    actions.append(
      button(
        tr("Use This Source"),
        async () => {
          await api("/sources/select", "POST", { id: item.id });
          state.source = item.id;
          updateSources(await api("/sources"));
          navigate("tracks");
        },
        "check",
        "",
      ),
      button(
        tr("Resync Library"),
        () => api("/sources/refresh", "POST", { id: item.id }),
        "refresh",
      ),
      button(
        tr("Forget Source"),
        async () => {
          if (confirm(tr('Forget "{name}"?', { name: item.name }))) {
            await api(
              `/sources?${new URLSearchParams({ id: item.id })}`,
              "DELETE",
            );
            await loadView();
          }
        },
        "close",
      ),
    );
    for (const action of actions.children)
      action.append(el("span", "", action.getAttribute("aria-label")));
    actions.lastElementChild.classList.add("destructive");
    card.append(sourceIcon(item.kind), label, edit, actions);
    $("content").append(card);
  }
}

let previousOperation = "";

function showSourceProgress(value) {
  const refreshed = previousOperation === "refreshing";
  const active = ["adding", "switching", "refreshing"].includes(value.state);
  $("source-progress").hidden = !active;
  if (active) {
    const progress = value.progress;
    $("source-progress").textContent = tr(
      "{status} · {stage} {completed}{total}",
      {
        status: {
          adding: tr("Adding source"),
          switching: tr("Changing source"),
          refreshing: tr("Refreshing"),
        }[value.state],
        stage: {
          connecting: tr("Connecting"),
          albums: tr("Albums"),
          tracks: tr("Tracks"),
          artists: tr("Artists"),
          genres: tr("Genres"),
          playlists: tr("Playlists"),
          home: tr("Home"),
          artwork: tr("Artwork"),
          files: tr("Files"),
          finalizing: tr("Preparing library..."),
        }[progress.stage],
        completed: progress.completed,
        total: progress.total == null ? "" : ` / ${progress.total}`,
      },
    );
  }
  if (value.state === "failed") notice(value.message);
  if (
    value.state === "idle" &&
    previousOperation &&
    previousOperation !== "idle"
  )
    run(async () => {
      updateSources(await api("/sources"));
      if (route !== "home" || !refreshed) await loadView();
    });
  previousOperation = value.state;
}

let editingSource = null;

async function openSource(item = null) {
  editingSource = item
    ? await api(`/sources/edit?${new URLSearchParams({ id: item.id })}`)
    : null;
  $("source-form").reset();
  $("source-error").textContent = "";
  $("source-title").textContent = item ? tr("Edit Source") : tr("Add Source");
  $("source-submit").textContent = item ? tr("Save") : tr("Add Source");
  $("source-kind").querySelector('[value="plex"]')?.remove();
  if (editingSource?.kind === "plex")
    $("source-kind").append(new Option(tr("Plex"), "plex"));
  $("source-kind").value = editingSource
    ? { webdav: "web_dav", subsonic: "open_subsonic" }[editingSource.kind] ||
      editingSource.kind
    : "local";
  $("source-kind").disabled = !!editingSource;
  sourceFields();
  if (editingSource?.kind === "local")
    $("source-paths").value = editingSource.roots.join("\n");
  if (editingSource?.preset) {
    const preset = editingSource.preset;
    $("source-label").value = preset.credentials.source_name;
    $("server-url").value =
      preset.plex_settings?.address_override || preset.credentials.server_url;
    $("username").value = preset.credentials.username;
    $("trust-certificate").checked = preset.credentials.trust_invalid_cert;
    $("authentication").value =
      preset.file_settings?.authentication ||
      preset.credentials.open_subsonic_authentication ||
      "password";
  }
  $("source-dialog").showModal();
}

function sourceFields() {
  const kind = $("source-kind").value;
  const local = kind === "local";
  $("local-fields").hidden = !local;
  $("remote-fields").hidden = local;
  $("source-paths").required = local;
  $("server-url").required = !local && kind !== "plex";
  for (const id of ["username", "password"]) {
    $(id).hidden = kind === "plex";
    document.querySelector(`label[for="${id}"]`).hidden = kind === "plex";
  }
  const files = kind === "web_dav" || kind === "smb";
  const choices = files
    ? [
        ["password", tr("Password")],
        ["anonymous", tr("Anonymous")],
        ["bearer", tr("Bearer token")],
      ]
    : [
        ["password", tr("Password")],
        ["legacy_password", tr("Legacy password")],
        ["api_key", tr("API key")],
      ];
  $("authentication").replaceChildren(
    ...choices.map(([value, label]) => new Option(label, value)),
  );
  const hidden = ["jellyfin", "emby", "local", "plex"].includes(kind);
  $("authentication").hidden = hidden;
  document.querySelector('label[for="authentication"]').hidden = hidden;
}

function init() {
  $("source-menu-button").addEventListener("click", () =>
    run(async () => {
      updateSources(await api("/sources"));
      const menu = $("source-menu");
      menu.replaceChildren();
      for (const item of sources) {
        const choose = button(
          item.name,
          async () => {
            menu.hidePopover();
            await api("/sources/select", "POST", { id: item.id });
            state.source = item.id;
            updateSources(await api("/sources"));
            navigate(route === "genres" ? "home" : route);
          },
          null,
          "menu-item",
        );
        choose.replaceChildren(
          sourceIcon(item.kind),
          document.createTextNode(item.name),
        );
        if (item.id === state.source) choose.append(icon("check"));
        choose.setAttribute("role", "menuitemradio");
        choose.setAttribute("aria-checked", String(item.id === state.source));
        menu.append(choose);
      }
      const manage = button(
        tr("Manage"),
        () => {
          menu.hidePopover();
          navigate("sources");
        },
        "edit",
        "menu-item",
      );
      manage.append(document.createTextNode(tr("Manage")));
      const add = button(
        tr("Add a new source"),
        () => {
          menu.hidePopover();
          openSource();
        },
        "add",
        "menu-item",
      );
      add.append(document.createTextNode(tr("Add a new source")));
      menu.append(manage, add);
      if (libraries.length) {
        menu.append(el("div", "menu-heading", tr("Server Library")));
        for (const library of [
          { id: null, name: tr("All Music") },
          ...libraries,
        ]) {
          const choose = button(
            library.name,
            async () => {
              menu.hidePopover();
              await api("/sources/library", "POST", {
                source: state.source,
                library: library.id,
              });
              updateSources(await api("/sources"));
              navigate(route === "genres" ? "home" : route);
            },
            null,
            "menu-item",
          );
          if (library.id === state.selectedLibrary)
            choose.append(icon("check"));
          choose.setAttribute("role", "menuitemradio");
          choose.setAttribute(
            "aria-checked",
            String(library.id === state.selectedLibrary),
          );
          menu.append(choose);
        }
      }
      openPopup(menu, $("source-menu-button"));
    }),
  );
  $("source-kind").addEventListener("change", sourceFields);
  sourceFields();
  htmx.defineExtension("source-form", {
    onEvent(name, event) {
      if (name === "htmx:configRequest") {
        event.detail.headers["Content-Type"] = "application/json";
        event.detail.verb = editingSource ? "patch" : "post";
      }
    },
    encodeParameters() {
      return JSON.stringify(sourceInput());
    },
  });
  $("source-form").addEventListener("htmx:beforeRequest", () => {
    $("source-error").textContent = "";
  });
  $("source-form").addEventListener("htmx:afterRequest", (event) =>
    run(async () => {
      const xhr = event.detail.xhr;
      if (!xhr.status) return;
      const result = JSON.parse(xhr.responseText);
      if (!event.detail.successful) {
        $("source-error").textContent = result.error;
        return;
      }
      if (!editingSource) state.source = result.id;
      $("source-dialog").close();
      $("password").value = "";
      updateSources(await api("/sources"));
      navigate(editingSource ? "sources" : "home");
    }),
  );
}

function sourceInput() {
  const kind = $("source-kind").value,
    name = $("source-label").value.trim(),
    url = $("server-url").value.trim(),
    username = $("username").value,
    secret = $("password").value,
    trust = $("trust-certificate").checked;
  let input;
  if (kind === "local")
    input = {
      type: "local",
      data: {
        roots: $("source-paths")
          .value.split("\n")
          .map((path) => path.trim())
          .filter(Boolean),
      },
    };
  else if (["web_dav", "smb"].includes(kind))
    input = {
      type: kind,
      data: {
        name: name || "My music",
        settings: {
          url,
          alternate_urls: [],
          folders: [],
          username,
          domain: "",
          authentication: $("authentication").value,
          trust_invalid_certificate: trust,
          certificate_pem: null,
          require_smb_encryption: false,
        },
        credentials: { secret, headers: [] },
      },
    };
  else {
    const credentials = {
      source_name: name || null,
      server_url: url,
      username,
      secret,
      trust_invalid_cert: trust,
    };
    input = ["jellyfin", "emby"].includes(kind)
      ? {
          type: "jellyfin_emby",
          data: { kind, credentials, use_instant_mix: false },
        }
      : {
          type: "open_subsonic",
          data: {
            kind,
            credentials,
            authentication: $("authentication").value,
          },
        };
  }
  if (editingSource) {
    const preset = editingSource.preset;
    if (kind === "plex")
      input = {
        type: "plex",
        data: {
          settings: {
            ...preset.plex_settings,
            name,
            address_override: url || null,
            trust_invalid_cert: trust,
          },
        },
      };
    else if (["web_dav", "smb"].includes(kind))
      input = {
        type: "files",
        data: {
          name,
          settings: {
            ...preset.file_settings,
            url,
            username,
            authentication: $("authentication").value,
            trust_invalid_certificate: trust,
          },
          credentials: { secret: secret || null, headers: null },
        },
      };
    else if (kind === "jellyfin" || kind === "emby") {
      delete input.data.kind;
      input.data.connect_manually = !preset.emby_connect;
      input.data.use_instant_mix = preset.use_instant_mix || false;
    } else if (
      kind === "local" &&
      $("source-paths").value === editingSource.roots.join("\n")
    )
      input.data.roots = editingSource.roots;
    input.data.source_id = editingSource.id;
  }
  return input;
}
export { renderSources, showSourceProgress, sources, updateSources, init };
