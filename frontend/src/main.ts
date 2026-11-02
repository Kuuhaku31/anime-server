
// frontend/src/main.ts
// 组装常驻播放器, 文件列表和共享缓存任务列表.
import "../style.css";
import { request } from "./api/anime-server.ts";
import { PlaybackController } from "./playback/playback-controller.ts";
import { attachControls, element } from "./ui/controls.ts";
import { fileAddress, Library } from "./ui/library.ts";
import { CacheStore } from "./state/cache-store.ts";
import { CacheList } from "./ui/cache-list.ts";
import type { PlaybackQuery } from "./state/player-state.ts";

const player = element<HTMLVideoElement>("#player");
const caches = new CacheStore();
const controller = new PlaybackController(player, caches);
attachControls(controller, player);
const library = new Library((file) => controller.open(file), (error) => controller.fail(error), async (file) => {
  const query: PlaybackQuery = { path: file[0] };
  const configs = file[1].configs;
  if (configs.video_stream_index !== null) {
    query.video_stream_index = configs.video_stream_index;
    query.audio_stream_index = configs.audio_stream_index;
    query.subtitle_stream_index = configs.subtitle_stream_index;
  }
  await caches.build(query);
});
const cacheList = new CacheList(caches, (handleId) => controller.openCached(handleId), (path) => library.info(path));
controller.addEventListener("change", () => {
  library.select(controller.file ? fileAddress(controller.file) : null);
  cacheList.select(controller.handleId);
});
library.addEventListener("change", () => cacheList.render());

/** 读取默认主题并加载路径型媒体列表. */
async function initialize(): Promise<void> {
  const config = await request<{ default_dark_mode: boolean }>("/api/config");
  document.documentElement.dataset.theme = config.default_dark_mode ? "dark" : "light";
  await Promise.all([library.load(), caches.start()]);
}

initialize().catch((error) => controller.fail(error));
