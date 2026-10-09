export function absoluteTime(mediaTime: number, start: number, duration: number): number {
  if (!Number.isFinite(mediaTime)) return start;
  return Math.min(duration, Math.max(0, start + mediaTime));
}

export function rangeReady(position: number, bufferedUntil: number, duration: number, complete: boolean): boolean {
  return Number.isFinite(position) && position >= 0 && position < bufferedUntil
    && (complete || bufferedUntil >= Math.min(duration, position + 0.5));
}

export function formatTime(seconds: number): string {
  const value = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
  const hour = Math.floor(value / 3600);
  const minute = Math.floor(value / 60) % 60;
  const second = String(value % 60).padStart(2, "0");
  return hour ? `${hour}:${String(minute).padStart(2, "0")}:${second}` : `${minute}:${second}`;
}
