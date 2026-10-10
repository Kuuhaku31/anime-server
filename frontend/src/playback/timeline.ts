// 统一原视频时间轴和已生成范围的边界计算.
import type { PlaybackSession } from "../state/player-state.ts";

// 使用服务端已验证前缀展示生成进度, 完成标记优先于分片末端的时间舍入差异.
export function cacheProgress(session: PlaybackSession): { percent: number; text: string } {
  const seconds = Math.min(session.duration, session.buffered_until);
  const percent = session.complete ? 100 : Math.min(100, 100 * seconds / session.duration);
  const state = session.complete ? "Complete" : session.error ? "Generation failed"
    : session.stopping ? "Stopping" : session.generating
      ? session.encoding_threads > 0 ? `Generating (${session.encoding_threads} threads)` : "Preparing / queued"
      : "Stopped";
  return {
    percent,
    text: `${state}: ${percent.toFixed(1)}% / ${formatTime(seconds)} of ${formatTime(session.duration)}`,
  };
}

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
