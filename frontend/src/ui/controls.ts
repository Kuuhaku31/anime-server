
// frontend/src/ui/controls.ts
// 绑定播放器控件, 将用户操作转交控制器并同步界面状态.
import type { PlaybackController } from "../playback/playback-controller.ts";
import { formatTime } from "../playback/timeline.ts";

/** 读取必需页面元素, 缺失时明确报错. */
export function element<T extends HTMLElement>(selector: string): T {
  const value = document.querySelector<T>(selector);
  if (!value) throw new Error(`Missing page element: ${selector}`);
  return value;
}

/** 绑定播放和全屏悬浮控件, 不在控件中维护业务状态. */
export function attachControls(controller: PlaybackController, video: HTMLVideoElement): void {
  const play = element<HTMLButtonElement>("#toggle-play");
  const playLabel = element("#play-label");
  const seek = element<HTMLInputElement>("#seek");
  const clock = element<HTMLOutputElement>("#clock");
  const volume = element<HTMLInputElement>("#volume");
  const mute = element<HTMLButtonElement>("#mute");
  const muteLabel = element("#mute-label");
  const fullscreen = element<HTMLButtonElement>("#fullscreen");
  const fullscreenLabel = element("#fullscreen-label");
  const title = element("#fullscreen-title");
  const panel = element<HTMLElement>(".player-panel");
  const controls = element<HTMLElement>(".player-controls");
  let scrubbing = false;
  let hovering = false;
  let fullscreenPending = false;
  let activePage = true;
  let hideTimer: ReturnType<typeof setTimeout> | null = null;
  let clickTimer: ReturnType<typeof setTimeout> | null = null;

  play.onclick = () => controller.toggle();
  element<HTMLButtonElement>("#stop").onclick = () => controller.stop();
  element<HTMLButtonElement>("#restart").onclick = () => controller.seek(0);
  element<HTMLButtonElement>("#retry").onclick = () => controller.retry();
  element<HTMLButtonElement>("#retry-probe").onclick = () => controller.retry(true);
  video.addEventListener("click", (event) => {
    cancelClick();
    panel.focus({ preventScroll: true });
    if (event.detail > 1) return;
    // 等待双击判定, 避免切换全屏时同时切换两次播放状态.
    clickTimer = setTimeout(() => {
      clickTimer = null;
      controller.toggle();
    }, 300);
  });
  video.addEventListener("dblclick", (event) => {
    event.preventDefault();
    cancelClick();
    void toggleFullscreen().catch((error) => controller.fail(error));
  });

  // input 只预览时间, change 才提交目标, 状态刷新不得覆盖正在拖动的滑块.
  seek.addEventListener("input", () => {
    scrubbing = true;
    clock.textContent = `${formatTime(Number(seek.value))} / ${formatTime(controller.duration)}`;
    revealControls();
  });
  seek.addEventListener("change", () => {
    scrubbing = false;
    controller.seek(Number(seek.value));
    revealControls();
  });
  seek.addEventListener("pointerdown", () => { scrubbing = true; revealControls(); });
  seek.addEventListener("blur", finishScrubbing);
  window.addEventListener("pointerup", finishScrubbing);
  window.addEventListener("pointercancel", finishScrubbing);
  volume.oninput = () => {
    video.volume = Number(volume.value);
    video.muted = video.volume === 0;
  };
  mute.onclick = () => {
    const silent = video.muted || video.volume === 0;
    video.muted = !silent;
    if (silent && video.volume === 0) video.volume = 1;
  };
  video.addEventListener("volumechange", syncVolume);
  for (const event of ["play", "pause", "ended"]) video.addEventListener(event, render);
  fullscreen.onclick = (event) => {
    // 鼠标操作后把快捷键焦点交给播放器, 键盘操作则保留按钮焦点.
    if (event.detail) panel.focus({ preventScroll: true });
    void toggleFullscreen().catch((error) => controller.fail(error));
  };
  document.addEventListener("fullscreenchange", syncFullscreen);
  panel.addEventListener("danmaku-settings-change", revealControls);
  window.addEventListener("resize", syncViewport);
  window.addEventListener("orientationchange", syncViewport);
  window.visualViewport?.addEventListener("resize", syncViewport);
  window.visualViewport?.addEventListener("scroll", syncViewport);
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) return;
    cancelClick();
    panel.classList.remove("expanded");
    syncFullscreen();
  });
  panel.addEventListener("pointermove", revealControls);
  panel.addEventListener("pointerdown", revealControls);
  panel.addEventListener("focusin", revealControls);
  panel.addEventListener("focusout", () => queueMicrotask(revealControls));
  controls.addEventListener("pointerenter", () => { hovering = true; revealControls(); });
  controls.addEventListener("pointerleave", () => { hovering = false; revealControls(); });
  panel.addEventListener("keydown", (event) => {
    revealControls();
    // Escape 在表单控件内也能退出页内全屏, 其余快捷键保留原生输入行为.
    if (event.key === "Escape" && isFullscreen()) {
      event.preventDefault();
      void toggleFullscreen().catch((error) => controller.fail(error));
      return;
    }
    if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement
      || event.target instanceof HTMLButtonElement || event.ctrlKey || event.metaKey || event.altKey) return;
    switch (event.key.toLowerCase()) {
      case " ":
      case "k": event.preventDefault(); controller.toggle(); break;
      case "arrowleft": event.preventDefault(); controller.seek(controller.position - 10); break;
      case "arrowright": event.preventDefault(); controller.seek(controller.position + 10); break;
      case "m": mute.click(); break;
      case "f": event.preventDefault(); void toggleFullscreen().catch((error) => controller.fail(error)); break;
    }
  });
  controller.addEventListener("change", render);
  window.addEventListener("pagehide", () => {
    activePage = false;
    clearHideTimer();
    cancelClick();
    scrubbing = false;
    hovering = false;
    panel.classList.remove("expanded", "controls-hidden", "fullscreen-error");
    document.body.classList.remove("player-expanded");
  });
  window.addEventListener("pageshow", () => {
    activePage = true;
    syncFullscreen();
    render();
  });
  syncVolume();
  syncFullscreen();
  render();

  /** 同步静音图标和音量滑块, 保留图标节点和无障碍名称. */
  function syncVolume(): void {
    const silent = video.muted || video.volume === 0;
    muteLabel.textContent = silent ? "Unmute" : "Mute";
    mute.setAttribute("aria-label", muteLabel.textContent);
    mute.setAttribute("aria-pressed", String(silent));
    mute.title = `${muteLabel.textContent} (M)`;
    volume.value = String(video.muted ? 0 : video.volume);
  }

  /** 原生全屏和页内全屏使用同一个交互状态. */
  function isFullscreen(): boolean {
    return document.fullscreenElement === panel || panel.classList.contains("expanded");
  }

  /** 切换显示模式, 原生请求被拒绝时使用明确记录的页内替代模式. */
  async function toggleFullscreen(): Promise<void> {
    if (fullscreenPending) return;
    fullscreenPending = true;
    fullscreen.disabled = true;
    try {
      if (document.fullscreenElement === panel) await document.exitFullscreen();
      else if (panel.classList.contains("expanded")) panel.classList.remove("expanded");
      else if (!isAppleMobile() && panel.requestFullscreen) {
        try { await panel.requestFullscreen(); }
        catch (error) {
          console.info("Using in-page fullscreen because the Fullscreen API was rejected", error);
          panel.classList.add("expanded");
        }
      } else panel.classList.add("expanded");
    } finally {
      fullscreenPending = false;
      fullscreen.disabled = false;
      syncFullscreen();
    }
  }

  /** 同步全屏入口, 滚动锁和控件状态, 原生 Escape 退出也执行清理. */
  function syncFullscreen(): void {
    const active = isFullscreen();
    fullscreenLabel.textContent = active ? "Exit fullscreen" : "Fullscreen";
    fullscreen.setAttribute("aria-label", fullscreenLabel.textContent);
    fullscreen.setAttribute("aria-pressed", String(active));
    fullscreen.title = `${fullscreenLabel.textContent} (F)`;
    document.body.classList.toggle("player-expanded", panel.classList.contains("expanded"));
    panel.classList.toggle("fullscreen-error", active && !!controller.error);
    syncViewport();
    hovering = controls.matches(":hover");
    revealControls();
  }

  /** 页内全屏跟随移动端可见视口, 地址栏或软键盘变化不遮住退出入口. */
  function syncViewport(): void {
    if (!panel.classList.contains("expanded")) return;
    const viewport = window.visualViewport;
    panel.style.setProperty("--player-viewport-width", `${viewport?.width ?? window.innerWidth}px`);
    panel.style.setProperty("--player-viewport-height", `${viewport?.height ?? window.innerHeight}px`);
    panel.style.setProperty("--player-viewport-left", `${viewport?.offsetLeft ?? 0}px`);
    panel.style.setProperty("--player-viewport-top", `${viewport?.offsetTop ?? 0}px`);
  }

  /** 用户仍在操作或播放需要处理时保持控件可见. */
  function canHide(): boolean {
    const focus = document.activeElement;
    return activePage && !document.hidden && isFullscreen() && !video.paused && !video.ended
      && !controller.pending && !controller.error && !scrubbing && !hovering
      && !panel.classList.contains("danmaku-settings-open")
      && !(focus && controls.contains(focus) && focus.matches(":focus-visible"));
  }

  /** iPad 桌面 UA 也必须留在网页层, 不进入无法覆盖 Canvas 的系统视频全屏. */
  function isAppleMobile(): boolean {
    return typeof navigator !== "undefined" && (/iPad|iPhone|iPod/i.test(navigator.userAgent)
      || navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
  }

  /** 取消旧倒计时, 避免退出全屏后残留隐藏动作. */
  function clearHideTimer(): void {
    if (hideTimer !== null) clearTimeout(hideTimer);
    hideTimer = null;
  }

  /** 只在缺少计时器时排定隐藏, 轮询和 timeupdate 不重置用户空闲时间. */
  function scheduleHide(): void {
    if (!canHide()) {
      clearHideTimer();
      panel.classList.remove("controls-hidden");
    } else if (hideTimer === null && !panel.classList.contains("controls-hidden")) {
      hideTimer = setTimeout(() => {
        hideTimer = null;
        if (canHide()) panel.classList.add("controls-hidden");
      }, 3000);
    }
  }

  /** 用户交互重新显示控件并重新计算空闲期限. */
  function revealControls(): void {
    panel.classList.remove("controls-hidden");
    clearHideTimer();
    scheduleHide();
  }

  /** 拖动结束或取消时释放自动隐藏保护, seek 提交仍由 change 事件负责. */
  function finishScrubbing(): void {
    if (!scrubbing) return;
    scrubbing = false;
    revealControls();
  }

  /** 双击和页面离开时取消尚未确认的单击操作. */
  function cancelClick(): void {
    if (clickTimer !== null) clearTimeout(clickTimer);
    clickTimer = null;
  }

  /** 同步业务状态, 播放失败时恢复可见的反馈区而不是隐藏诊断. */
  function render(): void {
    play.disabled = !controller.file || controller.pending;
    playLabel.textContent = video.paused ? "Play" : "Pause";
    play.dataset.playing = String(!video.paused);
    play.setAttribute("aria-label", playLabel.textContent);
    play.title = `${playLabel.textContent} (K)`;
    seek.disabled = !controller.info;
    seek.max = String(controller.duration);
    if (!scrubbing) {
      seek.value = String(controller.position);
      clock.textContent = `${formatTime(controller.position)} / ${formatTime(controller.duration)}`;
    }
    seek.style.setProperty("--played", `${100 * controller.position / (controller.duration || 1)}%`);
    seek.style.setProperty("--buffered", `${100 * controller.buffered / (controller.duration || 1)}%`);
    element("#playback-status").textContent = controller.message;
    element("#error").textContent = controller.error;
    element("#error-actions").hidden = !controller.error;
    element("#meta").textContent = controller.file?.[0] ?? "Select a video from the library";
    title.textContent = controller.file?.[0].split("/").at(-1) ?? "";
    title.title = controller.file?.[0] ?? "";
    panel.classList.toggle("fullscreen-error", isFullscreen() && !!controller.error);
    panel.setAttribute("aria-busy", String(controller.pending));
    scheduleHide();
  }
}
