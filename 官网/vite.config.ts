import path from "path"
import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"
import { copyFileSync } from "fs"

export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    {
      name: "copy-htaccess",
      closeBundle() {
        try {
          copyFileSync(
            path.resolve(__dirname, "source/.htaccess"),
            path.resolve(__dirname, "source/.htaccess")
          )
        } catch (e) {
          // .htaccess 已存在，无需复制
        }
      },
    },
  ],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    outDir: "source",
    rollupOptions: {
      output: {
        entryFileNames: "js/main.js",
        chunkFileNames: "js/[name].js",
        assetFileNames: (assetInfo) => {
          if (assetInfo.name?.endsWith(".css")) {
            return "css/style.css"
          }
          return "assets/[name][extname]"
        },
      },
    },
  },
})
