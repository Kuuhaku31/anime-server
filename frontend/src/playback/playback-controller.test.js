import assert from "node:assert/strict";
import { test } from "node:test";
import { PlaybackController } from "./playback-controller.ts";
import { preferNativeHls } from "./hls-adapter.ts";

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
  source_version: "test", duration: 120,
  video_tracks: [{ stream_index: 0 }],
  audio_tracks: [{ stream_index: 1, is_default: true }, { stream_index: 2 }],
  subtitle_tracks: [{ stream_index: 3 }],
};

function server(context) {
  const calls = [];
  const sessions = new Map();
  context.mock.method(globalThis, "fetch", async (url, options = {}) => {
    const body = options.body ? JSON.parse(options.body) : null;
    const method = options.method ?? "GET";
    calls.push({ url, method, body });
    let result;
    if (url.endsWith("/streams")) result = info;
    else if (url.endsWith("/progress") && method === "GET") result = { watch_pos: 12.375 };
    else if (method === "DELETE") {
      sessions.delete(url.split("/").at(-1));
      return new Response(null, { status: 204 });
    } else if (url.endsWith("/playback-plan")) {
      const id = `session-${calls.filter((call) => call.url.endsWith("/playback-plan")).length}`;
      result = {
        session_id: id, url: `/playback/${id}/index.m3u8`, start_time: body.position,
        duration: 120, state: "ready", ready: true, buffered_until: 120, error: null,
      };
      sessions.set(id, result);
    } else if (url.endsWith("/status")) result = sessions.get(url.split("/").at(-2));
    else if (method === "PUT") return new Response(null, { status: 204 });
    else throw new Error(`Unexpected request: ${method} ${url}`);
    return Response.json(result);
  });
  return { calls, sessions, creations: () => calls.filter((call) => call.url.endsWith("/playback-plan")) };
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

test("track changes and subtitle off preserve fractional position and pause state", async (context) => {
  const api = server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  assert.equal(api.creations()[0].body.position, 12.375);
  video.currentTime = 1.625;
  video.dispatchEvent(new Event("timeupdate"));
  video.pause();
  await operation(controller, () => controller.select({ audio: 2, subtitle: 3 }));
  assert.equal(controller.error, "");
  assert.equal(video.paused, true);
  assert.equal(controller.position, 14);
  assert.equal(api.sessions.size, 1);
  assert.deepEqual(api.creations()[1].body, {
    source_version: "test", video_stream_index: 0,
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

test("local seeking reuses the session and remote seeking retains play state", async (context) => {
  const api = server(context);
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  controller.seek(18.75);
  assert.equal(controller.position, 18.75);
  assert.equal(api.creations().length, 1);
  await operation(controller, () => controller.seek(7.375));
  assert.equal(api.creations()[1].body.position, 7.375);
  assert.equal(video.paused, false);
  assert.equal(controller.position, 7.375);
  await operation(controller, () => controller.stop());
});

test("decoder failure releases the session without an automatic playback request", async (context) => {
  const api = server(context);
  context.mock.method(console, "error", () => {});
  const video = new Video();
  const controller = new PlaybackController(video);
  await operation(controller, () => controller.open(file));
  video.currentTime = 2.5;
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
  assert.deepEqual(api.creations()[0].body, api.creations()[1].body);
  assert.equal(controller.error, "");
  await operation(controller, () => controller.stop());
});
