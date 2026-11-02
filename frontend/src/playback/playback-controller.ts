
// frontend/src/playback/playback-controller.ts
// 协调最小播放会话, 本地等待位置和浏览器媒体引擎.
import { createPlayback, getPlayback, isAbort, releasePlayback, rescanMedia, saveConfigs, wait } from "../api/anime-server.ts";
import type { CacheStatus, MediaEntry, MediaInfo, PlaybackQuery, PlaybackSession, StreamSelection } from "../state/player-state.ts";
import { MediaAdapter } from "./hls-adapter.ts";
import { absoluteTime, formatTime, rangeReady } from "./timeline.ts";

/** 管理换片, 选轨, seek 和轮询, 不向服务端发送等待目标. */
export class PlaybackController extends EventTarget {
  /** 当前选中的路径和媒体信息. */
  file: MediaEntry | null = null;
  /** 播放创建时重新探测的媒体信息. */
  info: MediaInfo | null = null;
  /** 当前不透明会话和清单地址. */
  session: PlaybackSession | null = null;
  /** 当前服务端缓存快照. */
  cache: CacheStatus | null = null;
  /** 服务端确认的实际三类流索引. */
  selection: StreamSelection | null = null;
  /** 是否正在等待用户操作完成. */
  pending = false;
  /** 当前需要显示的错误. */
  error = "";
  /** 当前播放状态说明. */
  message = "Select a video from the library";

  /** 绑定浏览器媒体事件和必要的进度轮询. */
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
    // 未完成阶段每秒刷新, Completed 不需要心跳, 等待阶段自行轮询.
    window.setInterval(() => {
      if (this.session && this.cache?.phase !== "completed") void this.report();
    }, 1000);
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

  /** 返回原媒体的完整时长. */
  get duration(): number { return this.info?.metadata.duration ?? 0; }
  /** 返回实际播放位置或当前本地等待位置. */
  get position(): number {
    return !this.pending && this.loaded && this.session
      ? absoluteTime(this.video.currentTime, 0, this.duration) : this.savedPosition;
  }
  /** 返回浏览器下载缓冲末端, 不等同于服务端生成进度. */
  get buffered(): number {
    if (!this.session || !this.loaded) return 0;
    const ranges = this.video.buffered;
    return ranges.length ? Math.min(this.duration, ranges.end(ranges.length - 1)) : 0;
  }

  /** 按列表路径打开媒体, 默认位置取自创建响应的最新配置. */
  open(file: MediaEntry): void {
    this.enqueue(async (signal) => {
      await this.persist();
      await this.detach();
      signal.throwIfAborted();
      this.file = file;
      this.info = file[1];
      this.selection = null;
      this.savedPosition = 0;
      this.message = "Reading media tracks...";
      this.changed();
      // 缺省选择交由服务端解析, 本地不计算 media_id 或 handle_id.
      await this.start(undefined, true, signal);
    });
  }

  /** 手动重试播放, 可先重新扫描列表路径. */
  retry(probe = false): void {
    this.enqueue(async (signal) => {
      if (!this.file) return;
      const initialized = this.selection !== null;
      if (probe) {
        const entry = await rescanMedia(this.file[0], signal);
        signal.throwIfAborted();
        if (!entry) throw new Error("Media file was removed");
        this.file = entry;
        this.info = entry[1];
        this.selection = null;
      }
      await this.start(initialized ? this.savedPosition : undefined, true, signal);
    });
  }

  /** 切换任一实际流并保留用户原来的播放或暂停意图. */
  select(selection: StreamSelection): void {
    if (!this.info || !this.file) return;
    // 在入队触发临时暂停前保存意图, 换轨后继续保持原来的播放或暂停状态.
    const playing = this.playRequested;
    this.enqueue(async (signal) => {
      this.selection = selection;
      await this.persist();
      await this.start(this.savedPosition, playing, signal);
    });
  }

