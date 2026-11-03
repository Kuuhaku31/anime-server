
// frontend/src/api/anime-server.ts
// 封装媒体配置, 剧集识别, 缓存管理和稳定句柄资源地址.

import type {
  CacheBuild, CacheEntry, CacheList, MediaConfigs, MediaEntry, MediaEpisode, MediaIdentification,
  MediaInfo, PlaybackQuery, ScanChanges,
} from "../state/player-state.ts";

/** 保留服务端返回的 HTTP 状态和错误正文. */
export class ApiError extends Error {
  /** 服务端响应的 HTTP 状态码. */
  status: number;

  /** 构造可供界面显示的请求错误. */
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/** 执行 JSON 请求, 失败时不返回默认业务数据. */
export async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const response = await fetch(url, options);
  if (!response.ok) {
    const body = await response.text();
    throw new ApiError(response.status, body || `HTTP ${response.status}`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

/** 序列化 JSON 请求体并保留操作取消信号. */
export function jsonRequest<T>(url: string, body: unknown, method = "POST", signal?: AbortSignal): Promise<T> {
  return request<T>(url, {
    method, signal, headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  });
}

/** 获取数据库中的媒体路径和共享配置, 不触发媒体探测. */
export function listMedia(): Promise<MediaEntry[]> {
  return request("/api/media");
}

/** 按数据库内容 ID 获取单个媒体的实时元数据. */
export function getMediaMetadata(fileId: string, signal?: AbortSignal): Promise<MediaInfo> {
  return request(`/api/media/${encodeURIComponent(fileId)}/metadata`, { signal });
}

/** 请求全量扫描并返回索引变更. */
export function scanMedia(): Promise<ScanChanges> {
  return request("/api/media/scan", { method: "POST" });
}

/** 重扫指定路径并返回轻量数据库条目, 文件已经移除时返回 null. */
export function rescanMedia(path: string, signal?: AbortSignal): Promise<MediaEntry | null> {
  return jsonRequest("/api/media/rescan", path, "POST", signal);
}

/** 仅保存或清空剧集 ID, 不改动共享播放进度和轨道偏好. */
export function saveMediaEpisode(fileId: string, episodeId: number | null): Promise<MediaEpisode> {
  return jsonRequest(`/api/media/${encodeURIComponent(fileId)}/episode`, { episode_id: episodeId }, "PUT");
}

/** 将路径字符串发给后端, 由后端读取文件信息并调用识别 API. */
export function identifyMedia(path: string): Promise<MediaIdentification> {
  return jsonRequest("/api/media/identify", path);
}

/** 按路径保存完整播放配置, 卸载时允许使用 keepalive. */
export function saveConfigs(path: string, configs: MediaConfigs, keepalive = false): Promise<void> {
  return request("/api/media/configs", {
    method: "PUT", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, configs }), keepalive,
  });
}

/** 提交转码请求, 与读取已有 HLS 资源分离. */
export function buildCache(query: PlaybackQuery): Promise<CacheBuild> {
  return jsonRequest("/api/cache", query);
}

/** 获取全部缓存及其转码资源占用合计. */
export function listCache(): Promise<CacheList> {
  return request("/api/cache");
}

/** 查询已有缓存, 不隐式启动或恢复生成. */
export function getCache(handleId: string, signal?: AbortSignal): Promise<CacheEntry> {
  return request(`/api/cache/${encodeURIComponent(handleId)}`, { signal });
}

/** 由服务端解析同内容的源路径并重新构建已有缓存. */
export function retryCache(handleId: string): Promise<CacheBuild> {
  return request(`/api/cache/${encodeURIComponent(handleId)}/retry`, { method: "POST" });
}

/** 停止转码并保留已经发布的缓存前缀. */
export function stopCache(handleId: string): Promise<CacheEntry> {
  return request(`/api/cache/${encodeURIComponent(handleId)}/stop`, { method: "POST" });
}

/** 立即停止并删除共享缓存, 不保护其他播放器的引用. */
export function deleteCache(handleId: string): Promise<void> {
  return request(`/api/cache/${encodeURIComponent(handleId)}`, { method: "DELETE" });
}

/** 使用稳定句柄和单个资源名称构造同源播放请求. */
export function playbackUrl(handleId: string, assetName = "index.m3u8"): string {
  return `/playback/${encodeURIComponent(handleId)}/${encodeURIComponent(assetName)}`;
}

/** 判断错误是否来自被替代的用户操作. */
export function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/** 在操作取消时立即结束轮询等待. */
export function wait(milliseconds: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const abort = (): void => { clearTimeout(timer); reject(signal.reason); };
    const timer = window.setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, milliseconds);
    signal.addEventListener("abort", abort, { once: true });
  });
}
