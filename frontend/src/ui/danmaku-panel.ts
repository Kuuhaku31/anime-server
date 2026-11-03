
// frontend/src/ui/danmaku-panel.ts
// 手动加载弹幕并同步时间列表, 为画面渲染器发布同一份只读数组.

import { loadDanmaku } from "../api/anime-server.ts";
import { formatTime } from "../playback/timeline.ts";
import type { DanmakuComment } from "../state/player-state.ts";
import { element } from "./controls.ts";

/** 管理当前媒体的弹幕请求和列表, 不修改播放控制器或缓存任务. */
export class DanmakuPanel extends EventTarget {
  /** 绑定手动加载与刷新, 页面隐藏时释放在途请求和布局观察. */
  constructor() {
    super();
    this.loadButton.onclick = () => { void this.load(false); };
    this.refreshButton.onclick = () => { void this.load(true); };
    this.resizeObserver = new ResizeObserver(() => this.followPosition(true));
    this.resizeObserver.observe(this.list);
    window.addEventListener("pagehide", () => {
      this.operation?.abort();
      this.operation = null;
      this.resizeObserver.disconnect();
      this.renderStatus();
    });
    window.addEventListener("pageshow", () => {
      this.resizeObserver.observe(this.list);
      this.followPosition(true);
    });
    this.renderStatus();
  }

  /** 暴露最近一次成功加载的原始数组, null 表示当前媒体尚未加载. */
  get data(): readonly DanmakuComment[] | null { return this.comments; }

  /** 同步媒体关联和播放位置, 切换媒体或关联时清空旧列表且不自动加载. */
  sync(path: string | null, episodeId: number | null, position: number): void {
    this.position = position;
    if (this.path !== path || this.episodeId !== episodeId) {
      this.operation?.abort();
      this.operation = null;
      this.path = path;
      this.episodeId = episodeId;
      this.comments = null;
      this.dispatchEvent(new Event("change"));
      this.rows = [];
      this.active = -1;
      this.error = "";
      this.list.replaceChildren();
      this.list.scrollTop = 0;
      this.renderStatus();
    }
    this.followPosition();
  }

  /** 弹幕区域, 加载状态不影响播放器自身的 aria-busy. */
  private panel = element<HTMLElement>("#danmaku-panel");
  /** 只在此容器中滚动, 不移动播放器或整页. */
  private list = element<HTMLOListElement>("#danmaku-list");
  /** 当前剧集 ID 标签. */
  private episode = element("#danmaku-episode");
  /** 已加载条数或当前操作说明. */
  private status = element("#danmaku-status");
  /** 局部错误区域, 不触发播放失败. */
  private diagnostic = element("#danmaku-error");
  /** 使用已有文件缓存的加载按钮. */
  private loadButton = element<HTMLButtonElement>("#danmaku-load");
  /** 显式跳过文件缓存的刷新按钮. */
  private refreshButton = element<HTMLButtonElement>("#danmaku-refresh");
  /** 检测全屏退出和尺寸改变, 恢复当前条目的可见位置. */
  private resizeObserver: ResizeObserver;
  /** 当前播放器选择的源路径, 相同剧集的不同路径也视为切换. */
  private path: string | null = null;
  /** 当前源路径已保存的剧集 ID. */
  private episodeId: number | null = null;
  /** 最近一次成功加载的数据, null 表示尚未加载. */
  private comments: DanmakuComment[] | null = null;
  /** 与弹幕索引一一对应的已渲染行, 时间更新不重建 DOM. */
  private rows: HTMLLIElement[] = [];
  /** 最近一条已到达弹幕的索引, -1 表示尚未到达首条. */
  private active = -1;
  /** 原媒体时间轴上的播放位置或 seek 目标. */
  private position = 0;
  /** 当前请求的取消控制器, 同时作为迟到响应的身份标记. */
  private operation: AbortController | null = null;
  /** 当前在途请求是否为强制刷新, 仅用于按钮文案. */
  private refreshing = false;
  /** 当前请求或响应校验错误, 成功后清空. */
  private error = "";

  /** 保留已显示列表直到新响应成功, 被切换替代的请求不能修改新状态. */
  private async load(refresh: boolean): Promise<void> {
    if (!this.path || this.episodeId === null || this.operation) return;
    const episodeId = this.episodeId;
    const operation = new AbortController();
    this.operation = operation;
    this.refreshing = refresh;
    this.error = "";
    this.renderStatus();
    try {
      const comments = await loadDanmaku(episodeId, refresh, operation.signal);
      if (operation.signal.aborted || this.operation !== operation) return;
      const fragment = document.createDocumentFragment();
      const rows = comments.map((comment) => this.row(comment));
      for (const row of rows) fragment.append(row);
      this.list.replaceChildren(fragment);
      this.comments = comments;
      this.dispatchEvent(new Event("change"));
      this.rows = rows;
      this.active = -1;
      this.followPosition(true);
    } catch (error) {
      if (!operation.signal.aborted && this.operation === operation) {
        this.error = error instanceof Error ? error.message : String(error);
      }
    } finally {
      if (this.operation === operation) {
        this.operation = null;
        this.renderStatus();
      }
    }
  }

