
// frontend/src/playback/timeline.test.ts
// 验证缓存快照和本地原时间轴边界.
import assert from "node:assert/strict";
import { test } from "node:test";
import { absoluteTime, cacheProgress, rangeReady, formatTime } from "./timeline.ts";
import type { CacheStatus } from "../state/player-state.ts";

test("cache generation shows verified time independently of playback and completion rounding", () => {
  // 播放目标和浏览器缓冲不参与百分比, 完成标记将舍入不足的末端显示为 100%.
  const cache: CacheStatus = { buffered_until: 30, phase: "running", error: null };
  const resources = { encoder_slots: 1, process_slots: 1, ffmpeg_threads: 2 };
  assert.deepEqual(cacheProgress(cache, 120, resources), {
    percent: 25, determinate: true, state: "Generating", timing: "0:30 of 2:00",
    text: "Generating: 25.0% / 0:30 of 2:00",
  });
  assert.match(cacheProgress({ ...cache, phase: "loading" }, 120, resources).text, /^Preparing \/ queued/);
  assert.match(cacheProgress({ ...cache, phase: "ready" }, 120, resources).text, /^Ready/);
  assert.match(cacheProgress({ ...cache, phase: "stopping" }, 120, resources).text, /^Stopping/);
  assert.match(cacheProgress({ ...cache, phase: "failed" }, 120, resources).text, /^Generation failed/);
  const complete = cacheProgress({ ...cache, phase: "completed", buffered_until: 119.99 }, 120, resources);
  assert.equal(complete.percent, 100);
  assert.match(complete.text, /^Complete: 100.0%/);
  assert.equal(cacheProgress({ ...cache, buffered_until: 120.1 }, 120, resources).percent, 100);
  assert.match(cacheProgress(cache, 120, { encoder_slots: 0, process_slots: 0, ffmpeg_threads: 0 }).text,
    /^Queued \/ preparing/);
  assert.match(cacheProgress(cache, 0, resources).text, /duration unavailable/);
  assert.equal(cacheProgress(cache, 0, resources).percent, 0);
  assert.equal(cacheProgress(cache, 0, resources).determinate, false);
  assert.equal(cacheProgress(cache, Number.NaN, resources).determinate, false);
  assert.equal(cacheProgress({ ...cache, buffered_until: -1 }, 120, resources).percent, 0);
  const unknownComplete = cacheProgress({ ...cache, phase: "completed" }, 0, resources);
  assert.equal(unknownComplete.determinate, true);
  assert.equal(unknownComplete.percent, 100);
  assert.match(unknownComplete.text, /100.0%.*duration unavailable/);
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
