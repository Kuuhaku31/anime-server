interface VideoFile {
  pre16mib_md5: string;
  name: string;
  size: number;
  ddp_epi_id: number | null;
  danmaku_offset: number;
  watch_pos: number | null;
  watched_datetime: string | null;
  path: string;
  name_no_ext: string;
  extension: string;
}

interface VideoDetailResponse {
  video: VideoFile;
  media_url: string;
}

interface WebConfig {
  default_dark_mode: boolean;
}

const filesEl = document.querySelector<HTMLDivElement>("#files")!;
const filesCountEl = document.querySelector<HTMLSpanElement>("#files-count")!;
const player = document.querySelector<HTMLVideoElement>("#player")!;
const meta = document.querySelector<HTMLDivElement>("#meta")!;
const scanButton = document.querySelector<HTMLButtonElement>("#scan")!;
let currentId: string | null = null;
let saveTimer: number | null = null;

async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const response = await fetch(url, options);
  if (!response.ok) throw new Error(await response.text());
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

async function loadFiles(): Promise<void> {
  const files = await request<VideoFile[]>("/api/files");
  filesCountEl.textContent = String(files.length);
  filesEl.innerHTML = files.length ? "" : '<p class="empty-state">No videos found.</p>';
  for (const file of files) {
    const row = document.createElement("div");
    row.className = "file-row";
    row.dataset.fileId = file.pre16mib_md5;
    row.innerHTML = `
      <div class="file-info">
        <strong>${escapeHtml(file.name)}</strong>
        <span>${formatSize(file.size)} / ${file.ddp_epi_id === null ? "unidentified" : `episode ${file.ddp_epi_id}`}</span>
      </div>
      <div class="actions">
        <button data-open="${file.pre16mib_md5}">Play</button>
        <button data-identify="${file.pre16mib_md5}">Identify</button>
      </div>`;
    filesEl.appendChild(row);
  }
}

filesEl.addEventListener("click", async (event) => {
  const target = event.target as HTMLElement;
  const openId = target.dataset.open;
  const identifyId = target.dataset.identify;
  if (openId) await openVideo(openId);
  if (identifyId) {
    await request(`/api/files/${encodeURIComponent(identifyId)}/identify`, { method: "POST" });
    await loadFiles();
  }
});

scanButton.addEventListener("click", async () => {
  scanButton.disabled = true;
  try {
    await request("/api/scan", { method: "POST" });
    await loadFiles();
  } finally {
    scanButton.disabled = false;
  }
});

player.addEventListener("timeupdate", () => {
  if (currentId === null) return;
  if (saveTimer !== null) return;
  saveTimer = window.setTimeout(async () => {
    saveTimer = null;
    await saveProgress();
  }, 1000);
});

window.addEventListener("beforeunload", () => {
  void saveProgress();
});

async function openVideo(id: string): Promise<void> {
  const detail = await request<VideoDetailResponse>(`/api/files/${encodeURIComponent(id)}`);
  currentId = id;
  filesEl.querySelectorAll(".file-row.active").forEach((row) => row.classList.remove("active"));
  filesEl.querySelector(`[data-file-id="${id}"]`)?.classList.add("active");
  player.src = detail.media_url;
  meta.textContent = `${detail.video.ddp_epi_id === null ? "Unidentified" : `Episode ${detail.video.ddp_epi_id}`} / ${detail.video.name}`;
  const progress = await request<{ watch_pos: number | null }>(
    `/api/files/${encodeURIComponent(id)}/progress`,
  );
  if (progress.watch_pos !== null && progress.watch_pos > 1 && Number.isFinite(progress.watch_pos)) {
    player.addEventListener(
      "loadedmetadata",
      () => {
        player.currentTime = progress.watch_pos ?? 0;
      },
      { once: true },
    );
  }
  await player.play().catch(() => undefined);
}

async function saveProgress(): Promise<void> {
  if (currentId === null || !Number.isFinite(player.currentTime)) return;
  await fetch(`/api/files/${encodeURIComponent(currentId)}/progress`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      position: player.currentTime,
    }),
  });
}

function escapeHtml(value: string): string {
  return value.replace(
    /[&<>'"]/g,
    (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[char]!,
  );
}

function formatSize(size: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = size;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index++;
  }
  return `${value.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

async function initialize(): Promise<void> {
  const config = await request<WebConfig>("/api/config");
  document.documentElement.dataset.theme = config.default_dark_mode ? "dark" : "light";
  await loadFiles();
}

void initialize().catch((error) => {
  filesEl.textContent = `読み込み失敗: ${String(error)}`;
});
