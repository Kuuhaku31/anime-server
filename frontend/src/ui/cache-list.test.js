
// frontend/src/ui/cache-list.test.js
// 验证缓存操作的源文件和阶段边界, 以及显式删除确认.

import assert from "node:assert/strict";
import { test } from "node:test";
import { CacheList, cacheActions } from "./cache-list.ts";
import { CacheStore } from "../state/cache-store.ts";

/** 构造包含已发布前缀的缓存样本. */
function entry(phase = "ready", sourcePath = "/media/<video>.mkv", bufferedUntil = 8) {
  return {
    handle_id: "a".repeat(32), source_path: sourcePath,
    spec: { video_stream_index: 0, audio_stream_index: null, subtitle_stream_index: null },
    status: { phase, buffered_until: bufferedUntil, error: null,
      resources: { encoder_slots: 0, process_slots: 0, ffmpeg_threads: 0 } },
  };
}

test("cache actions distinguish reading a prefix from starting or stopping generation", () => {
  assert.deepEqual(cacheActions(entry()), { play: true, retry: true, stop: false, delete: true });
  assert.deepEqual(cacheActions(entry("running")), { play: true, retry: false, stop: true, delete: true });
  assert.equal(cacheActions(entry("loading", "/media/video.mkv", 0)).play, false);
  assert.equal(cacheActions(entry("stopping")).play, false);
  assert.deepEqual(cacheActions(entry("completed", null)), { play: false, retry: false, stop: false, delete: true });
  assert.equal(cacheActions(entry("failed")).play, true);
});

/** 提供缓存列表所需的最小 DOM, 包含原生确认对话框的状态. */
class Element {
  /** 模拟直接子节点. */
  children = [];
  /** 模拟行与按钮的属性. */
  dataset = {};
  /** 模拟安全文本内容. */
  textContent = "";
  /** 模拟按钮禁用状态. */
  disabled = false;
  /** 模拟确认对话框显示状态. */
  open = false;
  /** 记录显示属性. */
  attributes = {};
  /** 追加直接子节点. */
  append(...children) { this.children.push(...children); }
  /** 替换直接子节点. */
  replaceChildren(...children) { this.children = children; }
  /** 保存点击监听器. */
  addEventListener(_type, listener) { this.click = listener; }
  /** 保存可访问性标签. */
  setAttribute(name, value) { this.attributes[name] = value; }
  /** 打开删除确认. */
  showModal() { this.open = true; }
  /** 关闭删除确认. */
  close() { this.open = false; }
}

/** 区分操作按钮与普通节点. */
class Button extends Element {}

test("deletion requires confirmation and leaves playback selection to the controller", async (context) => {
  globalThis.HTMLButtonElement = Button;
  const nodes = new Map(["#caches", "#cache-count", "#cache-error", "#cache-resources", "#cache-delete-dialog",
    "#cache-delete-name", "#cache-delete-cancel", "#cache-delete-confirm", "#cache-refresh"]
    .map((selector) => [selector, new Element()]));
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector),
    createElement: (tag) => tag === "button" ? new Button() : new Element(),
  };
  const store = new CacheStore();
  const cache = entry();
  store.entries.set(cache.handle_id, cache);
  const played = [];
  let deleted = 0;
  context.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.equal(options.method, "DELETE");
    deleted++;
    return new Response(null, { status: 204 });
  });
  new CacheList(store, (handleId) => played.push(handleId), () => null);
  const list = nodes.get("#caches");
  assert.equal(list.children[0].children[0].textContent, "<video>.mkv");
  const buttons = list.children[0].children.at(-1).children;
  list.click({ target: buttons.find((button) => button.dataset.action === "play") });
  assert.deepEqual(played, [cache.handle_id]);
  list.click({ target: buttons.find((button) => button.dataset.action === "delete") });
  assert.equal(nodes.get("#cache-delete-dialog").open, true);
  assert.equal(deleted, 0);
  nodes.get("#cache-delete-confirm").onclick();
  await new Promise(setImmediate);
  assert.equal(deleted, 1);
  assert.equal(store.entries.size, 0);
});
