import { useState, type ReactNode } from 'react'
import { motion, useReducedMotion } from 'motion/react'

const GITHUB_URL = 'https://github.com/NORMAL-EX/LetRecovery'
const RELEASE_URL = `${GITHUB_URL}/releases`

type IconProps = {
  children: ReactNode
  size?: number
}

function Icon({ children, size = 24 }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      className="icon"
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
  <Icon size={18}>
    <path d="M7 17 17 7M8 7h9v9" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
  </Icon>
)

const ArrowDown = () => (
  <Icon size={18}>
    <path d="M12 4v14m0 0-5-5m5 5 5-5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
  </Icon>
)

const featureItems = [
  {
    number: '01',
    title: '装系统，不赌运气',
    body: '从 WIM、ESD、SWM、GHO 到 ISO，先识别镜像、目标磁盘与引导环境，再决定如何写入。',
    icon: (
      <Icon>
        <path d="M4 6.5h16v11H4zM8 21h8M12 17.5V21" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6" />
        <path d="m8.5 11 2 2 5-5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.8" />
      </Icon>
    ),
  },
  {
    number: '02',
    title: '备份，也要能找回来',
    body: '完整或增量捕获系统镜像，保留名称、描述与格式选择，让恢复不是一份来历不明的文件。',
    icon: (
      <Icon>
        <path d="M5 4h12l2 2v14H5z" stroke="currentColor" strokeLinejoin="round" strokeWidth="1.6" />
        <path d="M8 4v6h8V4M8 16h8" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6" />
      </Icon>
    ),
  },
  {
    number: '03',
    title: '进得去，也退得出',
    body: '桌面端与 WinPE 双端协作。当前系统无法直接处理时，把任务安全交接给离线环境继续完成。',
    icon: (
      <Icon>
        <path d="M12 3a9 9 0 1 0 9 9" stroke="currentColor" strokeLinecap="round" strokeWidth="1.6" />
        <path d="M12 7v5l3 2M16 3h5v5" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6" />
      </Icon>
    ),
  },
  {
    number: '04',
    title: '工具不多一步，边界不少一步',
    body: 'BitLocker、引导修复、驱动迁移、分区与密码处理各归其位；高风险操作保持确认与复核。',
    icon: (
      <Icon>
        <path d="M14.5 6.5a4 4 0 0 0-5 5L4 17l3 3 5.5-5.5a4 4 0 0 0 5-5l-2.5 2-2.5-2.5z" stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.6" />
      </Icon>
    ),
  },
]

const workflow = [
  ['读懂现场', '识别镜像、固件、磁盘、分区与加密状态，先把“要动哪里”说清楚。'],
  ['验证意图', '检查格式、架构、引导代际与校验值；不确定的状态不会被包装成成功。'],
  ['执行与交接', '能在线完成就直接执行，需要离线环境时再进入 WinPE，并保留进度与诊断。'],
]

