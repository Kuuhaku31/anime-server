
// frontend/src/state/cache-store.test.js
// 验证缓存 Map 的操作顺序, 失败通知和过期响应隔离.

import assert from "node:assert/strict";
import { test } from "node:test";
import { CacheStore } from "./cache-store.ts";

/** 构造公开缓存记录, 不伪造内部目录或会话标识. */
function entry(handleId = "a".repeat(32)) {
  return {
    handle_id: handleId, source_path: "/media/video.mkv",
    spec: { media_id: "b".repeat(32), video_stream_index: 0, audio_stream_index: null, subtitle_stream_index: null, version: "test" },
    phase: "ready", buffered_until: 8, error: null,
    resources: { encoder_slots: 0, process_slots: 0, ffmpeg_threads: 0 },
  };
}

// 删除先完成时, 在途列表响应不能把已删除的 handle 放回 Map.
test("a late list response cannot resurrect a deleted cache", async (context) => {
  const cache = entry();
  const store = new CacheStore();
  store.entries.set(cache.handle_id, cache);
  let resolveList;
  context.mock.method(globalThis, "fetch", async (_url, options = {}) => {
    if (options.method === "DELETE") return new Response(null, { status: 204 });
    return new Promise((resolve) => { resolveList = resolve; });
  });
  const first = store.refresh();
  assert.equal(store.refresh(), first);
  await store.remove(cache.handle_id);
  resolveList(Response.json({ entries: [cache] }));
  await first;
  assert.equal(store.entries.size, 0);
});

// 重试不创建播放操作, 停止归零资源, 请求失败保留旧条目并显示诊断.
test("retry stop and errors update only their cache entry", async (context) => {
  context.mock.method(console, "error", () => {});
  const cache = entry();
  const store = new CacheStore();
  store.entries.set(cache.handle_id, cache);
  let fail = false;
  const calls = [];
  context.mock.method(globalThis, "fetch", async (url, options = {}) => {
    calls.push({ url, method: options.method ?? "GET" });
    if (fail) return new Response("cannot delete cache", { status: 500 });
    if (url.endsWith("/retry")) {
      return Response.json({ cache: { ...cache, phase: "running",
        resources: { encoder_slots: 1, process_slots: 1, ffmpeg_threads: 2 } }, media: {} });
    }
    return Response.json(cache);
  });
  await store.retry(cache.handle_id);
  assert.equal(store.resources.ffmpeg_threads, 2);
  assert.equal(store.entries.get(cache.handle_id).phase, "running");
  await store.stop(cache.handle_id);
  assert.equal(store.entries.get(cache.handle_id).phase, "ready");
  assert.equal(store.resources.ffmpeg_threads, 0);
  fail = true;
  await assert.rejects(store.remove(cache.handle_id), /cannot delete cache/);
  assert.match(store.error, /cannot delete cache/);
  assert.equal(store.entries.size, 1);
  assert.equal(store.pending.size, 0);
  assert.deepEqual(calls.map((call) => call.method), ["POST", "POST", "DELETE"]);
});

// 构建响应晚于删除时, 只能重新拉取事实, 不能用旧构建快照复活条目.
test("an obsolete build response reconciles with the server after deletion", async (context) => {
  const cache = entry();
  const store = new CacheStore();
  store.entries.set(cache.handle_id, cache);
  let finishBuild;
  context.mock.method(globalThis, "fetch", async (_url, options = {}) => {
    if (options.method === "POST") return new Promise((resolve) => { finishBuild = resolve; });
    if (options.method === "DELETE") return new Response(null, { status: 204 });
    return Response.json({ entries: [] });
  });
  const building = store.build({ path: cache.source_path });
  await store.remove(cache.handle_id);
  finishBuild(Response.json({ cache, media: {} }));
  await building;
  assert.equal(store.entries.size, 0);
});

// 两个独立构建完成顺序不同, 不能因整表查询被另一操作作废而丢失先完成的句柄.
test("independent concurrent builds both remain in the shared map", async (context) => {
  const first = entry("a".repeat(32));
  const second = entry("c".repeat(32));
  const store = new CacheStore();
  const builds = [];
  let finishList;
  context.mock.method(globalThis, "fetch", async (_url, options = {}) => {
    if (options.method === "POST") return new Promise((resolve) => builds.push(resolve));
    return new Promise((resolve) => { finishList = resolve; });
  });
  const firstBuild = store.build({ path: first.source_path });
  const secondBuild = store.build({ path: "/media/second.mkv" });
  builds[0](Response.json({ cache: first, media: {} }));
  await new Promise((resolve) => setImmediate(resolve));
  builds[1](Response.json({ cache: second, media: {} }));
  await secondBuild;
  finishList?.(Response.json({ entries: [first] }));
  await firstBuild;
  assert.deepEqual([...store.entries.keys()].sort(), [first.handle_id, second.handle_id]);
});
