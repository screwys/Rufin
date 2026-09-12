import { showQueue } from "./queue.js";
import { tr } from "./ui.js";
import {
  applyAppearance,
  applyColors,
  coverUrls,
  $,
  notice,
  run,
} from "./ui.js";

import { disposeHome, loadView, viewRequest } from "./library.js";

import { closeMenus } from "./menus.js";

import { coverRequest, showLyrics, showPlayback } from "./player.js";

import { showSourceProgress, updateSources } from "./sources.js";

let token =
  new URLSearchParams(location.hash.slice(1)).get("token") ||
  sessionStorage.getItem("rufin-token") ||
  "";

let session = null;

async function api(path, method = "GET", data, signal = session?.signal) {
  const response = await fetch(`/api${path}`, {
    method,
    signal,
    headers: {
      Authorization: `Bearer ${token}`,
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
    throw new Error(
      result.error ||
        tr("Request failed ({status})", { status: response.status }),
    );
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
  token = "";
  sessionStorage.removeItem("rufin-token");
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
  session?.abort();
  session = new AbortController();
  const [configured, appearance] = await Promise.all([
    api("/sources"),
    api("/appearance"),
  ]);
  applyAppearance(appearance);
  sessionStorage.setItem("rufin-token", token);
  $("token").value = "";
  $("login-error").textContent = "";
  $("login-dialog").close();
  $("app").inert = false;
  updateSources(configured);
  await loadView();
  stream(
    "/events",
    (value) => {
      if (Object.hasOwn(value, "playback")) showPlayback(value.playback);
      if (value.queue) showQueue(value.queue);
      if (value.source) showSourceProgress(value.source);
      if (value.lyrics) showLyrics(value.lyrics);
      if (value.appearance) applyColors(value.appearance);
    },
    session.signal,
  );
}

async function stream(path, receive, signal) {
  let interrupted = false;
  while (!signal.aborted) {
    try {
      const response = await fetch(`/api${path}`, {
        signal,
        headers: { Authorization: `Bearer ${token}` },
      });
      if (response.status === 401) {
        disconnect();
        return;
      }
      if (!response.ok) throw new Error(tr("Could not receive live updates"));
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
      if (!interrupted) notice(tr("Connection lost. Reconnecting..."));
      interrupted = true;
    }
    if (!signal.aborted)
      await new Promise((resolve) => {
        const timer = setTimeout(done, 1500);
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
  htmx.config.allowEval = false;
  htmx.config.allowScriptTags = false;
  htmx.config.historyCacheSize = 0;
  document.addEventListener("htmx:configRequest", (event) => {
    event.detail.headers.Authorization = `Bearer ${token}`;
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
    if (confirm(tr("Disconnect from Rufin?"))) disconnect();
  });
  if (token)
    connect().catch((error) => {
      if (!session?.signal.aborted) {
        $("login-error").textContent = error.message;
        $("login-dialog").showModal();
      }
    });
  else $("login-dialog").showModal();
}

const state = {
  visibleItems: [],
  source: "",
  playback: null,
  currentId: null,
  selectedLibrary: null,
  randomSource: null,
};

export { init, api, fragment, session, token, state };
