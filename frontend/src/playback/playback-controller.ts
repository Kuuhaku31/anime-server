import { isAbort, jsonRequest, releaseSession, request, saveProgress, wait } from "../api/anime-server.ts";
import type { MediaInfo, PlaybackSession, Selection, VideoFile } from "../state/player-state.ts";
import { MediaAdapter } from "./hls-adapter.ts";
import { absoluteTime, needsNewStream } from "./timeline.ts";

export class PlaybackController extends EventTarget {
  file: VideoFile | null = null;
  info: MediaInfo | null = null;
  session: PlaybackSession | null = null;
  selection: Selection = { audio: null, subtitle: null };
  pending = false;
  error = "";
  message = "Select a video from the library";
  private video: HTMLVideoElement;
  private adapter: MediaAdapter;
  private operation = new AbortController();
  private queue: Promise<void> = Promise.resolve();
  private savedPosition = 0;
  private loaded = false;
  private playRequested = false;
  private reporting = false;
  private saving: Promise<void> = Promise.resolve();

  constructor(video: HTMLVideoElement) {
    super();
    this.video = video;
    this.adapter = new MediaAdapter(video);
    video.addEventListener("timeupdate", () => {
      if (this.loaded && !this.pending) this.savedPosition = this.position;
      this.changed();
    });
    video.addEventListener("waiting", () => {
      if (!this.pending && this.loaded) { this.message = "Buffering..."; this.changed(); }
    });
    video.addEventListener("playing", () => {
      if (!this.loaded) return;
      this.playRequested = true;
      this.message = "Playing";
      this.changed();
    });
    video.addEventListener("pause", () => {
      if (!this.pending && this.loaded) {
        this.playRequested = false;
        this.message = "Paused";
        this.persist().catch((error) => this.fail(error));
      }
      this.changed();
    });
    video.addEventListener("ended", () => {
      if (this.pending || !this.loaded) return;
      this.savedPosition = this.duration;
      this.loaded = false;
      this.playRequested = false;
      this.enqueue(async () => {
        await this.persist();
        await this.detach();
        this.message = "Ended";
      });
    });
    window.setInterval(() => {
      if (!this.pending && this.loaded && !video.paused) this.persist().catch((error) => this.fail(error));
    }, 5000);
    window.setInterval(() => { void this.report(); }, 10_000);
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) this.persist(true).catch((error) => console.error("Progress save failed", error));
      else if (!this.pending) void this.report();
    });
    window.addEventListener("pagehide", () => {
      this.savedPosition = this.position;
      this.persist(true).catch((error) => console.error("Progress save failed", error));
      this.operation.abort();
      this.detach(true).catch((error) => console.error("Session release failed", error));
    });
    window.addEventListener("pageshow", (event) => {
      if (event.persisted && this.file) {
        this.pending = false;
        this.message = "Session closed. Press Play to resume.";
        this.changed();
      }
    });
  }

  get duration(): number { return this.info?.duration ?? 0; }
  get position(): number {
    return !this.pending && this.loaded && this.session
      ? absoluteTime(this.video.currentTime, this.session.start_time, this.duration) : this.savedPosition;
  }
  get buffered(): number {
    if (!this.session || !this.loaded) return 0;
    const ranges = this.video.buffered;
    return ranges.length ? Math.min(this.duration, this.session.start_time + ranges.end(ranges.length - 1)) : 0;
  }

  open(file: VideoFile): void {
    this.enqueue(async (signal) => {
      await this.persist();
      await this.detach();
      signal.throwIfAborted();
      this.file = file;
      this.info = null;
      this.savedPosition = 0;
      this.message = "Reading media tracks...";
      this.changed();
      const [info, progress] = await Promise.all([
        request<MediaInfo>(`/api/files/${file.pre16mib_md5}/streams`, { signal }),
        request<{ watch_pos: number | null }>(`/api/files/${file.pre16mib_md5}/progress`, { signal }),
      ]);
      signal.throwIfAborted();
      this.info = info;
      this.selection = {
        audio: (info.audio_tracks.find((t) => t.is_default) ?? info.audio_tracks[0])?.stream_index ?? null,
        subtitle: info.subtitle_tracks.find((t) => t.is_default || t.is_forced)?.stream_index ?? null,
      };
      this.savedPosition = progress.watch_pos && progress.watch_pos < info.duration - 2 ? progress.watch_pos : 0;
      await this.start(this.savedPosition, true, signal);
    });
  }

  retry(probe = false): void {
    this.enqueue(async (signal) => {
      if (!this.file) return;
      if (probe || !this.info) {
        const previous = this.info;
        const info = await request<MediaInfo>(`/api/files/${this.file.pre16mib_md5}/${probe ? "probe" : "streams"}`, {
          method: probe ? "POST" : "GET", signal,
        });
        signal.throwIfAborted();
        this.info = info;
        if (!previous || (this.selection.audio !== null && !info.audio_tracks.some((t) => t.stream_index === this.selection.audio))) {
          this.selection.audio = (info.audio_tracks.find((t) => t.is_default) ?? info.audio_tracks[0])?.stream_index ?? null;
        }
        if (!info.subtitle_tracks.some((t) => t.stream_index === this.selection.subtitle)) this.selection.subtitle = null;
      }
      await this.start(this.savedPosition, true, signal);
    });
  }

  select(selection: Selection): void {
    if (!this.info || !this.file) return;
    const playing = this.playRequested;
    this.enqueue(async (signal) => {
      this.selection = selection;
      await this.persist();
      await this.start(this.savedPosition, playing, signal);
    });
  }

  seek(position: number): void {
    if (!this.info) return;
    if (!Number.isFinite(position)) {
      this.fail(new Error("Playback position must be finite"));
      return;
    }
    const target = Math.max(0, Math.min(position, this.duration - 0.1));
    if (this.session && this.loaded && !this.pending
      && !needsNewStream(target, this.session.start_time, this.session.buffered_until)) {
      this.video.currentTime = target - this.session.start_time;
      this.savedPosition = target;
      this.persist().catch((error) => this.fail(error));
      this.changed();
    } else {
      const playing = this.playRequested;
      this.enqueue(async (signal) => {
        this.savedPosition = target;
        await this.persist();
        await this.start(target, playing, signal);
      });
    }
  }

  toggle(): void {
    if (this.pending || !this.file) return;
    if (!this.loaded || !this.session) {
      this.enqueue(async (signal) => {
        await this.start(this.savedPosition >= this.duration ? 0 : this.savedPosition, true, signal);
      });
    } else if (this.video.paused) {
      this.playRequested = true;
      this.video.play().catch((error) => this.fatal(error));
    } else { this.video.pause(); }
  }

  stop(): void {
    this.enqueue(async () => {
      this.playRequested = false;
      await this.persist();
      await this.detach();
      this.message = "Stopped";
    });
  }

  fail(error: unknown): void {
    if (isAbort(error)) return;
    console.error(error);
    this.error = error instanceof Error ? error.message : String(error);
    this.message = "Playback needs attention";
    this.changed();
  }

  private fatal(error: unknown): void {
    if (isAbort(error)) return;
    this.savedPosition = this.position;
    this.loaded = false;
    this.playRequested = false;
    this.operation.abort();
    this.adapter.destroy();
    this.pending = false;
    this.fail(error);
    const session = this.session;
    this.session = null;
    this.queue = this.queue.then(async () => {
      try {
        await Promise.all([this.persist(), session ? releaseSession(session.session_id) : Promise.resolve()]);
      } catch (cleanupError) {
        console.error("Playback cleanup failed", cleanupError);
        this.error += `\nSession cleanup failed: ${String(cleanupError)}`;
        this.changed();
      }
    });
  }

  private changed(): void { this.dispatchEvent(new Event("change")); }

  private enqueue(action: (signal: AbortSignal) => Promise<void>): void {
    this.savedPosition = this.position;
    this.operation.abort();
    const operation = new AbortController();
    this.operation = operation;
    this.pending = true;
    this.error = "";
    this.video.pause();
    this.changed();
    this.queue = this.queue.then(async () => {
      if (operation.signal.aborted) return;
      try { await action(operation.signal); }
      catch (error) {
        if (!operation.signal.aborted) {
          this.playRequested = false;
          try { await this.detach(); }
          catch (cleanupError) { console.error("Session cleanup failed", cleanupError); }
          if (!operation.signal.aborted) this.fail(error);
        }
      } finally {
        if (!operation.signal.aborted) { this.pending = false; this.changed(); }
      }
    });
  }

  private async start(position: number, autoplay: boolean, signal: AbortSignal): Promise<void> {
    if (!this.file || !this.info) throw new Error("Load media information first");
    await this.detach();
    signal.throwIfAborted();
    this.savedPosition = position;
    this.playRequested = autoplay;
    this.message = "Preparing stream...";
    this.changed();
    // Read the creation response even if superseded, so its session can be released.
    const session = await jsonRequest<PlaybackSession>(`/api/files/${this.file.pre16mib_md5}/playback-plan`, {
      source_version: this.info.source_version, video_stream_index: this.info.video_tracks[0].stream_index,
      audio_stream_index: this.selection.audio, subtitle_stream_index: this.selection.subtitle, position,
    });
    if (signal.aborted) { await releaseSession(session.session_id); signal.throwIfAborted(); }
    this.session = session;
    const deadline = Date.now() + 180_000;
    while (!this.session.ready) {
      if (this.session.error || ["failed", "cancelled"].includes(this.session.state)) {
        throw new Error(this.session.error || "Playback session stopped");
      }
      if (Date.now() > deadline) throw new Error("Playback startup timed out. Stop another stream or retry.");
      this.message = this.session.state === "queued" ? "Waiting for a media process slot..." : "Generating first segment...";
      this.changed();
      await wait(500, signal);
      this.session = await request<PlaybackSession>(`/api/playback/${session.session_id}/status`, { signal });
    }
    signal.throwIfAborted();
    await this.adapter.attach(this.session.url, position - this.session.start_time, signal, (error) => this.fatal(error));
    this.loaded = true;
    this.message = autoplay ? "Starting playback..." : "Paused";
    if (autoplay) {
      try { await this.video.play(); }
      catch (error) {
        if (error instanceof DOMException && error.name === "NotAllowedError") {
          this.playRequested = false;
          this.message = "Press Play to start";
        } else { throw error; }
      }
    }
  }

  private async detach(keepalive = false): Promise<void> {
    const previous = this.session;
    this.session = null;
    this.loaded = false;
    this.adapter.destroy();
    if (previous) await releaseSession(previous.session_id, keepalive);
  }

  private persist(keepalive = false): Promise<void> {
    if (!this.file || !this.info) return Promise.resolve();
    const id = this.file.pre16mib_md5;
    const position = this.position;
    if (keepalive) return saveProgress(id, position, true);
    const task = this.saving.catch((error) => console.error("Previous progress save failed", error))
      .then(() => saveProgress(id, position));
    this.saving = task;
    return task;
  }

  private async report(): Promise<void> {
    if (this.reporting || this.pending || !this.session) return;
    const id = this.session.session_id;
    this.reporting = true;
    try {
      const status = await request<PlaybackSession>(`/api/playback/${id}/status`);
      if (this.session?.session_id !== id || this.pending) return;
      this.session = status;
      if (status.error || ["failed", "cancelled"].includes(status.state)) {
        throw new Error(status.error || "Playback session stopped");
      }
      this.changed();
    } catch (error) {
      if (this.session?.session_id === id && !this.pending) this.fatal(error);
    } finally { this.reporting = false; }
  }
}
