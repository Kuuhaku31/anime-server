
// frontend/src/playback/danmaku-renderer.ts
// 在真实视频内容矩形内绘制弹幕, 同步媒体帧, seek 和全屏尺寸变化.

import type { DanmakuComment, DanmakuSettings } from "../state/player-state.ts";
import { DanmakuLayout } from "./danmaku-layout.ts";

/** 管理 Canvas 生命周期, 只消费已经手动加载的共享弹幕数组. */
export class DanmakuRenderer {
  /** 绑定媒体和布局事件, 渲染能力缺失时只报告弹幕局部错误. */
  constructor(video: HTMLVideoElement, canvas: HTMLCanvasElement, report: (message: string) => void) {
    this.video = video;
    this.canvas = canvas;
    this.context = canvas.getContext("2d");
    if (!this.context) report("Canvas 2D is unavailable. The comment list is still available.");
    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(video);
    video.addEventListener("loadedmetadata", () => this.resize());
    video.addEventListener("resize", () => this.resize());
    video.addEventListener("emptied", () => { this.waiting = false; this.mediaTime = null; this.rebuild(); });
    video.addEventListener("seeking", () => {
      this.waiting = false;
      this.mediaTime = null;
      this.cancelFrame();
      this.layout?.reset();
      this.clear();
    });
    video.addEventListener("seeked", () => { this.waiting = false; this.layout?.reset(); this.update(); });
    video.addEventListener("waiting", () => { this.waiting = true; this.cancelFrame(); });
    video.addEventListener("playing", () => { this.waiting = false; this.update(); });
    for (const event of ["play", "pause", "ended", "ratechange", "loadeddata", "timeupdate"]) {
      video.addEventListener(event, () => this.update());
    }
    window.addEventListener("resize", () => this.resize());
    window.addEventListener("orientationchange", () => this.resize());
    window.visualViewport?.addEventListener("resize", () => this.resize());
    document.addEventListener("visibilitychange", () => {
      this.layout?.reset();
      this.update();
    });
    window.addEventListener("pagehide", () => {
      this.activePage = false;
      this.cancelFrame();
      this.resizeObserver.disconnect();
      this.clear();
    });
    window.addEventListener("pageshow", () => {
      this.activePage = true;
      this.resizeObserver.observe(video);
      this.resize();
    });
  }

  /** 替换数据源时清除旧轨道, null 表示当前媒体尚未手动加载. */
  setComments(comments: readonly DanmakuComment[] | null): void {
    if (this.comments === comments) return;
    this.comments = comments;
    this.rebuild();
  }

  /** 应用已校验设置, 时间偏移和字号等变更都从当前时间重建. */
  configure(settings: Readonly<DanmakuSettings>): void {
    this.settings = { ...settings };
    this.rebuild();
  }

  /** 控制器等待或卸载时禁用画面层, 避免在旧帧上显示新媒体的弹幕. */
  sync(available: boolean): void {
    if (this.available === available) return;
    this.available = available;
    this.layout?.reset();
    this.update();
  }

  /** 当前浏览器媒体元素. */
  private video: HTMLVideoElement;
  /** 位于视频上方且不接收输入的透明覆盖层. */
  private canvas: HTMLCanvasElement;
  /** Canvas 绘图能力, 缺失时保持列表和播放功能可用. */
  private context: CanvasRenderingContext2D | null;
  /** 监测视频容器的布局变化. */
  private resizeObserver: ResizeObserver;
  /** 与列表共用的只读数组引用. */
  private comments: readonly DanmakuComment[] | null = null;
  /** 当前页面已应用的弹幕设置, 未读取服务端配置前不绘制. */
  private settings: Readonly<DanmakuSettings> | null = null;
  /** 当前字体和几何尺寸下的轨道状态. */
  private layout: DanmakuLayout | null = null;
  /** 真实视频内容矩形的 CSS 像素宽度. */
  private width = 0;
  /** 真实视频内容矩形的 CSS 像素高度. */
  private height = 0;
  /** 浏览器实际使用的 Canvas 像素倍率. */
  private ratio = 1;
  /** 控制器是否已挂载当前媒体且没有操作等待. */
  private available = false;
  /** 最近一次媒体事件是否进入缓冲等待. */
  private waiting = false;
  /** 最近一个可绘制媒体帧的时间, 缓冲期间保留静止弹幕而不是清屏. */
  private mediaTime: number | null = null;
  /** 页面是否仍处于可恢复的活动生命周期中. */
  private activePage = true;
  /** 已排定的媒体帧回调, 暂停和卸载时取消. */
  private videoFrame: number | null = null;
  /** 不支持媒体帧回调时使用的动画帧回调. */
  private animationFrame: number | null = null;

