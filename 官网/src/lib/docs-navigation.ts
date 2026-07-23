import type { Lang } from './i18n-context'

export interface SidebarItem {
  text: string
  link?: string
  items?: SidebarItem[]
  collapsed?: boolean
}

// The logical links are language-independent; only their labels differ.
const sidebarZh: SidebarItem[] = [
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
      { text: 'Secure Boot 与 PCA', link: '/docs/guide/secure-boot-pca' },
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
      { text: '使用与分发规则', link: '/docs/guide/terms' },
      { text: '常见问题', link: '/docs/guide/faq' },
      { text: '交流社区', link: '/docs/guide/community' },
    ],
  },
]

const sidebarEn: SidebarItem[] = [
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
      { text: 'Secure Boot and PCA', link: '/docs/guide/secure-boot-pca' },
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
      { text: 'Use and Distribution Terms', link: '/docs/guide/terms' },
      { text: 'FAQ', link: '/docs/guide/faq' },
      { text: 'Community', link: '/docs/guide/community' },
    ],
  },
]

export function getSidebar(lang: Lang): SidebarItem[] {
  return lang === 'en' ? sidebarEn : sidebarZh
}

/** Default document shared by both languages. */
export const firstDocLink = '/docs/guide/what-is-letrecovery'

function normalize(path: string): string {
  const normalized = path.split('#')[0].split('?')[0]
  if (normalized.length > 1 && normalized.endsWith('/')) {
    return normalized.slice(0, -1)
  }
  return normalized || '/docs'
}

export function isActiveLink(pathname: string, link: string): boolean {
  return normalize(pathname) === normalize(link)
}
