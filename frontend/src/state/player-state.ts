// 定义前后端共享的数据契约, 不在前端推导播放配置或缓存身份.
export interface VideoFile {
  pre16mib_md5: string;
  name: string;
  size: number;
  audio_stream_index: number | null;
  subtitle_stream_index: number | null;
  ddp_epi_id: number | null;
  danmaku_offset: number;
  watch_pos: number | null;
  watched_datetime: string | null;
  // 列表返回实际展示副本的地址, 单文件重扫不影响同指纹其他副本.
  path: string;
  name_no_ext: string;
  extension: string;
}

export interface Track {
  // 容器的全局流索引, 不等于当前音轨或字幕数组的下标.
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
  // 不透明租约 ID, 不可用于推导 variant-key 或磁盘目录.
  session_id: string;
  url: string;
  mime_type: "application/vnd.apple.mpegurl";
  // 完成状态与任务活动分开表达, 前端不推断额外的缓存状态枚举.
  complete: boolean;
  generating: boolean;
  stopping: boolean;
  encoding_threads: number;
  // 本会话等待的绝对秒数, 默认由服务端读取观看记录后确定.
  position: number;
  // 仅表示存在可播放分片, 目标可播仍须 position < buffered_until.
  ready: boolean;
  start_time: number;
  // 服务端已验证连续前缀的右边界, 不是浏览器已下载的缓冲范围.
  buffered_until: number;
  duration: number;
  error: string | null;
}

export interface PlaybackStart extends PlaybackSession {
  media: MediaInfo;
  selection: Selection;
}

// null 是显式关闭轨道, 不是要求服务端重新选择默认轨.
export interface Selection {
  audio: number | null;
  subtitle: number | null;
}
