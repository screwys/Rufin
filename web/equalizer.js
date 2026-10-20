import { api } from "./connection.js";
import { tr, notice } from "./ui.js";
const $ = (id) => document.getElementById(`eq-${id}`);
let view, stream, sending, response, deferredView, clipTimer, nameAction, responseSending, signature = "", latestProfile = "";
const pending = new Map();
let writes = Promise.resolve();
const number = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
function element(tag, text) { const el = document.createElement(tag); if (text) el.textContent = text; return el; }
function checked(id, value) { $(id).checked = value; }
function value(id, next) { if (document.activeElement !== $(id)) $(id).value = next; }
function request(operation, extra = {}) {
  const payload = { operation, output: view.output, ...extra };
  const result = writes.then(() => api("/equalizer", "POST", payload));
  writes = result.catch(() => {});
  return result;
}
function report(error) { notice(error.message); $("status").textContent = error.message; }
function queue(type, data, key = type) {
  pending.set(`${view.output}:${key}:${data?.index ?? ""}`, { output: view.output, edit: { [type]: data } });
  if (!sending) sending = drain().finally(() => { sending = null; if (deferredView) { const next = deferredView; deferredView = null; render(next); } });
}
async function drain() {
  try { while (pending.size) { const [key, item] = pending.entries().next().value; pending.delete(key); await request("preview", item); } }
  catch (error) { pending.clear(); report(error); deferredView = await api("/equalizer"); throw error; }
}
async function flush() { while (sending) await sending; }
async function commit() { await flush(); await request("commit"); }
async function operation(name, extra) { const output = view.output; await flush(); const result = await request(name, { output, ...extra }); await refreshPresets(); render(await api("/equalizer")); return result; }
function action(fn) { return () => Promise.resolve().then(fn).catch(report); }
function watch() {
  stream?.close();
  if (!document.getElementById("equalizer-dialog").open) return;
  stream = new EventSource(`/api/equalizer/events?meter=true&spectrum=${$("spectrum").checked}`);
  stream.addEventListener("equalizer", event => render(JSON.parse(event.data)));
}
async function refreshPresets() {
  const list = await api("/equalizer/presets");
  $("preset").replaceChildren(...list.map(p => { const o = element("option", p.builtin ? tr(p.name) : p.name); o.value = p.id; return o; }), Object.assign(element("option", tr("Custom")), { value: "" }));
  if (view) $("preset").value = view.preset_id || "";
}
function render(next) {
  if (!document.getElementById("equalizer-dialog").open) return;
  const busy = document.activeElement?.closest("#equalizer-dialog") && (pending.size || sending);
  if (busy && view && view.output === next.output) { deferredView = next; view.peak_db = next.peak_db; view.spectrum_db = next.spectrum_db; draw(); return; }
  view = next;
  checked("enabled", view.enabled); checked("auto", view.profile.automatic_preamp); checked("crossfeed", view.crossfeed); checked("mono", view.mono); checked("assigned", view.assigned);
  value("preset", view.preset_id || ""); value("layout", view.profile.layout); value("preamp", view.preamp_db); value("preamp-number", view.preamp_db);
  value("bass", view.profile.bass_db); value("treble", view.profile.treble_db); value("balance", view.balance); value("cast-format", view.cast_format);
  $("bass-value").textContent = `${number.format(view.profile.bass_db)} dB`; $("treble-value").textContent = `${number.format(view.profile.treble_db)} dB`;
  $("rename").disabled = view.preset_builtin || !(view.preset_id || view.save_target); $("delete").disabled = $("rename").disabled;
  $("apply").hidden = !view.cast || !view.pending; $("cast-format").hidden = !view.cast; $("spectrum").disabled = view.cast;
  for (const input of document.querySelectorAll("#equalizer-dialog input, #equalizer-dialog select")) input.disabled = !view.available;
  $("spectrum").disabled = view.cast || !view.available;
  $("clip").textContent = view.cast ? tr("Encoding peak") : tr("Clipping");
  $("peak").textContent = view.cast && view.peak_db.length ? `${number.format(Math.max(...view.peak_db))} dBFS` : "";
  $("status").textContent = !view.available ? tr("Equalizer is controlled by the receiver") : view.error || (view.modified ? tr("Unsaved") : "");
  if (view.profile.layout === "Parametric") $("mode").value = "parametric";
  const nextSignature = `${$("mode").value}:${JSON.stringify(view.profile.bands.map(b => [$("mode").value === "graphic" ? b.frequency_hz : null, b.kind, Object.keys(b.width)[0]]))}`;
  if (signature !== nextSignature) { signature = nextSignature; buildBands(); }
  for (const input of document.querySelectorAll("[data-eq-band]")) {
    const b = view.profile.bands[Number(input.dataset.eqBand)]; if (document.activeElement === input && input.type === "number") continue;
    if (input.dataset.field === "gain") input.value = b.gain_db;
    if (input.dataset.field === "frequency") input.value = b.frequency_hz;
    if (input.dataset.field === "width") input.value = Object.values(b.width)[0].value;
    if (input.dataset.field === "enabled") input.checked = b.enabled;
    if (input.dataset.field === "kind") input.value = b.kind;
    if (input.dataset.field === "unit") input.value = Object.keys(b.width)[0];
  }
  draw();
  const stamp = JSON.stringify([view.output, view.enabled, view.sample_rate, view.profile]);
  if (stamp !== latestProfile) { latestProfile = stamp; void loadResponse(stamp).catch(report); }
}
function bandInput(index, field, min, max, step = .1) {
  const input = element("input"); Object.assign(input, { type: "number", min, max, step: "any" }); input.dataset.eqBand = index; input.dataset.field = field;
  return input;
}
function bindNumber(input, type, payload, reflect = () => {}) {
  const preview = () => {
    const value = input.valueAsNumber;
    if (!Number.isFinite(value) || (input.min !== "" && value < Number(input.min)) || (input.max !== "" && value > Number(input.max))) return;
    if ((type === "BandFrequency" || type === "BandWidth") && value <= 0) return;
    reflect(value); queue(type, payload(value)); draw();
  };
  input.addEventListener("input", preview);
  input.addEventListener("blur", action(commit));
  input.addEventListener("keydown", event => { if (event.key === "Enter") { preview(); void commit().catch(report); } });
}
function buildBands() {
  const parametric = $("mode").value === "parametric";
  $("bands").replaceChildren(); $("filters").replaceChildren(); $("bands").hidden = parametric; $("add").hidden = !parametric; $("layout").hidden = parametric;
  $("graph").style.minWidth = `${parametric ? 350 : Math.max(350, view.profile.bands.length * 62 + 48)}px`;
  view.profile.bands.forEach((band, index) => {
    if (!parametric) {
      const column = element("div"); column.className = "eq-band";
      const gain = bandInput(index, "gain", -24, 12); gain.ariaLabel = `${number.format(band.frequency_hz)} Hz, dB`;
      const slider = bandInput(index, "gain", -24, 12); slider.type = "range"; slider.step = ".1"; slider.ariaLabel = `${number.format(band.frequency_hz)} Hz`;
      slider.addEventListener("input", () => { view.profile.bands[index].gain_db = Number(slider.value); gain.value = slider.value; queue("BandGain", { index, value: Number(slider.value) }, `gain:${index}`); draw(); });
      slider.addEventListener("change", action(commit));
      bindNumber(gain, "BandGain", value => ({ index, value }), value => { view.profile.bands[index].gain_db = value; slider.value = value; });
      const reset = () => { queue("BandGain", { index, value: 0 }, `gain:${index}`); void commit().catch(report); };
      slider.addEventListener("dblclick", reset);
      const label = element("button", band.frequency_hz >= 1000 ? `${number.format(band.frequency_hz / 1000)}k` : number.format(band.frequency_hz)); label.type = "button"; label.title = tr("Reset"); label.addEventListener("click", reset);
      column.append(gain, slider, label); $("bands").append(column);
    } else {
      const row = element("div"); row.className = "eq-filter";
      const enabled = bandInput(index, "enabled", 0, 1); enabled.type = "checkbox"; enabled.ariaLabel = tr("Enabled"); enabled.addEventListener("change", action(async () => { queue("BandEnabled", { index, value: enabled.checked }); await commit(); }));
      const kind = element("select"); kind.dataset.eqBand = index; kind.dataset.field = "kind"; kind.ariaLabel = tr("Filter type");
      for (const [id, name] of [["Peak", tr("Peak")], ["LowShelf", tr("Low shelf")], ["HighShelf", tr("High shelf")]]) kind.append(Object.assign(element("option", name), { value: id }));
      kind.addEventListener("change", action(async () => { queue("BandKind", { index, value: kind.value }); await commit(); }));
      const frequency = bandInput(index, "frequency", 0, 100000); frequency.ariaLabel = tr("Frequency");
      const gain = bandInput(index, "gain", -24, 12); gain.ariaLabel = tr("Gain");
      const unit = Object.keys(band.width)[0]; const width = bandInput(index, "width", 0, unit === "Q" ? "" : 100000, .01); width.ariaLabel = unit === "Q" ? "Q" : tr("Bandwidth");
      for (const [input, type] of [[frequency, "BandFrequency"], [gain, "BandGain"], [width, "BandWidth"]]) bindNumber(input, type, value => ({ index, value: type === "BandWidth" ? { [unit]: { value } } : value }));
      const units = element("select"); units.ariaLabel = tr("Width unit"); units.dataset.eqBand = index; units.dataset.field = "unit";
      for (const id of ["Q", "Hertz"]) units.append(Object.assign(element("option", id === "Q" ? "Q" : "Hz"), { value: id }));
      units.value = unit;
      units.addEventListener("change", action(async () => { queue("BandWidthUnit", { index, q: units.value === "Q" }); await commit(); }));
      const remove = element("button", tr("Delete")); remove.type = "button"; remove.addEventListener("click", action(async () => { await flush(); queue("RemoveBand", { index }); await commit(); }));
      row.append(enabled, kind, frequency, element("span", "Hz"), gain, element("span", "dB"), width, units, remove); $("filters").append(row);
    }
  });
}
async function loadResponse() {
  if (responseSending || !$("response").checked || !view) return;
  responseSending = true;
  try {
    while (document.getElementById("equalizer-dialog").open && $("response").checked) {
      const stamp = latestProfile;
      const next = await api(`/equalizer/response?output=${encodeURIComponent(view.output)}`);
      if (stamp === latestProfile) { response = next; draw(); break; }
    }
  } finally { responseSending = false; }
}

