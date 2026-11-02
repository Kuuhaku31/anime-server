
// frontend/src/state/cache-store.ts
// 统一维护按 handle_id 索引的内存缓存列表和后台轮询.

import { buildCache, deleteCache, listCache, retryCache, stopCache } from "../api/anime-server.ts";
import type { CacheBuild, CacheEntry, PlaybackQuery, WorkerResources } from "./player-state.ts";

/** 为缓存列表与播放器提供同一份服务端快照, 不写入 localStorage. */
export class CacheStore extends EventTarget {
  /** 当前服务端存在的缓存记录. */
  readonly entries = new Map<string, CacheEntry>();
  /** 正在执行显式操作的句柄, 防止重复点击. */
  readonly pending = new Set<string>();
  /** 最近一次刷新或操作失败, 不使用空列表掩盖错误. */
  error = "";

  /** 汇总当前快照中的 worker 占用, 构建和删除后也立即更新. */
  get resources(): WorkerResources {
    const total = { encoder_slots: 0, process_slots: 0, ffmpeg_threads: 0 };
    for (const entry of this.entries.values()) {
      total.encoder_slots += entry.status.resources.encoder_slots;
      total.process_slots += entry.status.resources.process_slots;
      total.ffmpeg_threads += entry.status.resources.ffmpeg_threads;
    }
    return total;
  }

  /** 首次拉取后开启唯一轮询, 稳定缓存仍周期同步其他客户端的删除. */
  async start(): Promise<void> {
    if (this.timer !== null) return;
    this.timer = window.setInterval(() => {
      if (document.hidden) return;
      const active = [...this.entries.values()].some((entry) =>
        ["loading", "running", "stopping"].includes(entry.status.phase));
      if (active || Date.now() - this.refreshedAt >= 5000) {
        void this.refresh().catch((error) => console.error("Cache polling failed", error));
      }
    }, 1000);
    await this.refresh();
  }

  /** 停止轮询, 不停止服务端转码或删除缓存. */
  dispose(): void {
    if (this.timer !== null) window.clearInterval(this.timer);
    this.timer = null;
  }

  /** 合并并发刷新, 只让操作开始之后仍有效的整表响应替换 Map. */
  refresh(): Promise<void> {
    if (this.refreshing) return this.refreshing;
    const revision = this.revision;
    this.refreshing = listCache().then((response) => {
      if (revision !== this.revision) return;
      this.entries.clear();
      for (const entry of response.entries) this.entries.set(entry.handle_id, entry);
      this.refreshedAt = Date.now();
      this.error = "";
      this.changed();
    }).catch((error: unknown) => {
      this.failed(error);
      throw error;
    }).finally(() => { this.refreshing = null; });
    return this.refreshing;
  }

  /** 构建独立于播放器操作, 被换片替代的结果仍属于共享缓存列表. */
  async build(query: PlaybackQuery): Promise<CacheBuild> {
    const revision = ++this.revision;
    try {
      const response = await buildCache(query);
      if (revision >= (this.handleRevisions.get(response.cache.handle_id) ?? 0)) this.update(response.cache);
      else {
        // 构建响应可能晚于删除, 此时重新查询而不是用旧响应复活条目.
        await this.refreshing;
        await this.refresh();
      }
      return response;
    } catch (error) {
      this.failed(error);
      throw error;
    }
  }

  /** 重新构建已有句柄, 只更新列表而不自动切换播放器. */
  async retry(handleId: string): Promise<CacheBuild> {
    return this.mutate(handleId, async () => {
      const response = await retryCache(handleId);
      this.update(response.cache);
      return response;
    });
  }

  /** 停止转码, 已发布前缀继续保留在列表中. */
  async stop(handleId: string): Promise<void> {
    await this.mutate(handleId, async () => { this.update(await stopCache(handleId)); });
  }

  /** 立即删除共享缓存, change 事件通知当前播放器解除引用. */
  async remove(handleId: string): Promise<void> {
    await this.mutate(handleId, async () => {
      await deleteCache(handleId);
      this.entries.delete(handleId);
      this.error = "";
      this.handleRevisions.set(handleId, ++this.revision);
      this.changed();
    });
  }

  /** 当前自动刷新计时器. */
  private timer: number | null = null;
  /** 最近一次成功应用整表快照的时刻. */
  private refreshedAt = 0;
  /** 合并并发列表查询的 Promise. */
  private refreshing: Promise<void> | null = null;
  /** 显式操作使较早的整表响应失效. */
  private revision = 0;
  /** 仅同一句柄的新操作能使旧构建响应失效, 不丢弃其他并行构建. */
  private handleRevisions = new Map<string, number>();

  /** 原子应用单个显式操作响应并拒绝之前的列表结果. */
  private update(entry: CacheEntry): void {
    this.handleRevisions.set(entry.handle_id, ++this.revision);
    this.entries.set(entry.handle_id, entry);
    this.error = "";
    this.changed();
  }

  /** 串行化同一条目的修改, 失败时保留旧快照并显式通知 UI. */
  private async mutate<T>(handleId: string, action: () => Promise<T>): Promise<T> {
    if (this.pending.has(handleId)) throw new Error("A cache operation is already in progress");
    this.pending.add(handleId);
    this.handleRevisions.set(handleId, ++this.revision);
    this.changed();
    try { return await action(); }
    catch (error) { this.failed(error); throw error; }
    finally {
      this.pending.delete(handleId);
      this.handleRevisions.set(handleId, ++this.revision);
      this.changed();
    }
  }

  /** 保存请求错误并通知列表, 自动轮询不会静默显示过期状态为最新数据. */
  private failed(error: unknown): void {
    console.error("Cache request failed", error);
    this.error = error instanceof Error ? error.message : String(error);
    this.changed();
  }

  /** 通知列表和播放器读取同一份 Map. */
  private changed(): void { this.dispatchEvent(new Event("change")); }
}
