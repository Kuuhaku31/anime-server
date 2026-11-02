
// frontend/src/ui/library.ts
// 展示路径和媒体信息列表, 并将点播动作交给播放控制器.

import { listMedia, rescanMedia, scanMedia } from "../api/anime-server.ts";
import type { MediaEntry, MediaInfo } from "../state/player-state.ts";
import { formatTime } from "../playback/timeline.ts";
import { element } from "./controls.ts";

/** 用容器内路径区分同内容媒体的不同副本. */
export function fileAddress(file: MediaEntry): string {
  return file[0];
}

/** 从容器路径取得显示名称, 不在 API 中维护冗余文件名. */
export function fileName(file: MediaEntry): string {
  return file[0].split("/").at(-1) ?? file[0];
}

/** 按当前模型中的字段排序, 相同值按完整路径稳定排序. */
export function sortFiles(files: MediaEntry[], order: SortOrder): MediaEntry[] {
  const compareText = (left: string, right: string): number => left.localeCompare(right, undefined, { numeric: true });
  return [...files].sort((left, right) => {
    let result = 0;
    if (order === "name-asc") result = compareText(fileName(left), fileName(right));
    else if (order === "name-desc") result = compareText(fileName(right), fileName(left));
    else if (order === "duration-asc") result = left[1].metadata.duration - right[1].metadata.duration;
    else if (order === "duration-desc") result = right[1].metadata.duration - left[1].metadata.duration;
    else if (order === "progress-desc") {
      result = (right[1].configs.position ?? 0) / right[1].metadata.duration
        - (left[1].configs.position ?? 0) / left[1].metadata.duration;
    }
    return result || compareText(left[0], right[0]);
  });
}

/** 管理媒体列表加载, 排序和按路径选中状态. */
export class Library extends EventTarget {
  /** 绑定列表事件和持久化排序选项. */
  constructor(open: (file: MediaEntry) => void, fail: (error: unknown) => void, build?: (file: MediaEntry) => Promise<void>) {
    super();
    this.open = open;
    this.build = build;
    const sort = element<HTMLSelectElement>("#files-sort");
    try {
      const saved = localStorage.getItem(SORT_KEY);
      if (isSortOrder(saved)) this.sort = saved;
    } catch (error) { fail(error); }
    sort.value = this.sort;
    sort.onchange = () => {
      if (!isSortOrder(sort.value)) {
        fail(new Error("Invalid library sort option"));
        return;
      }
      this.sort = sort.value;
      try { localStorage.setItem(SORT_KEY, this.sort); }
      catch (error) { fail(error); }
      this.render();
    };
    this.container.addEventListener("click", (event) => {
      const target = event.target;
      if (!(target instanceof HTMLButtonElement)) return;
      const file = this.files.find((file) => file[0] === target.dataset.path);
      if (!file) return;
      if (target.dataset.action === "open") this.open(file);
      else if (target.dataset.action === "build" && this.build) {
        const path = file[0];
        if (this.building.has(path)) return;
        this.building.add(path);
        this.render();
        this.build(file).catch(fail).finally(() => { this.building.delete(path); this.render(); });
      }
      else if (target.dataset.action === "scan") {
        const path = file[0];
        if (this.scanning.has(path)) return;
        this.scanning.add(path);
        target.disabled = true;
        target.textContent = "Scanning...";
        rescanMedia(path).then(() => this.load()).catch(fail).finally(() => {
          this.scanning.delete(path);
          for (const button of this.container.querySelectorAll<HTMLButtonElement>('button[data-action="scan"]')) {
            if (button.dataset.path === path) {
              button.disabled = false;
              button.textContent = "Rescan";
            }
          }
        });
      }
    });
    const scan = element<HTMLButtonElement>("#scan");
    scan.onclick = async () => {
      scan.disabled = true;
      try { await scanMedia(); await this.load(); }
      catch (error) { fail(error); }
      finally { scan.disabled = false; }
    };
  }

