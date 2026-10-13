// 在无浏览器环境验证会话生命周期, 播放意图和异步竞态.
import assert from "node:assert/strict";
import { test } from "node:test";
import { PlaybackController } from "./playback-controller.ts";
import { preferNativeHls } from "./hls-adapter.ts";

// 保留操作所需的事件和延迟, 禁用周期定时器以免测试遗留后台轮询.
globalThis.window = Object.assign(new EventTarget(), { setTimeout, setInterval: () => 0 });
globalThis.document = new EventTarget();

test("Safari uses native HLS while MSE browsers do not trust a native maybe result", () => {
  assert.equal(preferNativeHls(true, true, "Apple Computer, Inc."), true);
  assert.equal(preferNativeHls(true, true, "Google Inc."), false);
  assert.equal(preferNativeHls(false, true, ""), false);
  assert.equal(preferNativeHls(true, false, ""), true);
  assert.equal(preferNativeHls(false, false, ""), false);
});

class Video extends EventTarget {
  paused = true;
  currentTime = 0;
  src = "";
  error = null;
  failOnLoad = false;
  buffered = { length: 0 };

  canPlayType() { return "probably"; }
  removeAttribute(name) { if (name === "src") this.src = ""; }
  load() {
    if (!this.src) { this.currentTime = 0; return; }
    queueMicrotask(() => {
      if (this.failOnLoad) this.decodeError();
      else this.dispatchEvent(new Event("loadedmetadata"));
    });
  }
  pause() {
    if (this.paused) return;
    this.paused = true;
    this.dispatchEvent(new Event("pause"));
  }
  async play() {
    this.paused = false;
    this.dispatchEvent(new Event("playing"));
  }
  decodeError() {
    this.error = { code: 3, message: "Test decoder failure" };
    this.dispatchEvent(new Event("error"));
  }
}

const file = { pre16mib_md5: "a".repeat(32), name: "video.mkv" };
const info = {
  duration: 120,
  video_tracks: [{ stream_index: 0 }],
  audio_tracks: [{ stream_index: 1, is_default: true }, { stream_index: 2 }],
  subtitle_tracks: [{ stream_index: 3 }],
};

function server(context, generated = 120) {
  const calls = [];
  const sessions = new Map();
  context.mock.method(globalThis, "fetch", async (url, options = {}) => {
    const body = options.body ? JSON.parse(options.body) : null;
    const method = options.method ?? "GET";
    calls.push({ url, method, body });
    let result;
    if (url.endsWith("/probe")) result = info;
    else if (method === "DELETE") {
      sessions.delete(url.split("/").at(-1));
      return new Response(null, { status: 204 });
    } else if (url.endsWith("/playback")) {
      const sessionId = `session-${calls.filter((call) => call.url.endsWith("/playback")).length}`;
      result = {
        session_id: sessionId, url: `/playback/${sessionId}/index.m3u8`, start_time: 0,
        mime_type: "application/vnd.apple.mpegurl", duration: 120, position: body?.position ?? 12.375,
        phase: "running", buffered_until: generated,
      };
      sessions.set(sessionId, result);
      // 模拟服务端默认选择, 明确区分字段缺省与显式 null.
      result = { ...result, media: info, selection: {
        audio: body && "audio_stream_index" in body ? body.audio_stream_index : 1,
        subtitle: body && "subtitle_stream_index" in body ? body.subtitle_stream_index : null,
      } };
    } else if (url.endsWith("/target")) {
      result = sessions.get(url.split("/").at(-2));
      result.position = body.position;
    } else if (url.endsWith("/status")) result = sessions.get(url.split("/").at(-2));
    else if (method === "PUT") return new Response(null, { status: 204 });
    else throw new Error(`Unexpected request: ${method} ${url}`);
    return Response.json(result);
  });
  return { calls, sessions, creations: () => calls.filter((call) => call.url.endsWith("/playback")) };
}

async function until(predicate) {
  const deadline = Date.now() + 2000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, "player operation timed out");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

async function operation(controller, action) {
  action();
  await until(() => !controller.pending);
}

