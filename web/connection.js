import { showQueue } from "./queue.js";
import { tr } from "./ui.js";
import {
  applyAppearance,
  initialAppearance,
  applyColors,
  coverUrls,
  $,
  notice,
  run,
} from "./ui.js";

import { disposeHome, loadView, restoreView, viewRequest } from "./library.js";

import { closeMenus } from "./menus.js";

import { coverRequest, showLyrics, showPlayback } from "./player.js";

import { showSourceProgress, updateSources } from "./sources.js";
import { refreshPins, resetPins } from "./pins.js";

let session = null;
let initialPage = true;
let leavingPage = false;
let csrf = document.querySelector('meta[name="rufin-csrf"]').content;
let token = csrf
  ? ""
  : new URLSearchParams(location.hash.slice(1)).get("token") || "";

async function login() {
  if (!token) return;
  const response = await fetch("/session", {
    method: "POST",
    headers: { Accept: "application/json" },
    body: new URLSearchParams({ token }),
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error);
  csrf = result.csrf;
  token = "";
}

function browserHeaders() {
  return { "X-Rufin-CSRF": csrf };
}

async function api(path, method = "GET", data, signal = session?.signal) {
  const response = await fetch(`/api${path}`, {
    method,
    signal,
    headers: {
      ...browserHeaders(),
      ...(data === undefined ? {} : { "Content-Type": "application/json" }),
    },
    body: data === undefined ? undefined : JSON.stringify(data),
  });
  if (response.status === 401) {
    disconnect();
    throw new Error(
      tr("The API token was not accepted. Please connect again."),
    );
  }
  const result = await response.json();
  if (!response.ok)
    throw Object.assign(new Error(
      result.error ||
        tr("Request failed ({status})", { status: response.status }),
    ), { conflict: result.conflict === true });
  return result;
}

async function fragment(path, target, signal = session?.signal, select) {
  signal?.throwIfAborted();
  const abort = () => htmx.trigger(target, "htmx:abort");
  let error;
  const completed = (event) => {
    if (event.detail.elt !== target) return;
    const xhr = event.detail.xhr;
    if (xhr.status === 401) disconnect();
    if (xhr.status >= 400) {
      let message = tr("Request failed ({status})", { status: xhr.status });
      try {
        message = JSON.parse(xhr.responseText).error || message;
      } catch {}
      error = new Error(message);
    }
  };
  target.addEventListener("htmx:afterRequest", completed);
  signal?.addEventListener("abort", abort, { once: true });
  try {
    await htmx.ajax("GET", path, {
      source: target,
      target,
      select,
      swap: "innerHTML",
    });
    signal?.throwIfAborted();
    if (error) throw error;
  } catch (error) {
    signal?.throwIfAborted();
    throw error || new Error(tr("Could not load this page"));
  } finally {
    target.removeEventListener("htmx:afterRequest", completed);
    signal?.removeEventListener("abort", abort);
  }
}

function disconnect() {
  htmx.trigger($("source-form"), "htmx:abort");
  disposeHome();
  closeMenus();
  session?.abort();
  viewRequest?.abort();
  coverRequest?.abort();
  session = null;
  resetPins();
  token = "";
  for (const url of coverUrls.values()) URL.revokeObjectURL(url);
  coverUrls.clear();
  state.playback = null;
  state.currentId = null;
  showQueue(null);
  for (const id of ["content", "queue", "lyrics"]) $(id).replaceChildren();
  $("app").inert = true;
  document.querySelectorAll("dialog[open]").forEach((dialog) => dialog.close());
  $("login-dialog").showModal();
}

async function connect() {
  const rendered = initialPage && $("content").dataset.rendered === "true";
  session?.abort();
  session = new AbortController();
  await login();
  const [configured, appearance] = await Promise.all([
    api("/sources"),
    rendered ? initialAppearance : api("/appearance"),
  ]);
  applyAppearance(appearance);
  $("token").value = "";
  $("login-error").textContent = "";
  $("login-dialog").close();
  $("app").inert = false;
  updateSources(configured);
  if (initialPage) {
    restoreView();
    if (rendered) {
      state.source = $("content").dataset.source;
      state.selectedLibrary = $("content").dataset.folder || null;
    }
    initialPage = false;
  }
  await loadView(rendered);
  stream(
    "/events",
    (value) => {
      if (Object.hasOwn(value, "playback")) showPlayback(value.playback);
      if (value.queue) showQueue(value.queue);
      if (value.source) showSourceProgress(value.source);
      if (value.lyrics) showLyrics(value.lyrics);
      if (value.appearance) applyColors(value.appearance);
      if (value.pins_changed) run(refreshPins);
    },
    session.signal,
  );
}

async function stream(path, receive, signal) {
  let interrupted = false;
  while (!signal.aborted) {
    let retryAfter = 1500;
    try {
      const response = await fetch(`/api${path}`, {
        signal,
        headers: browserHeaders(),
      });
      if (response.status === 401) {
        disconnect();
        return;
      }
      if (response.status === 429)
        retryAfter = Number(response.headers.get("Retry-After")) * 1000 || 1500;
      if (!response.ok) throw new Error(tr("Could not receive live updates"));
      leavingPage = false;
      if (interrupted) {
        state.randomSource = null;
        run(async () => {
          updateSources(await api("/sources"));
          await loadView();
        });
        interrupted = false;
      }
      const reader = response.body.getReader(),
        decoder = new TextDecoder();
      let buffer = "";
      while (!signal.aborted) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let end;
        while ((end = buffer.indexOf("\n\n")) >= 0) {
          const event = buffer.slice(0, end);
          buffer = buffer.slice(end + 2);
          const data = event
            .split("\n")
            .filter((line) => line.startsWith("data:"))
            .map((line) => line.slice(5).trimStart())
            .join("\n");
          if (data) receive(JSON.parse(data));
        }
      }
      interrupted = true;
    } catch (error) {
      if (signal.aborted) return;
      if (!interrupted && !leavingPage)
        notice(tr("Connection lost. Reconnecting..."));
      interrupted = true;
    }
    if (!signal.aborted)
      await new Promise((resolve) => {
        const timer = setTimeout(done, retryAfter);
        function done() {
          clearTimeout(timer);
          signal.removeEventListener("abort", done);
          resolve();
        }
        signal.addEventListener("abort", done, { once: true });
      });
  }
}

