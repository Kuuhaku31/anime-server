
// frontend/src/ui/cache-list.ts
// 展示稳定句柄缓存列表, worker 资源和独立管理操作.

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

/** 将共享缓存 Map 渲染为独立于视频文件列表的任务面板. */
export class CacheList {
  /** 绑定缓存管理按钮和明确的删除确认, 所有动态文本使用 textContent. */
  constructor(store: CacheStore, play: (handleId: string) => void, media: (path: string | null) => MediaInfo | null) {
    this.store = store;
    this.media = media;
    store.addEventListener("change", () => this.render());
    this.container.addEventListener("click", (event) => {
      const target = event.target;
      if (!(target instanceof HTMLButtonElement)) return;
      const handleId = target.dataset.handleId;
      if (!handleId || target.disabled) return;
      const entry = store.entries.get(handleId);
      if (!entry || store.pending.has(handleId)) return;
      const action = target.dataset.action;
      if (action === "play" && cacheActions(entry).play) play(handleId);
      else if (action === "retry" && cacheActions(entry).retry) {
        void store.retry(handleId).catch((error) => console.error("Cache retry failed", error));
      } else if (action === "stop" && cacheActions(entry).stop) {
        void store.stop(handleId).catch((error) => console.error("Cache stop failed", error));
      } else if (action === "delete") {
        this.deleting = handleId;
        element("#cache-delete-name").textContent = entry.source_path ?? handleId;
        this.dialog.showModal();
      }
    });
    element<HTMLButtonElement>("#cache-delete-cancel").onclick = () => this.dialog.close();
    element<HTMLButtonElement>("#cache-delete-confirm").onclick = () => {
      const handleId = this.deleting;
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
    const rows = [...this.store.entries.values()].sort((left, right) => left.handle_id.localeCompare(right.handle_id))
      .map((entry) => this.row(entry));
    this.container.replaceChildren(...rows);
    if (!rows.length) this.container.textContent = "No caches yet. Build one from the video files.";
  }

  /** 列表共用的内存数据源. */
  private store: CacheStore;
  /** 查找已经探测过的源媒体信息. */
  private media: (path: string | null) => MediaInfo | null;
  /** 任务列表容器. */
  private container = element<HTMLDivElement>("#caches");
  /** 删除确认对话框. */
  private dialog = element<HTMLDialogElement>("#cache-delete-dialog");
  /** 等待用户确认删除的稳定标识. */
  private deleting: string | null = null;
  /** 当前播放器引用的稳定标识. */
  private active: string | null = null;

  /** 为一个句柄创建安全文本和具有明确边界的操作按钮. */
  private row(entry: CacheEntry): HTMLDivElement {
    const row = document.createElement("div");
    row.className = `cache-row${entry.handle_id === this.active ? " active" : ""}`;
    row.dataset.handleId = entry.handle_id;
    const name = document.createElement("strong");
    name.textContent = entry.source_path?.split("/").at(-1) ?? "Source unavailable";
    name.title = entry.source_path ?? entry.handle_id;
    const identity = document.createElement("code");
    identity.textContent = entry.handle_id;
    const selection = document.createElement("span");
    const spec = entry.spec;
    selection.textContent = `Video ${spec.video_stream_index} / Audio ${spec.audio_stream_index ?? "off"} / Subtitles ${spec.subtitle_stream_index ?? "off"}`;
    const duration = this.media(entry.source_path)?.metadata.duration ?? 0;
    const progress = cacheProgress(entry.status, duration);
    const state = document.createElement("span");
    state.textContent = progress.text;
    state.dataset.phase = entry.status.phase;
    const bar = document.createElement("progress");
    bar.max = 100;
    if (duration > 0 || entry.status.phase === "completed") bar.value = progress.percent;
    bar.setAttribute("aria-label", progress.text);
    const resources = document.createElement("span");
    const usage = entry.status.resources;
    resources.textContent = `Encoder ${usage.encoder_slots}/1 / process ${usage.process_slots}/1 / threads ${usage.ffmpeg_threads}`;
    const actions = document.createElement("div");
    actions.className = "actions cache-actions";
    const enabled = cacheActions(entry);
    for (const action of ["play", "retry", "stop", "delete"] as const) {
      const button = document.createElement("button");
      button.type = "button";
      button.dataset.handleId = entry.handle_id;
      button.dataset.action = action;
      button.textContent = { play: "Play", retry: "Retry / resume", stop: "Stop build", delete: "Delete" }[action];
      button.disabled = this.store.pending.has(entry.handle_id) || !enabled[action];
      actions.append(button);
    }
    row.append(name, identity, selection, state, bar, resources);
    if (entry.status.error) {
      const error = document.createElement("span");
      error.className = "cache-diagnostic";
      error.textContent = entry.status.error;
      row.append(error);
    }
    row.append(actions);
    return row;
  }
}
