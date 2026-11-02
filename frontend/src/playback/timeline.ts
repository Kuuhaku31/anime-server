
// frontend/src/playback/timeline.ts
// 统一原视频时间轴和已生成范围的边界计算.
import type { CacheStatus } from "../state/player-state.ts";

/** 描述独立于播放位置的缓存进度显示. */
interface CacheProgress {
  /** 已生成内容占媒体时长的百分比. */
  percent: number;
  /** 带阶段和时长的可读说明. */
  text: string;
}

/** 使用服务端阶段与已验证前缀展示生成进度. */
export function cacheProgress(cache: CacheStatus, duration: number): CacheProgress {
  const seconds = Math.min(duration, cache.buffered_until);
  const percent = cache.phase === "completed" ? 100 : Math.min(100, 100 * seconds / duration);
  const state = {
    loading: "Preparing / queued",
    ready: "Ready",
    running: "Generating",
    stopping: "Stopping",
    completed: "Complete",
    failed: "Generation failed",
  }[cache.phase];
  return {
    percent,
    text: `${state}: ${percent.toFixed(1)}% / ${formatTime(seconds)} of ${formatTime(duration)}`,
  };
}

/** 将媒体元素位置约束到原视频时间轴. */
export function absoluteTime(mediaTime: number, start: number, duration: number): number {
  if (!Number.isFinite(mediaTime)) return start;
  return Math.min(duration, Math.max(0, start + mediaTime));
}

/** 按半开区间判断本地等待目标是否已有媒体样本. */
export function rangeReady(position: number, bufferedUntil: number): boolean {
  // 末端时间点尚无媒体样本, 只要严格进入已发布范围就不再额外等待.
  return Number.isFinite(position) && Number.isFinite(bufferedUntil) && position >= 0 && position < bufferedUntil;
}

/** 将秒数显示为播放器时间标签. */
export function formatTime(seconds: number): string {
  const value = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
  const hour = Math.floor(value / 3600);
  const minute = Math.floor(value / 60) % 60;
  const second = String(value % 60).padStart(2, "0");
  return hour ? `${hour}:${String(minute).padStart(2, "0")}:${second}` : `${minute}:${second}`;
}
