
// frontend/src/ui/library.test.js
// 验证媒体列表只在展开时请求 metadata 并仅创建缓存.

import assert from "node:assert/strict";
import { test } from "node:test";
import { initialSelection, Library, sortFiles } from "./library.ts";

/** 提供 UI 测试需要的轻量 DOM 节点和事件委托. */
class Element {
  /** 节点名称. */
  tagName;
  /** 当前子节点. */
  children = [];
  /** 行, 按钮和选择框的数据属性. */
  dataset = {};
  /** 可见文本. */
  textContent = "";
  /** 输入控件的值. */
  value = "";
  /** 控件禁用状态. */
  disabled = false;
  /** 原生 details 的展开状态. */
  open = false;
  /** 保存无障碍和选择属性. */
  attributes = {};
  /** 当前标题提示. */
  title = "";
  /** 原生类名状态接口. */
  classList = { toggle() {} };
  /** 记录事件委托监听器. */
  listeners = new Map();
  /** 保存父节点以支持 contains. */
  parent = null;

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

  /** 在测试中向委托回调分发带目标的事件. */
  emit(type, target) {
    this.listeners.get(type)?.({
      target,
      preventDefault() { this.defaultPrevented = true; },
    });
  }

  /** 保存辅助文本或无障碍属性. */
  setAttribute(name, value) { this.attributes[name] = value; }

  /** 判断节点是否包含指定后代. */
  contains(target) {
    return target === this || this.children.some((child) => child.contains(target));
  }

  /** 查询符合当前测试选择器的祖先. */
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

  /** 按谓词查找当前子树中的全部节点. */
  findAll(predicate) {
    return this.children.flatMap((child) => [
      ...(predicate(child) ? [child] : []),
      ...child.findAll(predicate),
    ]);
  }
}

/** 区分真实按钮类型检查. */
class Button extends Element {
  /** 初始化为按钮标签. */
  constructor() { super("button"); }
}

/** 区分真实选择框类型检查. */
class Select extends Element {
  /** 初始化为选择框标签. */
  constructor() { super("select"); }
}

/** 构造流元数据样本. */
function stream(index, codec = "h264", language = "eng", isDefault = false, isForced = false) {
  return { stream_index: index, codec_name: codec, language, title: null, is_default: isDefault, is_forced: isForced };
}

/** 构造完整媒体 metadata 响应. */
function info(position = null, configs = {}) {
  return {
    metadata: {
      duration: 120, format: "matroska",
      video_streams: [stream(4), stream(8, "hevc")],
      audio_streams: [stream(2, "aac"), stream(6, "opus")],
      subtitle_streams: [stream(3, "ass"), stream(7, "subrip", "jpn", false, true)],
    },
    configs: { position, video_stream_index: null, audio_stream_index: null, subtitle_stream_index: null, ...configs },
  };
}

/** 构造数据库返回的轻量路径列表条目. */
function entry(path, fileId, position = null) {
  return {
    file_id: fileId, path,
    configs: { position, video_stream_index: null, audio_stream_index: null, subtitle_stream_index: null },
  };
}

/** 安装媒体列表需要的最小 DOM 环境. */
function dom() {
  globalThis.Element = Element;
  globalThis.HTMLButtonElement = Button;
  globalThis.HTMLSelectElement = Select;
  const nodes = new Map(["#files", "#files-count", "#scan", "#files-sort"]
    .map((selector) => [selector, new Element(selector === "#files-sort" ? "select" : "div")]));
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector),
    createElement: (tag) => tag === "button" ? new Button()
      : tag === "select" ? new Select() : new Element(tag),
  };
  return nodes;
}

/** 查找某路径的详情行. */
function row(container, path) {
  return container.children.find((child) => child.dataset.filePath === path);
}

/** 查找展开行内指定轨道选择框. */
function select(rowElement, kind) {
  return rowElement.findAll((child) => child instanceof Select && child.dataset.stream === kind)[0];
}

/** 查找展开行内指定操作按钮. */
function action(rowElement, name) {
  return rowElement.findAll((child) => child instanceof Button && child.dataset.action === name)[0];
}

/** 等待异步 metadata 渲染完成. */
async function until(predicate) {
  const deadline = Date.now() + 2000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, "metadata request timed out");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

