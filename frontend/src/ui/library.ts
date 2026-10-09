import { request } from "../api/anime-server.ts";
import type { VideoFile } from "../state/player-state.ts";
import { element } from "./controls.ts";

export class Library {
  private container = element<HTMLDivElement>("#files");
  private files: VideoFile[] = [];
  private open: (file: VideoFile) => void;
  private active: string | null = null;

  constructor(open: (file: VideoFile) => void, fail: (error: unknown) => void) {
    this.open = open;
    this.container.addEventListener("click", (event) => {
      const target = event.target;
      if (!(target instanceof HTMLButtonElement)) return;
      const file = this.files.find((file) => file.pre16mib_md5 === target.dataset.id);
      if (!file) return;
      if (target.dataset.action === "open") this.open(file);
      else {
        request(`/api/files/${file.pre16mib_md5}/identify`, { method: "POST" })
          .then(() => this.load()).catch(fail);
      }
    });
    const scan = element<HTMLButtonElement>("#scan");
    scan.onclick = async () => {
      scan.disabled = true;
      try { await request("/api/scan", { method: "POST" }); await this.load(); }
      catch (error) { fail(error); }
      finally { scan.disabled = false; }
    };
  }

  async load(): Promise<void> {
    this.files = await request<VideoFile[]>("/api/files");
    element("#files-count").textContent = String(this.files.length);
    const rows = this.files.map((file) => {
      const row = document.createElement("div");
      row.className = `file-row${file.pre16mib_md5 === this.active ? " active" : ""}`;
      row.dataset.fileId = file.pre16mib_md5;
      const info = document.createElement("div");
      info.className = "file-info";
      const name = document.createElement("strong");
      name.textContent = file.name;
      name.title = file.name;
      const detail = document.createElement("span");
      detail.textContent = `${(file.size / 1024 / 1024).toFixed(1)} MB / ${file.ddp_epi_id === null ? "unidentified" : `episode ${file.ddp_epi_id}`}`;
      info.append(name, detail);
      const actions = document.createElement("div");
      actions.className = "actions";
      for (const [action, label] of [["open", "Play"], ["identify", "Identify"]]) {
        const button = document.createElement("button");
        button.type = "button";
        button.dataset.id = file.pre16mib_md5;
        button.dataset.action = action;
        button.textContent = label;
        if (action === "identify") button.title = "Mock identification; DandanPlay integration is not implemented yet";
        actions.append(button);
      }
      row.append(info, actions);
      return row;
    });
    this.container.replaceChildren(...rows);
    if (!rows.length) this.container.textContent = "No videos found. Check the media mount and scan again.";
  }

  select(id: string | null): void {
    if (id === this.active) return;
    this.active = id;
    for (const row of this.container.querySelectorAll<HTMLElement>(".file-row")) {
      row.classList.toggle("active", row.dataset.fileId === id);
    }
  }
}
