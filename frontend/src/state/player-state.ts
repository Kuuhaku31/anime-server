

// frontend/src/state/player-state.ts
// 定义按路径访问媒体和查询缓存状态的前后端契约.

/** 描述容器内的单个媒体流. */
export interface StreamInfo {
  /** 容器内的全局流索引, 不等于数组下标. */
  stream_index: number;
  /** 编解码器名称. */
  codec_name: string;
  /** 可选语言标签. */
  language: string | null;
  /** 可选轨道标题. */
  title: string | null;
  /** 是否为默认流. */
  is_default: boolean;
  /** 是否为强制字幕. */
  is_forced: boolean;
}

/** 描述实时探测的媒体元数据. */
export interface MediaMetadata {
  /** 原媒体有效播放时长秒数. */
  duration: number;
  /** 媒体容器格式. */
  format: string;
  /** 可播放视频流. */
  video_streams: StreamInfo[];
  /** 可选音频流. */
  audio_streams: StreamInfo[];
  /** 可选字幕流. */
  subtitle_streams: StreamInfo[];
}

/** 描述数据库中的共享播放配置. */
export interface MediaConfigs {
  /** 上次播放位置, null 表示没有记录. */
  position: number | null;
  /** 保存的视频流偏好. */
  video_stream_index: number | null;
  /** 保存的可选音频流偏好. */
  audio_stream_index: number | null;
  /** 保存的可选字幕流偏好. */
  subtitle_stream_index: number | null;
}

/** 组合媒体元数据和共享配置. */
export interface MediaInfo {
  /** 当前路径的实时探测信息. */
  metadata: MediaMetadata;
  /** 当前内容身份的持久化播放配置. */
  configs: MediaConfigs;
}

/** 描述媒体列表中数据库登记的路径和共享播放配置. */
export interface MediaEntry {
  /** 数据库内容记录 ID. */
  file_id: string;
  /** 容器内媒体路径. */
  path: string;
  /** 同内容路径共享的播放配置. */
  configs: MediaConfigs;
}

/** 描述播放器已获取元数据的媒体路径. */
export type PlaybackMedia = [path: string, info: MediaInfo];

/** 保存服务端确认的三类实际流索引. */
export interface StreamSelection {
  /** 实际选择的视频流索引. */
  video_stream_index: number;
  /** 实际选择的音频流索引, null 表示关闭. */
  audio_stream_index: number | null;
  /** 实际选择的字幕流索引, null 表示关闭. */
  subtitle_stream_index: number | null;
}

/** 提交按路径构建缓存的请求, 目标位置只由控制器本地维护. */
export interface PlaybackQuery {
  /** 要构建缓存的容器内媒体路径. */
  path: string;
  /** 请求的视频流, 缺省继承配置或默认选择. */
  video_stream_index?: number;
  /** 请求的音频流, 显式 null 关闭. */
  audio_stream_index?: number | null;
  /** 请求的字幕流, 显式 null 关闭. */
  subtitle_stream_index?: number | null;
}

/** 描述缓存实例的公开状态, 不包含内部目录. */
export interface CacheStatus {
  /** 当前缓存生命周期阶段. */
  phase: "loading" | "completed" | "ready" | "failed" | "stopping" | "running";
  /** 服务端已验证的连续前缀终点, 不是浏览器缓冲. */
  buffered_until: number;
  /** 恢复或生成失败的诊断, 正常状态为 null. */
  error: string | null;
}

/** 描述转码 worker 的许可占用, 不代表 CPU 使用率或浏览器解码开销. */
export interface WorkerResources {
  /** 活动转码 worker 占用的编码名额. */
  encoder_slots: number;
  /** 活动转码 worker 占用的媒体进程名额. */
  process_slots: number;
  /** 当前占用的 FFmpeg 编码线程预算. */
  ffmpeg_threads: number;
}

/** 描述服务端计算稳定缓存标识使用的实际流选择和编码契约. */
export interface CacheSpec extends StreamSelection {
  /** 源媒体内容标识, 不由前端计算. */
  media_id: string;
  /** 编码和续接契约版本. */
  version: string;
}

/** 前端缓存 Map 中的一条记录, 只使用 handle_id 作为主键. */
export interface CacheEntry {
  /** 稳定缓存标识, 不作为授权凭证. */
  handle_id: string;
  /** 服务端确认的实际流选择和媒体身份. */
  spec: CacheSpec;
  /** 当前生成进度. */
  status: CacheStatus;
  /** 当前转码 worker 占用. */
  resources: WorkerResources;
  /** 稳定选择的现存源路径, null 时不能播放或重试. */
  source_path: string | null;
}

/** 描述全部缓存条目. */
export interface CacheList {
  /** 按 handle_id 排序的缓存条目. */
  entries: CacheEntry[];
}

/** 构建响应不创建会话, 播放器直接使用返回的 handle_id. */
export interface CacheBuild {
  /** 新建或复用的缓存条目. */
  cache: CacheEntry;
  /** 重新探测的媒体元数据. */
  media_meta: MediaMetadata;
  /** 后端当前读取的播放配置. */
  media_configs: MediaConfigs;
}

/** 描述全量扫描对路径索引的变更. */
export interface ScanChanges {
  /** 新增路径数量. */
  added: number;
  /** 内容身份变更的路径数量. */
  updated: number;
  /** 删除路径数量. */
  removed: number;
  /** 扫描后的路径总数. */
  total: number;
}
