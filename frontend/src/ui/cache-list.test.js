
// frontend/src/ui/cache-list.test.js
// 验证缓存文件夹摘要, 多行展开和详情管理权限.

import assert from "node:assert/strict";
import { test } from "node:test";
import { CacheList, cacheActions } from "./cache-list.ts";
import { CacheStore } from "../state/cache-store.ts";

/** 构造包含已发布前缀的缓存样本. */
function entry(handleId = "a".repeat(32), phase = "ready", sourcePath = "/media/<video>.mkv", bufferedUntil = 8) {
  return {
    handle_id: handleId, source_path: sourcePath,
    spec: { media_id: "b".repeat(32), version: "test", video_stream_index: 0, audio_stream_index: null, subtitle_stream_index: null },
    phase, buffered_until: bufferedUntil, error: null,
    resources: { encoder_slots: 0, process_slots: 0, ffmpeg_threads: 0 },
  };
}

/** 提供缓存列表测试需要的轻量 DOM 节点. */
class Element {
  /** 节点名称. */
  tagName;
  /** 当前子节点. */
  children = [];
  /** 行和按钮的数据属性. */
  dataset = {};
  /** 可见文本. */
  textContent = "";
  /** 节点样式名. */
  className = "";
  /** 控件禁用状态. */
  disabled = false;
  /** 原生 details 展开状态. */
  open = false;
  /** 模拟确认框状态. */
  modalOpen = false;
  /** 保存无障碍属性. */
  attributes = {};
  /** 记录事件委托监听器. */
  listeners = new Map();
  /** 父级节点. */
  parent = null;
  /** 原生按钮点击处理器. */
  onclick = null;

  /** 创建指定标签的替身节点. */
  constructor(tagName = "div") { this.tagName = tagName.toUpperCase(); }

  /** 追加节点并记录其父级. */
  append(...children) {
    for (const child of children) {
      child.parent = this;
      this.children.push(child);
    }
  }

  /** 替换节点并设置新父级. */
  replaceChildren(...children) {
    this.children = [];
    this.append(...children);
  }

  /** 保存事件委托回调. */
  addEventListener(type, listener) { this.listeners.set(type, listener); }

  /** 向委托回调分发带目标的测试事件. */
  emit(type, target) {
    this.listeners.get(type)?.({ target, preventDefault() {} });
  }

  /** 保存无障碍属性. */
  setAttribute(name, value) { this.attributes[name] = value; }

  /** 判断节点是否包含指定后代. */
  contains(target) {
    return target === this || this.children.some((child) => child.contains(target));
  }

  /** 查找符合当前测试选择器的祖先. */
  closest(selector) {
    const [tag, attribute] = selector.match(/^[a-z]+|\[[^\]]+\]/gi) ?? [];
    const rawKey = attribute?.slice(1, -1).split("=")[0];
    const key = rawKey?.replace(/^data-/, "").replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    for (let node = this; node; node = node.parent) {
      if ((!tag || node.tagName.toLowerCase() === tag)
        && (!key || key in node.dataset)
        && (selector !== "button[data-action]" || "action" in node.dataset)) return node;
    }
    return null;
  }

  /** 在测试中展开子树查找. */
  findAll(predicate) {
    return this.children.flatMap((child) => [
      ...(predicate(child) ? [child] : []),
      ...child.findAll(predicate),
    ]);
  }

  /** 模拟原生删除确认框. */
  showModal() { this.modalOpen = true; }

  /** 模拟关闭确认框. */
  close() { this.modalOpen = false; }
}

/** 区分真实按钮类型检查. */
class Button extends Element {
  /** 初始化为按钮标签. */
  constructor() { super("button"); }
}

/** 安装缓存列表使用的最小 DOM 节点. */
function dom() {
  globalThis.Element = Element;
  globalThis.HTMLButtonElement = Button;
  const selectors = ["#caches", "#cache-count", "#cache-error", "#cache-resources", "#cache-delete-dialog",
    "#cache-delete-name", "#cache-delete-cancel", "#cache-delete-confirm", "#cache-refresh"];
  const nodes = new Map(selectors.map((selector) => [selector,
    new Element(selector === "#cache-delete-dialog" ? "dialog" : "div")]));
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector),
    createElement: (tag) => tag === "button" ? new Button() : new Element(tag),
  };
  return nodes;
}

