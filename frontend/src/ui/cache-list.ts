
// frontend/src/ui/cache-list.ts
// 展示可展开的缓存文件夹摘要, 状态和管理操作.

import type { CacheStore } from "../state/cache-store.ts";
import type { CacheEntry } from "../state/player-state.ts";
import type { Library } from "./library.ts";
import { cacheProgress } from "../playback/timeline.ts";
import { element } from "./controls.ts";

/** 根据缓存状态和源文件可用性决定按钮边界, 不让播放隐式触发构建. */
export function cacheActions(entry: CacheEntry): CacheActions {
  const phase = entry.status.phase;
  return {
    play: entry.source_path !== null && entry.status.buffered_until > 0
      && !["loading", "stopping"].includes(phase),
    encoding: phase === "running" ? "stop"
      : entry.source_path !== null && ["ready", "failed"].includes(phase) ? "retry" : null,
    encodingLabel: ["running", "stopping"].includes(phase) ? "Pause encoding"
      : phase === "loading" ? "Preparing..." : phase === "completed" ? "Completed" : "Retry / resume",
    delete: true,
  };
}

/** 将共享缓存 Map 渲染为可独立展开的缓存文件夹列表. */
export class CacheList {
  /** 绑定缓存管理按钮和明确的删除确认. */
  constructor(store: CacheStore, play: (handleId: string) => void, library: Library) {
    this.store = store;
    this.play = play;
    this.library = library;
    store.addEventListener("change", () => this.render());
    library.addEventListener("change", () => this.render());
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
    for (const path of new Set(entries.map((entry) => entry.source_path))) {
      if (path) void this.library.ensureMetadata(path);
    }
  }

  /** 共享缓存状态的唯一数据源. */
  private store: CacheStore;
  /** 缓存列表播放回调. */
  private play: (handleId: string) => void;
  /** 媒体 metadata 的唯一内存数据源及加载入口. */
  private library: Library;
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
    const enabled = cacheActions(entry);
    if (action === "play" && enabled.play) this.play(handleId);
    else if (action === "encoding" && enabled.encoding) {
      void this.store[enabled.encoding](handleId).catch((error) => console.error("Cache encoding control failed", error));
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
    row.className = `cache-row${entry.status.phase === "completed" ? " completed" : ""}${entry.handle_id === this.active ? " active" : ""}`;
    row.dataset.handleId = entry.handle_id;
    row.dataset.phase = entry.status.phase;
    row.open = expanded;

    const summary = document.createElement("summary");
    summary.dataset.handleId = entry.handle_id;
    const name = document.createElement("strong");
    name.textContent = entry.source_path?.split("/").at(-1) ?? "Source unavailable";
    name.title = entry.source_path ?? entry.handle_id;
    const identifier = document.createElement("code");
    identifier.textContent = entry.handle_id.slice(0, 12);
    identifier.title = entry.handle_id;
    const duration = this.library.info(entry.source_path)?.metadata.duration ?? 0;
    const progress = cacheProgress(entry.status, duration, entry.resources);
    const status = document.createElement("span");
    status.className = "cache-summary-state";
    status.dataset.phase = entry.status.phase;
    status.textContent = progress.state;
    const usage = entry.resources;
    const resources = document.createElement("span");
    resources.className = "cache-summary-resources";
    resources.textContent = `${usage.encoder_slots} worker / ${usage.ffmpeg_threads} threads`;
    const meter = document.createElement("span");
    meter.className = "cache-summary-progress";
    const percentage = document.createElement("span");
    percentage.className = "cache-progress-percent";
    percentage.textContent = progress.determinate ? `${progress.percent.toFixed(1)}%` : "--";
    const timing = document.createElement("span");
    timing.className = "cache-progress-time";
    timing.textContent = progress.timing;
    const metadataError = this.library.metadataError(entry.source_path);
    timing.title = metadataError ?? progress.timing;
    const bar = document.createElement("progress");
    bar.max = 100;
    if (progress.determinate) bar.value = progress.percent;
    bar.setAttribute("aria-label", `${name.textContent}: ${progress.text}`);
    meter.append(percentage, timing, bar);
    summary.append(name, identifier, status, resources, meter);
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
    const usage = entry.resources;
    const resources = document.createElement("p");
    resources.className = "item-status";
    resources.textContent = `Encoder ${usage.encoder_slots}/1 / process ${usage.process_slots}/1 / FFmpeg threads ${usage.ffmpeg_threads}`;
    details.append(identity, source, spec, tracks, status, resources);
    const metadataError = this.library.metadataError(entry.source_path);
    if (metadataError) {
      const diagnostic = document.createElement("p");
      diagnostic.className = "item-error";
      diagnostic.textContent = `Source duration unavailable: ${metadataError}`;
      details.append(diagnostic);
    }
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
    for (const action of ["play", "encoding", "delete"] as const) {
      const button = document.createElement("button");
      button.type = "button";
      button.dataset.handleId = entry.handle_id;
      button.dataset.action = action;
      button.textContent = {
        play: "Play", encoding: enabled.encodingLabel, delete: "Delete cache",
      }[action];
      button.disabled = this.store.pending.has(entry.handle_id) || !enabled[action];
      actions.append(button);
    }
    return actions;
  }
}

/** 描述缓存操作权限和单个编码切换按钮的当前动作. */
interface CacheActions {
  /** 已发布内容是否允许播放. */
  play: boolean;
  /** 当前可执行的编码操作, null 表示不可切换. */
  encoding: "retry" | "stop" | null;
  /** 编码切换按钮对应当前阶段的文案. */
  encodingLabel: string;
  /** 是否允许用户显式删除缓存. */
  delete: boolean;
}
