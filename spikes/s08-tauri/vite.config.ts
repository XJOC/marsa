import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// WebKit, WebView2, and WebKitGTK must all run the output: keep the build
// target conservative (es2020) instead of Vite's default.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { target: "es2020" },
});
