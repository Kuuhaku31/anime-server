
// frontend/src/ui/library.ts
// 展示可展开的媒体文件配置, 仅通过明确操作创建缓存.

import { listMedia, rescanMedia, scanMedia } from "../api/anime-server.ts";
import type { MediaEntry, MediaInfo, PlaybackQuery, StreamInfo, StreamSelection } from "../state/player-state.ts";
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

/** 根据已保存配置或各类别首条流生成初始选择. */
export function initialSelection(info: MediaInfo): StreamSelection | null {
  const { configs, metadata } = info;
  const video = configs.video_stream_index ?? metadata.video_streams[0]?.stream_index;
  if (video === undefined) return null;
  if (configs.video_stream_index !== null) {
    return {
      video_stream_index: video,
      audio_stream_index: configs.audio_stream_index,
      subtitle_stream_index: configs.subtitle_stream_index,
    };
  }
  return {
    video_stream_index: video,
    audio_stream_index: metadata.audio_streams[0]?.stream_index ?? null,
    subtitle_stream_index: metadata.subtitle_streams[0]?.stream_index ?? null,
  };
}

/** 管理媒体列表加载, 多行展开, 流选择和按路径缓存创建. */
export class Library extends EventTarget {
  /** 绑定列表事件, 排序偏好和缓存构建回调. */
  constructor(build: (query: PlaybackQuery) => Promise<unknown>, fail: (error: unknown) => void) {
    super();
    this.build = build;
    this.fail = fail;
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
    this.container.addEventListener("click", (event) => this.click(event));
    this.container.addEventListener("change", (event) => this.change(event));
    const scan = element<HTMLButtonElement>("#scan");
    scan.onclick = async () => {
      scan.disabled = true;
      try { await scanMedia(); await this.load(); }
      catch (error) { fail(error); }
      finally { scan.disabled = false; }
    };
  }

  /** 获取最新路径列表, 保留仍然存在路径的展开和选择状态. */
  async load(): Promise<void> {
    this.files = await listMedia();
    const paths = new Set(this.files.map(([path]) => path));
    this.expanded = new Set([...this.expanded].filter((path) => paths.has(path)));
    this.selections = new Map([...this.selections].filter(([path]) => paths.has(path)));
    this.errors = new Map([...this.errors].filter(([path]) => paths.has(path)));
    for (const [path, info] of this.files) {
      const current = this.selections.get(path);
      if (!this.selections.has(path) || !validSelection(info, current)) {
        this.selections.set(path, initialSelection(info));
      }
    }
    this.render();
    this.dispatchEvent(new Event("change"));
  }

  /** 为缓存列表提供已加载的源媒体时长, 不在轮询中重复探测. */
  info(path: string | null): MediaInfo | null {
    return this.files.find((file) => file[0] === path)?.[1] ?? null;
  }

  /** 当前媒体列表的 DOM 容器. */
  private container = element<HTMLDivElement>("#files");
  /** 当前路径和媒体信息列表. */
  private files: MediaEntry[] = [];
  /** 显示缓存构建请求的回调. */
  private build: (query: PlaybackQuery) => Promise<unknown>;
  /** 展示失败操作的回调. */
  private fail: (error: unknown) => void;
  /** 当前高亮的播放源路径. */
  private active: string | null = null;
  /** 多行独立的展开状态. */
  private expanded = new Set<string>();
  /** 每条路径当前编辑的流选择. */
  private selections = new Map<string, StreamSelection | null>();
  /** 正在单文件扫描的路径. */
  private scanning = new Set<string>();
  /** 正在创建缓存的路径. */
  private building = new Set<string>();
  /** 各路径最新一次操作的明确诊断. */
  private errors = new Map<string, string>();
  /** 当前使用的本地排序选项. */
  private sort: SortOrder = "path";

  /** 处理折叠, 展开和媒体项内的显式操作. */
  private click(event: Event): void {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const summary = target.closest<HTMLElement>("summary[data-path]");
    if (summary && this.container.contains(summary)) {
      event.preventDefault();
      const path = summary.dataset.path;
      if (!path) return;
      if (this.expanded.has(path)) this.expanded.delete(path);
      else this.expanded.add(path);
      this.render();
      return;
    }
    const button = target.closest<HTMLButtonElement>("button[data-action]");
    if (!button || !this.container.contains(button)) return;
    const path = button.dataset.path;
    if (!path || button.disabled) return;
    const file = this.files.find(([address]) => address === path);
    if (!file) return;
    if (button.dataset.action === "build") void this.createCache(file);
    else if (button.dataset.action === "scan") void this.rescan(path);
  }

