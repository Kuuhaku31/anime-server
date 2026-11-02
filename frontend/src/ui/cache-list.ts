
// frontend/src/ui/cache-list.ts
// 展示可展开的缓存文件夹摘要, 状态和管理操作.

import type { CacheStore } from "../state/cache-store.ts";
import type { CacheEntry, MediaInfo } from "../state/player-state.ts";
import { cacheProgress } from "../playback/timeline.ts";
import { element } from "./controls.ts";

/** 根据缓存状态和源文件可用性决定按钮边界, 不让播放隐式触发构建. */
export function cacheActions(entry: CacheEntry): Record<"play" | "retry" | "stop" | "delete", boolean> {
  return {
    play: entry.source_path !== null && entry.status.buffered_until > 0
      && !["loading", "stopping"].includes(entry.status.phase),
    retry: entry.source_path !== null && ["ready", "failed"].includes(entry.status.phase),
    stop: ["running", "stopping"].includes(entry.status.phase),
    delete: true,
  };
}

/** 将共享缓存 Map 渲染为可独立展开的缓存文件夹列表. */
export class CacheList {
  /** 绑定缓存管理按钮和明确的删除确认. */
  constructor(store: CacheStore, play: (handleId: string) => void, media: (path: string | null) => MediaInfo | null) {
    this.store = store;
    this.play = play;
    this.media = media;
    store.addEventListener("change", () => this.render());
    this.container.addEventListener("click", (event) => this.click(event));
    element<HTMLButtonElement>("#cache-delete-cancel").onclick = () => this.dialog.close();
    element<HTMLButtonElement>("#cache-delete-confirm").onclick = () => {
      const handleId = this.deleting;
      this.deleting = null;
      this.dialog.close();
      if (handleId) void store.remove(handleId).catch((error) => console.error("Cache deletion failed", error));
    };
    element<HTMLButtonElement>("#cache-refresh").onclick = () => {
      void store.refresh().catch((error) => console.error("Cache refresh failed", error));
    };
    this.render();
  }

  /** 高亮当前播放器引用的句柄, 不改变缓存任务状态. */
  select(handleId: string | null): void {
    if (handleId === this.active) return;
    this.active = handleId;
    this.render();
  }

  /** 使用库中现有时长展示生成百分比, 未知时长不编造百分比. */
  render(): void {
    element("#cache-count").textContent = String(this.store.entries.size);
    element("#cache-error").textContent = this.store.error;
    const total = this.store.resources;
    element("#cache-resources").textContent =
      `Workers ${total.encoder_slots} / processes ${total.process_slots} / FFmpeg threads ${total.ffmpeg_threads}`;
    const entries = [...this.store.entries.values()].sort((left, right) => left.handle_id.localeCompare(right.handle_id));
    const visible = new Set(entries.map((entry) => entry.handle_id));
    this.expanded = new Set([...this.expanded].filter((handleId) => visible.has(handleId)));
    const rows = entries.map((entry) => this.row(entry));
    this.container.replaceChildren(...rows);
    if (!rows.length) this.container.textContent = "No cache folders yet. Create one from a media file.";
  }

  /** 共享缓存状态的唯一数据源. */
  private store: CacheStore;
  /** 缓存列表播放回调. */
  private play: (handleId: string) => void;
  /** 查找已加载媒体信息的回调. */
  private media: (path: string | null) => MediaInfo | null;
  /** 任务列表容器. */
  private container = element<HTMLDivElement>("#caches");
  /** 删除确认对话框. */
  private dialog = element<HTMLDialogElement>("#cache-delete-dialog");
  /** 等待用户确认删除的稳定标识. */
  private deleting: string | null = null;
  /** 当前播放器引用的稳定标识. */
  private active: string | null = null;
  /** 多行独立的展开状态. */
  private expanded = new Set<string>();

  /** 处理行展开和仅在展开详情中出现的操作. */
  private click(event: Event): void {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const summary = target.closest<HTMLElement>("summary[data-handle-id]");
    if (summary && this.container.contains(summary)) {
      event.preventDefault();
      const handleId = summary.dataset.handleId;
      if (!handleId) return;
      if (this.expanded.has(handleId)) this.expanded.delete(handleId);
      else this.expanded.add(handleId);
      this.render();
      return;
    }
    const button = target.closest<HTMLButtonElement>("button[data-action]");
    if (!button || !this.container.contains(button) || button.disabled) return;
    const handleId = button.dataset.handleId;
    if (!handleId || this.store.pending.has(handleId)) return;
    const entry = this.store.entries.get(handleId);
    if (!entry) return;
    const action = button.dataset.action;
    if (action === "play" && cacheActions(entry).play) this.play(handleId);
    else if (action === "retry" && cacheActions(entry).retry) {
      void this.store.retry(handleId).catch((error) => console.error("Cache retry failed", error));
    } else if (action === "stop" && cacheActions(entry).stop) {
      void this.store.stop(handleId).catch((error) => console.error("Cache pause failed", error));
    } else if (action === "delete") {
      this.deleting = handleId;
      element("#cache-delete-name").textContent = entry.source_path ?? handleId;
      this.dialog.showModal();
    }
  }

