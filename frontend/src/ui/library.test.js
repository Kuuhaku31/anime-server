// 使用轻量 DOM 替身验证单文件重扫地址, 按钮状态和错误恢复.
import assert from "node:assert/strict";
import { test } from "node:test";
import { Library, sortFiles } from "./library.ts";

class Element {
  children = [];
  dataset = {};
  textContent = "";
  disabled = false;
  classList = { toggle() {} };
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  addEventListener(_, listener) { this.click = listener; }
  querySelectorAll() {
    return this.children.flatMap((child) => [
      ...(child instanceof Button && child.dataset.action === "scan" ? [child] : []),
      ...child.querySelectorAll(),
    ]);
  }
}
class Button extends Element {}

test("rescan submits the displayed path once and restores the button after errors", async () => {
  // 延迟网络响应以确认重复点击不会提交第二次重扫.
  globalThis.HTMLButtonElement = Button;
  const nodes = new Map(["#files", "#files-count", "#scan", "#files-sort"].map((selector) => [selector, new Element()]));
  globalThis.localStorage = { getItem: () => "name-asc", setItem() {} };
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector),
    createElement: (tag) => tag === "button" ? new Button() : new Element(),
  };
  const file = {
    pre16mib_md5: "a".repeat(32), name: "episode.part.MKV", size: 100, ddp_epi_id: null,
    path: "/media/series/episode.part.MKV",
  };
  const copy = { ...file, path: "/media/copies/episode.part.MKV" };
  const calls = [];
  const errors = [];
  let complete;
  globalThis.fetch = async (url, options) => {
    calls.push({ url, options });
    if (url === "/api/files") return { ok: true, status: 200, json: async () => [file, copy] };
    return new Promise((resolve) => { complete = resolve; });
  };
  const library = new Library(() => assert.fail("rescan must not start playback"), (error) => errors.push(error));
  await library.load();
  const container = nodes.get("#files");
  const button = container.querySelectorAll().find((button) => button.dataset.path === file.path);
  const other = container.querySelectorAll().find((button) => button.dataset.path === copy.path);
  container.click({ target: button });
  container.click({ target: button });
  assert.equal(button.disabled, true);
  assert.equal(button.textContent, "Scanning...");
  // 同指纹的另一地址是独立项目, 不因当前地址正在扫描而禁用.
  assert.equal(other.disabled, false);
  assert.equal(calls.length, 2);
  assert.equal(calls[1].url, "/api/scan/file");
  assert.equal(calls[1].options.method, "POST");
  assert.deepEqual(JSON.parse(calls[1].options.body), { path: "/media/series/episode.part.MKV" });
  complete({ ok: false, status: 422, text: async () => "cannot read media file" });
  await new Promise(setImmediate);
  assert.equal(errors.length, 1);
  assert.equal(button.disabled, false);
  assert.equal(button.textContent, "Rescan");
  container.click({ target: button });
  file.pre16mib_md5 = "b".repeat(32);
  complete({ ok: true, status: 200, json: async () => ({ ok: true, scanned: 1 }) });
  await new Promise(setImmediate);
  assert.equal(calls.filter((call) => call.url === "/api/files").length, 2);
  assert.equal(container.querySelectorAll().find((button) => button.dataset.path === file.path).disabled, false);
});

test("sort options order copies independently without mutating the input", () => {
  // 自然文件名排序兼容剧集编号, 相同大小按地址稳定排序.
  const files = [
    { name: "episode10.mkv", path: "/media/b.mkv", size: 20, watched_datetime: null },
    { name: "episode2.mkv", path: "/media/a.mkv", size: 10, watched_datetime: "2026-10-10T09:00:00Z" },
    { name: "episode1.mkv", path: "/media/c.mkv", size: 20, watched_datetime: "2026-10-11T09:00:00Z" },
  ];
  const paths = (order) => sortFiles(files, order).map((file) => file.path);
  assert.deepEqual(paths("path"), ["/media/a.mkv", "/media/b.mkv", "/media/c.mkv"]);
  assert.deepEqual(paths("name-asc"), ["/media/c.mkv", "/media/a.mkv", "/media/b.mkv"]);
  assert.deepEqual(paths("name-desc"), ["/media/b.mkv", "/media/a.mkv", "/media/c.mkv"]);
  assert.deepEqual(paths("size-asc"), ["/media/a.mkv", "/media/b.mkv", "/media/c.mkv"]);
  assert.deepEqual(paths("size-desc"), ["/media/b.mkv", "/media/c.mkv", "/media/a.mkv"]);
  assert.deepEqual(paths("recent"), ["/media/c.mkv", "/media/a.mkv", "/media/b.mkv"]);
  assert.equal(files[0].path, "/media/b.mkv");
});

test("sort preference survives new library instances and refreshes without refetching on change", async () => {
  // 共享浏览器存储模拟刷新页面, 确认选择持久化且排序操作不发出 API 请求.
  let saved = "size-desc";
  globalThis.localStorage = { getItem: () => saved, setItem: (_, value) => { saved = value; } };
  const nodes = new Map(["#files", "#files-count", "#scan", "#files-sort"].map((selector) => [selector, new Element()]));
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector),
    createElement: (tag) => tag === "button" ? new Button() : new Element(),
  };
  let requests = 0;
  globalThis.fetch = async () => {
    requests++;
    return { ok: true, status: 200, json: async () => [
      { name: "small.mkv", path: "/media/small.mkv", size: 1, ddp_epi_id: null },
      { name: "large.mkv", path: "/media/large.mkv", size: 10, ddp_epi_id: null },
    ] };
  };
  const fail = (error) => assert.fail(String(error));
  const library = new Library(() => {}, fail);
  await library.load();
  const sort = nodes.get("#files-sort");
  assert.equal(sort.value, "size-desc");
  assert.equal(nodes.get("#files").children[0].dataset.filePath, "/media/large.mkv");
  sort.value = "size-asc";
  sort.onchange();
  assert.equal(saved, "size-asc");
  assert.equal(requests, 1);
  assert.equal(nodes.get("#files").children[0].dataset.filePath, "/media/small.mkv");
  const restored = new Library(() => {}, fail);
  assert.equal(sort.value, "size-asc");
  await restored.load();
  assert.equal(nodes.get("#files").children[0].dataset.filePath, "/media/small.mkv");
  saved = "invalid-old-option";
  new Library(() => {}, fail);
  assert.equal(sort.value, "path");
});
