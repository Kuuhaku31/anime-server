
// frontend/src/ui/library.ts
// 展示轻量媒体列表, 展开时按需探测元数据并创建缓存.

import { getMediaMetadata, listMedia, rescanMedia, scanMedia } from "../api/anime-server.ts";
import type { MediaEntry, MediaInfo, PlaybackMedia, PlaybackQuery, StreamInfo, StreamSelection } from "../state/player-state.ts";
import { formatTime } from "../playback/timeline.ts";
import { element } from "./controls.ts";

/** 用容器内路径区分同内容媒体的不同副本. */
export function fileAddress(file: PlaybackMedia): string {
  return file[0];
}

/** 从容器路径取得显示名称, 不在 API 中维护冗余文件名. */
export function fileName(file: MediaEntry): string {
  return file.path.split("/").at(-1) ?? file.path;
}

/** 按列表字段排序, 未加载 metadata 的条目保持路径稳定顺序. */
export function sortFiles(
  files: MediaEntry[], order: SortOrder, metadata: ReadonlyMap<string, MediaInfo> = new Map(),
): MediaEntry[] {
  const compareText = (left: string, right: string): number => left.localeCompare(right, undefined, { numeric: true });
  return [...files].sort((left, right) => {
    let result = 0;
    if (order === "name-asc") result = compareText(fileName(left), fileName(right));
    else if (order === "name-desc") result = compareText(fileName(right), fileName(left));
    else if (order === "duration-asc" || order === "duration-desc") {
      const leftInfo = metadata.get(left.file_id);
      const rightInfo = metadata.get(right.file_id);
      if (leftInfo && rightInfo) {
        result = order === "duration-asc"
          ? leftInfo.metadata.duration - rightInfo.metadata.duration
          : rightInfo.metadata.duration - leftInfo.metadata.duration;
      }
    } else if (order === "progress-desc") {
      const leftInfo = metadata.get(left.file_id);
      const rightInfo = metadata.get(right.file_id);
      const leftPosition = left.configs.position ?? 0;
      const rightPosition = right.configs.position ?? 0;
      const leftProgress = leftInfo && leftInfo.metadata.duration > 0
        ? leftPosition / leftInfo.metadata.duration : leftPosition;
      const rightProgress = rightInfo && rightInfo.metadata.duration > 0
        ? rightPosition / rightInfo.metadata.duration : rightPosition;
      result = rightProgress - leftProgress;
    }
    return result || compareText(left.path, right.path);
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

/** 管理媒体列表, 展开状态, 延迟 metadata 和缓存构建. */
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

  /** 获取轻量路径列表, 保留现存路径的展开和已加载 metadata. */
  async load(): Promise<void> {
    this.files = await listMedia();
    const paths = new Set(this.files.map((file) => file.path));
    const fileIds = new Set(this.files.map((file) => file.file_id));
    this.expanded = new Set([...this.expanded].filter((path) => paths.has(path)));
    this.selections = new Map([...this.selections].filter(([path]) => paths.has(path)));
    this.errors = new Map([...this.errors].filter(([path]) => paths.has(path)));
    this.metadata = new Map([...this.metadata].filter(([fileId]) => fileIds.has(fileId)));
    this.metadataErrors = new Map([...this.metadataErrors].filter(([fileId]) => fileIds.has(fileId)));
    this.loading = new Map([...this.loading].filter(([fileId]) => fileIds.has(fileId)));
    for (const file of this.files) {
      const info = this.metadata.get(file.file_id);
      if (info && !validSelection(info, this.selections.get(file.path))) {
        this.selections.set(file.path, initialSelection({ metadata: info.metadata, configs: file.configs }));
      }
    }
    this.render();
    this.dispatchEvent(new Event("change"));
    await Promise.all(this.files
      .filter((file) => this.expanded.has(file.path) && !this.metadata.has(file.file_id))
      .map((file) => this.loadMetadata(file)));
  }

  /** 为缓存列表提供已加载源路径的媒体信息. */
  info(path: string | null): MediaInfo | null {
    if (!path) return null;
    const file = this.files.find((entry) => entry.path === path);
    return file ? this.metadata.get(file.file_id) ?? null : null;
  }

  /** 为缓存摘要加载源时长, 与媒体展开共享在途请求和失败记录. */
  ensureMetadata(path: string): Promise<void> {
    const file = this.files.find((entry) => entry.path === path);
    return file ? this.loadMetadata(file) : Promise.resolve();
  }

  /** 将源 metadata 诊断提供给缓存列表, 不把探测失败当作编码失败. */
  metadataError(path: string | null): string | null {
    const file = this.files.find((entry) => entry.path === path);
    return file ? this.metadataErrors.get(file.file_id) ?? null : null;
  }

  /** 高亮与当前播放器源路径相同的媒体项. */
  select(path: string | null): void {
    if (path === this.active) return;
    this.active = path;
    this.render();
  }

  /** 当前媒体列表的 DOM 容器. */
  private container = element<HTMLDivElement>("#files");
  /** 当前数据库登记的媒体路径列表. */
  private files: MediaEntry[] = [];
  /** 按数据库文件 ID 缓存的单文件 metadata. */
  private metadata = new Map<string, MediaInfo>();
  /** 同内容共享的在途请求, 移除后旧响应不能覆盖重扫结果. */
  private loading = new Map<string, Promise<void>>();
  /** 按内容 ID 缓存的 metadata 请求错误. */
  private metadataErrors = new Map<string, string>();
  /** 显示缓存构建请求的回调. */
  private build: (query: PlaybackQuery) => Promise<unknown>;
  /** 展示全局操作失败的回调. */
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
  /** 各路径重扫和缓存操作的诊断. */
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
      const file = this.files.find((item) => item.path === path);
      if (this.expanded.has(path)) this.expanded.delete(path);
      else {
        this.expanded.add(path);
        if (file) void this.loadMetadata(file);
      }
      this.render();
      return;
    }
    const button = target.closest<HTMLButtonElement>("button[data-action]");
    if (!button || !this.container.contains(button)) return;
    const path = button.dataset.path;
    if (!path || button.disabled) return;
    const file = this.files.find((item) => item.path === path);
    if (!file) return;
    if (button.dataset.action === "build") void this.createCache(file);
    else if (button.dataset.action === "scan") void this.rescan(path);
    else if (button.dataset.action === "metadata") void this.loadMetadata(file, true);
  }

  /** 加载单个 metadata 并在同内容列表项间共享结果. */
  private loadMetadata(file: MediaEntry, retry = false): Promise<void> {
    if (this.metadata.has(file.file_id) || this.metadataErrors.has(file.file_id) && !retry) {
      return Promise.resolve();
    }
    const pending = this.loading.get(file.file_id);
    if (pending) return pending;
    this.metadataErrors.delete(file.file_id);
    const request = getMediaMetadata(file.file_id).then((info) => {
      if (this.loading.get(file.file_id) !== request
        || !this.files.some((item) => item.file_id === file.file_id)) return;
      this.metadata.set(file.file_id, info);
      for (const item of this.files) {
        if (item.file_id !== file.file_id) continue;
        item.configs = info.configs;
        const selection = this.selections.get(item.path);
        if (!this.selections.has(item.path) || !validSelection(info, selection)) {
          this.selections.set(item.path, initialSelection(info));
        }
      }
    }).catch((error: unknown) => {
      if (this.loading.get(file.file_id) === request
        && this.files.some((item) => item.file_id === file.file_id)) {
        this.metadataErrors.set(file.file_id, error instanceof Error ? error.message : String(error));
      }
    }).finally(() => {
      if (this.loading.get(file.file_id) === request) {
        this.loading.delete(file.file_id);
        this.render();
        this.dispatchEvent(new Event("change"));
      }
    });
    this.loading.set(file.file_id, request);
    this.render();
    return request;
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
    const selection = this.selections.get(file.path);
    if (!this.metadata.has(file.file_id) || !selection || this.building.has(file.path)) return;
    this.building.add(file.path);
    this.errors.delete(file.path);
    this.render();
    try {
      await this.build({ path: file.path, ...selection });
    } catch (error) {
      this.errors.set(file.path, error instanceof Error ? error.message : String(error));
    } finally {
      this.building.delete(file.path);
      this.render();
    }
  }

  /** 重扫单路径并更新列表, 展开项随后重新按需获取 metadata. */
  private async rescan(path: string): Promise<void> {
    if (this.scanning.has(path)) return;
    this.scanning.add(path);
    this.errors.delete(path);
    this.render();
    try {
      const updated = await rescanMedia(path);
      this.selections.delete(path);
      const previous = this.files.find((file) => file.path === path);
      if (previous) {
        this.loading.delete(previous.file_id);
        this.metadata.delete(previous.file_id);
        this.metadataErrors.delete(previous.file_id);
      }
      await this.load();
      const current = this.files.find((file) => file.path === path);
      if (updated && current && this.expanded.has(path)) await this.loadMetadata(current);
    } catch (error) {
      this.errors.set(path, error instanceof Error ? error.message : String(error));
    } finally {
      this.scanning.delete(path);
      this.render();
    }
  }

  /** 按展开集合和排序结果渲染紧凑行, 所有文本通过 textContent 设置. */
  private render(): void {
    element("#files-count").textContent = String(this.files.length);
    const rows = sortFiles(this.files, this.sort, this.metadata).map((file) => this.row(file));
    this.container.replaceChildren(...rows);
    if (!rows.length) this.container.textContent = "No media files found. Check the media mount and scan again.";
  }

  /** 创建一个可独立展开的路径条目及可选详情. */
  private row(file: MediaEntry): HTMLDetailsElement {
    const { path, file_id: fileId, configs } = file;
    const expanded = this.expanded.has(path);
    const info = this.metadata.get(fileId) ?? null;
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
    const position = configs.position === null ? "Not watched" : `Watched ${formatTime(configs.position)}`;
    detail.textContent = info
      ? `${formatTime(info.metadata.duration)} / ${info.metadata.format} / ${position}` : position;
    detail.title = detail.textContent;
    summary.append(name, detail);
    row.append(summary);

    if (expanded) row.append(this.details(file, info, selection));
    return row;
  }

  /** 展示完整状态, metadata 请求结果或局部错误以及可用操作. */
  private details(file: MediaEntry, info: MediaInfo | null, selection: StreamSelection | null): HTMLDivElement {
    const { path, file_id: fileId, configs } = file;
    const details = document.createElement("div");
    details.className = "file-details";
    const source = document.createElement("code");
    source.className = "file-path";
    source.textContent = path;
    const saved = document.createElement("p");
    saved.className = "item-status";
    saved.textContent = `Saved position: ${configs.position === null ? "none" : formatTime(configs.position)}`;
    details.append(source, saved);

    if (info) {
      const status = document.createElement("p");
      status.className = "item-status";
      status.textContent = `Ready / ${formatTime(info.metadata.duration)} / ${info.metadata.format} / ${info.metadata.video_streams.length} video, ${info.metadata.audio_streams.length} audio, ${info.metadata.subtitle_streams.length} subtitle streams`;
      details.append(status);
      const tracks = document.createElement("div");
      tracks.className = "media-track-selectors";
      tracks.append(
        this.streamSelect(path, "video", "Video", info.metadata.video_streams, selection?.video_stream_index ?? null, false),
        this.streamSelect(path, "audio", "Audio", info.metadata.audio_streams, selection?.audio_stream_index ?? null, true),
        this.streamSelect(path, "subtitle", "Subtitles", info.metadata.subtitle_streams, selection?.subtitle_stream_index ?? null, true),
      );
      details.append(tracks);
    } else {
      const loading = document.createElement("p");
      loading.className = "item-status";
      loading.textContent = this.loading.has(fileId) ? "Loading metadata..." : "Metadata not loaded";
      details.append(loading);
    }

    const metadataError = this.metadataErrors.get(fileId);
    const error = this.errors.get(path) ?? metadataError;
    if (error) {
      const diagnostic = document.createElement("p");
      diagnostic.className = "item-error";
      diagnostic.textContent = error;
      details.append(diagnostic);
    }

    const actions = document.createElement("div");
    actions.className = "item-actions";
    if (info) {
      const build = document.createElement("button");
      build.type = "button";
      build.dataset.action = "build";
      build.dataset.path = path;
      build.className = "primary-button";
      build.textContent = this.building.has(path) ? "Creating cache..." : "Create cache folder";
      build.disabled = selection === null || this.building.has(path);
      actions.append(build);
    } else if (metadataError) {
      const retry = document.createElement("button");
      retry.type = "button";
      retry.dataset.action = "metadata";
      retry.dataset.path = path;
      retry.textContent = "Retry metadata";
      retry.disabled = this.loading.has(fileId);
      actions.append(retry);
    }
    const scan = document.createElement("button");
    scan.type = "button";
    scan.dataset.action = "scan";
    scan.dataset.path = path;
    scan.textContent = this.scanning.has(path) ? "Scanning..." : "Rescan";
    scan.disabled = this.scanning.has(path) || this.building.has(path);
    actions.append(scan);
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
