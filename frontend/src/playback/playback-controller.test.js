
// frontend/src/playback/playback-controller.test.js
// 在无浏览器环境验证路径契约, 本地等待位置和会话生命周期.
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

/** 模拟控制器需要的浏览器媒体事件. */
class Video extends EventTarget {
  /** 当前是否暂停. */
  paused = true;
  /** 媒体元素的当前秒数. */
  currentTime = 0;
  /** 当前媒体地址. */
  src = "";
  /** 最近一次模拟解码错误. */
  error = null;
  /** 是否在加载时模拟解码失败. */
  failOnLoad = false;
  /** 模拟尚未下载缓冲数据. */
  buffered = { length: 0 };

  /** 声明测试替身支持原生 HLS. */
  canPlayType() { return "probably"; }
  /** 清除当前媒体地址. */
  removeAttribute(name) { if (name === "src") this.src = ""; }
  /** 异步派发加载完成或解码错误. */
  load() {
    if (!this.src) { this.currentTime = 0; return; }
    queueMicrotask(() => {
      if (this.failOnLoad) this.decodeError();
      else this.dispatchEvent(new Event("loadedmetadata"));
    });
  }
  /** 切换为暂停并派发事件. */
  pause() {
    if (this.paused) return;
    this.paused = true;
    this.dispatchEvent(new Event("pause"));
  }
  /** 切换为播放并派发事件. */
  async play() {
    this.paused = false;
    this.dispatchEvent(new Event("playing"));
  }
  /** 派发明确的解码失败. */
  decodeError() {
    this.error = { code: 3, message: "Test decoder failure" };
    this.dispatchEvent(new Event("error"));
  }
}

const info = {
  metadata: {
    duration: 120, format: "matroska",
    video_streams: [{ stream_index: 0 }, { stream_index: 4 }],
    audio_streams: [{ stream_index: 1, is_default: true }, { stream_index: 2 }],
    subtitle_streams: [{ stream_index: 3 }],
  },
  configs: { position: 12.375, video_stream_index: null, audio_stream_index: null, subtitle_stream_index: null },
};
const file = ["/media/video.mkv", info];

/** 模拟新 HTTP 契约, 会话表只保存独立的缓存快照. */
function server(context, generated = 120) {
  const calls = [];
  const sessions = new Map();
  context.mock.method(globalThis, "fetch", async (url, options = {}) => {
    const body = options.body ? JSON.parse(options.body) : null;
    const method = options.method ?? "GET";
    calls.push({ url, method, body });
    let result;
    if (url.endsWith("/rescan")) result = file;
    else if (method === "DELETE") {
      sessions.delete(url.split("/").at(-1));
      return new Response(null, { status: 204 });
    } else if (url.endsWith("/playback")) {
      const sessionId = `session-${calls.filter((call) => call.url.endsWith("/playback")).length}`;
      result = {
        phase: "running", buffered_until: generated,
      };
      sessions.set(sessionId, result);
      // 模拟服务端默认选择, 明确区分字段缺省与显式 null.
      result = {
        session_id: sessionId, url: `/playback/${sessionId}/index.m3u8`, cache: result, media: info,
        video_stream_index: body.video_stream_index ?? 0,
        audio_stream_index: "audio_stream_index" in body ? body.audio_stream_index : 1,
        subtitle_stream_index: "subtitle_stream_index" in body ? body.subtitle_stream_index : null,
      };
    } else if (method === "GET" && url.startsWith("/api/playback/")) result = sessions.get(url.split("/").at(-1));
    else if (method === "PUT") return new Response(null, { status: 204 });
    else throw new Error(`Unexpected request: ${method} ${url}`);
    return Response.json(result);
  });
  return { calls, sessions, creations: () => calls.filter((call) => call.url.endsWith("/playback")) };
}

/** 在有限时间内等待控制器满足预期条件. */
async function until(predicate) {
  const deadline = Date.now() + 2000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, "player operation timed out");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/** 等待一个公开用户操作完成. */
async function operation(controller, action) {
  action();
  await until(() => !controller.pending);
}