function init() {
  // Firefox can reject the old stream before the replacement page is ready.
  window.addEventListener("beforeunload", () => {
    leavingPage = true;
  });
  window.addEventListener("pageshow", () => {
    leavingPage = false;
  });
  document.addEventListener("submit", (event) => {
    if (event.target.matches("[data-enhanced-form]")) event.preventDefault();
  });
  htmx.config.allowEval = false;
  htmx.config.allowScriptTags = false;
  htmx.config.historyCacheSize = 0;
  document.addEventListener("htmx:configRequest", (event) => {
    Object.assign(event.detail.headers, browserHeaders());
  });
  document.addEventListener("htmx:responseError", (event) => {
    if (event.detail.xhr.status === 401) disconnect();
  });
  if (location.hash)
    history.replaceState(null, "", location.pathname + location.search);
  $("login-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    token = $("token").value.trim();
    event.submitter.disabled = true;
    try {
      await connect();
    } catch (error) {
      $("login-error").textContent = error.message;
    } finally {
      event.submitter.disabled = false;
    }
  });
  $("login-dialog").addEventListener("cancel", (event) =>
    event.preventDefault(),
  );
  $("disconnect").addEventListener("click", () => {
    if (confirm(tr("Disconnect from Rufin?")))
      run(async () => {
        const response = await fetch("/session/logout", {
          method: "POST",
          body: new URLSearchParams({ csrf }),
        });
        if (!response.ok && response.status !== 401)
          throw new Error(
            tr("Request failed ({status})", { status: response.status }),
          );
        csrf = "";
        disconnect();
      });
  });
  $("login-dialog").close();
  if (token || csrf)
    connect().catch((error) => {
      if (!session?.signal.aborted) {
        $("login-error").textContent = error.message;
        if (token || !csrf) $("login-dialog").showModal();
        else notice(error.message);
      }
    });
  else $("login-dialog").showModal();
  document.documentElement.classList.add("enhanced");
  $("app").hidden = false;
}

const state = {
  visibleItems: [],
  source: "",
  playback: null,
  currentId: null,
  selectedLibrary: null,
  randomSource: null,
};

export { init, api, fragment, session, state };
