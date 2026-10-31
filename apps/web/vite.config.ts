import { defineConfig } from "vite";
import solid from "@solidjs/vite-plugin";

export default defineConfig({
  plugins: [solid({ start: true })],
  server: {
    host: "localhost",
    port: 3000,
    strictPort: true,
    proxy: {
      "/api": { target: "http://127.0.0.1:24816", ws: true, changeOrigin: true },
    },
  },
});
