import { tr } from "./ui.js";
import {
  cover,
  $,
  button,
  el,
  icon,
  noticeError,
  run,
  setIcon,
  showFavorite,
  slideText,
  time,
} from "./ui.js";

import { api, session, state } from "./connection.js";

import { markCurrent, openLinks, setFavorite } from "./library.js";
import { showQueueMenu } from "./queue.js";

let receivedAt = 0;

let seeking = false;

let seekPreview = null;

let lyricsDocument = null;

let activeLine = -1;

let coverRequest = null;
// The ring around Play shows once a refill lasts half a second, as GTK's does; an ordinary
// start or seek is over before then.
let refillTimer = null;

function showRefill(refilling) {
  const ring = $("play-pause").lastElementChild;
  if (!refilling) {
    clearTimeout(refillTimer);
    refillTimer = null;
    ring.hidden = true;
  } else if (ring.hidden && refillTimer === null) {
    refillTimer = setTimeout(() => {
      refillTimer = null;
      ring.hidden = false;
    }, 500);
  }
}

function showPlayback(value) {
  if (
    seekPreview != null &&
    !seeking &&
    (value?.current?.media_uri !== state.currentMediaUri ||
      value?.current?.run !== state.currentRun ||
      !value?.can_seek ||
      Math.abs((value?.position_ms || 0) - seekPreview) <= 1500 ||
      value?.error)
  )
    seekPreview = null;
  state.playback = value;
  receivedAt = performance.now();
  const track = value?.current?.track;
  $("player-menu").disabled = !track;
  for (const [id, text] of [["player-title", track?.title || tr("Nothing playing")], ["player-artist", track?.artist || ""], ["player-album", track?.album || ""]])
    if ($(id).textContent !== text || !$(id).classList.contains("text-slide")) {
      $(id).textContent = text;
      $(id).title = text;
      slideText($(id));
    }
  // The mini player's top bar names what plays, as GTK's window title does.
  $("mini-title").textContent = track ? [track.title, track.artist].filter(Boolean).join(" · ") : "";
  const playing = ["playing", "resolving", "buffering"].includes(value?.state);
  $("play-pause").setAttribute(
    "aria-label",
    playing ? tr("Pause") : tr("Play"),
  );
  setIcon($("play-pause").firstElementChild, playing ? "rufin-media-playback-pause-symbolic" : "rufin-media-playback-start-symbolic");
  showRefill(!!value?.refilling);
  $("shuffle").setAttribute("aria-pressed", String(value?.shuffle || false));
  $("repeat").setAttribute(
    "aria-pressed",
    String(value?.repeat !== "off" && !!value),
  );
  setIcon(
    $("repeat").firstElementChild,
    value?.repeat === "one" ? "rufin-repeat-one-symbolic" : "rufin-repeat-symbolic",
  );
  const repeatLabel = {
    off: tr("Repeat off"),
    all: tr("Repeat all"),
    one: tr("Repeat one"),
  }[value?.repeat || "off"];
  $("repeat").title = repeatLabel;
  $("auto-dj").setAttribute("aria-pressed", String(value?.auto_dj || false));
  showSleepTimer(value?.sleep_timer || null);
  $("repeat").setAttribute("aria-label", repeatLabel);
  $("mute").setAttribute("aria-pressed", String(value?.muted || false));
  $("mute").setAttribute(
    "aria-label",
    value?.muted ? tr("Unmute") : tr("Mute"),
  );
  if (!volumeEditing && !sendingVolume && !volumeRequest)
    $("volume").value = value?.volume ?? 1;
  updateVolumeIcon();
  $("seek").max = Math.max(1, value?.duration_ms || 0);
  $("seek").disabled = !track || !value.can_seek;
  $("duration").textContent = time(value?.duration_ms);
  if (
    state.currentMediaUri !== (value?.current?.media_uri ?? null) ||
    state.currentRun !== (value?.current?.run ?? null)
  ) {
    state.currentMediaUri = value?.current?.media_uri ?? null;
    state.currentRun = value?.current?.run ?? null;
    $("current-favorite").disabled = !track;
    lyricsDocument = null;
    activeLine = -1;
    showLyrics({ state: "empty" });
    coverRequest?.abort();
    coverRequest = new AbortController();
    if (!track) {
      $("mini-cover").replaceChildren(icon("rufin-symbolic"));
      delete $("mini-cover").dataset.coverIdentity;
      delete $("mini-cover").dataset.coverFull;
    }
    if (track) {
      const signal = AbortSignal.any([session.signal, coverRequest.signal]);
      run(async () => {
        const shown = await cover(
          $("mini-cover"),
          { uri: track.uri, original: true },
          signal,
          track.artwork_previews?.[0],
        );
        if (!shown && !signal.aborted)
          $("mini-cover").replaceChildren(icon("rufin-symbolic"));
      });
      run(async () => {
        await api("/lyrics", "POST", {});
        showLyrics(await api("/lyrics"));
      });
    }
  }
  // The heart shows the playback row's favorite, which follows every committed change.
  showFavorite($("current-favorite"), !!track?.favorite);
  for (const notice of value?.notices || []) {
    if (notice.type !== "operation_failed") continue;
    const message = [
      "Not Found",
      "File not found",
      "Resource not found",
      "Resource not found.",
      "the configured source no longer exists",
      "Could not play this track",
    ].includes(notice.error)
      ? tr("Could not play this track")
      : ({
          "Track unavailable": tr("Track unavailable"),
          "Format not supported": tr("Format not supported"),
          "Couldn't decode this track": tr("Couldn't decode this track"),
          "Access denied": tr("Access denied"),
          "Couldn't connect to the server": tr("Couldn't connect to the server"),
          "Audio output failed": tr("Audio output failed"),
        })[notice.error] || notice.error;
    noticeError(message);
  }
  markCurrent();
  updatePosition();
}

