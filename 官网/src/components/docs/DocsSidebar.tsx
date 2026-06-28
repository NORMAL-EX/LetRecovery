import { Link, useLocation } from 'react-router-dom'
import { type SidebarItem, isActiveLink } from '@/lib/docs'
import { useT } from '@/lib/i18n'
import { cn } from '@/lib/utils'

interface DocsSidebarProps {
  items: SidebarItem[]
  onNavigate?: () => void
}

const isExternal = (link: string) => /^https?:\/\//.test(link)

/** 文档左侧目录 —— 仿 coss.com/ui：分组标题 + 选中项整块圆角底色（不是变色文字）。 */
export default function DocsSidebar({ items, onNavigate }: DocsSidebarProps) {
  const { pathname } = useLocation()
  const t = useT()
  if (!items.length) return null

  return (
    <nav aria-label={t.nav.docsHeading} className="flex flex-col gap-5">
      {items.map((group) => (
        <div key={group.text} className="flex flex-col gap-1">
          <p className="px-3.5 text-sm font-medium text-sidebar-accent-foreground">
            {group.text}
          </p>
          <ul className="flex flex-col gap-0.5">
            {(group.items ?? []).map((item) => {
              if (!item.link) return null
              const active = isActiveLink(pathname, item.link)
              const className = cn(
                'flex h-9 items-center rounded-md px-3.5 text-sm transition-colors',
                active
                  ? 'bg-sidebar-accent font-medium text-sidebar-accent-foreground'
                  : 'text-sidebar-foreground hover:text-foreground',
              )
              return (
                <li key={item.link}>
                  {isExternal(item.link) ? (
                    <a
                      href={item.link}
                      target="_blank"
                      rel="noreferrer"
                      className={className}
                    >
                      {item.text}
                    </a>
                  ) : (
                    <Link
                      to={item.link}
                      onClick={onNavigate}
                      aria-current={active ? 'page' : undefined}
                      className={className}
                    >
                      {item.text}
                    </Link>
                  )}
                </li>
              )
            })}
          </ul>
        </div>
      ))}
    </nav>
  )
}
