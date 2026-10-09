import { defineConfig } from "vite";

export default defineConfig({
  build: { target: ["es2022", "safari15"] },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:56080",
      "/playback": "http://127.0.0.1:56080",
      "/health": "http://127.0.0.1:56080",
    },
  },
});