function showLyrics(value) {
  if (
    value.media_uri &&
    (value.media_uri !== state.currentMediaUri || value.run !== state.currentRun)
  )
    return;
  lyricsDocument = value.document || null;
  activeLine = -1;
  $("lyrics").replaceChildren();
  if (lyricsDocument) {
    $("lyrics").style.setProperty(
      "--lyrics-font-size",
      `${value.font_size || 19}px`,
    );
    const rendered = [];
    for (const [index, line] of lyricsDocument.lines.entries()) {
      if (!line.text.trim()) continue;
      rendered.push(line);
      const start =
        line.start_millis ??
        line.cue_lines?.find((cue) => cue.start_millis != null)?.start_millis;
      const position = Math.max(
        0,
        (start || 0) - (lyricsDocument.offset_millis || 0),
      );
      const row =
        start == null
          ? el("p", "lyric-line", line.text)
          : button(line.text, () => seek(position), null, "lyric-line");
      const reading = value.readings?.[index];
      if (reading?.segments) {
        row.replaceChildren();
        for (const segment of reading.segments) {
          if (segment.furigana) {
            const ruby = el("ruby", "", segment.surface);
            ruby.append(el("rt", "", segment.furigana));
            row.append(ruby);
          } else row.append(document.createTextNode(segment.surface));
        }
      }
      if (reading?.romanization)
        row.append(el("small", "romanization", reading.romanization));
      if (start != null)
        row.title = tr("Seek to {time}", { time: time(position) });
      $("lyrics").append(row);
    }
    lyricsDocument = { ...lyricsDocument, lines: rendered };
    if (value.show_romanization && value.pronunciation)
      for (const line of value.pronunciation.lines)
        if (line.start_millis == null && line.text.trim())
          $("lyrics").append(el("p", "lyric-line romanization", line.text));
    updatePosition();
  } else
    $("lyrics").append(
      el(
        "p",
        "empty-note",
        value.state === "loading"
          ? tr("Finding lyrics...")
          : value.state === "instrumental"
            ? tr("Instrumental")
            : tr("No lyrics available."),
      ),
    );
}

