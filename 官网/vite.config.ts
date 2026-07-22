import path from 'node:path'
import { readFileSync } from 'node:fs'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { cosspressMarkdown } from './plugins/markdown'

interface WebsiteVersionDocument {
  version?: unknown
}

// 官网只展示仓库中已经发布并提交的固定版本号。Release 流水线负责更新该文件；
// 普通官网重建不得再根据构建机器时间改变用户看到的版本。
const versionDocument = JSON.parse(
  readFileSync(new URL('./version.json', import.meta.url), 'utf8'),
) as WebsiteVersionDocument
if (
  typeof versionDocument.version !== 'string' ||
  !/^v\d{4}\.\d{1,2}\.\d{1,2}(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$/.test(versionDocument.version)
) {
  throw new Error('官网/version.json 必须包含有效的日期版本号')
}
const websiteVersion = versionDocument.version

// https://vite.dev/config/
export default defineConfig({
  plugins: [cosspressMarkdown(), react(), tailwindcss()],
  define: {
    // 打包时把固定发布版本注入前端（类型见 src/vite-env.d.ts）。
    __APP_VERSION__: JSON.stringify(websiteVersion),
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  build: {
    outDir: 'source',
    rollupOptions: {
      output: {
        entryFileNames: 'js/main.js',
        chunkFileNames: 'js/[name].js',
        manualChunks: {
          'react-vendor': ['react', 'react-dom', 'react-router-dom'],
        },
        assetFileNames: (assetInfo) => {
          if (assetInfo.name?.endsWith('.css')) {
            return 'css/style.css'
          }
          return 'assets/[name][extname]'
        },
      },
    },
  },
})
