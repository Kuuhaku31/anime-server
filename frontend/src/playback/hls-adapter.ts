// 按浏览器能力选择 HLS 引擎并管理媒体加载与释放.
import Hls from "hls.js";

// Safari 优先原生 HLS, 其他支持 MSE 的浏览器优先 hls.js, 加载失败后不切换引擎重试.
export function preferNativeHls(nativeSupport: boolean, mseSupport: boolean, vendor: string): boolean {
  return nativeSupport && (vendor === "Apple Computer, Inc." || !mseSupport);
}

export class MediaAdapter {
  private hls: Hls | null = null;
  private cleanup: (() => void) | null = null;
  private video: HTMLVideoElement;

  constructor(video: HTMLVideoElement) { this.video = video; }

  attach(url: string, position: number, signal: AbortSignal, fatal: (error: Error) => void): Promise<void> {
    this.destroy();
    signal.throwIfAborted();
    const mse = Hls.isSupported();
    // Chromium 即使声明支持原生 HLS, 也可能拒绝持续增长的 fMP4 event 清单.
    const native = preferNativeHls(!!this.video.canPlayType("application/vnd.apple.mpegurl"), mse, navigator.vendor);
    if (!native && !mse) {
      return Promise.reject(new Error("This browser has no supported HLS playback engine."));
    }
    return new Promise((resolve, reject) => {
      let settled = false;
      let failed = false;
      const fail = (error: Error): void => {
        if (failed || signal.aborted) return;
        failed = true;
        clearTimeout(timer);
        this.hls?.stopLoad();
        // 加载完成前拒绝 Promise, 完成后的解码错误改由 fatal 回调通知控制器.
        if (settled) fatal(error);
        else { settled = true; reject(error); }
      };
      const loaded = (): void => {
        if (settled || signal.aborted) return;
        try {
          // 元数据就绪后设置绝对位置, 实际缓冲和解码仍由媒体引擎异步完成.
          this.video.currentTime = Math.max(0, position);
          settled = true;
          clearTimeout(timer);
          resolve();
        } catch (error) { fail(error instanceof Error ? error : new Error(String(error))); }
      };
      const mediaError = (): void => {
        const error = this.video.error;
        fail(new Error(`Video error ${error?.code ?? "unknown"}: ${error?.message || "The browser could not decode this stream."}`));
      };
      const abort = (): void => {
        if (!settled) { settled = true; reject(signal.reason); }
        this.destroy();
      };
      const timer = window.setTimeout(() => fail(new Error("Timed out loading the HLS stream.")), 30_000);
      this.video.addEventListener("loadedmetadata", loaded);
      this.video.addEventListener("error", mediaError);
      signal.addEventListener("abort", abort, { once: true });
      this.cleanup = () => {
        clearTimeout(timer);
        this.video.removeEventListener("loadedmetadata", loaded);
        this.video.removeEventListener("error", mediaError);
        signal.removeEventListener("abort", abort);
        if (!settled) { settled = true; reject(new DOMException("Playback cancelled", "AbortError")); }
      };
      try {
        if (native) {
          this.video.src = url;
          this.video.load();
        } else {
          const hls = new Hls({
            startPosition: position, maxBufferLength: 30, backBufferLength: 30, lowLatencyMode: false,
          });
          this.hls = hls;
          hls.on(Hls.Events.ERROR, (_event, data) => {
            if (data.fatal || data.details === Hls.ErrorDetails.FRAG_PARSING_ERROR
              || data.details === Hls.ErrorDetails.BUFFER_ADD_CODEC_ERROR) {
              fail(new Error(`HLS ${data.type}: ${data.details}`));
            }
          });
          hls.attachMedia(this.video);
          hls.loadSource(url);
        }
      } catch (error) { fail(error instanceof Error ? error : new Error(String(error))); }
    });
  }

  // 先解除旧加载回调再重置 video, 避免旧媒体事件污染新会话.
  destroy(): void {
    this.cleanup?.();
    this.cleanup = null;
    this.hls?.destroy();
    this.hls = null;
    this.video.pause();
    this.video.removeAttribute("src");
    this.video.load();
  }
}
