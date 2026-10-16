import { run, tr } from "./ui.js";

export function qualityControls(root, initial, change) {
  const format = root.querySelector('[data-quality="format"]');
  const bitrate = root.querySelector('[data-quality="bitrate"]');
  const value = root.querySelector('[data-quality="bitrate-value"]');
  const channels = root.querySelector('[data-quality="channels"]');
  const lower = Number(bitrate.min), upper = Number(bitrate.max);
  let current, rate;
  function visible() {
    root.querySelector('[data-quality-row="bitrate"]').hidden = ["original", "flac"].includes(format.value);
    root.querySelector('[data-quality-row="channels"]').hidden = format.value === "original";
  }
  function showRate() {
    value.textContent = rate == null ? tr("Default") : `${rate} kbps`;
  }
  function set(quality) {
    current = quality;
    const profile = quality === "mp3" ? { codec: "mp3", bitrate_kbps: null, channels: "preserve" } : quality?.transcoded;
    format.value = profile?.codec || "original";
    rate = profile ? profile.bitrate_kbps : 128;
    bitrate.min = Math.min(lower, rate ?? 128);
    bitrate.max = Math.max(upper, rate ?? 128);
    bitrate.value = String(rate ?? 128);
    channels.value = profile?.channels || "preserve";
    showRate();
    visible();
  }
  bitrate.addEventListener("input", () => {
    rate = Number(bitrate.value);
    showRate();
  });
  for (const field of [format, bitrate, channels]) field.addEventListener("change", () => run(async () => {
    if (field === bitrate) rate = Number(bitrate.value);
    if (field === format && rate == null) rate = 128;
    const quality = format.value === "original" ? "original" : { transcoded: {
      codec: format.value,
      bitrate_kbps: format.value === "flac" ? null : rate,
      channels: channels.value,
    } };
    visible();
    try { set(await change(quality) ?? quality); }
    catch (error) { set(current); throw error; }
  }));
  set(initial);
  return { set };
}
