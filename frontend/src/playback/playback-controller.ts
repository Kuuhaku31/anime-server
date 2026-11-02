

// frontend/src/playback/playback-controller.ts
// 协调稳定缓存句柄, 本地播放位置和浏览器媒体引擎.

import { getMediaMetadata, isAbort, playbackUrl, rescanMedia, saveConfigs, wait } from "../api/anime-server.ts";
import { CacheStore } from "../state/cache-store.ts";
import type { CacheEntry, CacheStatus, MediaInfo, PlaybackMedia, PlaybackQuery, StreamSelection } from "../state/player-state.ts";
import { MediaAdapter } from "./hls-adapter.ts";
import { absoluteTime, formatTime, rangeReady } from "./timeline.ts";

/** 播放器只维护本地操作, 构建和缓存管理由共享 CacheStore 执行. */
export class PlaybackController extends EventTarget {
  /** 当前选中的源路径和媒体信息. */
  file: PlaybackMedia | null = null;
  /** 最近一次重新探测得到的媒体信息. */
  info: MediaInfo | null = null;
  /** 当前选中缓存的稳定标识, 本地停止不会删除它. */
  handleId: string | null = null;
  /** 所有缓存及其服务端状态的唯一前端数据源. */
  readonly caches: CacheStore;
  /** 服务端确认的实际流选择. */
  selection: StreamSelection | null = null;
  /** 是否正在等待当前用户操作. */
  pending = false;
  /** 当前需要展示的播放错误. */
  error = "";
  /** 当前播放状态说明. */
  message = "Select a video from the library";

