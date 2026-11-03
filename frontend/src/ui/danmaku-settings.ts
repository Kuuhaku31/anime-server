
// frontend/src/ui/danmaku-settings.ts
// 管理画面弹幕设置草稿, 当前页面预览和服务端持久化操作.

import { saveDanmakuSettings } from "../api/anime-server.ts";
import type { DanmakuSettings } from "../state/player-state.ts";
import { element } from "./controls.ts";

/** 分离设置草稿, 当前预览和已保存基线, 保存失败不撤销已有预览. */
export class DanmakuSettingsPanel extends EventTarget {
  /** 绑定独立设置表单, 不接管播放键盘或手动弹幕加载请求. */
  constructor() {
    super();
    this.openButton.onclick = () => this.setOpen(this.form.hidden);
    element<HTMLButtonElement>("#danmaku-settings-close").onclick = () => this.setOpen(false);
    this.toggle.onclick = () => {
      if (!this.applied || this.saving) return;
      const settings = { ...this.applied, enabled: !this.applied.enabled };
      this.enabled.checked = settings.enabled;
      this.apply(settings);
    };
    this.form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (this.saving) return;
      try { this.apply(this.readDraft()); }
      catch (error) { this.showError(error); }
    });
    this.saveButton.onclick = () => { void this.save(); };
    this.form.addEventListener("input", () => {
      this.diagnostic.textContent = "";
      this.status.textContent = "Draft changed. Apply to preview or Save to keep these settings.";
    });
    this.form.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      this.setOpen(false);
    });
    this.panel.addEventListener("pointerdown", (event) => {
      if (event.target instanceof Node && !this.form.contains(event.target) && !this.openButton.contains(event.target)) {
        this.setOpen(false, false);
      }
    });
    window.addEventListener("pagehide", () => this.setOpen(false, false));
  }

  /** 返回当前页面已经应用的设置, 编辑输入框不会立即改变这个值. */
  get value(): Readonly<DanmakuSettings> | null { return this.applied; }

  /** 首次读取配置后启用交互, 不使用可能与服务端不同的静默默认值. */
  initialize(settings: DanmakuSettings): void {
    validate(settings);
    this.saved = { ...settings };
    this.writeDraft(settings);
    this.fields.disabled = false;
    this.toggle.disabled = false;
    this.apply(settings);
  }

  /** 保持设置与播放器处于同一全屏元素内. */
  private panel = element<HTMLElement>(".player-panel");
  /** 当前页面启用状态的快捷切换按钮. */
  private toggle = element<HTMLButtonElement>("#danmaku-toggle");
  /** 设置弹层入口, 未加载配置时仍可打开查看说明. */
  private openButton = element<HTMLButtonElement>("#danmaku-settings-toggle");
  /** 独立弹层表单, 关闭时保留未应用的草稿. */
  private form = element<HTMLFormElement>("#danmaku-settings");
  /** 请求期间整体禁用字段和 Apply, Save 按钮. */
  private fields = element<HTMLFieldSetElement>("#danmaku-settings-fields");
  /** 启用状态草稿. */
  private enabled = element<HTMLInputElement>("#danmaku-enabled");
  /** 字号草稿, 单位为 CSS 像素. */
  private fontSize = element<HTMLInputElement>("#danmaku-font-size");
  /** 描边宽度草稿. */
  private outline = element<HTMLInputElement>("#danmaku-outline");
  /** 不透明度草稿. */
  private opacity = element<HTMLInputElement>("#danmaku-opacity");
  /** 滚动显示区域百分比草稿. */
  private area = element<HTMLInputElement>("#danmaku-area");
  /** 视频时间偏移秒数草稿. */
  private offset = element<HTMLInputElement>("#danmaku-offset");
  /** 持久化操作入口, 不与表单提交混淆. */
  private saveButton = element<HTMLButtonElement>("#danmaku-save");
  /** 本地预览和持久化结果说明. */
  private status = element("#danmaku-settings-status");
  /** 校验和保存错误, 不触发播放器故障. */
  private diagnostic = element("#danmaku-settings-error");
  /** 当前页面已经应用的设置. */
  private applied: DanmakuSettings | null = null;
  /** 最近一次服务端确认的持久化基线. */
  private saved: DanmakuSettings | null = null;
  /** 是否存在在途保存请求, 避免旧响应覆盖较新的草稿. */
  private saving = false;

  /** 打开设置时保持全屏控件可见, Escape 先关闭表单再允许退出全屏. */
  private setOpen(open: boolean, restoreFocus = true): void {
    const changed = this.form.hidden === open;
    this.form.hidden = !open;
    this.openButton.setAttribute("aria-expanded", String(open));
    this.panel.classList.toggle("danmaku-settings-open", open);
    if (!changed) return;
    this.panel.dispatchEvent(new Event("danmaku-settings-change"));
    if (open) element<HTMLButtonElement>("#danmaku-settings-close").focus({ preventScroll: true });
    else if (restoreFocus) this.openButton.focus({ preventScroll: true });
  }

  /** 从数字控件读取完整设置, 空值和无穷值都显式报错. */
  private readDraft(): DanmakuSettings {
    const settings = {
      enabled: this.enabled.checked,
      font_size: this.fontSize.valueAsNumber,
      outline_width: this.outline.valueAsNumber,
      opacity: this.opacity.valueAsNumber,
      display_area_percent: this.area.valueAsNumber,
      time_offset_seconds: this.offset.valueAsNumber,
    };
    validate(settings);
    return settings;
  }

  /** 只在初始化或成功保存后使用权威值回填整份草稿. */
  private writeDraft(settings: DanmakuSettings): void {
    this.enabled.checked = settings.enabled;
    this.fontSize.value = String(settings.font_size);
    this.outline.value = String(settings.outline_width);
    this.opacity.value = String(settings.opacity);
    this.area.value = String(settings.display_area_percent);
    this.offset.value = String(settings.time_offset_seconds);
  }

  /** 只更新当前页面并通知渲染器, 不发送请求或写浏览器持久存储. */
  private apply(settings: DanmakuSettings): void {
    this.applied = { ...settings };
    this.toggle.setAttribute("aria-pressed", String(settings.enabled));
    this.toggle.title = settings.enabled ? "Hide on-screen comments" : "Show on-screen comments";
    this.toggle.setAttribute("aria-label", this.toggle.title);
    this.diagnostic.textContent = "";
    const saved = this.saved;
    const persisted = saved && settings.enabled === saved.enabled && settings.font_size === saved.font_size
      && settings.outline_width === saved.outline_width && settings.opacity === saved.opacity
      && settings.display_area_percent === saved.display_area_percent && settings.time_offset_seconds === saved.time_offset_seconds;
    this.status.textContent = persisted ? "Using saved settings."
      : "Applied to this page only. Use Save to keep these settings.";
    this.dispatchEvent(new Event("change"));
  }

  /** 保存成功后才更新预览和基线, 失败保留输入与原先已应用效果. */
  private async save(): Promise<void> {
    if (this.saving || !this.applied) return;
    try {
      const settings = this.readDraft();
      this.saving = true;
      this.fields.disabled = true;
      this.toggle.disabled = true;
      this.form.setAttribute("aria-busy", "true");
      this.diagnostic.textContent = "";
      this.status.textContent = "Saving settings...";
      const saved = await saveDanmakuSettings(settings);
      validate(saved);
      this.saved = { ...saved };
      this.writeDraft(saved);
      this.apply(saved);
      this.status.textContent = "Saved to the server and applied. Other pages update after reload.";
    } catch (error) {
      this.status.textContent = "Not saved. The current preview is unchanged.";
      this.showError(error);
    } finally {
      this.saving = false;
      this.fields.disabled = this.applied === null;
      this.toggle.disabled = this.applied === null;
      this.form.setAttribute("aria-busy", "false");
    }
  }

  /** 错误只显示在设置表单内, 视频播放不因配置保存失败而中断. */
  private showError(error: unknown): void {
    this.diagnostic.textContent = error instanceof Error ? error.message : String(error);
  }
}

/** 与后端保持相同的完整设置和数值边界, 不用空值或默认值掩盖错误. */
function validate(settings: DanmakuSettings): void {
  if (!settings || typeof settings.enabled !== "boolean") throw new Error("Enabled must be a boolean.");
  for (const [name, value, minimum, maximum] of [
    ["Font size", settings.font_size, 12, 72],
    ["Outline width", settings.outline_width, 0, 6],
    ["Opacity", settings.opacity, 0.1, 1],
    ["Scrolling area", settings.display_area_percent, 10, 100],
    ["Time offset", settings.time_offset_seconds, -120, 120],
  ] as const) {
    if (!Number.isFinite(value) || value < minimum || value > maximum) {
      throw new Error(`${name} must be a finite number between ${minimum} and ${maximum}.`);
    }
  }
}
