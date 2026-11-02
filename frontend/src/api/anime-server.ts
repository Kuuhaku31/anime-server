
// frontend/src/api/anime-server.ts
// 封装同源媒体列表, 配置保存和最小播放会话接口.

import type { CacheStatus, MediaConfigs, MediaEntry, PlaybackQuery, PlaybackStart, ScanChanges } from "../state/player-state.ts";

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

/** 获取路径与媒体信息组成的列表. */
export function listMedia(): Promise<MediaEntry[]> {
  return request("/api/media");
}

/** 请求全量扫描并返回索引变更. */
export function scanMedia(): Promise<ScanChanges> {
  return request("/api/media/scan", { method: "POST" });
}

/** 重扫指定路径, 文件已经移除时返回 null. */
export function rescanMedia(path: string, signal?: AbortSignal): Promise<MediaEntry | null> {
  return jsonRequest("/api/media/rescan", { path }, "POST", signal);
}

/** 按路径保存完整播放配置, 卸载时允许使用 keepalive. */
export function saveConfigs(path: string, configs: MediaConfigs, keepalive = false): Promise<void> {
  return request("/api/media/configs", {
    method: "PUT", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, configs }), keepalive,
  });
}

/** 创建播放时保留响应以便释放被新操作替代的会话. */
export function createPlayback(query: PlaybackQuery): Promise<PlaybackStart> {
  return jsonRequest("/api/playback", query);
}

/** 查询缓存快照, 不发送位置或心跳. */
export function getPlayback(sessionId: string, signal?: AbortSignal): Promise<CacheStatus> {
  return request(`/api/playback/${sessionId}`, { signal });
}

/** 只释放当前会话映射, 不停止服务端共享缓存生成. */
export function releasePlayback(sessionId: string, keepalive = false): Promise<void> {
  return request(`/api/playback/${sessionId}`, { method: "DELETE", keepalive });
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