  /** 获取最新路径列表, 使用已有排序和选中状态重绘. */
  async load(): Promise<void> {
    this.files = await listMedia();
    this.render();
    this.dispatchEvent(new Event("change"));
  }

  /** 为缓存列表提供已加载的源媒体时长, 不在轮询中重复探测. */
  info(path: string | null): MediaInfo | null {
    return this.files.find((file) => file[0] === path)?.[1] ?? null;
  }

  /** 播放位置变化时仅更新高亮, 不重建整张列表. */
  select(path: string | null): void {
    if (path === this.active) return;
    this.active = path;
    for (const row of this.container.querySelectorAll<HTMLElement>(".file-row")) {
      row.classList.toggle("active", row.dataset.filePath === path);
    }
  }

  /** 文件列表的 DOM 容器. */
  private container = element<HTMLDivElement>("#files");
  /** 当前路径和媒体信息列表. */
  private files: MediaEntry[] = [];
  /** 交由播放控制器执行的打开动作. */
  private open: (file: MediaEntry) => void;
  /** 不切换播放器的独立缓存构建操作. */
  private build?: (file: MediaEntry) => Promise<void>;
  /** 当前高亮的媒体路径. */
  private active: string | null = null;
  /** 正在单文件扫描的路径, 防止重复提交. */
  private scanning = new Set<string>();
  /** 正在提交构建的路径, 防止重复点击. */
  private building = new Set<string>();
  /** 当前使用的本地排序选项. */
  private sort: SortOrder = "path";

  /** 使用文本节点展示路径元数据, 不把媒体文件名拼接为 HTML. */
  private render(): void {
    element("#files-count").textContent = String(this.files.length);
    const rows = sortFiles(this.files, this.sort).map((file) => {
      const [path, media] = file;
      const row = document.createElement("div");
      row.className = `file-row${path === this.active ? " active" : ""}`;
      row.dataset.filePath = path;
      const info = document.createElement("div");
      info.className = "file-info";
      const name = document.createElement("strong");
      name.textContent = fileName(file);
      name.title = path;
      const detail = document.createElement("span");
      detail.textContent = `${formatTime(media.metadata.duration)} / ${media.metadata.format}`;
      info.append(name, detail);
      const actions = document.createElement("div");
      actions.className = "actions";
      const buttons = [["open", "Play"], ...(this.build ? [["build", "Build"]] : []), ["scan", "Rescan"]];
      for (const [action, label] of buttons) {
        const button = document.createElement("button");
        button.type = "button";
        button.dataset.path = path;
        button.dataset.action = action;
        button.textContent = label;
        if (action === "open") button.title = "Build or reuse a cache, then play";
        if (action === "build") {
          button.title = "Build a cache without changing playback";
          button.disabled = this.building.has(path);
          if (button.disabled) button.textContent = "Building...";
        }
        if (action === "scan") {
          button.title = "Rehash this file path and update the database";
          button.disabled = this.scanning.has(path);
          if (button.disabled) button.textContent = "Scanning...";
        }
        actions.append(button);
      }
      row.append(info, actions);
      return row;
    });
    this.container.replaceChildren(...rows);
    if (!rows.length) this.container.textContent = "No videos found. Check the media mount and scan again.";
  }
}

/** 浏览器中保存列表排序的存储键. */
const SORT_KEY = "anime-server.library-sort";
/** 只提供新媒体模型能够支持的排序方式. */
const SORT_OPTIONS = ["path", "name-asc", "name-desc", "duration-desc", "duration-asc", "progress-desc"] as const;
/** 限制列表排序选项的有效值. */
type SortOrder = typeof SORT_OPTIONS[number];

/** 旧版本已移除的排序字段使用默认路径排序. */
function isSortOrder(value: string | null): value is SortOrder {
  return SORT_OPTIONS.some((option) => option === value);
}
