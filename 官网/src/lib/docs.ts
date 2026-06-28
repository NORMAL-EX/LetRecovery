// 文档（cosspress markdown）加载与导航。Markdown 在构建期由 plugins/markdown.ts
// 转换成 { html, raw, frontmatter, headings } 模块。
// 路由对中英文统一（/docs/guide/x），由语言上下文决定加载中文还是英文内容：
//   中文源文件 /docs/guide/x.md      → 逻辑路由 /docs/guide/x
//   英文源文件 /docs/en/guide/x.md   → 逻辑路由 /docs/guide/x（去掉 /en）

import type { Lang } from './i18n'

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
  frontmatter: DocFrontmatter
  headings: Heading[]
}

export interface SidebarItem {
  text: string
  link?: string
  items?: SidebarItem[]
  collapsed?: boolean
}

interface MarkdownModule {
  html: string
  raw: string
  frontmatter: DocFrontmatter
  headings: Heading[]
}

// 侧边栏：中英文链接一致（语言无关），仅文字不同。
export const sidebarZh: SidebarItem[] = [
  {
    text: '介绍',
    items: [
      { text: 'LetRecovery 是什么？', link: '/docs/guide/what-is-letrecovery' },
      { text: '快速开始', link: '/docs/guide/getting-started' },
    ],
  },
  {
    text: '核心功能',
    items: [
      { text: '系统安装', link: '/docs/guide/system-install' },
      { text: '简易模式', link: '/docs/guide/easy-mode' },
      { text: '系统备份', link: '/docs/guide/system-backup' },
      { text: '在线下载', link: '/docs/guide/online-download' },
      { text: 'BitLocker 加密盘重装', link: '/docs/guide/bitlocker' },
      { text: '高级选项', link: '/docs/guide/advanced-options' },
      { text: '工具箱', link: '/docs/guide/toolbox' },
    ],
  },
  {
    text: '进阶',
    items: [
      { text: '无损扩大 C 盘', link: '/docs/guide/expand-c-drive' },
      { text: 'Windows XP / 2003 安装', link: '/docs/guide/xp-install' },
      { text: '镜像引擎', link: '/docs/guide/wim-engine' },
    ],
  },
  {
    text: '参考',
    items: [
      { text: '命令行参数', link: '/docs/reference/command-line' },
    ],
  },
  {
    text: '更多',
    items: [
      { text: '常见问题', link: '/docs/guide/faq' },
      { text: '交流社区', link: '/docs/guide/community' },
    ],
  },
]

export const sidebarEn: SidebarItem[] = [
  {
    text: 'Introduction',
    items: [
      { text: 'What is LetRecovery?', link: '/docs/guide/what-is-letrecovery' },
      { text: 'Getting Started', link: '/docs/guide/getting-started' },
    ],
  },
  {
    text: 'Core Features',
    items: [
      { text: 'System Installation', link: '/docs/guide/system-install' },
      { text: 'Easy Mode', link: '/docs/guide/easy-mode' },
      { text: 'System Backup', link: '/docs/guide/system-backup' },
      { text: 'Online Download', link: '/docs/guide/online-download' },
      { text: 'BitLocker Reinstall', link: '/docs/guide/bitlocker' },
      { text: 'Advanced Options', link: '/docs/guide/advanced-options' },
      { text: 'Toolbox', link: '/docs/guide/toolbox' },
    ],
  },
  {
    text: 'Advanced',
    items: [
      { text: 'Lossless C: Expansion', link: '/docs/guide/expand-c-drive' },
      { text: 'Windows XP / 2003 Setup', link: '/docs/guide/xp-install' },
      { text: 'Image Engine', link: '/docs/guide/wim-engine' },
    ],
  },
  {
    text: 'Reference',
    items: [
      { text: 'Command-Line Reference', link: '/docs/reference/command-line' },
    ],
  },
  {
    text: 'More',
    items: [
      { text: 'FAQ', link: '/docs/guide/faq' },
      { text: 'Community', link: '/docs/guide/community' },
    ],
  },
]

export function getSidebar(lang: Lang): SidebarItem[] {
  return lang === 'en' ? sidebarEn : sidebarZh
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

/** /docs 默认跳转到的第一篇文档（中英一致） */
export const firstDocLink = '/docs/guide/what-is-letrecovery'

export function isActiveLink(pathname: string, link: string): boolean {
  return normalize(pathname) === normalize(link)
}