test("active generation refreshes every second while paused and completion keeps only heartbeat polling", async (context) => {
  // 捕获定时回调而不等待真实时间, 验证刷新频率和停止后的请求边界.
  const timers = new Map();
  context.mock.method(window, "setInterval", (callback, milliseconds) => {
    timers.set(milliseconds, callback);
    return 0;
  });
  const api = server(context, 30);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  video.pause();
  let changes = 0;
  controller.addEventListener("change", () => { changes += 1; });
  const session = [...api.sessions.values()][0];
  session.buffered_until = 60;
  timers.get(1000)();
  await until(() => controller.session.buffered_until === 60);
  assert.ok(changes > 0);
  assert.equal(video.paused, true);
  session.phase = "complete";
  timers.get(1000)();
  await until(() => controller.session.phase === "complete");
  const polls = () => api.calls.filter((call) => call.url.endsWith("/status")).length;
  const completedPolls = polls();
  timers.get(1000)();
  assert.equal(polls(), completedPolls);
  timers.get(10_000)();
  await until(() => polls() > completedPolls);
  await operation(controller, () => controller.stop());
  const stoppedPolls = polls();
  timers.get(1000)();
  timers.get(10_000)();
  assert.equal(polls(), stoppedPolls);
});

test("track changes and subtitle off preserve fractional position and pause state", async (context) => {
  const api = server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  assert.equal(api.creations()[0].body, null);
  assert.equal(controller.position, 12.375);
  assert.deepEqual(controller.selection, { audio: 1, subtitle: null });
  assert.equal(api.calls.some((call) => call.url.endsWith("/streams") || call.method === "GET" && call.url.endsWith("/progress")), false);
  video.currentTime = 14;
  video.dispatchEvent(new Event("timeupdate"));
  video.pause();
  await operation(controller, () => controller.select({ audio: 2, subtitle: 3 }));
  assert.equal(controller.error, "");
  assert.equal(video.paused, true);
  assert.equal(controller.position, 14);
  assert.equal(api.sessions.size, 1);
  assert.deepEqual(api.creations()[1].body, {
    audio_stream_index: 2, subtitle_stream_index: 3, position: 14,
  });
  const release = api.calls.findIndex((call) => call.method === "DELETE");
  const replacement = api.calls.findIndex((call) => call === api.creations()[1]);
  assert.ok(release < replacement);
  await operation(controller, () => controller.select({ audio: 2, subtitle: null }));
  assert.equal(api.creations()[2].body.subtitle_stream_index, null);
  assert.equal(api.creations()[2].body.position, 14);
  assert.equal(video.paused, true);
  await operation(controller, () => controller.stop());
  assert.equal(api.sessions.size, 0);
});

test("forward and backward seeks within cached ranges reuse the session", async (context) => {
  const api = server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  controller.seek(18.75);
  assert.equal(controller.position, 18.75);
  assert.equal(api.creations().length, 1);
  await operation(controller, () => controller.seek(7.375));
  assert.equal(api.creations().length, 1);
  assert.equal(video.paused, false);
  assert.equal(controller.position, 7.375);
  await operation(controller, () => controller.stop());
});

test("immediate track switching keeps a pause whose browser event has not arrived yet", async (context) => {
  // HTMLMediaElement.paused 先改变, pause 事件稍后到达, 不能靠事件更新唯一的播放意图.
  server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  context.mock.method(video, "pause", function () { this.paused = true; });
  controller.toggle();
  await operation(controller, () => controller.select({ audio: 2, subtitle: null }));
  assert.equal(controller.error, "");
  assert.equal(video.paused, true);
  await operation(controller, () => controller.stop());
});

test("decoder failure releases the session without an automatic playback request", async (context) => {
  const api = server(context);
  context.mock.method(console, "error", () => {});
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  video.currentTime = 14.875;
  video.decodeError();
  await until(() => api.sessions.size === 0);
  assert.match(controller.error, /Test decoder failure/);
  assert.equal(controller.session, null);
  assert.equal(video.paused, true);
  assert.equal(controller.position, 14.875);
  assert.equal(api.creations().length, 1);
  assert.ok(api.calls.some((call) => call.method === "PUT" && call.body.position === 14.875));
});

test("invalid seek input is reported without a new playback request", async (context) => {
  const api = server(context);
  context.mock.method(console, "error", () => {});
  const controller = new PlaybackController(new Video());
  await operation(controller, () => controller.open(file));
  controller.seek(Number.NaN);
  assert.match(controller.error, /position must be finite/);
  assert.equal(api.creations().length, 1);
  await operation(controller, () => controller.stop());
});

