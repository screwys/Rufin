import { run, tr } from "./ui.js";

// Shows a download or stream quality as the app offers it ({value, bitrate, channels}) and
// posts each edit; `change(edit)` returns what the app made of it. With `follows`, a first
// choice leaves the quality to the one set for every source, which the app shows as no value.
export function qualityControls(root, initial, change, follows = false) {
  const format = root.querySelector('[data-quality="format"]');
  if (follows) format.prepend(new Option(tr("Default"), "default"));
  const bitrate = root.querySelector('[data-quality="bitrate"]');
  const value = root.querySelector('[data-quality="bitrate-value"]');
  const channels = root.querySelector('[data-quality="channels"]');
  const lower = Number(bitrate.min), upper = Number(bitrate.max);
  let current;
  function set(view) {
    current = view;
    const profile = view.value?.transcoded;
    const rate = profile?.bitrate_kbps ?? null;
    format.value = view.value == null ? "default" : profile?.codec || "original";
    if (rate != null) {
      bitrate.min = Math.min(lower, rate);
      bitrate.max = Math.max(upper, rate);
      bitrate.value = String(rate);
    }
    value.textContent = rate == null ? tr("Default") : `${rate} kbps`;
    channels.value = profile?.channels || "preserve";
    root.querySelector('[data-quality-row="bitrate"]').hidden = !view.bitrate;
    root.querySelector('[data-quality-row="channels"]').hidden = !view.channels;
  }
  bitrate.addEventListener("input", () => {
    value.textContent = `${bitrate.value} kbps`;
  });
  const edits = [
    [format, () => format.value === "default" ? "default" : { codec: format.value === "original" ? null : format.value }],
    [bitrate, () => ({ bitrate_kbps: Number(bitrate.value) })],
    [channels, () => ({ channels: channels.value })],
  ];
  for (const [field, edit] of edits) field.addEventListener("change", () => run(async () => {
    try { set(await change(edit())); }
    catch (error) { set(current); throw error; }
  }));
  set(initial);
  return { set };
}