async function seek(position) {
  seekPreview = position;
  updatePosition();
  try {
    await api("/playback/seek", "POST", { position_ms: position });
  } catch (error) {
    seekPreview = null;
    updatePosition();
    throw error;
  }
}

function updatePosition() {
  const position =
    seekPreview ??
    Math.min(
      state.playback?.duration_ms || Infinity,
      (state.playback?.position_ms || 0) +
        (state.playback?.state === "playing"
          ? performance.now() - receivedAt
          : 0),
    );
  if (!seeking) {
    $("seek").value = position;
    $("position").textContent = time(position);
  }
  $("seek").style.setProperty(
    "--seek-fill",
    `${(Number($("seek").value) / Number($("seek").max)) * 100}%`,
  );
  if (!lyricsDocument) return;
  let active = -1;
  for (let i = 0; i < lyricsDocument.lines.length; i++) {
    const start = lyricsDocument.lines[i].start_millis;
    if (
      start != null &&
      start <= position + (lyricsDocument.offset_millis || 0)
    )
      active = i;
  }
  if (active === activeLine) return;
  if (activeLine >= 0)
    $("lyrics").children[activeLine]?.classList.remove("active");
  activeLine = active;
  if (active >= 0) {
    const line = $("lyrics").children[active];
    line?.classList.add("active");
    scrollLyrics();
  }
}

function scrollLyrics() {
  const scroller = $("lyrics"),
    line = scroller.children[activeLine];
  if (!line) return;
  scroller.scrollTo({
    top:
      scroller.scrollTop +
      line.getBoundingClientRect().top -
      scroller.getBoundingClientRect().top -
      scroller.clientHeight / 2 +
      line.offsetHeight / 2,
    behavior: matchMedia("(prefers-reduced-motion: reduce)").matches
      ? "instant"
      : "smooth",
  });
}

function updateVolumeIcon() {
  const volume = Number($("volume").value);
  $("volume").style.setProperty("--volume-fill", `${volume * 100}%`);
  setIcon(
    $("mute").firstElementChild,
    state.playback?.muted || volume <= 0
      ? "rufin-audio-volume-muted-symbolic"
      : volume <= 1 / 3
        ? "rufin-audio-volume-low-symbolic"
        : volume <= 2 / 3
          ? "rufin-audio-volume-medium-symbolic"
          : "rufin-audio-volume-high-symbolic",
  );
}

let volumeEditing = false;

let volumeRequest = null;

let sendingVolume = false;

async function setVolume(persist) {
  volumeRequest = { volume: Number($("volume").value), persist };
  updateVolumeIcon();
  if (sendingVolume) return;
  sendingVolume = true;
  try {
    while (volumeRequest) {
      const request = volumeRequest;
      volumeRequest = null;
      await api("/playback/volume", "POST", request);
    }
  } finally {
    sendingVolume = false;
  }
}

