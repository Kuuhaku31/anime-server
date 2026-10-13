// 协调播放会话, 用户操作和媒体引擎, 保持绝对进度与播放意图一致.
import { isAbort, jsonRequest, releaseSession, request, saveProgress, wait } from "../api/anime-server.ts";
import type { MediaInfo, PlaybackSession, PlaybackStart, Selection, VideoFile } from "../state/player-state.ts";
import { MediaAdapter } from "./hls-adapter.ts";
import { absoluteTime, formatTime, rangeReady } from "./timeline.ts";

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
  // 等待转码或重置 video 时仍保留原视频绝对位置, 不能依赖此时的 currentTime.
  private savedPosition = 0;
  private loaded = false;
  // 用户意图独立于为换轨或等待缓存而触发的临时 pause.
  private playRequested = false;
  private reporting = false;
  private saving: Promise<void> = Promise.resolve();
  private targeting: Promise<void> = Promise.resolve();

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
    // 活动阶段每秒刷新, 静态阶段仅保留低频心跳, 等待阶段由 loadRange 自行轮询.
    window.setInterval(() => {
      if (this.session && ["loading", "running", "stopping"].includes(this.session.phase)) void this.report();
    }, 1000);
    window.setInterval(() => {
      if (this.session && !["loading", "running", "stopping"].includes(this.session.phase)) void this.report();
    }, 10_000);
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
  // 浏览器已下载的缓冲末端, 与服务端 buffered_until 表示的生成进度不同.
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
      // 默认点击仅提交 file ID, 轨道和数据库观看进度均由服务端决定.
      await this.start(undefined, true, signal);
    });
  }

  retry(probe = false): void {
    this.enqueue(async (signal) => {
      if (!this.file) return;
      const initialized = this.info !== null;
      if (probe) {
        const info = await request<MediaInfo>(`/api/files/${this.file.pre16mib_md5}/probe`, { method: "POST", signal });
        signal.throwIfAborted();
        this.info = info;
      }
      await this.start(initialized ? this.savedPosition : undefined, true, signal);
    });
  }

  select(selection: Selection): void {
    if (!this.info || !this.file) return;
    // 在入队触发临时暂停前保存意图, 换轨后继续保持原来的播放或暂停状态.
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
    // 已生成范围直接 seek, 网络缓冲由 HLS 负责, 不必重新创建会话.
    if (this.session && this.loaded && !this.pending
      && rangeReady(target, this.session.buffered_until)) {
      const sessionId = this.session.session_id;
      this.video.currentTime = target;
      this.savedPosition = target;
      Promise.all([this.persist(), this.updateTarget(target)]).catch((error) => {
        if (this.session?.session_id === sessionId) this.fatal(error);
      });
      this.changed();
    } else {
      const playing = this.playRequested;
      this.enqueue(async (signal) => {
        this.savedPosition = target;
        await this.persist();
        if (this.session) {
          // 未覆盖目标只卸载当前媒体并等待同一会话, 不重启顺序生成任务.
          this.loaded = false;
          this.adapter.destroy();
          await this.loadRange(target, playing, signal);
        } else { await this.start(target, playing, signal); }
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
    } else {
      // 暂停意图同步更新, 避免 pause 事件尚未派发时立即换轨又恢复播放.
      this.playRequested = false;
      this.video.pause();
    }
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

  // 终止加载并释放租约, 保留错误等待手动 Retry, 不自动切换输出策略.
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

  // 新操作取消旧等待, 但仍按顺序完成旧操作清理, 防止多个会话同时接管播放器.
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

  private async start(position: number | undefined, autoplay: boolean, signal: AbortSignal): Promise<void> {
    if (!this.file) throw new Error("Select a video first");
    await this.detach();
    signal.throwIfAborted();
    this.savedPosition = position ?? 0;
    this.playRequested = autoplay;
    this.message = "Preparing stream...";
    this.changed();
    // 即使操作被新请求替代也读取创建结果, 确保不会遗留未知的服务端租约.
    const url = `/api/files/${this.file.pre16mib_md5}/playback`;
    const response = position === undefined
      ? await request<PlaybackStart>(url, { method: "POST" })
      : await jsonRequest<PlaybackStart>(url, {
        audio_stream_index: this.selection.audio, subtitle_stream_index: this.selection.subtitle, position,
      });
    const { media, selection, ...session } = response;
    if (signal.aborted) { await releaseSession(session.session_id); signal.throwIfAborted(); }
    this.info = media;
    this.selection = selection;
    this.savedPosition = session.position;
    this.session = session;
    await this.loadRange(session.position, autoplay, signal, false);
  }

  // 目标严格落入已发布的半开区间后才挂载 HLS, 等待期间不创建新会话.
  private async loadRange(position: number, autoplay: boolean, signal: AbortSignal, notify = true): Promise<void> {
    if (!this.session) throw new Error("Playback session is missing");
    if (notify) await this.updateTarget(position, signal);
    const sessionId = this.session.session_id;
    let deadline = Date.now() + 180_000;
    let generated = this.session.buffered_until;
    let activity = this.activity(this.session);
    while (true) {
      signal.throwIfAborted();
      if (!this.session || this.session.session_id !== sessionId) throw new Error("Playback session changed");
      if (rangeReady(position, this.session.buffered_until)) break;
      if (this.session.phase === "complete") throw new Error("Cached video does not cover the requested position.");
      // 超时只限制持续无进展, 生成前进或调度状态变化都会刷新等待期限.
      if (this.session.buffered_until > generated || this.activity(this.session) !== activity) {
        generated = this.session.buffered_until;
        activity = this.activity(this.session);
        deadline = Date.now() + 180_000;
      }
      if (Date.now() > deadline) throw new Error("Cache generation made no progress for 180 seconds. Retry later.");
      const phaseLabel = this.session.phase === "failed" ? "Cache generation failed"
        : this.session.phase === "stopping" ? "Stopping cache generation"
          : this.session.phase === "running" ? "Generating cache"
            : "Preparing cache or waiting for a media process slot";
      this.message = `${phaseLabel}: ${formatTime(this.session.buffered_until)} / ${formatTime(this.duration)}. Waiting for ${formatTime(position)}...`;
      this.changed();
      await wait(500, signal);
      const status: PlaybackSession = await request<PlaybackSession>(`/api/playback/${sessionId}/status`, { signal });
      signal.throwIfAborted();
      this.session = status;
    }
    signal.throwIfAborted();
    await this.adapter.attach(this.session.url, position, signal, (error) => this.fatal(error));
    this.loaded = true;
    this.error = "";
    this.playRequested = autoplay;
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

  private activity(session: PlaybackSession): string {
    return session.phase;
  }

  private updateTarget(position: number, signal?: AbortSignal): Promise<void> {
    const sessionId = this.session?.session_id;
    if (!sessionId) return Promise.reject(new Error("Playback session is missing"));
    // 连续拖动按顺序发送目标, 避免较旧的请求最后到达服务端.
    const task = this.targeting.then(async () => {
      signal?.throwIfAborted();
      if (this.session?.session_id !== sessionId) return;
      const status = await jsonRequest<PlaybackSession>(`/api/playback/${sessionId}/target`, { position }, "POST", signal);
      signal?.throwIfAborted();
      if (this.session?.session_id === sessionId) this.session = status;
    });
    this.targeting = task.catch((error) => {
      if (!isAbort(error)) console.error("Playback target update failed", error);
    });
    return task;
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
    // 入队时捕获文件和位置, 避免执行前换片而把旧进度写到新文件.
    const fileId = this.file.pre16mib_md5;
    const position = this.position;
    // 普通保存串行发送, 卸载请求绕过队列以尽力在页面关闭前发出.
    if (keepalive) return saveProgress(fileId, position, true);
    const task = this.saving.catch((error) => console.error("Previous progress save failed", error))
      .then(() => saveProgress(fileId, position));
    this.saving = task;
    return task;
  }

  private async report(): Promise<void> {
    if (this.reporting || this.pending || !this.session) return;
    const sessionId = this.session.session_id;
    this.reporting = true;
    try {
      const status = await request<PlaybackSession>(`/api/playback/${sessionId}/status`);
      // 响应返回前可能已换轨或进入拖动等待, 旧响应不能覆盖当前操作状态.
      if (this.session?.session_id !== sessionId || this.pending) return;
      this.session = status;
      if (status.phase === "failed" && rangeReady(this.position, status.buffered_until)) {
        this.error = "";
        this.message = "Playing cached range. Cache generation stopped.";
      }
      this.changed();
    } catch (error) {
      if (this.session?.session_id === sessionId && !this.pending) this.fatal(error);
    } finally { this.reporting = false; }
  }
}