// 完成状态不再需要任何心跳, 停止后不再发出状态请求.
test("active generation refreshes while paused and completion stops polling", async (context) => {
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
  await until(() => controller.cache.buffered_until === 60);
  assert.ok(changes > 0);
  assert.equal(video.paused, true);
  session.phase = "completed";
  timers.get(1000)();
  await until(() => controller.cache.phase === "completed");
  const polls = () => api.calls.filter((call) => call.method === "GET" && call.url.startsWith("/api/playback/")).length;
  const completedPolls = polls();
  timers.get(1000)();
  assert.equal(polls(), completedPolls);
  assert.equal(timers.has(10_000), false);
  await operation(controller, () => controller.stop());
  const stoppedPolls = polls();
  timers.get(1000)();
  assert.equal(polls(), stoppedPolls);
});

// 视频, 音频和字幕切换必须保持暂停意图, 位置不写入创建请求.
test("track changes and subtitle off preserve fractional position and pause state", async (context) => {
  const api = server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  assert.deepEqual(api.creations()[0].body, { path: file[0] });
  assert.equal(controller.position, 12.375);
  assert.deepEqual(controller.selection, { video_stream_index: 0, audio_stream_index: 1, subtitle_stream_index: null });
  assert.equal(api.calls.some((call) => call.url.endsWith("/streams") || call.method === "GET" && call.url.endsWith("/progress")), false);
  video.currentTime = 14;
  video.dispatchEvent(new Event("timeupdate"));
  video.pause();
  await operation(controller, () => controller.select({ video_stream_index: 4, audio_stream_index: 2, subtitle_stream_index: 3 }));
  assert.equal(controller.error, "");
  assert.equal(video.paused, true);
  assert.equal(controller.position, 14);
  assert.equal(api.sessions.size, 1);
  assert.deepEqual(api.creations()[1].body, {
    path: file[0], video_stream_index: 4, audio_stream_index: 2, subtitle_stream_index: 3,
  });
  const release = api.calls.findIndex((call) => call.method === "DELETE");
  const replacement = api.calls.findIndex((call) => call === api.creations()[1]);
  assert.ok(release < replacement);
  await operation(controller, () => controller.select({ video_stream_index: 4, audio_stream_index: 2, subtitle_stream_index: null }));
  assert.equal(api.creations()[2].body.subtitle_stream_index, null);
  assert.equal("position" in api.creations()[2].body, false);
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
  await operation(controller, () => controller.select({ video_stream_index: 0, audio_stream_index: 2, subtitle_stream_index: null }));
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
  assert.ok(api.calls.some((call) => call.method === "PUT" && call.body.path === file[0] && call.body.configs.position === 14.875));
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
    path: file[0], video_stream_index: 0, audio_stream_index: 1, subtitle_stream_index: null,
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
  controller.cache = { ...session };
  controller.seek(65.375);
  await until(() => controller.message.includes("Waiting for 1:05"));
  assert.equal(controller.pending, true);
  assert.equal(video.paused, true);
  assert.equal(controller.position, 65.375);
  assert.equal(api.creations().length, 1);
  assert.equal(api.calls.some((call) => call.url.endsWith("/target") || call.url.endsWith("/heartbeat")), false);
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
  controller.open([file[0], { ...info, configs: { ...info.configs, position: 99 } }]);
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
  await operation(controller, () => controller.select({ video_stream_index: 0, audio_stream_index: null, subtitle_stream_index: null }));
  assert.deepEqual(controller.selection, { video_stream_index: 0, audio_stream_index: null, subtitle_stream_index: null });
  assert.equal(api.creations()[1].body.audio_stream_index, null);
  await operation(controller, () => controller.stop());
});

// 已发布前缀仍可播放, 失败且未覆盖的目标则明确报错而不是无期限等待.
test("a published prefix remains usable while an uncovered failed range reports an error", async (context) => {
  const api = server(context);
  context.mock.method(console, "error", () => {});
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
  await operation(controller, () => controller.seek(30));
  assert.match(controller.error, /Cache generation failed/);
  assert.equal(api.sessions.size, 0);
  await operation(controller, () => controller.stop());
});

// 已保存关闭偏好时显式发送 null, 避免缺省字段重新启用默认音轨.
test("saved disabled tracks are explicit in a path-based playback request", async (context) => {
  const api = server(context);
  const controller = new PlaybackController(new Video());
  await operation(controller, () => controller.open([
    file[0], { ...info, configs: { ...info.configs, video_stream_index: 0 } },
  ]));
  assert.deepEqual(api.creations()[0].body, {
    path: file[0], video_stream_index: 0, audio_stream_index: null, subtitle_stream_index: null,
  });
  assert.equal(controller.selection.audio_stream_index, null);
  await operation(controller, () => controller.stop());
});
