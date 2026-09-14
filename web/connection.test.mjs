// Run with node --experimental-vm-modules --test web/connection.test.mjs.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { setImmediate } from "node:timers/promises";
import test from "node:test";
import vm from "node:vm";

async function controller() {
  const notices = [];
  const window = new EventTarget();
  const document = new EventTarget();
  const elements = new Map();
  const element = (id) => {
    if (!elements.has(id))
      elements.set(id, Object.assign(new EventTarget(), {
        close() {}, showModal() {}, dataset: {}, classList: { add() {} },
      }));
    return elements.get(id);
  };
  document.querySelector = () => ({ content: "session-csrf" });
  document.documentElement = element("html");
  const noop = () => {};
  const modules = {
    "./ui.js": {
      tr: (text) => text, applyAppearance: noop, applyColors: noop, initialAppearance: {},
      coverUrls: new Map(), $: element, notice: (text) => notices.push(text),
      run: async (action) => action(),
    },
    "./queue.js": { showQueue: noop },
    "./library.js": { disposeHome: noop, loadView: noop, restoreView: noop, viewRequest: null },
    "./menus.js": { closeMenus: noop },
    "./player.js": { coverRequest: null, showLyrics: noop, showPlayback: noop },
    "./sources.js": { showSourceProgress: noop, updateSources: noop },
    "./pins.js": { refreshPins: noop, resetPins: noop },
  };
  let ready;
  const opened = new Promise((resolve) => { ready = resolve; });
  const context = vm.createContext({
    window, document, Event, AbortController, AbortSignal, URLSearchParams,
    TextDecoder, setTimeout, clearTimeout,
    location: { hash: "" },
    htmx: { config: {} },
    sessionStorage: { getItem: () => null, removeItem: noop },
    fetch: async (path, options) => {
      if (path !== "/api/events") return new Response("{}");
      return new Response(new ReadableStream({
        start(stream) {
          options.signal.addEventListener("abort", () => stream.error(new Error("aborted")), { once: true });
          ready(stream);
        },
      }));
    },
  });
  const source = await readFile(new URL("./connection.js", import.meta.url), "utf8");
  const module = new vm.SourceTextModule(source, { context });
  const linked = new Map();
  await module.link((name) => {
    if (!linked.has(name)) {
      const exports = modules[name];
      linked.set(name, new vm.SyntheticModule(Object.keys(exports), function () {
        for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
      }, { context }));
    }
    return linked.get(name);
  });
  await module.evaluate();
  module.namespace.init();
  return { notices, window, stream: await opened, close: () => module.namespace.session.abort() };
}

for (const [name, leaving, restored, expected] of [
  ["a refresh does not report the old page's cancelled stream", true, false, 0],
  ["a real stream failure is still reported", false, false, 1],
  ["a restored page reports subsequent connection failures", true, true, 1],
]) {
  test(name, async () => {
    const page = await controller();
    try {
      await setImmediate();
      if (leaving) page.window.dispatchEvent(new Event("beforeunload"));
      if (restored) page.window.dispatchEvent(new Event("pageshow"));
      await setImmediate();
      page.stream.error(new TypeError("NetworkError when attempting to fetch resource"));
      await setImmediate();
      assert.equal(page.notices.length, expected);
    } finally {
      page.close();
    }
  });
}
