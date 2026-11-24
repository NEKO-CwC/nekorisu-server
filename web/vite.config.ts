import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const backend = "http://127.0.0.1:8080";

export default defineConfig({
  base: "/",
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": backend,
      "/healthz": backend,
      "/mcp": backend,
      "/oauth": backend,
      "/.well-known": backend,
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