function App() {
  const [menuOpen, setMenuOpen] = useState(false)
  const reduceMotion = useReducedMotion()
  const reveal = reduceMotion
    ? {}
    : {
        initial: { opacity: 0, y: 24 },
        whileInView: { opacity: 1, y: 0 },
        viewport: { once: true, amount: 0.22 },
        transition: { duration: 0.55, ease: [0.16, 1, 0.3, 1] as const },
      }

  const closeMenu = () => setMenuOpen(false)

  return (
    <div className="site-shell">
      <header className="site-header">
        <div className="page-container header-inner">
          <a className="brand" href="#top" aria-label="LetRecovery 首页" onClick={closeMenu}>
            <img alt="" className="brand-mark" height="36" src="/letrecovery.png" width="36" />
            <span>LetRecovery</span>
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
            <a href="#features" onClick={closeMenu}>能力</a>
            <a href="#safety" onClick={closeMenu}>安全边界</a>
            <a href="#workflow" onClick={closeMenu}>工作方式</a>
            <a href={GITHUB_URL} rel="noreferrer" target="_blank" onClick={closeMenu}>源码</a>
            <a className="nav-cta" href={RELEASE_URL} rel="noreferrer" target="_blank" onClick={closeMenu}>
              下载 <ArrowUpRight />
            </a>
          </nav>
        </div>
      </header>

      <main id="top">
        <section className="hero-section">
          <div className="page-container hero-grid">
            <motion.div
              className="hero-copy"
              initial={reduceMotion ? undefined : { opacity: 0, y: 24 }}
              animate={reduceMotion ? undefined : { opacity: 1, y: 0 }}
              transition={{ duration: 0.65, ease: [0.16, 1, 0.3, 1] }}
            >
              <div className="eyebrow">
                <span className="eyebrow-dot" />
                Windows 系统安装与恢复工具
              </div>
              <h1>
                重装系统，<br />
                不该像在<em>拆炸弹。</em>
              </h1>
              <p className="hero-lead">
                LetRecovery 把镜像、分区、引导与 WinPE 串成一条可验证的恢复路径。每一步知道自己在做什么，也知道何时应该停下。
              </p>
              <div className="hero-actions">
                <a className="btn btn-primary" href={RELEASE_URL} rel="noreferrer" target="_blank">
                  获取最新版 <ArrowUpRight />
                </a>
                <a className="btn btn-secondary" href="#workflow">
                  看它如何工作 <ArrowDown />
                </a>
              </div>
              <div className="hero-meta" aria-label="产品信息">
                <span>v2026.7.9</span>
                <span>Windows 10 / 11</span>
                <span>源码公开 · 非商业使用免费</span>
              </div>
            </motion.div>

            <motion.figure
              className="hero-art"
              initial={reduceMotion ? undefined : { opacity: 0, x: 32, rotate: 1.5 }}
              animate={reduceMotion ? undefined : { opacity: 1, x: 0, rotate: 0 }}
              transition={{ delay: 0.12, duration: 0.8, ease: [0.16, 1, 0.3, 1] }}
            >
              <img
                alt="一只手把散乱的系统线路梳理成一条清晰、稳定的恢复路径"
                fetchPriority="high"
                height="1024"
                loading="eager"
                src="/hero-recovery.png"
                width="1536"
              />
              <figcaption>
                <span>写入之前</span>
                先确认目标，再开始恢复。
              </figcaption>
            </motion.figure>
          </div>
        </section>

        <section className="proof-strip" aria-label="核心支持范围">
          <div className="page-container proof-grid">
            <div><strong>5+</strong><span>镜像与介质格式</span></div>
            <div><strong>双端</strong><span>桌面环境 + WinPE</span></div>
            <div><strong>SHA-256</strong><span>联网资源优先校验</span></div>
            <div><strong>UEFI / Legacy</strong><span>新旧引导路径并存</span></div>
          </div>
        </section>

        <section className="features-section section-content" id="features">
          <div className="page-container">
            <motion.div className="section-heading" {...reveal}>
              <div>
                <span className="section-kicker">它能做什么</span>
                <h2>复杂留给工具，<br />选择留给你。</h2>
              </div>
              <p>
                安装、备份、离线恢复和系统维护不是四个孤立按钮，而是一套共享目标校验、镜像处理与错误语义的工作流。
              </p>
            </motion.div>

            <div className="features-grid">
              {featureItems.map((feature, index) => (
                <motion.article
                  className="feature-card"
                  key={feature.number}
                  {...reveal}
                  transition={{ duration: 0.5, delay: index * 0.07, ease: [0.16, 1, 0.3, 1] }}
                >
                  <div className="feature-card-top">
                    <span className="feature-icon">{feature.icon}</span>
                    <span className="feature-number">{feature.number}</span>
                  </div>
                  <h3>{feature.title}</h3>
                  <p>{feature.body}</p>
                </motion.article>
              ))}
            </div>
          </div>
        </section>

        <section className="safety-section" id="safety">
          <div className="page-container safety-grid">
            <motion.div className="safety-copy" {...reveal}>
              <span className="section-kicker section-kicker--light">安全不是弹窗，是顺序</span>
              <h2>每一次写入之前，<br />都先把目标说清楚。</h2>
              <p>
                系统工具最怕的不是报错，而是在不确定时继续。LetRecovery 的边界很朴素：看不清，就停；校验不对，就不写。
              </p>
              <a href={`${GITHUB_URL}/blob/main/SECURITY.md`} rel="noreferrer" target="_blank">
                阅读安全说明 <ArrowUpRight />
              </a>
            </motion.div>

            <ol className="safety-list">
              <motion.li {...reveal}>
                <span>01</span>
                <div><h3>目标身份复核</h3><p>不只相信 UI 里缓存的盘符；执行前重新核对磁盘、容量与分区身份。</p></div>
              </motion.li>
              <motion.li {...reveal}>
                <span>02</span>
                <div><h3>写盘前预检</h3><p>PCA、架构、引导文件与镜像类型在格式化之前完成检查，失败就关闭流程。</p></div>
              </motion.li>
              <motion.li {...reveal}>
                <span>03</span>
                <div><h3>错误保留上下文</h3><p>用户看到清晰结论，日志保留下层原因；启动失败、退出码与文本错误分别判断。</p></div>
              </motion.li>
            </ol>
          </div>
        </section>

        <section className="workflow-section section-content" id="workflow">
          <div className="page-container workflow-grid">
            <motion.div className="workflow-heading" {...reveal}>
              <span className="section-kicker">工作方式</span>
              <h2>三步，把混乱<br />变成可预期。</h2>
              <p>真正可靠的恢复，不从“开始安装”开始，而从读懂当前机器开始。</p>
              <div className="format-cloud" aria-label="支持的镜像与平台关键词">
                {['WIM', 'ESD', 'SWM', 'GHO', 'ISO', 'BitLocker', 'PCA2023', 'WinPE'].map((item) => (
                  <span key={item}>{item}</span>
                ))}
              </div>
            </motion.div>

            <ol className="workflow-list">
              {workflow.map(([title, body], index) => (
                <motion.li key={title} {...reveal}>
                  <span className="workflow-index">{String(index + 1).padStart(2, '0')}</span>
                  <div><h3>{title}</h3><p>{body}</p></div>
                </motion.li>
              ))}
            </ol>
          </div>
        </section>

        <section className="closing-section">
          <div className="page-container">
            <motion.div className="closing-card" {...reveal}>
              <div>
                <span className="section-kicker">准备好了</span>
                <h2>把下一次重装，<br />变成一件可预期的事。</h2>
              </div>
              <div className="closing-actions">
                <a className="btn btn-primary" href={RELEASE_URL} rel="noreferrer" target="_blank">
                  下载 LetRecovery <ArrowUpRight />
                </a>
                <p>需要管理员权限 · 建议先备份重要数据</p>
              </div>
            </motion.div>
          </div>
        </section>
      </main>

      <footer className="site-footer">
        <div className="page-container footer-grid">
          <div className="footer-brand">
            <a className="brand brand--footer" href="#top">
              <img alt="" className="brand-mark" height="36" src="/letrecovery.png" width="36" />
              <span>LetRecovery</span>
            </a>
            <p>面向非商业场景、源码公开的 Windows 系统安装与恢复工具。</p>
          </div>
          <div className="footer-links">
            <a href={RELEASE_URL} rel="noreferrer" target="_blank">下载</a>
            <a href={GITHUB_URL} rel="noreferrer" target="_blank">GitHub</a>
            <a href={`${GITHUB_URL}/issues`} rel="noreferrer" target="_blank">问题反馈</a>
            <a href={`${GITHUB_URL}/blob/main/LICENSE`} rel="noreferrer" target="_blank">许可证</a>
          </div>
          <p className="footer-meta">© 2026 NORMAL-EX · PolyForm Noncommercial 1.0.0</p>
        </div>
      </footer>
    </div>
  )
}

export default App
