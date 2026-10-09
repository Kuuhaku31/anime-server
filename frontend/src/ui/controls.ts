import type { PlaybackController } from "../playback/playback-controller.ts";
import { formatTime } from "../playback/timeline.ts";
import type { MediaInfo, Selection, Track } from "../state/player-state.ts";

export function element<T extends HTMLElement>(selector: string): T {
  const value = document.querySelector<T>(selector);
  if (!value) throw new Error(`Missing page element: ${selector}`);
  return value;
}

function label(track: Track): string {
  return [track.title, track.language, track.codec_name.toUpperCase(), track.is_forced ? "forced" : null]
    .filter(Boolean).join(" / ");
}

export function attachControls(controller: PlaybackController, video: HTMLVideoElement): void {
  const play = element<HTMLButtonElement>("#toggle-play");
  const seek = element<HTMLInputElement>("#seek");
  const clock = element<HTMLOutputElement>("#clock");
  const audio = element<HTMLSelectElement>("#audio-track");
  const subtitles = element<HTMLSelectElement>("#subtitle-track");
  const volume = element<HTMLInputElement>("#volume");
  const mute = element<HTMLButtonElement>("#mute");
  const fullscreen = element<HTMLButtonElement>("#fullscreen");
  const panel = element<HTMLElement>(".player-panel");
  let previousInfo: MediaInfo | null = null;
  let scrubbing = false;
  play.onclick = () => controller.toggle();
  element<HTMLButtonElement>("#stop").onclick = () => controller.stop();
  element<HTMLButtonElement>("#restart").onclick = () => controller.seek(0);
  element<HTMLButtonElement>("#retry").onclick = () => controller.retry();
  element<HTMLButtonElement>("#retry-probe").onclick = () => controller.retry(true);
  video.addEventListener("click", () => controller.toggle());
  seek.addEventListener("input", () => {
    scrubbing = true;
    clock.textContent = `${formatTime(Number(seek.value))} / ${formatTime(controller.duration)}`;
  });
  seek.addEventListener("change", () => { controller.seek(Number(seek.value)); scrubbing = false; });
  const select = (): void => {
    const selection: Selection = {
      audio: audio.value === "off" ? null : Number(audio.value),
      subtitle: subtitles.value === "off" ? null : Number(subtitles.value),
    };
    controller.select(selection);
  };
  audio.onchange = select;
  subtitles.onchange = select;
  volume.oninput = () => { video.volume = Number(volume.value); video.muted = video.volume === 0; };
  mute.onclick = () => { video.muted = !video.muted; };
  video.addEventListener("volumechange", () => {
    mute.textContent = video.muted ? "Unmute" : "Mute";
    volume.value = String(video.muted ? 0 : video.volume);
  });
  const toggleFullscreen = async (): Promise<void> => {
    if (document.fullscreenElement) await document.exitFullscreen();
    else if (panel.classList.contains("expanded")) panel.classList.remove("expanded");
    else if (panel.requestFullscreen) {
      try { await panel.requestFullscreen(); }
      catch (error) {
        console.info("Using in-page fullscreen because the Fullscreen API was rejected", error);
        panel.classList.add("expanded");
      }
    } else { panel.classList.add("expanded"); }
    fullscreen.textContent = document.fullscreenElement || panel.classList.contains("expanded") ? "Exit fullscreen" : "Fullscreen";
  };
  fullscreen.onclick = () => { toggleFullscreen().catch((error) => controller.fail(error)); };
  document.addEventListener("fullscreenchange", () => {
    fullscreen.textContent = document.fullscreenElement ? "Exit fullscreen" : "Fullscreen";
  });
  panel.addEventListener("keydown", (event) => {
    if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement
      || event.target instanceof HTMLButtonElement || event.ctrlKey || event.metaKey || event.altKey) return;
    switch (event.key.toLowerCase()) {
      case " ":
      case "k": event.preventDefault(); controller.toggle(); break;
      case "arrowleft": event.preventDefault(); controller.seek(controller.position - 10); break;
      case "arrowright": event.preventDefault(); controller.seek(controller.position + 10); break;
      case "m": video.muted = !video.muted; break;
      case "f": toggleFullscreen().catch((error) => controller.fail(error)); break;
      case "escape": panel.classList.remove("expanded"); break;
    }
  });
  controller.addEventListener("change", () => {
    const info = controller.info;
    if (info && previousInfo !== info) {
      previousInfo = info;
      audio.replaceChildren(new Option("No audio", "off"), ...info.audio_tracks.map((track) => new Option(label(track), String(track.stream_index))));
      subtitles.replaceChildren(new Option("Off", "off"), ...info.subtitle_tracks.map((track) => new Option(label(track), String(track.stream_index))));
    }
    audio.value = controller.selection.audio === null ? "off" : String(controller.selection.audio);
    subtitles.value = controller.selection.subtitle === null ? "off" : String(controller.selection.subtitle);
    audio.disabled = subtitles.disabled = !info || controller.pending;
    play.disabled = !controller.file || controller.pending;
    play.textContent = video.paused ? "Play" : "Pause";
    seek.disabled = !controller.info;
    seek.max = String(controller.duration);
    if (!scrubbing) {
      seek.value = String(controller.position);
      clock.textContent = `${formatTime(controller.position)} / ${formatTime(controller.duration)}`;
    }
    seek.style.setProperty("--played", `${100 * controller.position / (controller.duration || 1)}%`);
    seek.style.setProperty("--buffered", `${100 * controller.buffered / (controller.duration || 1)}%`);
    element("#playback-status").textContent = controller.message;
    element("#error").textContent = controller.error;
    element("#error-actions").hidden = !controller.error;
    element("#meta").textContent = controller.file?.name ?? "Select a video from the library";
    panel.setAttribute("aria-busy", String(controller.pending));
  });
}
