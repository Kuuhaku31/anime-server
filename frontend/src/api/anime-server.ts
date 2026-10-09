export class ApiError extends Error {
  status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

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

export function releaseSession(id: string, keepalive = false): Promise<void> {
  return request(`/api/playback/${id}`, { method: "DELETE", keepalive });
}

export function saveProgress(id: string, position: number, keepalive = false): Promise<void> {
  return request(`/api/files/${id}/progress`, {
    method: "PUT", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ position }), keepalive,
  });
}

export function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

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
