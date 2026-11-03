
// frontend/src/main.ts
// 组装播放器, 共享弹幕数据, Canvas 覆盖层和可保存的画面设置.
import "../style.css";
import { getConfig } from "./api/anime-server.ts";
import { PlaybackController } from "./playback/playback-controller.ts";
import { DanmakuRenderer } from "./playback/danmaku-renderer.ts";
import { attachControls, element } from "./ui/controls.ts";
import { fileAddress, Library } from "./ui/library.ts";
import { CacheStore } from "./state/cache-store.ts";
import { CacheList } from "./ui/cache-list.ts";
import { DanmakuPanel } from "./ui/danmaku-panel.ts";
import { DanmakuSettingsPanel } from "./ui/danmaku-settings.ts";

const player = element<HTMLVideoElement>("#player");
const caches = new CacheStore();
const controller = new PlaybackController(player, caches);
attachControls(controller, player);
const library = new Library((query) => caches.build(query), (error) => controller.fail(error));
const cacheList = new CacheList(caches, (handleId) => controller.openCached(handleId), library);
const danmaku = new DanmakuPanel();
const settings = new DanmakuSettingsPanel();
const overlay = new DanmakuRenderer(player, element<HTMLCanvasElement>("#danmaku-canvas"),
  (message) => { element("#danmaku-render-error").textContent = message; });
danmaku.addEventListener("change", () => overlay.setComments(danmaku.data));
settings.addEventListener("change", () => {
  if (settings.value) overlay.configure(settings.value);
  syncDanmaku();
});
controller.addEventListener("change", () => {
  library.select(controller.file ? fileAddress(controller.file) : null);
  cacheList.select(controller.handleId);
  syncDanmaku();
});
library.addEventListener("change", syncDanmaku);

/** 读取当前关联和偏移后的时间, 播放器等待时不在旧视频帧上绘制. */
function syncDanmaku(): void {
  const path = controller.file ? fileAddress(controller.file) : null;
  overlay.sync(!!controller.file && !!controller.handleId && !controller.pending);
  danmaku.sync(path, library.episodeId(path), controller.position + (settings.value?.time_offset_seconds ?? 0));
}

/** 读取已保存的主题和弹幕设置, 再加载路径型媒体列表. */
async function initialize(): Promise<void> {
  const config = await getConfig();
  document.documentElement.dataset.theme = config.default_dark_mode ? "dark" : "light";
  settings.initialize(config.danmaku);
  await Promise.all([library.load(), caches.start()]);
}

initialize().catch((error) => controller.fail(error));
