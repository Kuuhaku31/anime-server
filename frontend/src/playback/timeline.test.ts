// 验证时间换算和缓存半开区间, 特别覆盖末端相等与非有限数值.
import assert from "node:assert/strict";
import { test } from "node:test";
import { absoluteTime, cacheProgress, rangeReady, formatTime } from "./timeline.ts";
import type { PlaybackSession } from "../state/player-state.ts";

test("cache generation shows verified time independently of playback and completion rounding", () => {
  // 播放目标和浏览器缓冲不参与百分比, 完成标记将舍入不足的末端显示为 100%.
  const session: PlaybackSession = {
    session_id: "test", url: "/test", mime_type: "application/vnd.apple.mpegurl",
    duration: 120, position: 12, start_time: 0, buffered_until: 30,
    phase: "running",
  };
  assert.deepEqual(cacheProgress(session), { percent: 25, text: "Generating: 25.0% / 0:30 of 2:00" });
  assert.match(cacheProgress({ ...session, phase: "loading" }).text, /^Preparing \/ queued/);
  assert.match(cacheProgress({ ...session, phase: "ready" }).text, /^Ready/);
  assert.match(cacheProgress({ ...session, phase: "stopping" }).text, /^Stopping/);
  assert.match(cacheProgress({ ...session, phase: "failed" }).text, /^Generation failed/);
  const complete = cacheProgress({ ...session, phase: "complete", buffered_until: 119.99 });
  assert.equal(complete.percent, 100);
  assert.match(complete.text, /^Complete: 100.0%/);
  assert.equal(cacheProgress({ ...session, buffered_until: 120.1 }).percent, 100);
});

test("fractional HLS source offsets preserve the original timeline", () => {
  assert.equal(absoluteTime(12.5, 600, 1400), 612.5);
  assert.equal(absoluteTime(900, 600, 1400), 1400);
  assert.equal(absoluteTime(Number.NaN, 600, 1400), 600);
  assert.equal(absoluteTime(1.25, 7.375, 20), 8.625);
});

test("a covered target starts immediately and the exact end still waits", () => {
  // 生成范围按半开区间判断, 不因 complete 与否增加额外余量.
  assert.equal(rangeReady(7.375, 8), true);
  assert.equal(rangeReady(7.999, 8), true);
  assert.equal(rangeReady(8, 8), false);
  assert.equal(rangeReady(15, 8), false);
  assert.equal(rangeReady(19.9, 20), true);
  assert.equal(rangeReady(20, 20), false);
  assert.equal(rangeReady(Number.NaN, 20), false);
  assert.equal(rangeReady(1, Number.NaN), false);
  assert.equal(rangeReady(-1, 20), false);
});

test("time labels support episodes longer than an hour", () => {
  assert.equal(formatTime(71), "1:11");
  assert.equal(formatTime(3671), "1:01:11");
});
