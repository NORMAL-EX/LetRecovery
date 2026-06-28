import type { Plugin } from 'vite'
import matter from 'gray-matter'
import MarkdownIt from 'markdown-it'
import anchor from 'markdown-it-anchor'
import container from 'markdown-it-container'
import Shiki from '@shikijs/markdown-it'
import { transformerMetaHighlight } from '@shikijs/transformers'
import { slugify } from '../src/lib/slugify'

type Heading = { level: number; title: string; slug: string }

// 容器无显式标题时的默认标题，按文档语言（路径含 /docs/en/ 则英文）取用。
const DEFAULT_TITLES: Record<'zh' | 'en', Record<string, string>> = {
  zh: {
    tip: '提示',
    info: '信息',
    success: '成功',
    warning: '警告',
    danger: '危险',
    details: '详情',
  },
  en: {
    tip: 'TIP',
    info: 'INFO',
    success: 'SUCCESS',
    warning: 'WARNING',
    danger: 'DANGER',
    details: 'Details',
  },
}

// 在 transform() 里按当前文件路径设置；md.render 为同步执行，渲染期间不会被改动。
let currentLocale: 'zh' | 'en' = 'zh'

let mdPromise: Promise<MarkdownIt> | null = null

async function getMd(): Promise<MarkdownIt> {
  if (mdPromise) return mdPromise
  mdPromise = (async () => {
    const md = MarkdownIt({ html: true, linkify: true, typographer: true })

    // Shiki syntax highlighting with light + dark themes (CSS-variable based).
    md.use(
      await Shiki({
        themes: { light: 'github-light', dark: 'github-dark' },
        transformers: [
          transformerMetaHighlight(),
          {
            name: 'cosspress-lang-attr',
            pre(node: any) {
              node.properties['data-lang'] =
                (this as any).options?.lang ?? 'text'
            },
          },
        ],
      }),
    )

    // Heading anchors: add `id` slugs (used by the outline) plus a clickable
    // `#` permalink before each heading, revealed on hover via CSS.
    // 整个标题作为指向自身锚点的链接（仿 coss.com/ui）：没有前置 "#"，hover 显示下划线。
    md.use(anchor, {
      slugify,
      level: [1, 2, 3, 4, 5, 6],
      permalink: anchor.permalink.headerLink(),
    })

    // Wrap every table in a rounded container so it reads as a card.
    md.renderer.rules.table_open = () => '<div class="table-wrapper">\n<table>\n'
    md.renderer.rules.table_close = () => '</table>\n</div>\n'

    // VitePress-style containers rendered as native coss ui Alerts.
    const ALERT_BASE =
      'relative my-5 grid w-full items-start gap-x-2 gap-y-0.5 rounded-xl border px-3.5 py-3 text-card-foreground text-sm has-[>svg]:grid-cols-[calc(var(--spacing)*4)_1fr] has-[>svg]:gap-x-2 [&>svg]:h-lh [&>svg]:w-4'
    const ALERT_VARIANT: Record<string, string> = {
      tip: 'border-success/32 bg-success/4 [&>svg]:text-success',
      success: 'border-success/32 bg-success/4 [&>svg]:text-success',
      info: 'border-info/32 bg-info/4 [&>svg]:text-info',
      warning: 'border-warning/32 bg-warning/4 [&>svg]:text-warning',
      danger: 'border-destructive/32 bg-destructive/4 [&>svg]:text-destructive',
    }
    const svg = (paths: string) =>
      `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths}</svg>`
    const ALERT_ICON: Record<string, string> = {
      tip: svg('<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>'),
      success: svg('<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>'),
      info: svg('<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>'),
      warning: svg(
        '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>',
      ),
      danger: svg('<circle cx="12" cy="12" r="10"/><path d="M12 8v4"/><path d="M12 16h.01"/>'),
    }

    for (const type of ['tip', 'success', 'info', 'warning', 'danger'] as const) {
      md.use(container, type, {
        render(tokens: any[], idx: number) {
          const token = tokens[idx]
          if (token.nesting === 1) {
            const info = token.info.trim().slice(type.length).trim()
            const title = md.utils.escapeHtml(
              info || DEFAULT_TITLES[currentLocale][type],
            )
            return `<div role="alert" data-slot="alert" class="${ALERT_BASE} ${ALERT_VARIANT[type]}">${ALERT_ICON[type]}<div data-slot="alert-title" class="font-medium text-muted-foreground [svg~&]:col-start-2">${title}</div><div data-slot="alert-description" class="cosspress-alert-desc text-muted-foreground [svg~&]:col-start-2">\n`
          }
          return '</div></div>\n'
        },
      })
    }

    md.use(container, 'details', {
      render(tokens: any[], idx: number) {
        const token = tokens[idx]
        if (token.nesting === 1) {
          const info = token.info.trim().slice('details'.length).trim()
          const title = md.utils.escapeHtml(
            info || DEFAULT_TITLES[currentLocale].details,
          )
          return `<details class="cosspress-details my-5 rounded-xl border bg-card px-3.5 py-3 text-sm"><summary class="cursor-pointer select-none font-medium">${title}</summary><div class="cosspress-alert-desc mt-2 text-muted-foreground">\n`
        }
        return '</div></details>\n'
      },
    })

    return md
  })()
  return mdPromise
}

