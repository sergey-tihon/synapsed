import { defineConfig } from "vite";
import solid from "@solidjs/vite-plugin";

export default defineConfig({
  root: "inspector",
  plugins: [solid()],
  base: "./",
  build: {
    outDir: "../com.sergey-tihon.synapsed.sdPlugin/bin/inspector",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        entryFileNames: "inspector.js",
        assetFileNames: (asset) => (asset.name?.endsWith(".css") ? "assets/style.css" : "assets/[name][extname]"),
      },
    },
  },
});
