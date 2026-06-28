import { Link, useLocation } from 'react-router-dom'
import { Github, Sun, Moon, Menu, Check, Languages } from 'lucide-react'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  Menu as DropdownMenu,
  MenuTrigger,
  MenuPopup,
  MenuItem,
} from '@/components/ui/menu'
import {
  Sheet,
  SheetTrigger,
  SheetPopup,
  SheetPanel,
  SheetTitle,
} from '@/components/ui/sheet'
import DocsSidebar from '@/components/docs/DocsSidebar'
import DocsSearch from '@/components/docs/DocsSearch'
import { getSidebar } from '@/lib/docs'
import { useTheme } from '@/hooks/useTheme'
import { useLang, useT } from '@/lib/i18n'
import { CircleHalf } from '@/components/icons/CircleHalf'
import { cn } from '@/lib/utils'

const navItems = [
  { key: 'home', path: '/' },
  { key: 'docs', path: '/docs' },
  { key: 'community', path: '/qqg' },
  { key: 'about', path: '/about' },
] as const

const GITHUB_URL = 'https://github.com/NORMAL-EX/LetRecovery'

const Header: React.FC = () => {
  const { theme, setTheme, resolvedTheme } = useTheme()
  const { lang, setLang } = useLang()
  const t = useT()
  const location = useLocation()
  const [mobileNavOpen, setMobileNavOpen] = useState(false)
  const isDocs = location.pathname.startsWith('/docs')

  const isActive = (path: string) =>
    path === '/' ? location.pathname === '/' : location.pathname.startsWith(path)

  const getThemeIcon = () => {
    if (theme === 'system') return <CircleHalf className="size-4" />
    return resolvedTheme === 'dark' ? <Moon className="size-4" /> : <Sun className="size-4" />
  }

  const GithubButton = () => (
    <Button
      variant="ghost"
      size="icon"
      aria-label={t.common.github}
      render={<a href={GITHUB_URL} target="_blank" rel="noopener noreferrer" />}
    >
      <Github className="size-4" />
    </Button>
  )

  const LanguageMenu = () => (
    <DropdownMenu>
      <MenuTrigger
        render={
          <Button variant="ghost" size="icon">
            <Languages className="size-4" />
            <span className="sr-only">{t.common.language}</span>
          </Button>
        }
      />
      <MenuPopup className="min-w-[140px] menu-popup-animated" align="end">
        <MenuItem onClick={() => setLang('zh')} className="flex cursor-pointer items-center gap-2">
          {t.common.zh}
          {lang === 'zh' && <Check className="ml-auto size-4" />}
        </MenuItem>
        <MenuItem onClick={() => setLang('en')} className="flex cursor-pointer items-center gap-2">
          {t.common.en}
          {lang === 'en' && <Check className="ml-auto size-4" />}
        </MenuItem>
      </MenuPopup>
    </DropdownMenu>
  )

  const ThemeMenu = () => (
    <DropdownMenu>
      <MenuTrigger
        render={
          <Button variant="ghost" size="icon">
            {getThemeIcon()}
            <span className="sr-only">{t.common.toggleTheme}</span>
          </Button>
        }
      />
      <MenuPopup className="min-w-[140px] menu-popup-animated" align="end">
        <MenuItem onClick={() => setTheme('light')} className="flex cursor-pointer items-center gap-2">
          <Sun className="size-4" />
          {t.common.light}
          {theme === 'light' && <Check className="ml-auto size-4" />}
        </MenuItem>
        <MenuItem onClick={() => setTheme('dark')} className="flex cursor-pointer items-center gap-2">
          <Moon className="size-4" />
          {t.common.dark}
          {theme === 'dark' && <Check className="ml-auto size-4" />}
        </MenuItem>
        <MenuItem onClick={() => setTheme('system')} className="flex cursor-pointer items-center gap-2">
          <CircleHalf className="size-4" />
          {t.common.system}
          {theme === 'system' && <Check className="ml-auto size-4" />}
        </MenuItem>
      </MenuPopup>
    </DropdownMenu>
  )

  const drawerLinkClass = (active: boolean) =>
    cn(
      'flex h-9 items-center gap-2 rounded-md px-3.5 text-sm transition-colors',
      active
        ? 'bg-sidebar-accent font-medium text-sidebar-accent-foreground'
        : 'text-sidebar-foreground hover:text-foreground',
    )

  return (
    <header className="sticky top-0 z-50 w-full bg-sidebar/80 backdrop-blur-sm before:absolute before:inset-x-0 before:bottom-0 before:h-px before:bg-border/64">
      <div className="container relative flex h-(--header-height) w-full items-center justify-between gap-2">
        {/* 左：折叠菜单（手机始终在；平板仅文档页，展开时带动画 + 标题右移）+ 品牌 */}
        <div className="flex items-center">
          <Sheet open={mobileNavOpen} onOpenChange={setMobileNavOpen}>
            <div
              className={cn(
                'flex items-center overflow-hidden transition-all duration-300 ease-out',
                isDocs
                  ? 'me-1 w-9 opacity-100 lg:me-0 lg:w-0 lg:opacity-0'
                  : 'me-1 w-9 opacity-100 md:me-0 md:w-0 md:opacity-0',
              )}
            >
              <SheetTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t.nav.menu}
                    className="shrink-0"
                  >
                    <Menu className="size-5" />
                  </Button>
                }
              />
            </div>

            <SheetPopup side="left" className="max-w-xs">
              <SheetTitle className="sr-only">{t.nav.menu}</SheetTitle>
              <SheetPanel>
                <div className="flex flex-col gap-6">
                  {/* 站点导航（仅手机端；平板顶栏已有导航） */}
                  <div className="flex flex-col gap-[22px] md:hidden">
                    <p className="font-heading text-base leading-none">{t.nav.menu}</p>
                    <ul className="flex flex-col gap-0.5">
                      {navItems
                        .filter((item) => item.path !== '/docs')
                        .map((item) => (
                          <li key={item.key}>
                            <Link
                              to={item.path}
                              onClick={() => setMobileNavOpen(false)}
                              aria-current={isActive(item.path) ? 'page' : undefined}
                              className={drawerLinkClass(isActive(item.path))}
                            >
                              {t.nav[item.key]}
                            </Link>
                          </li>
                        ))}
                    </ul>
                  </div>

                  {/* 文档目录（"文档"标题仅手机端，用于区隔站点导航；平板不需要） */}
                  <div className="flex flex-col gap-[22px]">
                    <p className="font-heading text-base leading-none md:hidden">
                      {t.nav.docsHeading}
                    </p>
                    <DocsSidebar
                      items={getSidebar(lang)}
                      onNavigate={() => setMobileNavOpen(false)}
                    />
                  </div>
                </div>
              </SheetPanel>
            </SheetPopup>
          </Sheet>

          <Link
            to="/"
            aria-label="LetRecovery"
            className="-mt-0.5 shrink-0 font-heading text-2xl text-foreground transition-opacity hover:opacity-80"
          >
            LetRecovery
          </Link>
        </div>

        {/* 右侧：导航（平板+）+ 文档搜索（仅文档页）+ GitHub/语言/主题 */}
        <div className="flex items-center gap-0.5">
          <nav className="hidden items-center gap-1 md:flex">
            {navItems.map((item) => (
              <Button
                key={item.key}
                variant="ghost"
                size="sm"
                data-pressed={isActive(item.path) || undefined}
                className={
                  isActive(item.path)
                    ? 'text-foreground'
                    : 'text-muted-foreground hover:text-foreground'
                }
                render={<Link to={item.path} />}
              >
                {t.nav[item.key]}
              </Button>
            ))}
          </nav>

          <span
            className="mx-1 hidden h-5 w-px bg-border md:block"
            aria-hidden="true"
          />

          <DocsSearch active={isDocs} />
          <GithubButton />
          <LanguageMenu />
          <ThemeMenu />
        </div>
      </div>
    </header>
  )
}

export default Header
