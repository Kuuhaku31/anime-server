// 组装播放器和文件列表, 从服务端读取页面默认配置.
import "../style.css";
import { request } from "./api/anime-server.ts";
import { PlaybackController } from "./playback/playback-controller.ts";
import { attachControls, element } from "./ui/controls.ts";
import { fileAddress, Library } from "./ui/library.ts";

const player = element<HTMLVideoElement>("#player");
const controller = new PlaybackController(player);
attachControls(controller, player);
const library = new Library((file) => controller.open(file), (error) => controller.fail(error));
// 播放状态只由控制器维护, 列表订阅变化后仅同步当前文件高亮.
controller.addEventListener("change", () => library.select(controller.file ? fileAddress(controller.file) : null));

async function initialize(): Promise<void> {
  const config = await request<{ default_dark_mode: boolean }>("/api/config");
  document.documentElement.dataset.theme = config.default_dark_mode ? "dark" : "light";
  await library.load();
}

initialize().catch((error) => controller.fail(error));
