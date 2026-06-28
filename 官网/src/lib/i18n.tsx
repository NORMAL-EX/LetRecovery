import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from 'react'

export type Lang = 'zh' | 'en'

const STORAGE_KEY = 'letrecovery-lang'

function detectInitial(): Lang {
  try {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved === 'zh' || saved === 'en') return saved
  } catch {
    /* ignore */
  }
  if (
    typeof navigator !== 'undefined' &&
    !navigator.language.toLowerCase().startsWith('zh')
  ) {
    return 'en'
  }
  return 'zh'
}

interface LangContextValue {
  lang: Lang
  setLang: (l: Lang) => void
}

const LangContext = createContext<LangContextValue>({
  lang: 'zh',
  setLang: () => {},
})

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectInitial)

  useEffect(() => {
    document.documentElement.lang = lang === 'en' ? 'en' : 'zh-CN'
  }, [lang])

  const setLang = (l: Lang) => {
    setLangState(l)
    try {
      localStorage.setItem(STORAGE_KEY, l)
    } catch {
      /* ignore */
    }
  }

  return (
    <LangContext.Provider value={{ lang, setLang }}>
      {children}
    </LangContext.Provider>
  )
}

export function useLang() {
  return useContext(LangContext)
}

export function useT(): Dict {
  return translations[useLang().lang]
}

/* ========================== 翻译字典 ========================== */

interface Dict {
  nav: {
    home: string
    docs: string
    about: string
    community: string
    menu: string
    docsHeading: string
  }
  common: {
    github: string
    toggleTheme: string
    light: string
    dark: string
    system: string
    language: string
    zh: string
    en: string
  }
  home: {
    heroTitleLines: string[]
    heroDesc: string
    download: string
    joinCommunity: string
    screenshotAlt: string
    dialogTitle: string
    dialogDesc: string
    alertTitle: string
    alertDesc: string
    sourceLabel: string
    close: string
    featuresTitle: string
    featuresDesc: string
    features: { title: string; desc: string }[]
    demoTitle: string
    demoDescLine: string
    demoCreditPrefix: string
    demoCreditSuffix: string
  }
  about: {
    title: string
    desc: string
    cardTitle: string
    version: string
    license: string
    copyright: string
    source: string
    ackTitle: string
    acks: string[]
    contributors: string
  }
  qqgroup: {
    title: string
    desc: string
    groupName: string
    capacity: string
    status: string
    viewQr: string
    join: string
    qrAlt: string
  }
  footer: {
    friends: string
    crafted: string
  }
  notFound: {
    title: string
    desc: string
    home: string
  }
  docs: {
    copyMarkdown: string
    copied: string
    onThisPage: string
    notFound: string
    notFoundDesc: string
    searchLabel: string
    searchPlaceholder: string
    searchEmpty: string
    searchEmptyHint: string
    goToPage: string
  }
}

const zh: Dict = {
  nav: {
    home: '主页',
    docs: '文档',
    about: '关于',
    community: '社区',
    menu: '菜单',
    docsHeading: '文档',
  },
  common: {
    github: 'GitHub 仓库',
    toggleTheme: '切换主题',
    light: '浅色模式',
    dark: '深色模式',
    system: '跟随系统',
    language: '语言',
    zh: '简体中文',
    en: 'English',
  },
  home: {
    heroTitleLines: ['一款纯净的', '系统重装工具'],
    heroDesc:
      '采用 Rust + egui 精心打造，拥有极致的运行效率，零广告零捆绑的纯净体验，简洁直观的操作界面让电脑小白也能轻松上手。',
    download: '立即下载',
    joinCommunity: '加入社区',
    screenshotAlt: 'LetRecovery 界面预览',
    dialogTitle: '下载 LetRecovery',
    dialogDesc: '获取最新版本的 LetRecovery 系统重装工具',
    alertTitle: '关于此下载',
    alertDesc:
      '下方所有下载源提供的均为同一份内容——已内置 WinPE 环境的 LetRecovery 完整版，下载后开箱即用，无需再单独获取 WinPE。各下载源内容完全一致，您可任选其一，建议优先选择访问速度较快的来源。',
    sourceLabel: '选择下载源：',
    close: '关闭',
    featuresTitle: '产品特性',
    featuresDesc: '探索 LetRecovery 的核心功能，让系统重装变得前所未有的简单。',
    features: [
      { title: '极致高效', desc: '基于 Rust 语言开发，享有卓越的性能表现和极低的资源占用，系统重装快人一步。' },
      { title: '纯净无捆绑', desc: '完全开源透明，不附带任何广告或捆绑软件，给您一个清爽干净的使用体验。' },
      { title: '简单易用', desc: '精心设计的现代化界面，操作直观明了，即使是电脑小白也能轻松驾驭。' },
      { title: '快速部署', desc: '一键式操作流程，从启动到完成系统重装仅需几步，大幅节省您的宝贵时间。' },
      { title: '功能强大', desc: '支持多种系统镜像格式，提供丰富的自定义选项，满足各类重装需求。' },
      { title: '安全可靠', desc: '采用先进的安全机制，确保数据传输和系统安装过程的稳定与安全。' },
    ],
    demoTitle: '操作演示',
    demoDescLine: '观看视频了解如何使用 LetRecovery 快速完成系统重装',
    demoCreditPrefix: '（该视频由 ',
    demoCreditSuffix: ' 制作）',
  },
  about: {
    title: '关于',
    desc: '了解 LetRecovery 的项目信息与贡献人员',
    cardTitle: '关于 LetRecovery',
    version: '版本',
    license: '许可证',
    copyright: '版权所有',
    source: '开源地址',
    ackTitle: '致谢',
    acks: [
      '部分系统镜像及 PE 下载服务由 Cloud-PE 云盘提供',
      '感谢 电脑病毒爱好者 提供 WinPE 及制作宣传视频',
      '以及 Cloud-PE 项目的全体贡献人员',
    ],
    contributors: '贡献人员',
  },
  qqgroup: {
    title: '加入社区',
    desc: '选择一个合适的 QQ 群加入，与其他用户交流使用心得',
    groupName: 'LetRecovery 交流群',
    capacity: '500人',
    status: '可用',
    viewQr: '查看二维码',
    join: '点击加入',
    qrAlt: '群二维码',
  },
  footer: {
    friends: '友情链接',
    crafted: '由 Cloud-PE Dev 用心打造',
  },
  notFound: {
    title: '页面不存在',
    desc: '你访问的页面可能已被移动或删除，请检查网址是否正确。',
    home: '返回首页',
  },
  docs: {
    copyMarkdown: '复制 Markdown',
    copied: '已复制',
    onThisPage: '本页大纲',
    notFound: '页面不存在',
    notFoundDesc: '没有找到这篇文档，请从左侧目录重新选择。',
    searchLabel: '搜索文档',
    searchPlaceholder: '搜索文档…',
    searchEmpty: '没有找到相关内容',
    searchEmptyHint: '换个关键词试试',
    goToPage: '跳转到页面',
  },
}