  /** 绑定浏览器事件, 并订阅共享缓存状态而不额外启动状态轮询. */
  constructor(video: HTMLVideoElement, caches = new CacheStore()) {
    super();
    this.video = video;
    this.caches = caches;
    this.adapter = new MediaAdapter(video);
    caches.addEventListener("change", () => this.cacheChanged());
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
        this.unload();
        this.message = "Ended";
      });
    });
    window.setInterval(() => {
      if (!this.pending && this.loaded && !video.paused) this.persist().catch((error) => this.fail(error));
    }, 5000);
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) this.persist(true).catch((error) => console.error("Progress save failed", error));
      else void this.report();
    });
    window.addEventListener("pagehide", () => {
      this.savedPosition = this.position;
      this.persist(true).catch((error) => console.error("Progress save failed", error));
      this.operation.abort();
      this.unload();
    });
    window.addEventListener("pageshow", (event) => {
      if (event.persisted && this.file) {
        this.pending = false;
        this.message = "Press Play to resume.";
        void this.report();
        this.changed();
      }
    });
  }

  /** 从共享 Map 获取当前状态, 不保存第二份会过期的缓存快照. */
  get cache(): CacheStatus | null {
    return this.handleId ? this.caches.entries.get(this.handleId)?.status ?? null : null;
  }
  /** 原媒体的完整时长. */
  get duration(): number { return this.info?.metadata.duration ?? 0; }
  /** 实际播放位置或当前尚未覆盖的本地目标. */
  get position(): number {
    return !this.pending && this.loaded && this.handleId
      ? absoluteTime(this.video.currentTime, 0, this.duration) : this.savedPosition;
  }
  /** 浏览器下载缓冲末端, 不等同于服务端生成进度. */
  get buffered(): number {
    if (!this.handleId || !this.loaded) return 0;
    const ranges = this.video.buffered;
    return ranges.length ? Math.min(this.duration, ranges.end(ranges.length - 1)) : 0;
  }

  /** 文件列表点播先发送 PlaybackQuery 构建, 然后通过句柄加载资源. */
  open(file: PlaybackMedia): void {
    this.enqueue(async (signal) => {
      await this.persist();
      this.unload();
      this.handleId = null;
      signal.throwIfAborted();
      this.file = file;
      this.info = file[1];
      this.selection = null;
      this.savedPosition = 0;
      await this.build(undefined, true, signal);
    });
  }

  /** 缓存列表播放只解析源媒体和读取已有资源, 不提交任何构建请求. */
  openCached(handleId: string): void {
    this.enqueue(async (signal) => {
      await this.persist();
      this.unload();
      this.handleId = null;
      await this.caches.refresh();
      signal.throwIfAborted();
      const entry = this.requireCache(handleId);
      if (!entry.source_path) throw new Error("Cache source media is unavailable");
      const media = await rescanMedia(entry.source_path, signal);
      signal.throwIfAborted();
      if (!media) throw new Error("Cache source media was removed");
      const info = await getMediaMetadata(media.file_id, signal);
      signal.throwIfAborted();
      await this.caches.refresh();
      signal.throwIfAborted();
      const current = this.requireCache(handleId);
      if (current.source_path !== media.path) throw new Error("Cache source changed. Refresh the cache list.");
      this.file = [media.path, info];
      this.info = info;
      this.useCache(current);
      const saved = this.info.configs.position ?? 0;
      this.savedPosition = saved < this.duration - 2 ? saved : 0;
      await this.waitForRange(this.savedPosition, true, signal);
    });
  }

  /** 手动重试构建, 可先重新扫描源文件, 不自动循环重建失败任务. */
  retry(probe = false): void {
    this.enqueue(async (signal) => {
      if (!this.file) return;
      const initialized = this.selection !== null;
      if (probe) {
        const entry = await rescanMedia(this.file[0], signal);
        signal.throwIfAborted();
        if (!entry) throw new Error("Media file was removed");
        this.file = [entry.path, await getMediaMetadata(entry.file_id, signal)];
        this.info = this.file[1];
        this.selection = null;
      }
      await this.build(initialized ? this.savedPosition : undefined, true, signal);
    });
  }

  /** 换轨构建对应缓存, 保留小数位置和原来的播放意图. */
  select(selection: StreamSelection): void {
    if (!this.info || !this.file) return;
    const playing = this.playRequested;
    this.enqueue(async (signal) => {
      this.selection = selection;
      await this.persist();
      await this.build(this.savedPosition, playing, signal);
    });
  }

  /** seek 只修改本地目标, 不因未覆盖范围而重新构建. */
  seek(position: number): void {
    if (!this.info) return;
    if (!Number.isFinite(position)) { this.fail(new Error("Playback position must be finite")); return; }
    const target = Math.max(0, Math.min(position, this.duration - 0.1));
    if (this.handleId && this.cache && this.loaded && !this.pending && rangeReady(target, this.cache.buffered_until)) {
      const handleId = this.handleId;
      this.video.currentTime = target;
      this.savedPosition = target;
      this.persist().catch((error) => { if (this.handleId === handleId) this.fatal(error); });
      this.changed();
    } else {
      const playing = this.playRequested;
      this.enqueue(async (signal) => {
        this.savedPosition = target;
        await this.persist();
        this.unload();
        if (this.handleId) await this.waitForRange(target, playing, signal);
        else await this.build(target, playing, signal);
      });
    }
  }

  /** 本地停止后复用现有句柄, 不把 Play 隐式转换成转码重试. */
  toggle(): void {
    if (this.pending || !this.file) return;
    if (!this.loaded) {
      this.enqueue(async (signal) => {
        const target = this.savedPosition >= this.duration ? 0 : this.savedPosition;
        if (this.handleId) {
          await this.caches.refresh();
          signal.throwIfAborted();
          await this.waitForRange(target, true, signal);
        } else await this.build(target, true, signal);
      });
    } else if (this.video.paused) {
      this.playRequested = true;
      this.video.play().catch((error) => this.fatal(error));
    } else {
      this.playRequested = false;
      this.video.pause();
    }
  }

  /** Stop 只保存位置和卸载播放器, 不停止或删除共享转码任务. */
  stop(): void {
    this.enqueue(async () => {
      this.playRequested = false;
      await this.persist();
      this.unload();
      this.message = "Stopped";
    });
  }

  /** 展示失败, 被新操作替代的取消不显示为业务错误. */
  fail(error: unknown): void {
    if (isAbort(error)) return;
    console.error(error);
    this.error = error instanceof Error ? error.message : String(error);
    this.message = "Playback needs attention";
    this.changed();
  }

  /** 当前浏览器媒体元素. */
  private video: HTMLVideoElement;
  /** 当前 HLS 媒体引擎. */
  private adapter: MediaAdapter;
  /** 取消被后续操作替代的等待和媒体加载. */
  private operation = new AbortController();
  /** 串行执行播放器操作, 不限制独立缓存构建. */
  private queue: Promise<void> = Promise.resolve();
  /** 卸载或等待期间保留的绝对播放位置. */
  private savedPosition = 0;
  /** 是否已经挂载浏览器媒体. */
  private loaded = false;
  /** 独立于临时暂停的用户播放意图. */
  private playRequested = false;
  /** 串行保存完整播放配置的队列. */
  private saving: Promise<void> = Promise.resolve();

  /** 删除当前句柄或源路径失效时立即停止本地播放, 不静默重建. */
  private cacheChanged(): void {
    if (this.handleId) {
      const entry = this.caches.entries.get(this.handleId);
      if (!entry) { this.fatal(new Error("The current cache was deleted")); return; }
      if (!entry.source_path) { this.fatal(new Error("Cache source media is unavailable")); return; }
      if (!this.pending && this.loaded && entry.status.phase === "failed") {
        if (!rangeReady(this.position, entry.status.buffered_until)) {
          this.fatal(new Error(entry.status.error || "Cache generation failed")); return;
        }
        this.message = "Playing cached range. Cache generation stopped.";
      }
    }
    this.changed();
  }

  /** 解码或资源故障只卸载播放器, 不删除仍可由其他客户端使用的缓存. */
  private fatal(error: unknown): void {
    if (isAbort(error)) return;
    this.savedPosition = this.position;
    this.playRequested = false;
    this.operation.abort();
    this.unload();
    this.handleId = null;
    this.pending = false;
    this.fail(error);
    this.persist().catch((saveError) => this.fail(saveError));
  }

  /** 通知控件读取当前本地状态. */
  private changed(): void { this.dispatchEvent(new Event("change")); }

  /** 新用户操作取消旧等待, 旧构建结果仍保留在共享缓存 Map 中. */
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
          this.unload();
          this.fail(error);
        }
      } finally {
        if (!operation.signal.aborted) { this.pending = false; this.changed(); }
      }
    });
  }

  /** 构建请求只含路径与流选择, 响应迟到不能接管新的播放器操作. */
  private async build(position: number | undefined, autoplay: boolean, signal: AbortSignal): Promise<void> {
    if (!this.file) throw new Error("Select a video first");
    this.unload();
    this.handleId = null;
    signal.throwIfAborted();
    this.savedPosition = position ?? 0;
    this.playRequested = autoplay;
    this.message = "Building cache...";
    this.changed();
    const path = this.file[0];
    const query: PlaybackQuery = { path, ...this.selection };
    if (!this.selection && this.file[1].configs.video_stream_index !== null) {
      query.video_stream_index = this.file[1].configs.video_stream_index;
      query.audio_stream_index = this.file[1].configs.audio_stream_index;
      query.subtitle_stream_index = this.file[1].configs.subtitle_stream_index;
    }
    const response = await this.caches.build(query);
    signal.throwIfAborted();
    this.info = { metadata: response.media_meta, configs: response.media_configs };
    this.file = [path, this.info];
    this.useCache(this.requireCache(response.cache.handle_id));
    const saved = this.info.configs.position ?? 0;
    const target = position ?? (saved < this.duration - 2 ? saved : 0);
    this.savedPosition = Math.max(0, Math.min(target, this.duration - 0.1));
    await this.waitForRange(this.savedPosition, autoplay, signal, true);
  }

  /** 接受服务端确认的稳定标识与三类实际流选择. */
  private useCache(entry: CacheEntry): void {
    this.handleId = entry.handle_id;
    const { video_stream_index, audio_stream_index, subtitle_stream_index } = entry.spec;
    this.selection = { video_stream_index, audio_stream_index, subtitle_stream_index };
  }

  /** 查找失败明确报错, 不用假状态掩盖并发删除. */
  private requireCache(handleId: string): CacheEntry {
    const entry = this.caches.entries.get(handleId);
    if (!entry) throw new Error("The requested cache was deleted");
    return entry;
  }

  /** 只有目标进入已发布范围才挂载 HLS, 资源请求仅包含句柄和文件名. */
  private async waitForRange(position: number, autoplay: boolean, signal: AbortSignal, building = false): Promise<void> {
    const handleId = this.handleId;
    if (!handleId) throw new Error("No cache selected");
    this.savedPosition = position;
    let deadline = Date.now() + 180_000;
    let generated = -1;
    let activity = "";
    while (true) {
      signal.throwIfAborted();
      if (this.handleId !== handleId) throw new Error("Playback cache changed");
      const entry = this.requireCache(handleId);
      if (!entry.source_path) throw new Error("Cache source media is unavailable");
      const cache = entry.status;
      if (cache.phase === "running") building = false;
      if (rangeReady(position, cache.buffered_until)) break;
      if (cache.phase === "failed") throw new Error(cache.error || "Cache generation failed. Retry the cache.");
      if (cache.phase === "completed") throw new Error("Cached video does not cover the requested position.");
      if (cache.phase === "stopping" || cache.phase === "ready" && !building) {
        throw new Error("Cache generation is stopped. Retry it from the cache list.");
      }
      if (cache.buffered_until > generated || cache.phase !== activity) {
        generated = cache.buffered_until;
        activity = cache.phase;
        deadline = Date.now() + 180_000;
      }
      if (Date.now() > deadline) throw new Error("Cache generation made no progress for 180 seconds. Retry later.");
      this.message = `Generating cache: ${formatTime(cache.buffered_until)} / ${formatTime(this.duration)}. Waiting for ${formatTime(position)}...`;
      this.changed();
      await wait(500, signal);
      await this.caches.refresh();
    }
    signal.throwIfAborted();
    await this.adapter.attach(playbackUrl(handleId), position, signal, (error) => this.fatal(error));
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
        } else throw error;
      }
    }
  }

  /** 卸载本地引擎但保留所选句柄, 不发送服务端删除请求. */
  private unload(): void {
    this.loaded = false;
    this.adapter.destroy();
  }

  /** 捕获路径和配置, 防止换片期间把旧位置写到新媒体. */
  private persist(keepalive = false): Promise<void> {
    if (!this.file || !this.info || !this.selection) return Promise.resolve();
    const path = this.file[0];
    const configs = { position: this.position, ...this.selection };
    this.info.configs = configs;
    if (keepalive) return saveConfigs(path, configs, true);
    const task = this.saving.catch((error) => console.error("Previous progress save failed", error))
      .then(() => saveConfigs(path, configs));
    this.saving = task;
    return task;
  }

  /** 恢复可见页面时通过共享仓库刷新, 避免播放器拥有第二条轮询链. */
  private async report(): Promise<void> {
    try { await this.caches.refresh(); }
    catch (error) { this.fail(error); }
  }
}
