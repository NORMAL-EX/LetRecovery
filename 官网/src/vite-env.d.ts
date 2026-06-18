/// <reference types="vite/client" />

export {}

declare global {
  // 由 vite.config.ts 的 define 在构建期注入（编译日期版本号，如 "v2026.06.15"）
  const __APP_VERSION__: string
}
