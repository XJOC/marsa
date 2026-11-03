import { defineConfig } from "vite";

// No React plugin: Vite compiles JSX with the automatic runtime from
// tsconfig's `"jsx": "react-jsx"`, and the spike needs no Fast Refresh.
// WebKit, WebView2, and WebKitGTK must all run the output: keep the build
// target conservative (es2020) instead of Vite's default.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { target: "es2020" },
});
