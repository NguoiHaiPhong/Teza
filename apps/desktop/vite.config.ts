import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          const normalized = id.replace(/\\/g, "/");
          if (normalized.includes("/node_modules/")) {
            if (
              normalized.includes("/node_modules/react/") ||
              normalized.includes("/node_modules/react-dom/") ||
              normalized.includes("/node_modules/scheduler/")
            ) {
              return "vendor-react";
            }
            if (normalized.includes("/node_modules/lucide-react/")) {
              return "vendor-icons";
            }
            if (normalized.includes("/node_modules/@tauri-apps/")) {
              return "vendor-tauri";
            }
            return;
          }
          if (normalized.includes("/src/i18n/")) {
            return "app-i18n";
          }
          if (
            normalized.includes("/src/utils/emojiCatalog") ||
            normalized.includes("/src/utils/retroEmoticons") ||
            normalized.includes("/src/utils/emojiStyleContext") ||
            normalized.includes("/src/components/RetroEmoticon")
          ) {
            return "app-emoji";
          }
        },
      },
    },
  },
});
