// 统一原视频时间轴和已生成范围的边界计算.
export function absoluteTime(mediaTime: number, start: number, duration: number): number {
  if (!Number.isFinite(mediaTime)) return start;
  return Math.min(duration, Math.max(0, start + mediaTime));
}

export function rangeReady(position: number, bufferedUntil: number): boolean {
  // 末端时间点尚无媒体样本, 只要严格进入已发布范围就不再额外等待.
  return Number.isFinite(position) && Number.isFinite(bufferedUntil) && position >= 0 && position < bufferedUntil;
}

export function formatTime(seconds: number): string {
  const value = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
  const hour = Math.floor(value / 3600);
  const minute = Math.floor(value / 60) % 60;
  const second = String(value % 60).padStart(2, "0");
  return hour ? `${hour}:${String(minute).padStart(2, "0")}:${second}` : `${minute}:${second}`;
}
