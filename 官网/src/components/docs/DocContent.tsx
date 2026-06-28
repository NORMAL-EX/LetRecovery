import { useCallback, useEffect, useMemo, useRef } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { type DocPageData, docTitle } from '@/lib/docs'
import { cn } from '@/lib/utils'
import DocsCopyPage from './DocsCopyPage'

/** 把文档里作者写的根相对链接（/guide/x）映射到官网内的 /docs 路由 */
function toDocsPath(href: string): string {
  if (href === '/') return '/docs'
  if (href.startsWith('/docs')) return href
  if (href.startsWith('/')) return '/docs' + href
  return href
}

// 图片加载中/失败时的占位图（public/img-placeholder.svg —— 灰色「图片」占位）
const IMG_PLACEHOLDER = '/img-placeholder.svg'

export default function DocContent({ page }: { page: DocPageData }) {
  const contentRef = useRef<HTMLDivElement>(null)
  const navigate = useNavigate()
  const location = useLocation()

  const title = docTitle(page)
  const description = page.frontmatter.description
  // 正文首个 h1 被剥掉单独当标题显示，这里把它原本的锚点 id 补回标题上，
  // 这样指向页面标题的同页锚点链接（如 #插件投稿）仍能正确定位。
  const titleId = page.headings.find((h) => h.level === 1)?.slug

  // 正文里首个 <h1> 与上面的大标题重复，去掉它（大纲只用 h2/h3，不受影响）
  const bodyHtml = useMemo(
    () => page.html.replace(/^\s*<h1[\s\S]*?<\/h1>/, ''),
    [page.html],
  )

  // 切换文档时：带锚点（分享链接）则滚到对应标题，否则回到顶部
  useEffect(() => {
    if (location.hash) {
      const el = document.getElementById(decodeURIComponent(location.hash.slice(1)))
      if (el) {
        el.scrollIntoView()
        return
      }
    }
    window.scrollTo(0, 0)
    // 仅依赖 page.route：点击标题更新 hash 时不应重新滚动
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [page.route])

  // 代码块复制按钮
  useEffect(() => {
    const host = contentRef.current
    if (!host) return
    const cleanups: Array<() => void> = []
    host.querySelectorAll<HTMLButtonElement>('[data-copy]').forEach((btn) => {
      const onClick = () => {
        const pre = btn.closest('.cosspress-code')?.querySelector('pre')
        const text = pre?.textContent ?? ''
        if (!navigator.clipboard) return
        void navigator.clipboard.writeText(text).then(() => {
          btn.classList.add('copied')
          window.setTimeout(() => btn.classList.remove('copied'), 2000)
        })
      }
      btn.addEventListener('click', onClick)
      cleanups.push(() => btn.removeEventListener('click', onClick))
    })
    return () => cleanups.forEach((fn) => fn())
  }, [page])

  // 图片加载失败 → 替换成醒目的占位（足够高、图标水平+垂直居中且放大）
  useEffect(() => {
    const host = contentRef.current
    if (!host) return
    const cleanups: Array<() => void> = []
    host.querySelectorAll('img').forEach((img) => {
      // 加载失败 → 替换成响应式占位图（填满正文宽度）
      const fail = () => {
        if (img.dataset.fallbackDone) return
        img.dataset.fallbackDone = '1'
        const alt = img.getAttribute('alt') || ''
        const fb = document.createElement('div')
        fb.className = 'my-5 w-full'
        fb.setAttribute('role', 'img')
        if (alt) fb.setAttribute('aria-label', alt)
        const ph = document.createElement('img')
        ph.src = IMG_PLACEHOLDER
        ph.alt = ''
        ph.className = 'w-full select-none rounded-xl'
        fb.appendChild(ph)
        img.replaceWith(fb)
      }
      // 加载中 → 占位背景（预留尺寸，响应式）
      if (!img.complete) {
        img.style.minHeight = '12rem'
        img.style.width = '100%'
        img.style.backgroundImage = `url(${IMG_PLACEHOLDER})`
        img.style.backgroundPosition = 'center'
        img.style.backgroundRepeat = 'no-repeat'
        img.style.backgroundSize = 'contain'
        const loaded = () => {
          img.style.minHeight = ''
          img.style.width = ''
          img.style.background = ''
        }
        img.addEventListener('load', loaded, { once: true })
        cleanups.push(() => img.removeEventListener('load', loaded))
      }
      if (img.complete && img.naturalWidth === 0) fail()
      else {
        img.addEventListener('error', fail)
        cleanups.push(() => img.removeEventListener('error', fail))
      }
    })
    return () => cleanups.forEach((fn) => fn())
  }, [page])

  // 挂载媒体岛：把 <div data-player="video" …> 渲染成 ArtPlayer（按需懒加载）
  useEffect(() => {
    const host = contentRef.current
    if (!host) return
    const islands = host.querySelectorAll<HTMLElement>('[data-player="video"]')
    if (!islands.length) return
    let roots: Array<{ unmount: () => void }> = []
    let cancelled = false
    void Promise.all([import('react-dom/client'), import('./VideoPlayer')]).then(
      ([{ createRoot }, { default: VideoPlayer }]) => {
        if (cancelled) return
        islands.forEach((el) => {
          el.innerHTML = ''
          const root = createRoot(el)
          root.render(
            <VideoPlayer
              src={el.getAttribute('data-src') ?? undefined}
              poster={el.getAttribute('data-poster') ?? undefined}
              title={el.getAttribute('data-title') ?? undefined}
              qualities={el.getAttribute('data-qualities') ?? undefined}
              audioTracks={el.getAttribute('data-audio-tracks') ?? undefined}
            />,
          )
          roots.push(root)
        })
      },
    )
    return () => {
      cancelled = true
      const toUnmount = roots
      roots = []
      // 延迟卸载，避免在 React 渲染期间同步卸载根
      window.setTimeout(() => toUnmount.forEach((r) => r.unmount()), 0)
    }
  }, [page])

  // 拦截正文内链接：站内文档 → 客户端路由；#锚点 → 平滑滚动；外链 → 默认
  const onContentClick = useCallback((e: React.MouseEvent) => {
    const a = (e.target as HTMLElement).closest('a')
    if (!a) return
    const href = a.getAttribute('href')
    if (!href) return
    if (
      /^https?:\/\//.test(href) ||
      a.target === '_blank' ||
      href.startsWith('mailto:')
    )
      return
    e.preventDefault()
    if (href.startsWith('#')) {
      const el = document.getElementById(decodeURIComponent(href.slice(1)))
      if (el) {
        el.scrollIntoView({ behavior: 'smooth' })
        // 点击标题后，URL 末尾加上 #锚点（便于复制/分享）
        navigate({ pathname: page.route, hash: href }, { replace: true })
      }
      return
    }
    const [path, hash] = href.split('#')
    navigate(toDocsPath(path))
    if (hash) {
      window.setTimeout(() => {
        document.getElementById(decodeURIComponent(hash))?.scrollIntoView()
      }, 60)
    }
  }, [navigate, page.route])

  // 用 useMemo 把正文按 bodyHtml 缓存成同一个元素引用：点击标题更新 hash（location 变化）
  // 引起重渲染时，React 跳过这个未变化的子树，不会重建 dangerouslySetInnerHTML 的 DOM——
  // 否则挂载在正文里的视频等 React 岛会被一并清空消失。
  const content = useMemo(
    () => (
      // biome-ignore lint: 渲染受信任的构建期生成 HTML
      <div
        ref={contentRef}
        onClick={onContentClick}
        className={cn(
          'prose prose-neutral max-w-none dark:prose-invert',
          'prose-headings:font-heading prose-headings:font-semibold prose-headings:tracking-tight',
          'prose-a:font-medium prose-a:text-foreground prose-a:underline-offset-4',
          'prose-img:rounded-xl prose-pre:bg-transparent prose-pre:p-0',
          '*:data-[slot=alert]:first:mt-0',
        )}
        dangerouslySetInnerHTML={{ __html: bodyHtml }}
      />
    ),
    [bodyHtml, onContentClick],
  )

  return (
    <article className="flex min-w-0 flex-col gap-8">
      {/* 页头：大标题 + 描述 + 复制 Markdown（与 coss.com/ui 一致） */}
      <header className="flex flex-col gap-2">
        <h1
          id={titleId}
          className="scroll-m-20 font-heading text-3xl font-semibold tracking-tight xl:text-4xl"
        >
          {title}
        </h1>
        {description && (
          <p className="text-muted-foreground sm:text-lg">{description}</p>
        )}
        <div className="flex items-center gap-2 pt-4">
          <DocsCopyPage raw={page.raw} />
        </div>
      </header>

      {/* 正文（已 memo，避免点击标题等重渲染时重建 DOM、清空 React 岛） */}
      {content}
    </article>
  )
}