  /** 保存展开详情中的临时流选择, 不写入观看配置. */
  private change(event: Event): void {
    const target = event.target;
    if (!(target instanceof HTMLSelectElement) || !this.container.contains(target)) return;
    const path = target.dataset.path;
    if (!path) return;
    const selection = this.selections.get(path);
    if (!selection) return;
    if (target.dataset.stream === "video") {
      const video = Number(target.value);
      if (Number.isInteger(video) && video >= 0) {
        this.selections.set(path, { ...selection, video_stream_index: video });
      }
    } else if (target.dataset.stream === "audio" || target.dataset.stream === "subtitle") {
      const index = target.value === "off" ? null : Number(target.value);
      if (index !== null && (!Number.isInteger(index) || index < 0)) return;
      this.selections.set(path, target.dataset.stream === "audio"
        ? { ...selection, audio_stream_index: index }
        : { ...selection, subtitle_stream_index: index });
    } else return;
  }

  /** 显式构建当前行所选轨道的缓存, 不切换播放器或保存观看配置. */
  private async createCache(file: MediaEntry): Promise<void> {
    const [path] = file;
    const selection = this.selections.get(path);
    if (!selection || this.building.has(path)) return;
    this.building.add(path);
    this.errors.delete(path);
    this.render();
    try {
      await this.build({ path, ...selection });
    } catch (error) {
      this.errors.set(path, error instanceof Error ? error.message : String(error));
      this.fail(error);
    } finally {
      this.building.delete(path);
      this.render();
    }
  }

  /** 重新扫描单路径, 更新媒体元数据并恢复本行操作状态. */
  private async rescan(path: string): Promise<void> {
    if (this.scanning.has(path)) return;
    this.scanning.add(path);
    this.errors.delete(path);
    this.render();
    try {
      await rescanMedia(path);
      this.selections.delete(path);
      await this.load();
    } catch (error) {
      this.errors.set(path, error instanceof Error ? error.message : String(error));
      this.fail(error);
    } finally {
      this.scanning.delete(path);
      this.render();
    }
  }

  /** 高亮与当前播放器源路径相同的媒体项. */
  select(path: string | null): void {
    if (path === this.active) return;
    this.active = path;
    this.render();
  }

  /** 按展开集合和排序结果渲染紧凑行, 所有文本通过 textContent 设置. */
  private render(): void {
    element("#files-count").textContent = String(this.files.length);
    const rows = sortFiles(this.files, this.sort).map((file) => this.row(file));
    this.container.replaceChildren(...rows);
    if (!rows.length) this.container.textContent = "No media files found. Check the media mount and scan again.";
  }

  /** 创建一个可独立展开的路径条目及可选详情. */
  private row(file: MediaEntry): HTMLDetailsElement {
    const [path, info] = file;
    const expanded = this.expanded.has(path);
    const selection = this.selections.get(path) ?? null;
    const row = document.createElement("details");
    row.className = `file-row${path === this.active ? " active" : ""}`;
    row.dataset.filePath = path;
    row.open = expanded;

    const summary = document.createElement("summary");
    summary.dataset.path = path;
    summary.setAttribute("aria-label", `${expanded ? "Collapse" : "Expand"} ${fileName(file)}`);
    const name = document.createElement("strong");
    name.textContent = fileName(file);
    name.title = path;
    const detail = document.createElement("span");
    detail.className = "file-summary-meta";
    const position = info.configs.position === null ? "Not watched" : `Watched ${formatTime(info.configs.position)}`;
    detail.textContent = `${formatTime(info.metadata.duration)} / ${info.metadata.format} / ${position}`;
    summary.append(name, detail);
    row.append(summary);

    if (expanded) row.append(this.details(path, info, selection));
    return row;
  }

