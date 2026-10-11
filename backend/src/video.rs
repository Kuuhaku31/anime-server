//! 声明媒体探测和播放处理模块.
// backend/src/video.rs

mod assets;
pub(crate) mod metadata;
pub(crate) mod process;
pub(crate) mod ffmpeg;
mod fragment;
pub(crate) mod sessions;
pub(crate) mod cache;
