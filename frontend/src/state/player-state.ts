
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

/** 列表条目仅包含容器路径和媒体信息, 不包含内容或缓存标识. */
export type MediaEntry = [path: string, info: MediaInfo];

/** 保存服务端确认的三类实际流索引. */
export interface StreamSelection {
  /** 实际选择的视频流索引. */
  video_stream_index: number;
  /** 实际选择的音频流索引, null 表示关闭. */
  audio_stream_index: number | null;
  /** 实际选择的字幕流索引, null 表示关闭. */
  subtitle_stream_index: number | null;
}

/** 提交按路径播放的请求, 目标位置只由控制器本地维护. */
export interface PlaybackQuery {
  /** 要播放的容器内媒体路径. */
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
}

/** 保存客户端需要的最小会话引用. */
export interface PlaybackSession {
  /** 不透明会话标识, 不能用于推导缓存身份. */
  session_id: string;
  /** 会话对应的同源 HLS 清单 URL. */
  url: string;
}

/** 描述创建播放后的初始状态和实际媒体选择. */
export interface PlaybackStart extends PlaybackSession, StreamSelection {
  /** 初始缓存状态. */
  cache: CacheStatus;
  /** 重新探测的媒体信息和当前配置. */
  media: MediaInfo;
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