/** 查找指定 handle 的行. */
function row(container, handleId) {
  return container.children.find((child) => child.dataset.handleId === handleId);
}

/** 查找指定操作按钮. */
function action(rowElement, name) {
  return rowElement.findAll((child) => child instanceof Button && child.dataset.action === name)[0];
}

/** 模拟共享媒体数据源, 允许测试 metadata 完成后的列表更新. */
class MediaLibrary extends EventTarget {
  /** 当前可读取的源 metadata. */
  metadata = null;
  /** 当前源 metadata 的局部错误. */
  error = null;
  /** 记录列表主动请求的源路径. */
  requested = [];

  /** 返回已有源信息, 不隐式加载. */
  info() { return this.metadata; }

  /** 记录按需加载, 测试通过 change 事件发布结果. */
  async ensureMetadata(path) { this.requested.push(path); }

  /** 返回独立于缓存编码状态的时长诊断. */
  metadataError() { return this.error; }
}

// 缓存操作依赖源文件和阶段, 无源 completed 不能播放或重试.
test("one encoding action follows the cache phase without starting playback", () => {
  assert.deepEqual(cacheActions(entry()), { play: true, encoding: "retry", encodingLabel: "Retry / resume", delete: true });
  assert.deepEqual(cacheActions(entry("b".repeat(32), "running")), {
    play: true, encoding: "stop", encodingLabel: "Pause encoding", delete: true,
  });
  assert.equal(cacheActions(entry("b".repeat(32), "loading", "/media/video.mkv", 0)).play, false);
  assert.equal(cacheActions(entry("b".repeat(32), "loading")).encoding, null);
  assert.deepEqual(cacheActions(entry("b".repeat(32), "stopping")), {
    play: false, encoding: null, encodingLabel: "Pause encoding", delete: true,
  });
  assert.deepEqual(cacheActions(entry("b".repeat(32), "completed", null)), {
    play: false, encoding: null, encodingLabel: "Completed", delete: true,
  });
  assert.equal(cacheActions(entry("b".repeat(32), "failed")).encoding, "retry");
  assert.equal(cacheActions(entry("b".repeat(32), "ready", null)).encoding, null);
});

// 折叠时无操作按钮, 多项可展开, 仅展开详情提供播放和删除.
test("cache rows expand independently and deletion requires confirmation", async (context) => {
  const nodes = dom();
  const store = new CacheStore();
  const cache = entry();
  const second = entry("c".repeat(32), "running", "/media/second.mkv", 4);
  store.entries.set(cache.handle_id, cache);
  store.entries.set(second.handle_id, second);
  const played = [];
  let deleted = 0;
  context.mock.method(globalThis, "fetch", async (_url, options) => {
    assert.equal(options.method, "DELETE");
    deleted++;
    return new Response(null, { status: 204 });
  });
  new CacheList(store, (handleId) => played.push(handleId), new MediaLibrary());
  const container = nodes.get("#caches");
  assert.equal(container.children.length, 2);
  assert.equal(container.children.every((item) => item.tagName === "DETAILS" && !item.open), true);
  assert.equal(container.findAll((child) => child instanceof Button).length, 0);

  container.emit("click", row(container, cache.handle_id).children[0]);
  container.emit("click", row(container, second.handle_id).children[0]);
  assert.equal(row(container, cache.handle_id).open, true);
  assert.equal(row(container, second.handle_id).open, true);
  assert.equal(action(row(container, second.handle_id), "encoding").textContent, "Pause encoding");
  container.emit("click", action(row(container, cache.handle_id), "play"));
  assert.deepEqual(played, [cache.handle_id]);
  container.emit("click", action(row(container, cache.handle_id), "delete"));
  assert.equal(nodes.get("#cache-delete-dialog").modalOpen, true);
  assert.equal(nodes.get("#cache-delete-name").textContent, "/media/<video>.mkv");
  assert.equal(deleted, 0);
  nodes.get("#cache-delete-cancel").onclick();
  assert.equal(nodes.get("#cache-delete-dialog").modalOpen, false);
  assert.equal(deleted, 0);

  container.emit("click", action(row(container, cache.handle_id), "delete"));
  nodes.get("#cache-delete-confirm").onclick();
  await new Promise(setImmediate);
  assert.equal(deleted, 1);
  assert.equal(store.entries.has(cache.handle_id), false);
  assert.equal(row(container, second.handle_id).open, true);
});