  /** 只更新按钮和诊断, 播放位置改变不会重复宣读或重绘整份列表. */
  private renderStatus(): void {
    const pending = this.operation !== null;
    const available = this.path !== null && this.episodeId !== null;
    this.episode.textContent = `DandanPlay episode ID: ${this.episodeId ?? "Not associated"}`;
    this.loadButton.disabled = !available || pending;
    this.refreshButton.disabled = !available || pending;
    this.loadButton.textContent = pending && !this.refreshing ? "Loading..." : "Load comments";
    this.refreshButton.textContent = pending && this.refreshing ? "Refreshing..." : "Refresh comments";
    this.panel.setAttribute("aria-busy", String(pending));
    this.diagnostic.textContent = this.error;
    this.status.textContent = !this.path ? "Select a media file to load comments."
      : this.episodeId === null ? "Save or identify an episode ID in the media list first."
        : pending ? "Loading comments..."
          : this.comments === null ? "Comments not loaded. Use Load comments or Refresh comments."
            : this.comments.length ? `${this.comments.length} comments loaded. Following playback.`
              : "No comments in this episode.";
  }

  /** 以内容为左侧主元素, 右侧显示时间, 其余原始字段放在次要信息行. */
  private row(comment: DanmakuComment): HTMLLIElement {
    const row = document.createElement("li");
    row.className = "danmaku-row";
    row.dataset.time = String(comment.time);
    const content = document.createElement("span");
    content.className = "danmaku-content";
    content.textContent = comment.content;
    const time = document.createElement("span");
    time.className = "danmaku-time";
    const hundredths = Math.round(comment.time * 100);
    // 极大但有限的上游时间保留原始秒数, 避免百分秒计算溢出或精度丢失.
    time.textContent = Number.isSafeInteger(hundredths)
      ? `${formatTime(hundredths / 100)}.${String(hundredths % 100).padStart(2, "0")}`
      : `${comment.time} s`;
    time.title = `${comment.time} seconds`;
    const details = document.createElement("div");
    details.className = "danmaku-details";
    const mode = document.createElement("span");
    const name = comment.mode === 1 ? "Scrolling" : comment.mode === 4 ? "Bottom"
      : comment.mode === 5 ? "Top" : "Unknown";
    mode.textContent = `Mode: ${name} (${comment.mode})`;
    const color = document.createElement("span");
    color.className = "danmaku-color";
    const hex = `#${comment.color.toString(16).padStart(6, "0").toUpperCase()}`;
    const swatch = document.createElement("span");
    swatch.className = "danmaku-swatch";
    swatch.style.backgroundColor = hex;
    swatch.setAttribute("aria-hidden", "true");
    color.append(swatch, document.createTextNode(hex));
    const sender = document.createElement("span");
    sender.textContent = `Sender: ${comment.sender || "(empty)"}`;
    const id = document.createElement("span");
    id.textContent = `cid: ${comment.comment_id ?? "unavailable"}`;
    details.append(mode, color, sender, id);
    row.append(content, time, details);
    return row;
  }

  /** 二分定位最近已到达条目, 只滚动弹幕容器, seek 不逐条补播中间位置. */
  private followPosition(force = false): void {
    const comments = this.comments;
    if (!comments) return;
    let left = 0;
    let right = comments.length;
    while (left < right) {
      const middle = Math.floor((left + right) / 2);
      if (comments[middle].time <= this.position) left = middle + 1;
      else right = middle;
    }
    const active = left - 1;
    if (active === this.active && !force) return;
    if (this.active >= 0) {
      this.rows[this.active].classList.remove("current");
      this.rows[this.active].removeAttribute("aria-current");
    }
    this.active = active;
    if (active < 0) { this.list.scrollTop = 0; return; }
    const row = this.rows[active];
    row.classList.add("current");
    row.setAttribute("aria-current", "true");
    if (!this.list.clientHeight) return;
    const bounds = this.list.getBoundingClientRect();
    const target = row.getBoundingClientRect();
    if (target.top < bounds.top) this.list.scrollTop += target.top - bounds.top;
    else if (target.bottom > bounds.bottom) {
      this.list.scrollTop += Math.min(target.top - bounds.top, target.bottom - bounds.bottom);
    }
  }
}
