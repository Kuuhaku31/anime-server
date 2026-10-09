import assert from "node:assert/strict";
import { test } from "node:test";
import { absoluteTime, needsNewStream, formatTime } from "./timeline.ts";

test("fractional HLS source offsets preserve the original timeline", () => {
  assert.equal(absoluteTime(12.5, 600, 1400), 612.5);
  assert.equal(absoluteTime(900, 600, 1400), 1400);
  assert.equal(absoluteTime(Number.NaN, 600, 1400), 600);
  assert.equal(absoluteTime(1.25, 7.375, 20), 8.625);
});

test("seek outside the generated HLS interval recreates the stream", () => {
  assert.equal(needsNewStream(700, 600, 800), false);
  assert.equal(needsNewStream(400, 600, 800), true);
  assert.equal(needsNewStream(1100, 600, 800), true);
});

test("time labels support episodes longer than an hour", () => {
  assert.equal(formatTime(71), "1:11");
  assert.equal(formatTime(3671), "1:01:11");
});
