// 封装同源 JSON 请求, 会话释放和观看进度保存.
export class ApiError extends Error {
  status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

// 保留失败响应的状态码和正文, 不把请求失败转换为默认业务数据.
export async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const response = await fetch(url, options);
  if (!response.ok) {
    const body = await response.text();
    throw new ApiError(response.status, body || `HTTP ${response.status}`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

export function jsonRequest<T>(url: string, body: unknown, method = "POST", signal?: AbortSignal): Promise<T> {
  return request<T>(url, {
    method, signal, headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  });
}

// 页面卸载时用 keepalive 尽力发送释放请求, 未送达的租约仍由服务端超时回收.
export function releaseSession(sessionId: string, keepalive = false): Promise<void> {
  return request(`/api/playback/${sessionId}`, { method: "DELETE", keepalive });
}

export function saveProgress(fileId: string, position: number, keepalive = false): Promise<void> {
  return request(`/api/files/${fileId}/progress`, {
    method: "PUT", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ position }), keepalive,
  });
}

export function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

// 等待绑定本轮操作信号, 换片或重新拖动取消后不再继续旧的状态轮询.
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