function init() {
  $("player-menu").removeAttribute("popovertarget");
  $("player-menu").addEventListener("click", () => {
    if (state.playback?.current)
      run(() => showQueueMenu(state.playback.current, $("player-menu")));
  });
  new ResizeObserver(scrollLyrics).observe($("lyrics"));
  for (const [id, field] of [
    ["player-artist", "artist"],
    ["player-album", "album"],
  ]) {
    $(id).addEventListener("click", () => {
      if (state.playback?.current?.track) openLinks(state.playback.current.track, field, $(id));
    });
  }
  $("play-pause").addEventListener("click", () =>
    run(() =>
      api(
        `/playback/${["playing", "resolving", "buffering"].includes(state.playback?.state) ? "pause" : "play"}`,
        "POST",
        {},
      ),
    ),
  );
  for (const action of ["previous", "next"])
    $(action).addEventListener("click", () =>
      run(() => api(`/playback/${action}`, "POST", {})),
    );
  $("shuffle").addEventListener("click", () =>
    run(() =>
      api("/playback/shuffle", "POST", { enabled: !state.playback?.shuffle }),
    ),
  );
  $("repeat").addEventListener("click", () =>
    run(() =>
      api("/playback/repeat", "POST", {
        mode: { off: "all", all: "one", one: "off" }[
          state.playback?.repeat || "off"
        ],
      }),
    ),
  );
  $("mute").addEventListener("click", () =>
    run(() => api("/playback/mute", "POST", { muted: !state.playback?.muted })),
  );
  $("volume").addEventListener("input", () => run(() => setVolume(false)));
  $("volume").addEventListener("change", () => run(() => setVolume(true)));
  $("volume").addEventListener("pointerdown", () => {
    volumeEditing = true;
  });
  for (const event of ["pointerup", "pointercancel", "blur"])
    $("volume").addEventListener(event, () => {
      volumeEditing = false;
    });
  $("current-favorite").addEventListener("click", () => {
    const track = state.playback?.current?.track;
    if (track) run(() => setFavorite(track));
  });
  $("seek").addEventListener("input", () => {
    seeking = true;
    seekPreview = Number($("seek").value);
    updatePosition();
    $("position").textContent = time(Number($("seek").value));
  });
  $("seek").addEventListener("change", () => {
    const position = Number($("seek").value);
    seeking = false;
    run(() => seek(position));
  });
  $("seek").addEventListener("blur", () => {
    if (seeking) seekPreview = null;
    seeking = false;
    updatePosition();
  });
  $("seek").addEventListener("pointercancel", () => {
    seeking = false;
    seekPreview = null;
    updatePosition();
  });
  document.addEventListener("keydown", (event) => {
    if (
      event.code === "Space" &&
      !event.defaultPrevented &&
      !["INPUT", "TEXTAREA", "SELECT", "BUTTON", "SUMMARY"].includes(
        event.target.tagName,
      ) &&
      !document.querySelector("dialog[open]")
    ) {
      event.preventDefault();
      $("play-pause").click();
    }
  });
  setInterval(() => {
    if (!document.hidden) updatePosition();
  }, 200);
  initSleepTimer();
  $("auto-dj").addEventListener("click", () =>
    run(() => api("/playback/auto-dj", "POST", {})),
  );
}

// The sleep timer runs in the player; the page shows it as GTK's popover does and counts the
// time left down between updates.
let sleepTimer = null;

function sleepTimerKind() {
  return document.querySelector('input[name="sleep-timer-kind"]:checked').value;
}