  /** 展示完整媒体状态, 轨道选择及缓存创建操作. */
  private details(path: string, info: MediaInfo, selection: StreamSelection | null): HTMLDivElement {
    const details = document.createElement("div");
    details.className = "file-details";
    const source = document.createElement("code");
    source.className = "file-path";
    source.textContent = path;
    const status = document.createElement("p");
    status.className = "item-status";
    status.textContent = `Ready / ${formatTime(info.metadata.duration)} / ${info.metadata.format} / ${info.metadata.video_streams.length} video, ${info.metadata.audio_streams.length} audio, ${info.metadata.subtitle_streams.length} subtitle streams`;
    const saved = document.createElement("p");
    saved.className = "item-status";
    saved.textContent = `Saved position: ${info.configs.position === null ? "none" : formatTime(info.configs.position)}`;
    details.append(source, status, saved);

    const tracks = document.createElement("div");
    tracks.className = "media-track-selectors";
    tracks.append(
      this.streamSelect(path, "video", "Video", info.metadata.video_streams, selection?.video_stream_index ?? null, false),
      this.streamSelect(path, "audio", "Audio", info.metadata.audio_streams, selection?.audio_stream_index ?? null, true),
      this.streamSelect(path, "subtitle", "Subtitles", info.metadata.subtitle_streams, selection?.subtitle_stream_index ?? null, true),
    );
    details.append(tracks);

    const error = this.errors.get(path);
    if (error) {
      const diagnostic = document.createElement("p");
      diagnostic.className = "item-error";
      diagnostic.textContent = error;
      details.append(diagnostic);
    }
    const actions = document.createElement("div");
    actions.className = "item-actions";
    const build = document.createElement("button");
    build.type = "button";
    build.dataset.action = "build";
    build.dataset.path = path;
    build.className = "primary-button";
    build.textContent = this.building.has(path) ? "Creating cache..." : "Create cache folder";
    build.disabled = selection === null || this.building.has(path);
    const scan = document.createElement("button");
    scan.type = "button";
    scan.dataset.action = "scan";
    scan.dataset.path = path;
    scan.textContent = this.scanning.has(path) ? "Scanning..." : "Rescan";
    scan.disabled = this.scanning.has(path) || this.building.has(path);
    actions.append(build, scan);
    details.append(actions);
    return details;
  }

  /** 创建一个带默认或显式关闭选择的流选择框. */
  private streamSelect(
    path: string, kind: StreamKind, title: string, streams: StreamInfo[], selected: number | null, optional: boolean,
  ): HTMLLabelElement {
    const label = document.createElement("label");
    label.className = "track-select";
    const caption = document.createElement("span");
    caption.textContent = title;
    const select = document.createElement("select");
    select.dataset.path = path;
    select.dataset.stream = kind;
    select.setAttribute("aria-label", `${title} stream for ${path.split("/").at(-1) ?? path}`);
    if (optional) {
      const off = document.createElement("option");
      off.value = "off";
      off.textContent = kind === "audio" ? "No audio" : "Off";
      select.append(off);
    }
    for (const stream of streams) {
      const option = document.createElement("option");
      option.value = String(stream.stream_index);
      option.textContent = streamLabel(stream);
      select.append(option);
    }
    select.value = selected === null ? "off" : String(selected);
    select.disabled = streams.length === 0;
    label.append(caption, select);
    return label;
  }
}

/** 浏览器中保存列表排序的存储键. */
const SORT_KEY = "anime-server.library-sort";
/** 只提供新媒体模型能够支持的排序方式. */
const SORT_OPTIONS = ["path", "name-asc", "name-desc", "duration-desc", "duration-asc", "progress-desc"] as const;
/** 限制列表排序选项的有效值. */
type SortOrder = typeof SORT_OPTIONS[number];
/** 限定流选择事件允许修改的字段. */
type StreamKind = "video" | "audio" | "subtitle";

/** 判断缓存的流选择是否仍属于重扫后的媒体元数据. */
function validSelection(info: MediaInfo, selection: StreamSelection | null | undefined): boolean {
  if (!selection || !info.metadata.video_streams.some((stream) => stream.stream_index === selection.video_stream_index)) {
    return false;
  }
  return (selection.audio_stream_index === null
      || info.metadata.audio_streams.some((stream) => stream.stream_index === selection.audio_stream_index))
    && (selection.subtitle_stream_index === null
      || info.metadata.subtitle_streams.some((stream) => stream.stream_index === selection.subtitle_stream_index));
}

/** 组合流标题, 语言, 编码和 default/forced 状态. */
function streamLabel(stream: StreamInfo): string {
  return [stream.title, stream.language, stream.codec_name.toUpperCase(),
    stream.is_default ? "default" : null, stream.is_forced ? "forced" : null]
    .filter(Boolean).join(" / ");
}

/** 旧版本已移除的排序值使用默认路径排序. */
function isSortOrder(value: string | null): value is SortOrder {
  return SORT_OPTIONS.some((option) => option === value);
}
