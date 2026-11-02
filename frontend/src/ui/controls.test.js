
// frontend/src/ui/controls.test.js
// 验证全屏控件空闲隐藏, 单双击区分和模式退出清理.

import assert from "node:assert/strict";
import { test } from "node:test";
import { attachControls } from "./controls.ts";

/** 提供控件测试所需的 DOM 节点和焦点状态. */
class Element extends EventTarget {
  /** 节点上的样式标记. */
  classList = classList();
  /** 播放图标使用的数据属性. */
  dataset = {};
  /** 无障碍属性的当前值. */
  attributes = {};
  /** 当前可见标签. */
  textContent = "";
  /** 输入控件的值. */
  value = "";
  /** 输入控件的禁用状态. */
  disabled = false;
  /** 模拟直接父节点. */
  parent = null;
  /** 模拟鼠标停留在节点内. */
  hovered = false;
  /** 模拟键盘可见焦点. */
  keyboardFocus = false;
  /** 原生 onclick 处理器. */
  onclick = null;
  /** 接收时间轴样式更新, 无需在单元测试中计算布局. */
  style = {
    /** 接收 CSS 自定义属性. */
    setProperty() {},
  };

  /** 记录无障碍标签和状态. */
  setAttribute(name, value) { this.attributes[name] = value; }

  /** 判断节点与目标的祖先关系. */
  contains(target) {
    for (let node = target; node; node = node.parent) if (node === this) return true;
    return false;
  }

  /** 提供控件自动隐藏使用的交互伪类. */
  matches(selector) { return selector === ":hover" ? this.hovered : this.keyboardFocus; }

  /** 把测试文档焦点移到当前节点. */
  focus() { document.activeElement = this; }

  /** 执行可用按钮的原生点击处理器. */
  click() { if (!this.disabled) this.onclick?.({ detail: 1 }); }
}

/** 区分键盘事件中的输入框. */
class Input extends Element {}
/** 区分键盘事件中的选择框. */
class Select extends Element {}
/** 区分键盘事件中的按钮. */
class Button extends Element {}

/** 模拟 DOMTokenList, 保留批量移除和强制切换语义. */
function classList() {
  const values = new Set();
  return {
    /** 添加多个状态标记. */
    add(...names) { names.forEach((name) => values.add(name)); },
    /** 移除多个状态标记. */
    remove(...names) { names.forEach((name) => values.delete(name)); },
    /** 判断状态标记是否存在. */
    contains(name) { return values.has(name); },
    /** 根据条件设置状态标记. */
    toggle(name, force = !values.has(name)) {
      if (force) values.add(name);
      else values.delete(name);
      return force;
    },
  };
}

/** 分发带鼠标或键盘字段的原生 Event, 可指定模拟冒泡目标. */
function emit(target, type, properties = {}) {
  const event = new Event(type, { cancelable: true });
  for (const [key, value] of Object.entries(properties)) Object.defineProperty(event, key, { value });
  target.dispatchEvent(event);
  return event;
}

/** 等待全屏 Promise 和焦点微任务完成. */
async function settle() { await new Promise(setImmediate); }