test("initial decode errors are terminal and manual retry repeats the same request", async (context) => {
  const api = server(context);
  context.mock.method(console, "error", () => {});
  const video = new Video();
  video.failOnLoad = true;
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  assert.match(controller.error, /Test decoder failure/);
  assert.equal(api.sessions.size, 0);
  assert.equal(api.creations().length, 1);
  video.failOnLoad = false;
  await operation(controller, () => controller.retry());
  assert.deepEqual(api.creations()[1].body, {
    audio_stream_index: 1, subtitle_stream_index: null, position: 12.375,
  });
  assert.equal(controller.error, "");
  await operation(controller, () => controller.stop());
});

test("seek waits for new segments without creating a new session and can be redirected", async (context) => {
  const api = server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  const session = [...api.sessions.values()][0];
  session.buffered_until = 20;
  controller.session = { ...session };
  controller.seek(65.375);
  await until(() => controller.message.includes("Waiting for 1:05"));
  assert.equal(controller.pending, true);
  assert.equal(video.paused, true);
  assert.equal(controller.position, 65.375);
  assert.equal(api.creations().length, 1);
  assert.ok(api.calls.some((call) => call.url.endsWith("/target") && call.body.position === 65.375));
  session.buffered_until = 80;
  await until(() => !controller.pending);
  assert.equal(controller.position, 65.375);
  assert.equal(video.paused, false);
  assert.equal(controller.error, "");
  assert.equal(api.creations().length, 1);
  video.pause();
  controller.seek(100);
  await until(() => controller.message.includes("Waiting for 1:40"));
  await operation(controller, () => controller.seek(7.375));
  assert.equal(controller.position, 7.375);
  assert.equal(video.paused, true);
  assert.equal(api.creations().length, 1);
  await operation(controller, () => controller.stop());
  assert.equal(api.sessions.size, 0);
});

test("resuming waits for the target range and stop cancels the wait", async (context) => {
  const api = server(context, 4);
  const video = new Video();
  const controller = new PlaybackController(video);
  controller.open(file);
  await until(() => controller.message.includes("Waiting for 0:12"));
  assert.equal(video.src, "");
  assert.equal(controller.position, 12.375);
  const session = [...api.sessions.values()][0];
  session.buffered_until = 16;
  await until(() => !controller.pending);
  assert.equal(controller.position, 12.375);
  assert.equal(video.currentTime, 12.375);
  controller.seek(95.125);
  await until(() => controller.message.includes("Waiting for 1:35"));
  await operation(controller, () => controller.stop());
  assert.equal(controller.position, 95.125);
  assert.equal(controller.message, "Stopped");
  assert.equal(controller.error, "");
  assert.equal(api.sessions.size, 0);
  assert.equal(api.creations().length, 1);
});

test("the exact cache boundary waits but even a sub-half-second covered target starts", async (context) => {
  // 测量严格的不等式, 而不是用一个更远的已缓存位置代替边界验证.
  const api = server(context, 12.375);
  const video = new Video();
  const controller = new PlaybackController(video);
  controller.open({ ...file, watch_pos: 99 });
  await until(() => controller.message.includes("Waiting for 0:12"));
  assert.equal(video.src, "");
  assert.equal(controller.position, 12.375);
  const session = [...api.sessions.values()][0];
  session.buffered_until = 12.376;
  await until(() => !controller.pending);
  assert.equal(controller.error, "");
  assert.equal(video.currentTime, 12.375);
  assert.equal(api.creations().length, 1);
  video.currentTime = 10;
  await operation(controller, () => controller.select({ audio: null, subtitle: null }));
  assert.deepEqual(controller.selection, { audio: null, subtitle: null });
  assert.equal(api.creations()[1].body.audio_stream_index, null);
  await operation(controller, () => controller.stop());
});

test("a published prefix remains usable while an uncovered failed range waits", async (context) => {
  const api = server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  const session = [...api.sessions.values()][0];
  session.phase = "failed";
  session.buffered_until = 20;
  await controller.report();
  assert.equal(controller.error, "");
  assert.equal(controller.message, "Playing cached range. Cache generation stopped.");
  assert.equal(video.paused, false);
  controller.seek(30);
  await until(() => controller.message.includes("Cache generation failed"));
  assert.equal(controller.pending, true);
  assert.equal(api.sessions.size, 1);
  await operation(controller, () => controller.stop());
});
