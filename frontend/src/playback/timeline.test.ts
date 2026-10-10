// 验证时间换算和缓存半开区间, 特别覆盖末端相等与非有限数值.
import assert from "node:assert/strict";
import { test } from "node:test";
import { absoluteTime, rangeReady, formatTime } from "./timeline.ts";

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
