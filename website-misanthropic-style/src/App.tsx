import { useState, type ReactNode } from 'react'

const GITHUB_URL = 'https://github.com/NORMAL-EX/LetRecovery'
const RELEASE_URL = `${GITHUB_URL}/releases`

type IconProps = {
  children: ReactNode
  size?: number
}

function Icon({ children, size = 16 }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      fill="none"
      focusable="false"
      height={size}
      viewBox="0 0 24 24"
      width={size}
    >
      {children}
    </svg>
  )
}

const ArrowUpRight = () => (
  <Icon>
    <path
      d="M7 17 17 7M8 7h9v9"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth="1.7"
    />
  </Icon>
)

const ArrowRight = () => (
  <Icon>
    <path
      d="M5 12h14m-5-5 5 5-5 5"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth="1.7"
    />
  </Icon>
)

const capabilities = [
  {
    title: '安装与重装',
    body: '从 WIM、ESD、SWM、GHO 到 ISO，先识别镜像、架构、启动方式与目标磁盘，再决定如何写入。',
    meta: [
      ['范围', 'Windows 10 / 11'],
      ['镜像', 'WIM · ESD · SWM · GHO · ISO'],
    ],
    href: RELEASE_URL,
    action: '获取最新版',
  },
  {
    title: '备份与恢复',
    body: '捕获完整或增量系统镜像，保留名称、描述与格式选择，让每份备份都清楚说明自己从哪里来。',
    meta: [
      ['方式', '完整捕获 · 增量捕获'],
      ['校验', 'MD5 · SHA-256'],
    ],
    href: '#principles',
    action: '查看安全原则',
  },
  {
    title: '桌面端与 WinPE',
    body: '能在当前系统完成的直接处理；必须离线时，再把任务和验证结果完整交接给 WinPE。',
    meta: [
      ['环境', '桌面系统 · WinPE'],
      ['启动', 'UEFI · Legacy'],
    ],
    href: GITHUB_URL,
    action: '查看源代码',
  },
]

const principles = [
  ['写入之前确认目标', '目标校验'],
  ['无法安全判断时停止', '失败关闭'],
  ['错误必须保留下层原因', '可诊断'],
  ['桌面与 WinPE 使用同一套边界', '双端一致'],
  ['联网资源优先使用 SHA-256 校验', '完整性'],
]

function App() {
  const [menuOpen, setMenuOpen] = useState(false)
  const closeMenu = () => setMenuOpen(false)

  return (
    <div className="site-shell">
      <header className="site-header">
        <div className="page-container header-inner">
          <a className="brand" href="#top" aria-label="LetRecovery 首页" onClick={closeMenu}>
            <img alt="" className="brand-mark" height="28" src="/letrecovery.png" width="28" />
            <span>LETRECOVERY</span>
          </a>

          <button
            aria-controls="primary-navigation"
            aria-expanded={menuOpen}
            aria-label={menuOpen ? '关闭导航' : '打开导航'}
            className="menu-button"
            onClick={() => setMenuOpen((open) => !open)}
            type="button"
          >
            <span />
            <span />
          </button>

          <nav
            aria-label="主导航"
            className={`primary-nav${menuOpen ? ' primary-nav--open' : ''}`}
            id="primary-navigation"
          >
            <a href="#capabilities" onClick={closeMenu}>能力</a>
            <a href="#principles" onClick={closeMenu}>安全原则</a>
            <a href={GITHUB_URL} rel="noreferrer" target="_blank" onClick={closeMenu}>源代码</a>
            <a className="button button-nav" href={RELEASE_URL} rel="noreferrer" target="_blank" onClick={closeMenu}>
              下载 LetRecovery <ArrowUpRight />
            </a>
          </nav>
        </div>
      </header>

      <main id="top">
        <section className="hero-section">
          <div className="page-container hero-grid">
            <h1>
              <span className="headline-line">系统安装与恢复，</span>
              <span className="headline-line">应该建立在</span>
              <span className="headline-line"><span className="headline-underline">清晰边界</span>上。</span>
            </h1>
            <p>
              LetRecovery 将镜像、磁盘、引导与 WinPE 串成一条可以核对、可以理解的恢复路径。在每一次写入之前，先把目标说清楚。
            </p>
          </div>

          <figure className="page-container hero-visual">
            <img
              alt="粗粝手绘线条描绘的系统恢复路径"
              fetchPriority="high"
              height="1024"
              loading="eager"
              src="/hero-recovery.png"
              width="1536"
            />
          </figure>
        </section>

        <section className="capabilities-section" id="capabilities">
          <div className="page-container">
            <h2 className="section-title">核心能力</h2>
            <div className="capability-grid">
              {capabilities.map((item) => (
                <article className="capability-card" key={item.title}>
                  <div>
                    <h3>{item.title}</h3>
                    <p>{item.body}</p>
                  </div>
                  <div className="capability-card-bottom">
                    <dl>
                      {item.meta.map(([term, value]) => (
                        <div key={term}>
                          <dt>{term}</dt>
                          <dd>{value}</dd>
                        </div>
                      ))}
                    </dl>
                    <a className="button button-card" href={item.href} rel={item.href.startsWith('http') ? 'noreferrer' : undefined} target={item.href.startsWith('http') ? '_blank' : undefined}>
                      {item.action} <ArrowRight />
                    </a>
                  </div>
                </article>
              ))}
            </div>
          </div>
        </section>

        <section className="principles-section" id="principles">
          <div className="page-container principles-grid">
            <h2>我们把系统恢复，<br />建立在清晰的边界上。</h2>
            <ol>
              {principles.map(([title, category]) => (
                <li key={title}>
                  <h3>{title}</h3>
                  <span>{category}</span>
                </li>
              ))}
            </ol>
          </div>
        </section>
      </main>

      <footer className="site-footer">
        <div className="page-container footer-grid">
          <a className="footer-symbol" href="#top" aria-label="返回顶部">
            <img alt="" height="32" src="/letrecovery.png" width="32" />
            <span>LETRECOVERY</span>
          </a>
          <div className="footer-links">
            <div>
              <h2>项目</h2>
              <a href={RELEASE_URL} rel="noreferrer" target="_blank">下载</a>
              <a href={GITHUB_URL} rel="noreferrer" target="_blank">GitHub</a>
              <a href={`${GITHUB_URL}/issues`} rel="noreferrer" target="_blank">问题反馈</a>
            </div>
            <div>
              <h2>说明</h2>
              <a href={`${GITHUB_URL}#readme`} rel="noreferrer" target="_blank">项目介绍</a>
              <a href={`${GITHUB_URL}/blob/main/LICENSE`} rel="noreferrer" target="_blank">许可证</a>
              <a href="#principles">安全原则</a>
            </div>
          </div>
          <p>© 2026 NORMAL-EX · PolyForm Noncommercial 1.0.0</p>
        </div>
      </footer>
    </div>
  )
}

export default App
