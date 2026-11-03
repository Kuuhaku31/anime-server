
// frontend/src/playback/danmaku-layout.ts
// 按媒体时间安排弹幕轨道, 检查入口和追尾碰撞并在满轨时复用.

import type { DanmakuComment, DanmakuSettings } from "../state/player-state.ts";

/** 描述一次已排轨的弹幕, 坐标和时间均不依赖墙钟. */
export interface ActiveDanmaku {
  /** 折叠控制空白后的单行显示文本, 不修改原始列表内容. */
  text: string;
  /** Canvas 使用的十六进制填充颜色. */
  color: string;
  /** 按文字颜色选择的对比描边颜色. */
  outline: string;
  /** 原始弹幕时间轴上的出现秒数. */
  start: number;
  /** 完全离开画面或结束固定显示的秒数. */
  end: number;
  /** 包含描边的文字包围盒宽度. */
  width: number;
  /** 滚动时每媒体秒移动的 CSS 像素数, 固定弹幕为 0. */
  speed: number;
  /** 从顶部开始计数的物理轨道, 三种模式共用以避免互相遮挡. */
  lane: number;
  /** 文字中心在视频内容矩形内的纵坐标. */
  y: number;
}

/** 使用有序原始数组增量排轨, seek 时只重建当前可见时间窗. */
export class DanmakuLayout {
  /** 绑定一次几何布局和文字测量函数, 不复制弹幕原始数组. */
  constructor(comments: readonly DanmakuComment[], width: number, height: number,
    settings: Readonly<DanmakuSettings>, measure: (text: string) => number) {
    this.comments = comments;
    this.width = width;
    this.settings = settings;
    this.measure = measure;
    this.duration = Math.max(4, Math.min(9, 3.8 * width / 682));
    this.margin = Math.max(6, settings.outline_width + 2);
    this.lineHeight = Math.ceil(settings.font_size * 1.3 + 2 * settings.outline_width);
    const count = Math.max(1, Math.floor((height - 2 * this.margin) / this.lineHeight));
    this.scrollLanes = Math.min(count, Math.max(1,
      Math.floor((height * settings.display_area_percent / 100 - 2 * this.margin) / this.lineHeight)));
    this.lanes = Array.from({ length: count }, () => []);
  }

  /** 清空旧轨道, 下一帧根据当前时间二分定位而不是补播整个 seek 区间. */
  reset(): void {
    this.lanes.forEach((lane) => { lane.length = 0; });
    this.previousTime = Number.NaN;
  }

  /** 返回当前应绘制的条目, 暂停时相同时间不会重新加入弹幕. */
  frame(time: number): readonly ActiveDanmaku[] {
    if (!Number.isFinite(this.previousTime) || time < this.previousTime
      || time - this.previousTime > this.duration) {
      this.reset();
      this.cursor = this.lowerBound(time - this.duration);
    }
    let prunedAt = Number.NaN;
    while (this.cursor < this.comments.length && this.comments[this.cursor].time <= time) {
      const comment = this.comments[this.cursor++];
      if (prunedAt !== comment.time) { this.expire(comment.time); prunedAt = comment.time; }
      this.add(comment);
    }
    this.expire(time);
    this.previousTime = time;
    return this.lanes.flat();
  }

  /** 与列表共享的时间升序数据源. */
  private comments: readonly DanmakuComment[];
  /** 视频实际内容宽度, 不包含黑边. */
  private width: number;
  /** 本次几何布局使用的只读设置. */
  private settings: Readonly<DanmakuSettings>;
  /** 使用实际 Canvas 字体测量文字宽度. */
  private measure: (text: string) => number;
  /** 随画面宽度缩放的滚动持续秒数. */
  private duration: number;
  /** 画面上边缘和下边缘保留的最小间距. */
  private margin: number;
  /** 包含描边和行间距的物理轨道高度. */
  private lineHeight: number;
  /** 滚动模式可使用的顶部轨道数, 至少保留一行. */
  private scrollLanes: number;
  /** 三种模式共享的轨道占用, 允许满轨复用后有多个条目. */
  private lanes: ActiveDanmaku[][];
  /** 下一条尚未排轨的原始弹幕索引. */
  private cursor = 0;
  /** 上次处理的弹幕时间, NaN 表示需要重建. */
  private previousTime = Number.NaN;

  /** 按模式确定速度和轨道顺序, 暗色文字使用白色描边. */
  private add(comment: DanmakuComment): void {
    const fixed = comment.mode === 4 || comment.mode === 5;
    const text = comment.content.replace(/[\s\u0000-\u001f\u007f]+/g, " ").trim();
    const width = this.measure(text) + 2 * this.settings.outline_width;
    const duration = fixed ? 3.8 : this.duration;
    const speed = fixed ? 0 : (this.width + width) / duration;
    const lane = this.chooseLane(speed, comment.mode);
    const dark = Math.max(comment.color >> 16, (comment.color >> 8) & 255, comment.color & 255) < 64;
    this.lanes[lane].push({
      text, color: `#${comment.color.toString(16).padStart(6, "0")}`, outline: dark ? "#fff" : "#000",
      start: comment.time, end: comment.time + duration, width, speed, lane,
      y: this.margin + (lane + 0.5) * this.lineHeight,
    });
  }

  /** 先选空轨道, 再选安全跟随轨道, 最后复用最早可安全接纳的轨道. */
  private chooseLane(speed: number, mode: number): number {
    const count = speed === 0 ? this.lanes.length : this.scrollLanes;
    let best = mode === 4 ? this.lanes.length - 1 : 0;
    let earliest = Number.POSITIVE_INFINITY;
    for (let index = 0; index < count; index++) {
      const lane = mode === 4 ? this.lanes.length - 1 - index : index;
      const occupants = this.lanes[lane];
      if (!occupants.length) return lane;
      const available = occupants.reduce((latest, item) => Math.max(latest, this.availableAt(item, speed)), 0);
      if (available < earliest) { best = lane; earliest = available; }
    }
    return best;
  }

  /** 同时满足入口间距和前项离屏前不被追尾, 固定弹幕则等待整段显示结束. */
  private availableAt(item: ActiveDanmaku, speed: number): number {
    if (!speed || !item.speed) return item.end;
    const gap = Math.max(8, this.settings.font_size * 0.4);
    const entry = item.start + (item.width + gap) / item.speed;
    const exit = item.end - (this.width - gap) / speed;
    return Math.min(item.end, Math.max(entry, exit));
  }

  /** 释放已经完全离屏的条目, 同时间密集弹幕只执行一次清理. */
  private expire(time: number): void {
    for (let index = 0; index < this.lanes.length; index++) {
      this.lanes[index] = this.lanes[index].filter((item) => item.end > time);
    }
  }

  /** 找到可见窗口的首个索引, 不按评论 ID 去重以免丢失合法重复内容. */
  private lowerBound(time: number): number {
    let left = 0;
    let right = this.comments.length;
    while (left < right) {
      const middle = Math.floor((left + right) / 2);
      if (this.comments[middle].time < time) left = middle + 1;
      else right = middle;
    }
    return left;
  }
}
