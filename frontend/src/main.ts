import "../style.css";
import { request } from "./api/anime-server.ts";
import { PlaybackController } from "./playback/playback-controller.ts";
import { attachControls, element } from "./ui/controls.ts";
import { Library } from "./ui/library.ts";

const player = element<HTMLVideoElement>("#player");
const controller = new PlaybackController(player);
attachControls(controller, player);
const library = new Library((file) => controller.open(file), (error) => controller.fail(error));
controller.addEventListener("change", () => library.select(controller.file?.pre16mib_md5 ?? null));

async function initialize(): Promise<void> {
  const config = await request<{ default_dark_mode: boolean }>("/api/config");
  document.documentElement.dataset.theme = config.default_dark_mode ? "dark" : "light";
  await library.load();
}

initialize().catch((error) => controller.fail(error));