  /** 创建一个紧凑缓存摘要及可选的完整状态详情. */
  private row(entry: CacheEntry): HTMLDetailsElement {
    const expanded = this.expanded.has(entry.handle_id);
    const row = document.createElement("details");
    row.className = `cache-row${entry.handle_id === this.active ? " active" : ""}`;
    row.dataset.handleId = entry.handle_id;
    row.open = expanded;

    const summary = document.createElement("summary");
    summary.dataset.handleId = entry.handle_id;
    const name = document.createElement("strong");
    name.textContent = entry.source_path?.split("/").at(-1) ?? "Source unavailable";
    name.title = entry.source_path ?? entry.handle_id;
    const identifier = document.createElement("code");
    identifier.textContent = entry.handle_id.slice(0, 12);
    identifier.title = entry.handle_id;
    const duration = this.media(entry.source_path)?.metadata.duration ?? 0;
    const progress = cacheProgress(entry.status, duration, entry.resources);
    const status = document.createElement("span");
    status.className = "cache-summary-state";
    status.dataset.phase = entry.status.phase;
    status.textContent = progress.text;
    const usage = entry.resources;
    const resources = document.createElement("span");
    resources.className = "cache-summary-resources";
    resources.textContent = `Worker ${usage.encoder_slots}/1 / ${usage.ffmpeg_threads} threads`;
    summary.append(name, identifier, status, resources);
    row.append(summary);
    if (expanded) row.append(this.details(entry, duration));
    return row;
  }

  /** 展示完整缓存身份, 生成状态和操作入口. */
  private details(entry: CacheEntry, duration: number): HTMLDivElement {
    const details = document.createElement("div");
    details.className = "cache-details";
    const identity = document.createElement("code");
    identity.textContent = entry.handle_id;
    identity.className = "cache-full-id";
    const source = document.createElement("p");
    source.className = "item-status";
    source.textContent = `Source: ${entry.source_path ?? "unavailable"}`;
    const spec = document.createElement("p");
    spec.className = "item-status";
    spec.textContent = `Media ${entry.spec.media_id} / version ${entry.spec.version}`;
    const tracks = document.createElement("p");
    tracks.className = "item-status";
    tracks.textContent = `Video ${entry.spec.video_stream_index} / Audio ${entry.spec.audio_stream_index ?? "off"} / Subtitles ${entry.spec.subtitle_stream_index ?? "off"}`;
    const status = document.createElement("p");
    status.className = "item-status";
    status.textContent = `State: ${entry.status.phase} / Published: ${entry.status.buffered_until.toFixed(2)}s${duration > 0 ? ` / Total: ${duration.toFixed(2)}s` : ""}`;
    const progress = cacheProgress(entry.status, duration, entry.resources);
    const bar = document.createElement("progress");
    bar.max = 100;
    if (duration > 0 || entry.status.phase === "completed") bar.value = progress.percent;
    bar.setAttribute("aria-label", progress.text);
    const usage = entry.resources;
    const resources = document.createElement("p");
    resources.className = "item-status";
    resources.textContent = `Encoder ${usage.encoder_slots}/1 / process ${usage.process_slots}/1 / FFmpeg threads ${usage.ffmpeg_threads}`;
    details.append(identity, source, spec, tracks, status, bar, resources);
    if (entry.status.error) {
      const diagnostic = document.createElement("p");
      diagnostic.className = "item-error";
      diagnostic.textContent = entry.status.error;
      details.append(diagnostic);
    }
    details.append(this.actions(entry));
    return details;
  }

  /** 创建具有明确可用条件的详情操作按钮. */
  private actions(entry: CacheEntry): HTMLDivElement {
    const actions = document.createElement("div");
    actions.className = "item-actions cache-actions";
    const enabled = cacheActions(entry);
    for (const action of ["play", "retry", "stop", "delete"] as const) {
      const button = document.createElement("button");
      button.type = "button";
      button.dataset.handleId = entry.handle_id;
      button.dataset.action = action;
      button.textContent = {
        play: "Play", retry: "Retry / resume", stop: "Pause encoding", delete: "Delete cache",
      }[action];
      button.disabled = this.store.pending.has(entry.handle_id) || !enabled[action];
      actions.append(button);
    }
    return actions;
  }
}
