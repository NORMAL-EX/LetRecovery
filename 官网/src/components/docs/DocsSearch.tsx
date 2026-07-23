import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { Search, BookOpen, CornerDownLeft, SearchX } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Empty,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
  EmptyDescription,
} from '@/components/ui/empty'
import {
  Command,
  CommandCollection,
  CommandDialog,
  CommandDialogPopup,
  CommandDialogTrigger,
  CommandEmpty,
  CommandFooter,
  CommandGroup,
  CommandGroupLabel,
  CommandInput,
  CommandItem,
  CommandList,
  CommandPanel,
} from '@/components/ui/command'
import { Kbd, KbdGroup } from '@/components/ui/kbd'
import { docTitle, getDocPage } from '@/lib/docs'
import { getSidebar } from '@/lib/docs-navigation'
import { useLang, useT } from '@/lib/i18n-hooks'
import { cn } from '@/lib/utils'

interface SearchItem {
  searchValue: string
  label: string
  url: string
}
interface SearchGroup {
  value: string
  items: SearchItem[]
}

function normalizeSearchValue(value: string): string {
  return value
    .normalize('NFKC')
    .toLocaleLowerCase()
    .replace(/[\p{P}\p{S}\s]+/gu, ' ')
    .trim()
}

function isSearchItem(item: unknown): item is SearchItem {
  if (typeof item !== 'object' || item === null) return false
  return (
    'searchValue' in item &&
    typeof item.searchValue === 'string' &&
    'label' in item &&
    typeof item.label === 'string'
  )
}

function matchesSearch(item: unknown, query: string): boolean {
  if (!isSearchItem(item)) return false
  const terms = normalizeSearchValue(query).split(' ').filter(Boolean)
  return terms.every((term) => item.searchValue.includes(term))
}

function searchItemLabel(item: unknown): string {
  return isSearchItem(item) ? item.label : ''
}

/** 文档搜索（仿 coss.com/ui command-menu）。始终挂载，由 `active`（是否在文档页）
 * 控制显隐，保证进出文档页都有宽度/透明度过渡。索引由构建期解析出的
 * 页面标题、描述、各级标题和正文可见文本组成。 */
export default function DocsSearch({
  active,
  className,
}: {
  active: boolean
  className?: string
}) {
  const { lang } = useLang()
  const t = useT()
  const [open, setOpen] = useState(false)

  const groups = useMemo<SearchGroup[]>(() => {
    return getSidebar(lang)
      .map((group) => ({
        value: group.text,
        items: (group.items ?? [])
          .filter((i) => i.link)
          .map((item) => {
            const page = getDocPage(item.link as string, lang)
            const title = page ? docTitle(page) : item.text
            const headings = page?.headings.map((heading) => heading.title).join(' ') ?? ''
            const description = String(page?.frontmatter.description ?? '')
            const body = page?.searchText ?? ''
            return {
              searchValue: normalizeSearchValue(
                `${group.text} ${item.text} ${title} ${description} ${headings} ${body}`,
              ),
              label: item.text,
              url: item.link as string,
            }
          }),
      }))
      .filter((g) => g.items.length > 0)
  }, [lang])

  // 离开文档页时关闭弹窗，避免再次进入时残留打开状态
  useEffect(() => {
    if (active) return
    const timeout = window.setTimeout(() => setOpen(false), 0)
    return () => window.clearTimeout(timeout)
  }, [active])

  // ⌘K / Ctrl+K / "/" 打开（仅在文档页生效）
  useEffect(() => {
    if (!active) return
    const onKey = (e: KeyboardEvent) => {
      if ((e.key === 'k' && (e.metaKey || e.ctrlKey)) || e.key === '/') {
        const el = e.target as HTMLElement | null
        if (
          el?.isContentEditable ||
          el instanceof HTMLInputElement ||
          el instanceof HTMLTextAreaElement
        )
          return
        e.preventDefault()
        setOpen((o) => !o)
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [active])

  return (
    <CommandDialog open={active && open} onOpenChange={setOpen}>
      {/* 展开/收起动画：宽度 + 透明度过渡（进出文档页都平滑） */}
      <div
        className={cn(
          'flex items-center overflow-hidden transition-all duration-300 ease-out',
          active
            ? 'w-9 opacity-100 md:w-40 lg:w-56'
            : 'pointer-events-none w-0 opacity-0',
          className,
        )}
      >
        <CommandDialogTrigger
          aria-label={t.docs.searchLabel}
          tabIndex={active ? undefined : -1}
          render={<Button variant="outline" />}
          className="h-9 w-9 shrink-0 justify-center gap-2 rounded-lg px-0 font-normal text-muted-foreground md:w-40 md:justify-start md:px-2.5 lg:w-56 lg:px-3"
        >
          <Search className="size-4 shrink-0" />
          <span className="hidden flex-1 truncate text-left text-sm md:inline">
            {t.docs.searchPlaceholder}
          </span>
          <KbdGroup className="hidden lg:flex">
            <Kbd>Ctrl</Kbd>
            <Kbd className="aspect-square">K</Kbd>
          </KbdGroup>
        </CommandDialogTrigger>
      </div>

      <CommandDialogPopup>
        <Command
          items={groups}
          filter={matchesSearch}
          itemToStringValue={searchItemLabel}
        >
          <CommandInput placeholder={t.docs.searchPlaceholder} />
          <CommandPanel>
            <CommandEmpty className="p-0">
              <Empty className="py-10 md:py-12">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <SearchX />
                  </EmptyMedia>
                  <EmptyTitle>{t.docs.searchEmpty}</EmptyTitle>
                  <EmptyDescription>{t.docs.searchEmptyHint}</EmptyDescription>
                </EmptyHeader>
              </Empty>
            </CommandEmpty>
            <CommandList>
              {(group: SearchGroup) => (
                <CommandGroup items={group.items} key={group.value}>
                  <CommandGroupLabel>{group.value}</CommandGroupLabel>
                  <CommandCollection>
                    {(item: SearchItem) => (
                      <CommandItem
                        key={item.url}
                        className="flex w-full items-center gap-2"
                        render={
                          <Link to={item.url} onClick={() => setOpen(false)} />
                        }
                      >
                        <BookOpen className="size-4 shrink-0 opacity-80" />
                        <span className="flex-1 truncate">{item.label}</span>
                      </CommandItem>
                    )}
                  </CommandCollection>
                </CommandGroup>
              )}
            </CommandList>
          </CommandPanel>
          <CommandFooter>
            <div className="flex items-center gap-2">
              <span className="whitespace-nowrap">{t.docs.goToPage}</span>
              <Kbd>
                <CornerDownLeft className="size-3" />
              </Kbd>
            </div>
          </CommandFooter>
        </Command>
      </CommandDialogPopup>
    </CommandDialog>
  )
}