/** 安装最小页面和控制器, 不依赖真实解码器或额外测试框架. */
function setup(context) {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  globalThis.HTMLInputElement = Input;
  globalThis.HTMLSelectElement = Select;
  globalThis.HTMLButtonElement = Button;
  const selectors = [
    "#toggle-play", "#play-label", "#seek", "#clock", "#volume", "#mute", "#mute-label",
    "#fullscreen", "#fullscreen-label", "#fullscreen-title", ".player-panel", ".player-controls",
    "#stop", "#restart", "#retry", "#retry-probe", "#playback-status", "#error", "#error-actions", "#meta",
  ];
  const buttons = new Set(["#toggle-play", "#mute", "#fullscreen", "#stop", "#restart", "#retry", "#retry-probe"]);
  const nodes = new Map(selectors.map((selector) => [selector,
    buttons.has(selector) ? new Button() : ["#seek", "#volume"].includes(selector) ? new Input() : new Element()]));
  globalThis.document = Object.assign(new EventTarget(), {
    body: new Element(), fullscreenElement: null, activeElement: null,
    /** 返回已安装的必需页面节点. */
    querySelector(selector) { return nodes.get(selector); },
    /** 模拟用户退出原生全屏时的浏览器事件. */
    async exitFullscreen() {
      this.fullscreenElement = null;
      emit(this, "fullscreenchange");
    },
  });
  globalThis.window = new EventTarget();
  const panel = nodes.get(".player-panel");
  const controls = nodes.get(".player-controls");
  controls.parent = panel;
  for (const selector of ["#seek", "#volume", "#toggle-play", "#mute", "#fullscreen"]) {
    nodes.get(selector).parent = controls;
  }
  /** 模拟浏览器批准当前播放器的全屏请求. */
  panel.requestFullscreen = async () => {
    document.fullscreenElement = panel;
    emit(document, "fullscreenchange");
  };
  const video = Object.assign(new Element(), { paused: false, ended: false, volume: 1, muted: false, parent: panel });
  const controller = Object.assign(new EventTarget(), {
    file: ["/media/series/episode.mkv", {}], info: {}, duration: 120, position: 30, buffered: 60,
    pending: false, error: "", message: "Playing",
    toggle: context.mock.fn(() => {
      video.paused = !video.paused;
      emit(video, video.paused ? "pause" : "play");
    }),
    seek: context.mock.fn((position) => {
      controller.position = position;
      emit(controller, "change");
    }),
    stop: context.mock.fn(),
    retry: context.mock.fn(),
    fail: context.mock.fn((error) => {
      controller.error = error.message;
      emit(controller, "change");
    }),
  });
  attachControls(controller, video);
  return { nodes, panel, controls, video, controller };
}

// 双击只切换全屏, 不留下稍后执行的播放切换.
test("video single click toggles playback while double click only toggles fullscreen", async (context) => {
  const { nodes, panel, video, controller } = setup(context);
  emit(video, "click", { detail: 1 });
  context.mock.timers.tick(299);
  assert.equal(controller.toggle.mock.callCount(), 0);
  context.mock.timers.tick(1);
  assert.equal(controller.toggle.mock.callCount(), 1);
  assert.equal(nodes.get("#play-label").textContent, "Play");

  emit(video, "click", { detail: 1 });
  context.mock.timers.tick(50);
  emit(video, "click", { detail: 2 });
  emit(video, "dblclick");
  await settle();
  context.mock.timers.tick(4000);
  assert.equal(controller.toggle.mock.callCount(), 1);
  assert.equal(document.fullscreenElement, panel);
  assert.equal(panel.classList.contains("controls-hidden"), false);
  assert.equal(nodes.get("#fullscreen").attributes["aria-pressed"], "true");
  assert.equal(nodes.get("#fullscreen-title").textContent, "episode.mkv");
});

