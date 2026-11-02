
// frontend/src/main.ts
// 组装常驻播放器, 文件列表和共享缓存任务列表.
import "../style.css";
import { request } from "./api/anime-server.ts";
import { PlaybackController } from "./playback/playback-controller.ts";
import { attachControls, element } from "./ui/controls.ts";
import { fileAddress, Library } from "./ui/library.ts";
import { CacheStore } from "./state/cache-store.ts";
import { CacheList } from "./ui/cache-list.ts";

const player = element<HTMLVideoElement>("#player");
const caches = new CacheStore();
const controller = new PlaybackController(player, caches);
attachControls(controller, player);
const library = new Library((query) => caches.build(query), (error) => controller.fail(error));
const cacheList = new CacheList(caches, (handleId) => controller.openCached(handleId), library);
controller.addEventListener("change", () => {
  library.select(controller.file ? fileAddress(controller.file) : null);
  cacheList.select(controller.handleId);
});

/** 读取默认主题并加载路径型媒体列表. */
async function initialize(): Promise<void> {
  const config = await request<{ default_dark_mode: boolean }>("/api/config");
  document.documentElement.dataset.theme = config.default_dark_mode ? "dark" : "light";
  await Promise.all([library.load(), caches.start()]);
}

initialize().catch((error) => controller.fail(error));