type MdToken = ReturnType<MarkdownIt['parse']>[number]

/** Plain text of a heading's inline tokens — text + inline code only, matching
 * what markdown-it-anchor uses, so links/images/bold don't leak markup. */
function headingText(inline: MdToken | undefined): string {
  if (inline?.children?.length) {
    return inline.children
      .filter((t) => t.type === 'text' || t.type === 'code_inline')
      .map((t) => t.content)
      .join('')
  }
  return inline?.content ?? ''
}

function extractHeadings(md: MarkdownIt, content: string): Heading[] {
  const tokens = md.parse(content, {})
  const headings: Heading[] = []
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i]
    if (token.type !== 'heading_open') continue
    const level = Number(token.tag.slice(1))
    const inline = tokens[i + 1]
    const title = headingText(inline).trim()
    if (!title) continue
    // markdown-it-anchor has already assigned the real `id` (incl. de-dup like
    // `foo-1`) onto the heading token during parse — reuse it so the outline,
    // mobile dropdown and search slugs match the ids rendered into the HTML.
    const slug = token.attrGet('id') ?? slugify(title)
    headings.push({ level, title, slug })
  }
  return headings
}

const TICK_CHECK =
  '<svg class="cosspress-tick cosspress-tick-yes" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-label="yes"><path d="M20 6 9 17l-5-5"/></svg>'
const TICK_CROSS =
  '<svg class="cosspress-tick cosspress-tick-no" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-label="no"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>'

/** Replace status emoji (used a lot in comparison tables) with lucide icons.
 * Skips `<pre>…</pre>` and inline `<code>…</code>` so literal emoji inside code
 * samples are left intact. */
function replaceTicks(html: string): string {
  return html.replace(
    /<pre[\s\S]*?<\/pre>|<code[\s\S]*?<\/code>|[^<]+/g,
    (segment) => {
      if (segment[0] === '<') return segment // a code region — leave untouched
      return segment
        .replace(/✅|✔️?/g, TICK_CHECK)
        .replace(/❌|✖️?|❎|✗|✘/g, TICK_CROSS)
    },
  )
}

const COPY_BUTTON =
  '<button class="cosspress-code-copy" type="button" data-copy aria-label="Copy code">' +
  '<svg class="cosspress-copy-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>' +
  '<span class="cosspress-copied-text">Copied!</span>' +
  '</button>'

/**
 * Wrap each Shiki code block in a coss ui surface with line numbers (via CSS)
 * and a floating copy button, mirroring the hexo-theme-coss code block design.
 */
function wrapCodeBlocks(html: string): string {
  return html.replace(
    /<pre class="shiki[\s\S]*?<\/pre>/g,
    (block) => `<div class="cosspress-code not-prose">${COPY_BUTTON}${block}</div>`,
  )
}

const BADGE_VARIANT: Record<string, string> = {
  tip: 'border-success/32 bg-success/8 text-success',
  success: 'border-success/32 bg-success/8 text-success',
  warning: 'border-warning/32 bg-warning/8 text-warning',
  danger: 'border-destructive/32 bg-destructive/8 text-destructive',
  info: 'border-transparent bg-muted text-muted-foreground',
}

/** 把 VitePress 的 `<Badge>` 转成 coss 风格的行内徽标。
 *  支持两种写法：`<Badge type="x" text="y" />` 与 `<Badge type="x">子内容</Badge>`
 *  （子内容可含 HTML，如 `<span data-version="iso">`，原样保留）。 */
function replaceBadges(html: string): string {
  return html.replace(
    /<Badge\b([^>]*?)(?:\/>|>([\s\S]*?)<\/Badge>)/g,
    (_m, attrs: string, children?: string) => {
      const type = (/\btype\s*=\s*"([^"]*)"/.exec(attrs)?.[1] ?? 'tip').toLowerCase()
      const textAttr = /\btext\s*=\s*"([^"]*)"/.exec(attrs)?.[1]
      const body = children != null ? children.trim() : (textAttr ?? '')
      const variant = BADGE_VARIANT[type] ?? BADGE_VARIANT.tip
      return `<span class="ms-1.5 inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 align-middle font-medium text-xs leading-none ${variant}">${body}</span>`
    },
  )
}

export function cosspressMarkdown(): Plugin {
  return {
    name: 'cosspress-markdown',
    enforce: 'pre',
    async transform(code, id) {
      if (!id.endsWith('.md')) return
      const md = await getMd()
      const { data: frontmatter, content } = matter(code)
      // 按文件路径决定容器默认标题语言（同步设置，紧接着同步 render）
      currentLocale = id.replace(/\\/g, '/').includes('/docs/en/') ? 'en' : 'zh'
      const html = replaceBadges(replaceTicks(wrapCodeBlocks(md.render(content))))
      const headings = extractHeadings(md, content)
      return {
        code: [
          `export const html = ${JSON.stringify(html)};`,
          `export const raw = ${JSON.stringify(content)};`,
          `export const frontmatter = ${JSON.stringify(frontmatter)};`,
          `export const headings = ${JSON.stringify(headings)};`,
          `export default frontmatter;`,
        ].join('\n'),
        map: null,
      }
    },
  }
}