  /** 按 object-fit: contain 计算黑边之外的尺寸, DPR 上限避免移动端过大的画布. */
  private resize(): void {
    const bounds = this.video.getBoundingClientRect();
    const scale = this.video.videoWidth && this.video.videoHeight
      ? Math.min(bounds.width / this.video.videoWidth, bounds.height / this.video.videoHeight) : 0;
    const width = this.video.videoWidth * scale;
    const height = this.video.videoHeight * scale;
    const ratio = Math.max(1, Math.min(3, window.devicePixelRatio || 1));
    this.canvas.style.left = `${(bounds.width - width) / 2}px`;
    this.canvas.style.top = `${(bounds.height - height) / 2}px`;
    if (width === this.width && height === this.height && ratio === this.ratio) {
      this.update();
      return;
    }
    this.width = width;
    this.height = height;
    this.ratio = ratio;
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${height}px`;
    this.canvas.width = Math.round(width * ratio);
    this.canvas.height = Math.round(height * ratio);
    this.rebuild();
  }

  /** 字体先作用于 measureText, 重新排轨不保存另一份格式化弹幕数组. */
  private rebuild(): void {
    this.cancelFrame();
    const context = this.context;
    const settings = this.settings;
    this.layout = null;
    if (context && settings && this.comments && this.width > 0 && this.height > 0) {
      context.setTransform(this.canvas.width / this.width, 0, 0, this.canvas.height / this.height, 0, 0);
      context.font = `600 ${settings.font_size}px system-ui, "PingFang SC", "Microsoft YaHei", sans-serif`;
      context.textBaseline = "middle";
      context.lineJoin = "round";
      // Canvas 描边位于字形边缘两侧, 乘二后外侧宽度才等于设置值.
      context.lineWidth = settings.outline_width * 2;
      context.globalAlpha = settings.opacity;
      this.layout = new DanmakuLayout(this.comments, this.width, this.height, settings,
        (text) => context.measureText(text).width);
    }
    this.update();
  }

  /** 每次直接采样视频时间, 不在暂停, buffering 或倍速时积累墙钟漂移. */
  private update(): void {
    const context = this.context;
    const settings = this.settings;
    if (!this.waiting && this.video.readyState >= 2) this.mediaTime = this.video.currentTime;
    this.clear();
    if (!context || !settings?.enabled || !this.layout || !this.available || !this.activePage
      || document.hidden || this.mediaTime === null || this.video.seeking || this.video.error) {
      this.cancelFrame();
      return;
    }
    const time = this.mediaTime + settings.time_offset_seconds;
    for (const item of this.layout.frame(time)) {
      const x = (item.speed ? this.width - (time - item.start) * item.speed : (this.width - item.width) / 2)
        + settings.outline_width;
      context.fillStyle = item.color;
      if (settings.outline_width > 0) {
        context.strokeStyle = item.outline;
        context.strokeText(item.text, x, item.y);
      }
      context.fillText(item.text, x, item.y);
    }
    if (!this.video.paused && !this.video.ended && !this.waiting && this.comments?.length) this.scheduleFrame();
    else this.cancelFrame();
  }

  /** 支持时跟随已提交的视频帧, 其余浏览器只保留一个 rAF 循环. */
  private scheduleFrame(): void {
    if (this.videoFrame !== null || this.animationFrame !== null) return;
    if (typeof this.video.requestVideoFrameCallback === "function") {
      this.videoFrame = this.video.requestVideoFrameCallback(() => {
        this.videoFrame = null;
        this.update();
      });
    } else {
      this.animationFrame = window.requestAnimationFrame(() => {
        this.animationFrame = null;
        this.update();
      });
    }
  }

  /** 取消尚未执行的时钟回调, 不让页面恢复时叠加动画循环. */
  private cancelFrame(): void {
    if (this.videoFrame !== null) this.video.cancelVideoFrameCallback(this.videoFrame);
    if (this.animationFrame !== null) window.cancelAnimationFrame(this.animationFrame);
    this.videoFrame = null;
    this.animationFrame = null;
  }

  /** 清除整个透明画布, CSS 大小为零时也不保留旧媒体的像素. */
  private clear(): void {
    this.context?.clearRect(0, 0, this.width, this.height);
  }
}
