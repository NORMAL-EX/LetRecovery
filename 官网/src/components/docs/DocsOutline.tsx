import { useEffect, useMemo, useRef, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import type { Heading } from '@/lib/docs'
import { useT } from '@/lib/i18n'
import { cn } from '@/lib/utils'

/** 右侧"本页大纲" —— 仿 coss.com/ui。高亮用"确定性滚动位置"判定（不会来回闪），
 *  并特殊处理滚到底时高亮最后一个标题；点击后短暂锁定，避免与滚动联动打架。 */
export default function DocsOutline({ headings }: { headings: Heading[] }) {
  const items = useMemo(
    () => headings.filter((h) => h.level >= 2 && h.level <= 3),
    [headings],
  )
  const ids = useMemo(() => items.map((i) => i.slug), [items])
  const [active, setActive] = useState('')
  const lockUntil = useRef(0)
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const t = useT()

  useEffect(() => {
    if (!ids.length) {
      setActive('')
      return
    }
    const compute = () => {
      if (Date.now() < lockUntil.current) return
      const els = ids
        .map((id) => document.getElementById(id))
        .filter((el): el is HTMLElement => !!el)
      if (!els.length) return
      const doc = document.documentElement
      // 滚到底：高亮最后一个标题（修复"点击最后一个标题不更新"）
      if (window.scrollY + window.innerHeight >= doc.scrollHeight - 2) {
        setActive(els[els.length - 1].id)
        return
      }
      // 否则取"顶部已越过阈值"的最后一个标题（确定性，无闪烁）
      let current = els[0].id
      for (const el of els) {
        if (el.getBoundingClientRect().top <= 120) current = el.id
        else break
      }
      setActive(current)
    }
    compute()
    window.addEventListener('scroll', compute, { passive: true })
    window.addEventListener('resize', compute)
    return () => {
      window.removeEventListener('scroll', compute)
      window.removeEventListener('resize', compute)
    }
  }, [ids])

  if (!items.length) return null

  const onClick = (e: React.MouseEvent, slug: string) => {
    const el = document.getElementById(slug)
    if (!el) return
    e.preventDefault()
    lockUntil.current = Date.now() + 800 // 点击滚动期间锁定联动，避免来回闪
    setActive(slug)
    el.scrollIntoView({ behavior: 'smooth' })
    navigate({ pathname, hash: `#${slug}` }, { replace: true })
  }

  return (
    <nav aria-label={t.docs.onThisPage} className="flex flex-col gap-1 text-sm">
      <p className="flex h-7 items-center text-xs font-medium text-muted-foreground">
        {t.docs.onThisPage}
      </p>
      <div className="relative ms-3.5 flex flex-col gap-0.5 before:absolute before:inset-y-0 before:-left-3.25 before:w-px before:bg-border">
        {items.map((h) => (
          <a
            key={h.slug}
            href={`#${h.slug}`}
            onClick={(e) => onClick(e, h.slug)}
            data-active={active === h.slug}
            className={cn(
              'relative py-1 text-[0.8125rem] leading-5 no-underline transition-colors before:absolute before:inset-y-px before:-left-3.25 before:w-px before:rounded-full',
              h.level === 3 && 'ps-3.5',
              active === h.slug
                ? 'font-medium text-foreground before:w-0.5 before:bg-primary'
                : 'text-muted-foreground hover:text-foreground',
            )}
          >
            {h.title}
          </a>
        ))}
      </div>
    </nav>
  )
}