// 未保存配置选每类首条流, 已保存 null 保留显式关闭.
test("initial selection prefers saved values and otherwise chooses each first stream", () => {
  assert.deepEqual(initialSelection(info()), {
    video_stream_index: 4, audio_stream_index: 2, subtitle_stream_index: 3,
  });
  assert.deepEqual(initialSelection(info(10, {
    video_stream_index: 8, audio_stream_index: null, subtitle_stream_index: 7,
  })), {
    video_stream_index: 8, audio_stream_index: null, subtitle_stream_index: 7,
  });
  const noVideo = info();
  noVideo.metadata.video_streams = [];
  assert.equal(initialSelection(noVideo), null);
});

// 折叠媒体只返回轻量字段, 展开后再请求 metadata, 重复内容请求共享.
test("metadata loads on expand and cache creation sends the chosen query", async () => {
  const nodes = dom();
  globalThis.localStorage = { getItem: () => "path", setItem() {} };
  const first = entry("/media/series/episode.mkv", "a".repeat(32));
  const second = entry("/media/copies/episode.mkv", "a".repeat(32));
  const queries = [];
  const requests = [];
  const errors = [];
  globalThis.fetch = async (url) => {
    requests.push(url);
    if (url === "/api/media") return Response.json([first, second]);
    assert.equal(url, `/api/media/${first.file_id}/metadata`);
    return Response.json(info());
  };
  const library = new Library(async (query) => { queries.push(query); }, (error) => errors.push(error));
  await library.load();
  const container = nodes.get("#files");
  assert.equal(container.children.length, 2);
  assert.equal(container.children.every((item) => item.tagName === "DETAILS" && !item.open), true);
  assert.deepEqual(requests, ["/api/media"]);

  container.emit("click", row(container, first.path).children[0]);
  container.emit("click", row(container, second.path).children[0]);
  await until(() => select(row(container, first.path), "video"));
  assert.equal(requests.filter((url) => url.endsWith("/metadata")).length, 1);
  assert.equal(select(row(container, first.path), "video").value, "4");
  assert.equal(select(row(container, first.path), "audio").value, "2");
  assert.equal(select(row(container, first.path), "subtitle").value, "3");

  const audio = select(row(container, first.path), "audio");
  audio.value = "off";
  container.emit("change", audio);
  container.emit("click", action(row(container, first.path), "build"));
  await new Promise(setImmediate);
  assert.deepEqual(queries, [{
    path: first.path, video_stream_index: 4, audio_stream_index: null, subtitle_stream_index: 3,
  }]);
  assert.deepEqual(errors, []);
});

// metadata 请求失败只影响对应行, 可显式重试.
test("metadata errors are local and can be retried", async () => {
  const nodes = dom();
  globalThis.localStorage = { getItem: () => "name-asc", setItem() {} };
  const file = entry("/media/series/episode.mkv", "b".repeat(32));
  let metadataCalls = 0;
  globalThis.fetch = async (url) => {
    if (url === "/api/media") return Response.json([file]);
    metadataCalls++;
    return metadataCalls === 1
      ? new Response("cannot read media file", { status: 422 })
      : Response.json(info());
  };
  const library = new Library(async () => {}, () => {});
  await library.load();
  const container = nodes.get("#files");
  container.emit("click", row(container, file.path).children[0]);
  await until(() => action(row(container, file.path), "metadata"));
  assert.match(row(container, file.path).findAll((child) => child.className === "item-error")[0].textContent,
    /cannot read media file/);
  container.emit("click", action(row(container, file.path), "metadata"));
  await until(() => select(row(container, file.path), "video"));
  assert.equal(metadataCalls, 2);
});

// 排序依据轻量路径和已加载的 metadata, 不修改源数组.
test("entries sort by name and loaded playback data without mutation", () => {
  const files = [
    entry("/media/b/episode10.mkv", "c".repeat(32)),
    entry("/media/a/episode2.mkv", "b".repeat(32), 4),
    entry("/media/c/episode1.mkv", "a".repeat(32), 16),
  ];
  const paths = (order) => sortFiles(files, order).map((file) => file.path.split("/")[2]);
  assert.deepEqual(paths("path"), ["a", "b", "c"]);
  assert.deepEqual(paths("name-asc"), ["c", "a", "b"]);
  assert.deepEqual(paths("name-desc"), ["b", "a", "c"]);
  assert.deepEqual(paths("duration-asc"), ["a", "b", "c"]);
  assert.deepEqual(paths("duration-desc"), ["a", "b", "c"]);
  assert.deepEqual(paths("progress-desc"), ["c", "a", "b"]);
  assert.equal(files[0].path, "/media/b/episode10.mkv");
});