const en: Dict = {
  nav: {
    home: 'Home',
    docs: 'Docs',
    about: 'About',
    community: 'Community',
    menu: 'Menu',
    docsHeading: 'Documentation',
  },
  common: {
    github: 'GitHub repository',
    toggleTheme: 'Toggle theme',
    light: 'Light',
    dark: 'Dark',
    system: 'System',
    language: 'Language',
    zh: '简体中文',
    en: 'English',
  },
  home: {
    heroTitleLines: ['A clean, ad-free', 'Windows reinstall tool'],
    heroDesc:
      'Crafted with Rust + egui for blazing performance and a clean, ad-free, bloat-free experience. Its simple, intuitive interface makes reinstalling Windows easy — even for beginners.',
    download: 'Download',
    joinCommunity: 'Join Community',
    screenshotAlt: 'LetRecovery interface preview',
    dialogTitle: 'Download LetRecovery',
    dialogDesc: 'Get the latest version of the LetRecovery reinstall tool',
    alertTitle: 'About this download',
    alertDesc:
      'Every source below provides the same file — the full LetRecovery build with WinPE bundled in, ready to use out of the box, no separate WinPE needed. All sources are identical, so just pick whichever is fastest for you.',
    sourceLabel: 'Choose a download source:',
    close: 'Close',
    featuresTitle: 'Features',
    featuresDesc:
      "Explore LetRecovery's core capabilities that make reinstalling Windows simpler than ever.",
    features: [
      { title: 'Blazing Fast', desc: 'Built in Rust for outstanding performance and a tiny footprint — your reinstall stays a step ahead.' },
      { title: 'Clean & Bundle-free', desc: 'Fully open source and transparent, with zero ads or bundled software for a clean experience.' },
      { title: 'Easy to Use', desc: "A carefully designed modern interface that's intuitive enough for complete beginners." },
      { title: 'Fast Deployment', desc: 'A one-click workflow takes you from launch to a finished reinstall in just a few steps.' },
      { title: 'Powerful', desc: 'Supports many system image formats with rich customization options for every reinstall need.' },
      { title: 'Safe & Reliable', desc: 'Advanced safeguards keep data transfer and the install process stable and secure.' },
    ],
    demoTitle: 'Demo',
    demoDescLine: 'Watch a short video to see how LetRecovery reinstalls Windows quickly',
    demoCreditPrefix: '(Video by ',
    demoCreditSuffix: ')',
  },
  about: {
    title: 'About',
    desc: 'Project information and contributors of LetRecovery',
    cardTitle: 'About LetRecovery',
    version: 'Version',
    license: 'License',
    copyright: 'Copyright',
    source: 'Source',
    ackTitle: 'Acknowledgements',
    acks: [
      'System images and PE download service partly provided by Cloud-PE Drive',
      'Thanks to 电脑病毒爱好者 for the WinPE and the promo video',
      'And all contributors of the Cloud-PE project',
    ],
    contributors: 'Contributors',
  },
  qqgroup: {
    title: 'Join the Community',
    desc: 'Pick a QQ group to join and share tips with other users',
    groupName: 'LetRecovery Group',
    capacity: '500 members',
    status: 'Open',
    viewQr: 'View QR code',
    join: 'Join',
    qrAlt: 'group QR code',
  },
  footer: {
    friends: 'Friends',
    crafted: 'Crafted with care by Cloud-PE Dev',
  },
  notFound: {
    title: 'Page not found',
    desc: 'The page you are looking for may have been moved or deleted. Please check the URL.',
    home: 'Back to home',
  },
  docs: {
    copyMarkdown: 'Copy Markdown',
    copied: 'Copied',
    onThisPage: 'On this page',
    notFound: 'Page not found',
    notFoundDesc: 'This document could not be found. Please pick another from the menu.',
    searchLabel: 'Search docs',
    searchPlaceholder: 'Search documentation…',
    searchEmpty: 'No results found.',
    searchEmptyHint: 'Try a different keyword',
    goToPage: 'Go to Page',
  },
}

const translations: Record<Lang, Dict> = { zh, en }
