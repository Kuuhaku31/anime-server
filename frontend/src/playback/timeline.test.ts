import assert from "node:assert/strict";
import { test } from "node:test";
import { absoluteTime, rangeReady, formatTime } from "./timeline.ts";

test("fractional HLS source offsets preserve the original timeline", () => {
  assert.equal(absoluteTime(12.5, 600, 1400), 612.5);
  assert.equal(absoluteTime(900, 600, 1400), 1400);
  assert.equal(absoluteTime(Number.NaN, 600, 1400), 600);
  assert.equal(absoluteTime(1.25, 7.375, 20), 8.625);
});

test("cache ranges require a margin until complete and allow seeking near the end", () => {
  assert.equal(rangeReady(7.375, 8, 20, false), true);
  assert.equal(rangeReady(7.875, 8, 20, false), false);
  assert.equal(rangeReady(15, 8, 20, false), false);
  assert.equal(rangeReady(19.9, 20, 20, true), true);
  assert.equal(rangeReady(20, 20, 20, true), false);
  assert.equal(rangeReady(Number.NaN, 20, 20, true), false);
  assert.equal(rangeReady(-1, 20, 20, true), false);
});

test("time labels support episodes longer than an hour", () => {
  assert.equal(formatTime(71), "1:11");
  assert.equal(formatTime(3671), "1:01:11");
});