function draw() {
  if (!view) return;
  const svg = $("plot"), width = $("graph").clientWidth || 400, height = 265;
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`); svg.replaceChildren();
  const node = (tag, attrs) => { const n = document.createElementNS("http://www.w3.org/2000/svg", tag); for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v); svg.append(n); return n; };
  const responseGains = $("response").checked && response ? response.gain_db.filter(Number.isFinite) : [];
  const low = Math.min(-24, ...responseGains), high = Math.max(12, ...responseGains);
  const y = gain => 48 + (high - gain) / (high - low) * 168;
  for (const slider of document.querySelectorAll("#eq-bands input[type=range]")) {
    slider.style.height = `${y(-24) - y(12) + 16}px`;
    slider.style.marginTop = `${y(12) - 48}px`;
  }
  for (const db of [...new Set([Math.ceil(high), 12, 0, -12, -24, Math.floor(low)])]) { node("line", { x1: 40, x2: width - 20, y1: y(db), y2: y(db), class: "eq-grid" }); node("text", { x: 2, y: y(db) + 4, class: "eq-axis" }).textContent = `${db > 0 ? "+" : ""}${number.format(db)}`; }
  node("text", { x: 2, y: 28, class: "eq-axis" }).textContent = "dB";
  const bands = view.profile.bands, parametric = $("mode").value === "parametric";
  const x = i => 48 + (width - 48) * (i + .5) / Math.max(1, bands.length);
  function freqX(f) {
    if (parametric || !bands.length) return 48 + (width - 68) * Math.log(f / 20) / Math.log(1000);
    if (f <= bands[0].frequency_hz) return x(0);
    for (let i = 1; i < bands.length; i++) if (f < bands[i].frequency_hz) return x(i - 1) + (x(i) - x(i - 1)) * Math.log(f / bands[i - 1].frequency_hz) / Math.log(bands[i].frequency_hz / bands[i - 1].frequency_hz);
    return x(bands.length - 1);
  }
  if ($("spectrum").checked && !view.cast && view.spectrum_db.length) {
    const rate = view.sample_rate || 48000, n = view.spectrum_db.length;
    const points = view.spectrum_db.map((db, i) => [i * rate / (2 * (n - 1)), db]).filter(([f]) => f >= 20 && f <= 20000).map(([f, db]) => `${freqX(f)},${216 - Math.max(0, Math.min(1, (db + 90) / 90)) * 168}`).join(" ");
    node("polyline", { points, class: "eq-spectrum" }); node("text", { x: width - 70, y: 28, class: "eq-axis" }).textContent = "dBFS";
  }
  if ($("response").checked && response) node("polyline", { points: response.frequencies.map((f, i) => `${freqX(f)},${y(response.gain_db[i])}`).join(" "), class: "eq-response" });
  if (!parametric) node("polyline", { points: bands.map((b, i) => `${x(i)},${y(b.gain_db)}`).join(" "), class: "eq-curve" });
  if (view.peak_db.some(db => db >= 0)) {
    $("clip").classList.add("clipped"); clearTimeout(clipTimer);
    clipTimer = setTimeout(() => $("clip").classList.remove("clipped"), 1000);
  }
}
function askName(operation, title) {
  nameAction = { operation, output: view.output, id: view.preset_id || view.save_target };
  $("name-title").textContent = title;
  $("name-submit").textContent = title;
  $("name").value = operation === "duplicate" ? tr("Copy of {name}", { name: view.name }) : view.name;
  $("name-dialog").showModal(); $("name").focus(); $("name").select();
}
export function init() {
  const dialog = document.getElementById("equalizer-dialog");
  document.getElementById("open-equalizer").addEventListener("click", action(async () => { dialog.showModal(); await refreshPresets(); render(await api("/equalizer")); watch(); }));
  dialog.addEventListener("close", () => { clearTimeout(clipTimer); $("clip").classList.remove("clipped"); stream?.close(); stream = null; void flush().then(() => view && request("commit")).catch(report); });
  $("preset").addEventListener("focus", action(refreshPresets)); $("preset").addEventListener("change", action(() => operation("select", { preset: $("preset").value || null })));
  $("mode").addEventListener("change", () => { signature = ""; render(view); });
  $("layout").addEventListener("change", action(async () => { await flush(); queue("Layout", { value: $("layout").value }); await commit(); }));
  for (const [id, type] of [["enabled", "Enabled"], ["auto", "AutomaticPreamp"], ["crossfeed", "Crossfeed"], ["mono", "Mono"]]) $(id).addEventListener("change", action(async () => { queue(type, { value: $(id).checked }); await commit(); }));
  for (const [id, type] of [["preamp", "Preamp"], ["bass", "Bass"], ["treble", "Treble"], ["balance", "Balance"]]) { $(id).addEventListener("input", () => { queue(type, { value: Number($(id).value) }); if (id === "preamp") { $("preamp-number").value = $(id).value; checked("auto", false); } }); $(id).addEventListener("change", action(commit)); }
  bindNumber($("preamp-number"), "Preamp", value => ({ value }), value => { $("preamp").value = value; checked("auto", false); });
  $("assigned").addEventListener("change", action(() => operation("assign", { assigned: $("assigned").checked })));
  $("cast-format").addEventListener("change", action(async () => { queue("CastFormat", { value: $("cast-format").value }); await commit(); }));
  $("response").addEventListener("change", action(async () => { await loadResponse(); draw(); })); $("spectrum").addEventListener("change", watch);
  $("add").addEventListener("click", action(async () => { await flush(); queue("AddBand", null); await commit(); }));
  $("apply").addEventListener("click", action(() => operation("apply")));
  $("save").addEventListener("click", action(async () => { await flush(); if (view.save_target) await operation("save", { name: null }); else askName("save", tr("Save")); }));
  $("duplicate").addEventListener("click", action(async () => { await flush(); askName("duplicate", tr("Duplicate")); }));
  $("rename").addEventListener("click", action(async () => { await flush(); askName("rename", tr("Rename")); }));
  $("name-form").addEventListener("submit", event => {
    event.preventDefault(); const { operation: command, ...target } = nameAction;
    void operation(command, { ...target, name: $("name").value }).then(() => $("name-dialog").close()).catch(report);
  });
  $("delete").addEventListener("click", action(() => operation("delete", { id: view.preset_id || view.save_target })));
  $("import").addEventListener("click", () => $("file").click());
  $("file").addEventListener("change", action(async () => { const file = $("file").files[0]; if (file) await operation("import", { text: await file.text(), format: file.name.toLowerCase().endsWith(".json") ? "Json" : "Apo" }); $("file").value = ""; }));
  $("export").addEventListener("click", action(async () => { const format = $("format").value, result = await operation("export", { format }); const url = URL.createObjectURL(new Blob([result.text], { type: "text/plain" })); const a = element("a"); a.href = url; a.download = `equalizer.${format === "Json" ? "json" : "txt"}`; a.click(); setTimeout(() => URL.revokeObjectURL(url), 0); }));
  new ResizeObserver(draw).observe($("graph"));
}