// 后台时间更新不重置空闲期限, 但悬停, 拖动, 焦点和暂停会保持控件可见.
test("fullscreen controls hide after idle playback and stay visible during interaction", async (context) => {
  const { nodes, panel, controls, video, controller } = setup(context);
  nodes.get("#fullscreen").click();
  await settle();
  for (let i = 0; i < 3; i++) {
    context.mock.timers.tick(1000);
    emit(controller, "change");
  }
  assert.equal(panel.classList.contains("controls-hidden"), true);
  emit(panel, "pointermove");
  assert.equal(panel.classList.contains("controls-hidden"), false);
  emit(controls, "pointerenter");
  context.mock.timers.tick(4000);
  assert.equal(panel.classList.contains("controls-hidden"), false);
  emit(controls, "pointerleave");
  const seek = nodes.get("#seek");
  seek.value = "77";
  emit(seek, "input");
  emit(controller, "change");
  context.mock.timers.tick(4000);
  assert.equal(seek.value, "77");
  assert.equal(nodes.get("#clock").textContent, "1:17 / 2:00");
  assert.equal(panel.classList.contains("controls-hidden"), false);
  emit(seek, "change");
  assert.equal(controller.seek.mock.calls[0].arguments[0], 77);

  const play = nodes.get("#toggle-play");
  play.keyboardFocus = true;
  play.focus();
  emit(panel, "focusin");
  context.mock.timers.tick(4000);
  assert.equal(panel.classList.contains("controls-hidden"), false);
  panel.focus();
  emit(panel, "focusout");
  await settle();
  context.mock.timers.tick(3000);
  assert.equal(panel.classList.contains("controls-hidden"), true);
  video.paused = true;
  emit(video, "pause");
  context.mock.timers.tick(4000);
  assert.equal(panel.classList.contains("controls-hidden"), false);
  video.paused = false;
  emit(video, "play");
  controller.pending = true;
  emit(controller, "change");
  context.mock.timers.tick(4000);
  assert.equal(panel.classList.contains("controls-hidden"), false);
  controller.pending = false;
  emit(controller, "change");
  context.mock.timers.tick(3000);
  assert.equal(panel.classList.contains("controls-hidden"), true);
  await document.exitFullscreen();
  context.mock.timers.tick(4000);
  assert.equal(panel.classList.contains("controls-hidden"), false);
  assert.equal(nodes.get("#fullscreen").attributes["aria-pressed"], "false");
});

// 原生 API 被拒绝时使用页内全屏, Escape 即使来自输入框也能清理滚动锁.
test("fallback fullscreen exits from focused controls and pagehide cancels pending gestures", async (context) => {
  const { nodes, panel, video, controller } = setup(context);
  context.mock.method(panel, "requestFullscreen", async () => { throw new Error("Fullscreen denied"); });
  context.mock.method(console, "info", () => {});
  nodes.get("#fullscreen").click();
  await settle();
  assert.equal(panel.classList.contains("expanded"), true);
  assert.equal(document.body.classList.contains("player-expanded"), true);
  const escape = emit(panel, "keydown", { key: "Escape", target: nodes.get("#volume") });
  await settle();
  assert.equal(escape.defaultPrevented, true);
  assert.equal(panel.classList.contains("expanded"), false);
  assert.equal(document.body.classList.contains("player-expanded"), false);
  context.mock.timers.tick(4000);
  assert.equal(panel.classList.contains("controls-hidden"), false);

  nodes.get("#fullscreen").click();
  await settle();
  emit(video, "click", { detail: 1 });
  emit(window, "pagehide");
  context.mock.timers.tick(4000);
  assert.equal(controller.toggle.mock.callCount(), 0);
  assert.equal(panel.classList.contains("expanded"), false);
  assert.equal(document.body.classList.contains("player-expanded"), false);
});

// 全屏退出失败仍显示错误和恢复入口, 不让自动隐藏遮住诊断.
test("fullscreen failures restore visible controls and playback feedback", async (context) => {
  const { nodes, panel, controller } = setup(context);
  nodes.get("#fullscreen").click();
  await settle();
  context.mock.timers.tick(3000);
  assert.equal(panel.classList.contains("controls-hidden"), true);
  context.mock.method(document, "exitFullscreen", async () => { throw new Error("Cannot exit fullscreen"); });
  nodes.get("#fullscreen").click();
  await settle();
  assert.equal(controller.fail.mock.callCount(), 1);
  assert.equal(panel.classList.contains("fullscreen-error"), true);
  assert.equal(nodes.get("#error").textContent, "Cannot exit fullscreen");
  assert.equal(nodes.get("#error-actions").hidden, false);
  context.mock.timers.tick(4000);
  assert.equal(panel.classList.contains("controls-hidden"), false);
});
