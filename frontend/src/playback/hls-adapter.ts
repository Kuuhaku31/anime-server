import Hls from "hls.js";

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
    // Chromium can advertise native HLS yet reject a growing fMP4 event playlist.
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
        if (settled) fatal(error);
        else { settled = true; reject(error); }
      };
      const loaded = (): void => {
        if (settled || signal.aborted) return;
        try {
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