  /** 在前端更新目标位置, 已覆盖范围直接 seek, 其余范围轮询等待. */
  seek(position: number): void {
    if (!this.info) return;
    if (!Number.isFinite(position)) {
      this.fail(new Error("Playback position must be finite"));
      return;
    }
    const target = Math.max(0, Math.min(position, this.duration - 0.1));
    // 已生成范围直接 seek, 网络缓冲由 HLS 负责, 不必重新创建会话.
    if (this.session && this.cache && this.loaded && !this.pending
      && rangeReady(target, this.cache.buffered_until)) {
      const sessionId = this.session.session_id;
      this.video.currentTime = target;
      this.savedPosition = target;
      this.persist().catch((error) => {
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
          await this.waitForRange(target, playing, signal);
        } else { await this.start(target, playing, signal); }
      });
    }
  }

  /** 切换用户播放意图, 没有活动会话时重新创建播放. */
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

  /** 保存当前位置并释放当前会话, 不停止共享缓存生成. */
  stop(): void {
    this.enqueue(async () => {
      this.playRequested = false;
      await this.persist();
      await this.detach();
      this.message = "Stopped";
    });
  }

  /** 向界面报告可处理的失败, 不把取消操作显示为错误. */
  fail(error: unknown): void {
    if (isAbort(error)) return;
    console.error(error);
    this.error = error instanceof Error ? error.message : String(error);
    this.message = "Playback needs attention";
    this.changed();
  }

  /** 浏览器实际使用的视频元素. */
  private video: HTMLVideoElement;
  /** 当前 HLS 播放引擎. */
  private adapter: MediaAdapter;
  /** 取消被后续用户操作替代的等待. */
  private operation = new AbortController();
  /** 保证会话清理和用户操作按顺序执行. */
  private queue: Promise<void> = Promise.resolve();
  /** 重置媒体元素或等待转码期间保留的本地绝对位置. */
  private savedPosition = 0;
  /** 浏览器媒体是否已完成挂载. */
  private loaded = false;
  /** 独立于临时暂停的用户播放意图. */
  private playRequested = false;
  /** 防止普通状态轮询重叠. */
  private reporting = false;
  /** 串行保存完整播放配置的队列. */
  private saving: Promise<void> = Promise.resolve();

  /** 终止加载并释放会话, 保留错误等待手动重试. */
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
    this.cache = null;
    this.queue = this.queue.then(async () => {
      try {
        await Promise.all([this.persist(), session ? releasePlayback(session.session_id) : Promise.resolve()]);
      } catch (cleanupError) {
        console.error("Playback cleanup failed", cleanupError);
        this.error += `\nSession cleanup failed: ${String(cleanupError)}`;
        this.changed();
      }
    });
  }

  /** 通知控件同步当前状态. */
  private changed(): void { this.dispatchEvent(new Event("change")); }

  /** 新操作取消旧等待, 但按顺序清理旧会话以防多个会话接管播放器. */
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

  /** 创建会话并接收最新媒体配置, 等待位置不进入 PlaybackQuery. */
  private async start(position: number | undefined, autoplay: boolean, signal: AbortSignal): Promise<void> {
    if (!this.file) throw new Error("Select a video first");
    await this.detach();
    signal.throwIfAborted();
    this.savedPosition = position ?? 0;
    this.playRequested = autoplay;
    this.message = "Preparing stream...";
    this.changed();
    const path = this.file[0];
    const query: PlaybackQuery = { path, ...this.selection };
    // 已保存实际视频流说明用户曾完成选轨, 显式传递 null 才能保持关闭偏好.
    if (!this.selection && this.file[1].configs.video_stream_index !== null) {
      query.video_stream_index = this.file[1].configs.video_stream_index;
      query.audio_stream_index = this.file[1].configs.audio_stream_index;
      query.subtitle_stream_index = this.file[1].configs.subtitle_stream_index;
    }
    const response = await createPlayback(query);
    if (signal.aborted) { await releasePlayback(response.session_id); signal.throwIfAborted(); }
    const { media, cache, session_id, url, video_stream_index, audio_stream_index, subtitle_stream_index } = response;
    this.info = media;
    this.file = [path, media];
    this.selection = { video_stream_index, audio_stream_index, subtitle_stream_index };
    const saved = media.configs.position ?? 0;
    const target = position ?? (saved < this.duration - 2 ? saved : 0);
    this.savedPosition = Math.max(0, Math.min(target, this.duration - 0.1));
    this.session = { session_id, url };
    this.cache = cache;
    await this.waitForRange(this.savedPosition, autoplay, signal);
  }

  /** 只在目标进入已发布的半开区间后挂载 HLS, 等待期间不创建新会话. */
  private async waitForRange(position: number, autoplay: boolean, signal: AbortSignal): Promise<void> {
    if (!this.session || !this.cache) throw new Error("Playback session is missing");
    const sessionId = this.session.session_id;
    let deadline = Date.now() + 180_000;
    let generated = this.cache.buffered_until;
    let activity = this.cache.phase;
    while (true) {
      signal.throwIfAborted();
      if (!this.session || !this.cache || this.session.session_id !== sessionId) throw new Error("Playback session changed");
      if (this.cache.phase === "stopping") throw new Error("Cache generation is stopping. Retry playback later.");
      if (rangeReady(position, this.cache.buffered_until)) break;
      if (this.cache.phase === "failed") throw new Error("Cache generation failed. Retry playback.");
      if (this.cache.phase === "completed") throw new Error("Cached video does not cover the requested position.");
      // 超时只限制持续无进展, 生成前进或调度状态变化都会刷新等待期限.
      if (this.cache.buffered_until > generated || this.cache.phase !== activity) {
        generated = this.cache.buffered_until;
        activity = this.cache.phase;
        deadline = Date.now() + 180_000;
      }
      if (Date.now() > deadline) throw new Error("Cache generation made no progress for 180 seconds. Retry later.");
      const phaseLabel = this.cache.phase === "running" ? "Generating cache"
        : "Preparing cache or waiting for a media process slot";
      this.message = `${phaseLabel}: ${formatTime(this.cache.buffered_until)} / ${formatTime(this.duration)}. Waiting for ${formatTime(position)}...`;
      this.changed();
      await wait(500, signal);
      const status = await getPlayback(sessionId, signal);
      signal.throwIfAborted();
      this.cache = status;
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

  /** 卸载媒体引擎并释放旧会话映射. */
  private async detach(keepalive = false): Promise<void> {
    const previous = this.session;
    this.session = null;
    this.cache = null;
    this.loaded = false;
    this.adapter.destroy();
    if (previous) await releasePlayback(previous.session_id, keepalive);
  }

  /** 捕获当前路径, 位置和流选择, 防止异步换片把旧配置写到新媒体. */
  private persist(keepalive = false): Promise<void> {
    if (!this.file || !this.info || !this.selection) return Promise.resolve();
    const path = this.file[0];
    const configs = { position: this.position, ...this.selection };
    this.info.configs = configs;
    // 普通保存串行发送, 卸载请求绕过队列以尽力在页面关闭前发出.
    if (keepalive) return saveConfigs(path, configs, true);
    const task = this.saving.catch((error) => console.error("Previous progress save failed", error))
      .then(() => saveConfigs(path, configs));
    this.saving = task;
    return task;
  }

  /** 刷新当前会话缓存快照, 丢弃已经被换片或 seek 替代的响应. */
  private async report(): Promise<void> {
    if (this.reporting || this.pending || !this.session) return;
    const sessionId = this.session.session_id;
    this.reporting = true;
    try {
      const status = await getPlayback(sessionId);
      // 响应返回前可能已换轨或进入拖动等待, 旧响应不能覆盖当前操作状态.
      if (this.session?.session_id !== sessionId || this.pending) return;
      this.cache = status;
      if (status.phase === "failed") {
        if (!rangeReady(this.position, status.buffered_until)) throw new Error("Cache generation failed. Retry playback.");
        this.message = "Playing cached range. Cache generation stopped.";
      }
      if (status.phase === "stopping") throw new Error("Cache generation is stopping. Retry playback later.");
      this.changed();
    } catch (error) {
      if (this.session?.session_id === sessionId && !this.pending) this.fatal(error);
    } finally { this.reporting = false; }
  }
}