// 同一个按钮根据最新快照调用暂停或恢复, 在途请求和 stopping 都不能重复操作.
test("encoding toggle dispatches pause and retry with pending protection", async (context) => {
  const nodes = dom();
  const store = new CacheStore();
  const cache = entry("a".repeat(32), "running");
  store.entries.set(cache.handle_id, cache);
  const requests = [];
  let resolveStop;
  context.mock.method(globalThis, "fetch", async (url, options) => {
    requests.push(url);
    assert.equal(options.method, "POST");
    if (url.endsWith("/stop")) return new Promise((resolve) => { resolveStop = resolve; });
    assert.equal(url, `/api/cache/${cache.handle_id}/retry`);
    return Response.json({ cache: { ...cache, phase: "running" } });
  });
  new CacheList(store, () => assert.fail("encoding must not start playback"), new MediaLibrary());
  const container = nodes.get("#caches");
  container.emit("click", row(container, cache.handle_id).children[0]);
  assert.equal(row(container, cache.handle_id).findAll((child) => child instanceof Button).length, 3);
  container.emit("click", action(row(container, cache.handle_id), "encoding"));
  assert.equal(action(row(container, cache.handle_id), "encoding").disabled, true);
  container.emit("click", action(row(container, cache.handle_id), "encoding"));
  resolveStop(Response.json({ ...cache, phase: "stopping" }));
  await new Promise(setImmediate);
  assert.equal(action(row(container, cache.handle_id), "encoding").disabled, true);
  container.emit("click", action(row(container, cache.handle_id), "encoding"));
  assert.deepEqual(requests, [`/api/cache/${cache.handle_id}/stop`]);

  store.entries.set(cache.handle_id, { ...cache, phase: "ready" });
  store.dispatchEvent(new Event("change"));
  assert.equal(action(row(container, cache.handle_id), "encoding").textContent, "Retry / resume");
  container.emit("click", action(row(container, cache.handle_id), "encoding"));
  await new Promise(setImmediate);
  assert.deepEqual(requests, [`/api/cache/${cache.handle_id}/stop`, `/api/cache/${cache.handle_id}/retry`]);
  assert.equal(action(row(container, cache.handle_id), "encoding").textContent, "Pause encoding");
});

// 折叠摘要直接显示进度, metadata 到达和轮询均更新同一条进度条.
test("collapsed progress follows source duration and completion without duplicate detail bars", () => {
  const nodes = dom();
  const store = new CacheStore();
  const cache = entry();
  store.entries.set(cache.handle_id, cache);
  const library = new MediaLibrary();
  const list = new CacheList(store, () => {}, library);
  const container = nodes.get("#caches");
  const bars = () => row(container, cache.handle_id).findAll((child) => child.tagName === "PROGRESS");
  assert.deepEqual(library.requested, [cache.source_path]);
  assert.equal(row(container, cache.handle_id).open, false);
  assert.equal(bars().length, 1);
  assert.equal(bars()[0].value, undefined);
  library.metadata = { metadata: { duration: 32 } };
  library.dispatchEvent(new Event("change"));
  assert.equal(bars()[0].value, 25);
  store.entries.set(cache.handle_id, { ...cache, buffered_until: 16 });
  store.dispatchEvent(new Event("change"));
  assert.equal(bars()[0].value, 50);
  assert.match(bars()[0].attributes["aria-label"], /0:16 of 0:32/);
  container.emit("click", row(container, cache.handle_id).children[0]);
  assert.equal(bars().length, 1);
  assert.equal(row(container, cache.handle_id).children[0].contains(bars()[0]), true);

  library.metadata = null;
  library.error = "Source could not be probed";
  store.entries.set(cache.handle_id, { ...cache, phase: "completed" });
  list.select(cache.handle_id);
  assert.match(row(container, cache.handle_id).className, /completed active/);
  assert.equal(bars()[0].value, 100);
  assert.equal(action(row(container, cache.handle_id), "encoding").disabled, true);
  assert.match(row(container, cache.handle_id).findAll((child) => child.className === "item-error")[0].textContent,
    /Source could not be probed/);
});