function sleepCountdown(milliseconds) {
  const total = Math.ceil(Math.max(0, milliseconds) / 1000);
  const [hours, minutes, seconds] = [Math.floor(total / 3600), Math.floor(total / 60) % 60, total % 60];
  const pad = (value) => String(value).padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${minutes}:${pad(seconds)}`;
}

function sendSleepTimer(body) {
  run(() => api("/playback/sleep-timer", "POST", body));
}

function showSleepPresets() {
  const kind = sleepTimerKind();
  const values = $("sleep-timer").dataset[kind].split(",").map(Number);
  $("sleep-timer-presets").replaceChildren(
    ...values.map((value) => {
      const label = kind === "minutes" ? tr("{minutes} min", { minutes: value }) : value === 1 ? tr("End of this track") : String(value);
      const preset = el("button", "", label);
      preset.type = "button";
      preset.classList.toggle("wide", kind === "tracks" && value === 1);
      preset.dataset.value = value;
      // A preset fills the value; Start starts it.
      preset.addEventListener("click", () => {
        $("sleep-timer-value").value = value;
        highlightSleepPreset();
      });
      return preset;
    }),
  );
  $("sleep-timer-value").max = kind === "minutes" ? 240 : 100;
  // The running timer's value, or 30 minutes or the end of this track.
  $("sleep-timer-value").value = sleepTimer?.kind === kind ? sleepTimer.value : kind === "minutes" ? 30 : 1;
  $("sleep-timer-value-label").textContent = kind === "minutes" ? tr("Minutes") : tr("Tracks");
  $("sleep-timer-finish").hidden = kind !== "minutes";
  highlightSleepPreset();
}

function highlightSleepPreset() {
  const value = Math.round(Number($("sleep-timer-value").value));
  for (const preset of $("sleep-timer-presets").children)
    preset.setAttribute("aria-pressed", String(Number(preset.dataset.value) === value));
}

function startSleepTimer(kind, value) {
  $("sleep-timer").hidePopover();
  sendSleepTimer({
    action: "start",
    kind,
    value,
    finish_track: kind === "minutes" && $("sleep-timer-finish-track").checked,
  });
}

function tickSleepTimer() {
  if (!sleepTimer) return;
  const timed = sleepTimer.remaining_ms != null && (sleepTimer.kind === "minutes" || sleepTimer.tracks_left <= 1);
  const left = timed ? sleepCountdown(sleepTimer.remaining_ms - (performance.now() - sleepTimer.receivedAt)) : null;
  $("sleep-timer-countdown").textContent = left ?? sleepTimer.countdown;
  $("sleep-timer-short").textContent = left ?? sleepTimer.short;
}

function showSleepTimer(timer) {
  const kindChanged = sleepTimer?.kind !== timer?.kind || sleepTimer?.value !== timer?.value;
  sleepTimer = timer && { ...timer, receivedAt: performance.now() };
  $("sleep-timer-button").classList.toggle("running", !!timer);
  $("sleep-timer-short").hidden = !timer;
  $("sleep-timer-running").hidden = !timer;
  $("sleep-timer-title").hidden = !!timer;
  if (timer) {
    $("sleep-timer-caption").textContent = timer.caption;
    $("sleep-timer-caption").hidden = !timer.caption;
    $("sleep-timer-extend").textContent = timer.extend;
    $("sleep-timer-finish-track").checked = timer.finish_track;
  }
  const label = timer?.caption ? `${tr("Sleep timer")}\n${timer.caption}` : tr("Sleep timer");
  $("sleep-timer-button").title = label;
  if (kindChanged) {
    if (timer) document.querySelector(`input[name="sleep-timer-kind"][value="${timer.kind}"]`).checked = true;
    showSleepPresets();
  }
  tickSleepTimer();
}

function initSleepTimer() {
  // Opening shows the running timer's kind.
  $("sleep-timer").addEventListener("beforetoggle", (event) => {
    if (event.newState !== "open") return;
    if (sleepTimer) document.querySelector(`input[name="sleep-timer-kind"][value="${sleepTimer.kind}"]`).checked = true;
    showSleepPresets();
  });
  for (const input of document.querySelectorAll('input[name="sleep-timer-kind"]')) input.addEventListener("change", showSleepPresets);
  $("sleep-timer-value").addEventListener("input", highlightSleepPreset);
  $("sleep-timer-start").addEventListener("click", () => {
    const value = Math.round(Number($("sleep-timer-value").value));
    if (value >= 1) startSleepTimer(sleepTimerKind(), value);
  });
  $("sleep-timer-extend").addEventListener("click", () => sendSleepTimer({ action: "extend" }));
  $("sleep-timer-off").addEventListener("click", () => {
    $("sleep-timer").hidePopover();
    sendSleepTimer({ action: "off" });
  });
  $("sleep-timer-finish-track").addEventListener("change", () => {
    if (sleepTimer?.kind === "minutes") sendSleepTimer({ action: "finish_track", enabled: $("sleep-timer-finish-track").checked });
  });
  setInterval(() => {
    if (!document.hidden) tickSleepTimer();
  }, 1000);
  showSleepPresets();
}

export { coverRequest, showLyrics, showPlayback, init };
