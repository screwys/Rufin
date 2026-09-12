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
  time,
} from "./ui.js";

import { api, session, state } from "./connection.js";

import { goToMedia, markCurrent, setFavorite } from "./library.js";

let receivedAt = 0;

let seeking = false;

let seekPreview = null;

let currentFavoriteRequest = Promise.resolve();

let lyricsDocument = null;

let activeLine = -1;

let coverRequest = null;

function showPlayback(value) {
  if (
    seekPreview != null &&
    !seeking &&
    (value?.current?.id !== state.currentId ||
      !value?.can_seek ||
      Math.abs((value?.position_ms || 0) - seekPreview) <= 1500 ||
      value?.error)
  )
    seekPreview = null;
  state.playback = value;
  receivedAt = performance.now();
  const track = value?.current?.track;
  $("player-title").textContent = track?.title || tr("Nothing playing");
  $("player-artist").textContent = track?.artist || "";
  $("player-album").textContent = track?.album || "";
  const playing = ["playing", "resolving", "buffering"].includes(value?.state);
  $("play-pause").setAttribute(
    "aria-label",
    playing ? tr("Pause") : tr("Play"),
  );
  setIcon($("play-pause").firstElementChild, playing ? "pause" : "play");
  $("shuffle").setAttribute("aria-pressed", String(value?.shuffle || false));
  $("repeat").setAttribute(
    "aria-pressed",
    String(value?.repeat !== "off" && !!value),
  );
  setIcon(
    $("repeat").firstElementChild,
    value?.repeat === "one" ? "repeat-one" : "repeat",
  );
  const repeatLabel = {
    off: tr("Repeat off"),
    all: tr("Repeat all"),
    one: tr("Repeat one"),
  }[value?.repeat || "off"];
  $("repeat").title = repeatLabel;
  $("auto-dj").setAttribute("aria-pressed", String(value?.auto_dj || false));
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
  if (state.currentId !== (value?.current?.id || null)) {
    state.currentId = value?.current?.id || null;
    $("current-favorite").disabled = !track;
    $("current-favorite").dataset.favoriteUri = track?.uri || "";
    if (!track) showFavorite($("current-favorite"), false);
    lyricsDocument = null;
    activeLine = -1;
    showLyrics({ state: "empty" });
    coverRequest?.abort();
    coverRequest = new AbortController();
    $("mini-cover").replaceChildren(icon("cover-fallback"));
    if (track) {
      const id = state.currentId;
      currentFavoriteRequest = (async () => {
        const metadata = await api(
          `/media?${new URLSearchParams({ uri: track.uri })}`,
        );
        if (state.currentId !== id) return;
        showFavorite($("current-favorite"), metadata.favorite);
      })();
      run(() => currentFavoriteRequest);
      const signal = AbortSignal.any([session.signal, coverRequest.signal]);
      run(() => cover($("mini-cover"), track.uri, signal));
      run(async () => {
        await api("/lyrics", "POST", {});
        showLyrics(await api("/lyrics"));
      });
    }
  }
  for (const notice of value?.notices || [])
    if (notice.type === "operation_failed") noticeError(notice.error);
  markCurrent();
  updatePosition();
}

function showLyrics(value) {
  if (value.id && value.id !== state.currentId) return;
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
      ? "muted"
      : volume <= 1 / 3
        ? "volume-low"
        : volume <= 2 / 3
          ? "volume-medium"
          : "volume",
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
  new ResizeObserver(scrollLyrics).observe($("lyrics"));
  for (const [id, kind] of [
    ["player-artist", "artists"],
    ["player-album", "albums"],
  ]) {
    $(id).addEventListener("click", () => {
      if (state.playback?.current?.track)
        run(() => goToMedia(state.playback.current.track, kind, $(id)));
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
    const id = state.currentId;
    if (state.playback?.current?.track)
      run(async () => {
        await currentFavoriteRequest;
        if (state.currentId !== id) return;
        await setFavorite([
          {
            ...state.playback.current.track,
            favorite:
              $("current-favorite").getAttribute("aria-pressed") === "true",
          },
        ]);
      });
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
  $("auto-dj").addEventListener("click", () =>
    run(() => api("/playback/auto-dj", "POST", {})),
  );
}

export { coverRequest, showLyrics, showPlayback, init };
