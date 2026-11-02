
// frontend/src/ui/library.test.js
// 验证路径列表, 单文件重扫和新媒体字段排序.

import assert from "node:assert/strict";
import { test } from "node:test";
import { Library, sortFiles } from "./library.ts";

/** 提供列表事件委托需要的最小 DOM 替身. */
class Element {
  /** 当前子节点列表. */
  children = [];
  /** 行和按钮的数据属性. */
  dataset = {};
  /** 可见文本. */
  textContent = "";
  /** 按钮是否被禁用. */
  disabled = false;
  /** 高亮切换的替身. */
  classList = { toggle() {} };
  /** 追加模拟子节点. */
  append(...children) { this.children.push(...children); }
  /** 替换全部模拟子节点. */
  replaceChildren(...children) { this.children = children; }
  /** 保存容器点击监听器. */
  addEventListener(_, listener) { this.click = listener; }
  /** 查找全部单文件扫描按钮. */
  querySelectorAll() {
    return this.children.flatMap((child) => [
      ...(child instanceof Button && child.dataset.action === "scan" ? [child] : []),
      ...child.querySelectorAll(),
    ]);
  }
}

/** 区分按钮和其他 DOM 节点. */
class Button extends Element {}

/** 构造只包含路径和 MediaInfo 的列表条目. */
function entry(path, duration = 120, position = null) {
  return [path, {
    metadata: { duration, format: "matroska", video_streams: [], audio_streams: [], subtitle_streams: [] },
    configs: { position, video_stream_index: null, audio_stream_index: null, subtitle_stream_index: null },
  }];
}

/** 安装列表需要的最小 DOM 环境. */
function dom() {
  globalThis.HTMLButtonElement = Button;
  const nodes = new Map(["#files", "#files-count", "#scan", "#files-sort"].map((selector) => [selector, new Element()]));
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector),
    createElement: (tag) => tag === "button" ? new Button() : new Element(),
  };
  return nodes;
}

// 同内容的两个路径是独立列表项, 重扫其中一项不应禁用另一个按钮.
test("rescan submits the displayed path once and restores the button after errors", async () => {
  const nodes = dom();
  globalThis.localStorage = { getItem: () => "name-asc", setItem() {} };
  const file = entry("/media/series/episode.part.MKV");
  const copy = entry("/media/copies/episode.part.MKV");
  const calls = [];
  const errors = [];
  let complete;
  globalThis.fetch = async (url, options) => {
    calls.push({ url, options });
    if (url === "/api/media") return Response.json([file, copy]);
    return new Promise((resolve) => { complete = resolve; });
  };
  const library = new Library(() => assert.fail("rescan must not start playback"), (error) => errors.push(error));
  await library.load();
  const container = nodes.get("#files");
  const button = container.querySelectorAll().find((button) => button.dataset.path === file[0]);
  const other = container.querySelectorAll().find((button) => button.dataset.path === copy[0]);
  container.click({ target: button });
  container.click({ target: button });
  assert.equal(button.disabled, true);
  assert.equal(other.disabled, false);
  assert.equal(calls.length, 2);
  assert.equal(calls[1].url, "/api/media/rescan");
  assert.deepEqual(JSON.parse(calls[1].options.body), { path: file[0] });
  complete(new Response("cannot read media file", { status: 422 }));
  await new Promise(setImmediate);
  assert.equal(errors.length, 1);
  assert.equal(button.disabled, false);
  assert.equal(button.textContent, "Rescan");
  container.click({ target: button });
  complete(Response.json(file));
  await new Promise(setImmediate);
  assert.equal(calls.filter((call) => call.url === "/api/media").length, 2);
  assert.equal(container.querySelectorAll().find((button) => button.dataset.path === file[0]).disabled, false);
});

// 文件名由路径推导, 时长和进度取自 MediaInfo, 排序不修改原数组.
test("path tuples sort by name duration and playback progress without mutation", () => {
  const files = [
    entry("/media/b/episode10.mkv", 20),
    entry("/media/a/episode2.mkv", 10, 4),
    entry("/media/c/episode1.mkv", 20, 16),
  ];
  const paths = (order) => sortFiles(files, order).map((file) => file[0].split("/")[2]);
  assert.deepEqual(paths("path"), ["a", "b", "c"]);
  assert.deepEqual(paths("name-asc"), ["c", "a", "b"]);
  assert.deepEqual(paths("name-desc"), ["b", "a", "c"]);
  assert.deepEqual(paths("duration-asc"), ["a", "b", "c"]);
  assert.deepEqual(paths("duration-desc"), ["b", "c", "a"]);
  assert.deepEqual(paths("progress-desc"), ["c", "a", "b"]);
  assert.equal(files[0][0], "/media/b/episode10.mkv");
});

// 已移除的旧排序值回到路径排序, 有效偏好在刷新后保留.
test("sort preference survives refreshes without refetching on local changes", async () => {
  let saved = "duration-desc";
  globalThis.localStorage = { getItem: () => saved, setItem: (_, value) => { saved = value; } };
  const nodes = dom();
  let requests = 0;
  globalThis.fetch = async () => {
    requests++;
    return Response.json([entry("/media/short.mkv", 1), entry("/media/long.mkv", 10)]);
  };
  const fail = (error) => assert.fail(String(error));
  const library = new Library(() => {}, fail);
  await library.load();
  const sort = nodes.get("#files-sort");
  assert.equal(sort.value, "duration-desc");
  assert.equal(nodes.get("#files").children[0].dataset.filePath, "/media/long.mkv");
  sort.value = "duration-asc";
  sort.onchange();
  assert.equal(saved, "duration-asc");
  assert.equal(requests, 1);
  assert.equal(nodes.get("#files").children[0].dataset.filePath, "/media/short.mkv");
  await new Library(() => {}, fail).load();
  assert.equal(sort.value, "duration-asc");
  saved = "size-desc";
  new Library(() => {}, fail);
  assert.equal(sort.value, "path");
});
