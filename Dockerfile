# 分别编译前端和 Rust 后端,
# 然后把编译产物与运行所需的依赖放进一个精简的 Debian 镜像中,
# 最终启动 anime-server 服务.

# Frontend build
FROM node:22-bookworm AS frontend-builder
# 工作目录为容器内部的 /src/frontend
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY frontend/tsconfig.json frontend/vite.config.ts frontend/index.html frontend/style.css ./
COPY frontend/src ./src
RUN npm run build

# Rust build
FROM rust:1-bookworm AS rust-builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY backend ./backend
# -p 用于指定要构建的包
# --locked 确保 Cargo.lock 文件与 Cargo.toml 文件一致
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p anime-server && \
    cp /src/target/release/anime-server /tmp/anime-server

# Runtime image
FROM debian:bookworm-slim
WORKDIR /app

# apt-get update 刷新软件包索引, 让 APT 知道当前软件源有哪些软件包以及相应版本信息
# ffmpeg 提供媒体探测, HLS 转码, 音轨选择和字幕渲染
# fonts-noto-cjk 为缺少内嵌字体的 CJK 字幕提供回退字体
# ca-certificates 提供受信任的 CA 根证书, 支持 HTTPS 等 TLS 连接中的证书验证
# --no-install-recommends 表示不额外安装仅被标记为推荐项的依赖
# rm -rf /var/lib/apt/lists/* 删除 APT 的软件包索引缓存, 减少镜像体积
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates ffmpeg fonts-noto-cjk && \
    rm -rf /var/lib/apt/lists/*

COPY --from=rust-builder /tmp/anime-server /app/anime-server
COPY --from=frontend-builder /src/frontend/dist /app/web
EXPOSE 8080
ENV ANIME_SERVER_CONFIG=/config/config.json
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s \
    CMD ["/app/anime-server", "--healthcheck"]

CMD ["/app/anime-server"]
