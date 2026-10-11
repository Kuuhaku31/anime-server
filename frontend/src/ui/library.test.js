// 使用轻量 DOM 替身验证单文件重扫地址, 按钮状态和错误恢复.
import assert from "node:assert/strict";
import { test } from "node:test";
import { Library } from "./library.ts";

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
  const nodes = new Map(["#files", "#files-count", "#scan"].map((selector) => [selector, new Element()]));
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector),
    createElement: (tag) => tag === "button" ? new Button() : new Element(),
  };
  const file = {
    pre16mib_md5: "a".repeat(32), name: "episode.part.MKV", size: 100, ddp_epi_id: null,
    path: "/media/series", name_no_ext: "episode.part", extension: "MKV",
  };
  const copy = { ...file, path: "/media/copies" };
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
  const button = container.querySelectorAll()[0];
  container.click({ target: button });
  container.click({ target: button });
  assert.equal(button.disabled, true);
  assert.equal(button.textContent, "Scanning...");
  // 同指纹的另一地址是独立项目, 不因当前地址正在扫描而禁用.
  assert.equal(container.querySelectorAll()[1].disabled, false);
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
  assert.equal(container.querySelectorAll()[0].disabled, false);
  assert.equal(container.querySelectorAll()[0].dataset.path, "/media/series/episode.part.MKV");
});
