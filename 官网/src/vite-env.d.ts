/// <reference types="vite/client" />

// Markdown 文档由 plugins/markdown.ts 在构建期转换为模块，导出预渲染 HTML、frontmatter 与标题列表
declare module '*.md' {
  export const html: string
  export const frontmatter: Record<string, unknown> & {
    title?: string
    description?: string
    layout?: string
  }
  export const headings: { level: number; title: string; slug: string }[]
  const fm: Record<string, unknown>
  export default fm
}

export {}

declare global {
  // 由 vite.config.ts 的 define 在构建期注入（编译日期版本号，如 "v2026.06.15"）
  const __APP_VERSION__: string
}
