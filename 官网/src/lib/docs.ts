// 文档（cosspress markdown）加载与导航。Markdown 在构建期由 plugins/markdown.ts
// 转换成 { html, raw, frontmatter, headings } 模块。
// 路由对中英文统一（/docs/guide/x），由语言上下文决定加载中文还是英文内容：
//   中文源文件 /docs/guide/x.md      → 逻辑路由 /docs/guide/x
//   英文源文件 /docs/en/guide/x.md   → 逻辑路由 /docs/guide/x（去掉 /en）

import type { Lang } from './i18n-context'

export interface Heading {
  level: number
  title: string
  slug: string
}

export interface DocFrontmatter {
  title?: string
  description?: string
  layout?: string
  [key: string]: unknown
}

export interface DocPageData {
  /** 逻辑路由，始终以 /docs 开头（中英文一致） */
  route: string
  /** 源文件路径，如 /docs/guide/getting-started.md 或 /docs/en/guide/... */
  file: string
  html: string
  /** 原始 markdown 正文（去掉 frontmatter），用于"复制 Markdown" */
  raw: string
  /** 构建期从可见 Markdown token 提取的正文搜索文本 */
  searchText: string
  frontmatter: DocFrontmatter
  headings: Heading[]
}

interface MarkdownModule {
  html: string
  raw: string
  searchText: string
  frontmatter: DocFrontmatter
  headings: Heading[]
}

// 构建期把每篇文档都吃进来（中英文都在）。
const modules = import.meta.glob<MarkdownModule>('/docs/**/*.md', {
  eager: true,
})

/** 源文件路径 → { 语言, 逻辑路由 } */
function fileToLogical(file: string): { lang: Lang; route: string } {
  const isEn = file.startsWith('/docs/en/')
  let route = file.replace(/^\/docs\/en/, '/docs').replace(/\.md$/, '')
  route = route.replace(/\/index$/, '')
  if (route.length > 1 && route.endsWith('/')) route = route.slice(0, -1)
  return { lang: isEn ? 'en' : 'zh', route: route || '/docs' }
}

const zhPages = new Map<string, DocPageData>()
const enPages = new Map<string, DocPageData>()

for (const [file, mod] of Object.entries(modules)) {
  // 跳过 home 布局（官网首页已有 Hero）
  if (mod.frontmatter?.layout === 'home') continue
  const { lang, route } = fileToLogical(file)
  const page: DocPageData = {
    route,
    file,
    html: mod.html,
    raw: mod.raw ?? '',
    searchText: mod.searchText ?? '',
    frontmatter: mod.frontmatter ?? {},
    headings: mod.headings ?? [],
  }
  ;(lang === 'en' ? enPages : zhPages).set(route, page)
}

/** 去掉末尾斜杠 / hash 用于匹配 */
function normalize(path: string): string {
  const p = path.split('#')[0].split('?')[0]
  if (p.length > 1 && p.endsWith('/')) return p.slice(0, -1)
  return p || '/docs'
}

export function getDocPage(
  pathname: string,
  lang: Lang,
): DocPageData | undefined {
  const route = normalize(pathname)
  const primary = lang === 'en' ? enPages : zhPages
  // 英文缺失时回退到中文，保证不空白
  return primary.get(route) ?? zhPages.get(route)
}

export function docTitle(page: DocPageData): string {
  if (page.frontmatter.title) return String(page.frontmatter.title)
  const h1 = page.headings.find((h) => h.level === 1)
  return h1?.title ?? page.route.split('/').filter(Boolean).pop() ?? '文档'
}
