export interface VideoFile {
  pre16mib_md5: string;
  name: string;
  size: number;
  ddp_epi_id: number | null;
  danmaku_offset: number;
  watch_pos: number | null;
  watched_datetime: string | null;
}

export interface Track {
  stream_index: number;
  codec_name: string;
  language: string | null;
  title: string | null;
  is_default: boolean;
  is_forced: boolean;
}

export interface MediaInfo {
  duration: number;
  video_tracks: Track[];
  audio_tracks: Track[];
  subtitle_tracks: Track[];
}

export interface PlaybackSession {
  session_id: string;
  url: string;
  mime_type: "application/vnd.apple.mpegurl";
  state: "queued" | "starting" | "ready" | "complete" | "failed" | "cancelled";
  ready: boolean;
  start_time: number;
  buffered_until: number;
  duration: number;
  error: string | null;
}

export interface Selection {
  audio: number | null;
  subtitle: number | null;
}
