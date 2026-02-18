import { Link, useLocation } from 'react-router-dom'
import { Github, Sun, Moon, Menu, X, Check } from 'lucide-react'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  Menu as DropdownMenu,
  MenuTrigger,
  MenuPopup,
  MenuItem,
} from '@/components/ui/menu'
import { useTheme } from '@/hooks/useTheme'
import { CircleHalf } from '@/components/icons/CircleHalf'

const Header: React.FC = () => {
  const { theme, setTheme, resolvedTheme } = useTheme()
  const location = useLocation()
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)

  const navItems = [
    { name: '主页', path: '/' },
    { name: '许可证', path: '/license' },
  ]

  const isActive = (path: string) => {
    return location.pathname === path
  }

  const getThemeIcon = () => {
    if (theme === 'system') {
      return <CircleHalf className="size-4" />
    }
    return resolvedTheme === 'dark' ? <Moon className="size-4" /> : <Sun className="size-4" />
  }

  return (
    <header className="sticky top-0 z-50 w-full border-b border-border/40 bg-background">
      <div className="container mx-auto px-4">
        <div className="flex h-14 items-center justify-between">
          {/* Logo/Title */}
          <Link 
            to="/" 
            className="flex items-center gap-2 font-bold text-lg text-foreground hover:text-foreground/80 transition-colors"
          >
            LetRecovery
          </Link>

          {/* Desktop Navigation */}
          <nav className="hidden md:flex items-center gap-1">
            {navItems.map((item) => (
              <Link
                key={item.name}
                to={item.path}
                className={`px-3 py-2 text-sm font-medium transition-colors rounded-md ${
                  isActive(item.path)
                    ? 'text-foreground'
                    : 'text-muted-foreground hover:text-foreground'
                }`}
              >
                {item.name}
              </Link>
            ))}

            {/* Github Button */}
            <Button
              variant="outline"
              size="sm"
              className="button-header"
              render={
                <a
                  href="https://github.com/NORMAL-EX/LetRecovery"
                  target="_blank"
                  rel="noopener noreferrer"
                />
              }
            >
              <Github className="size-4 mr-1.5" />
              Github
            </Button>

            {/* Theme Toggle */}
            <DropdownMenu>
              <MenuTrigger
                render={
                  <Button variant="outline" size="icon" className="ml-2 button-header">
                    {getThemeIcon()}
                    <span className="sr-only">切换主题</span>
                  </Button>
                }
              />
              <MenuPopup className="min-w-[140px] menu-popup-animated" align="end">
                <MenuItem
                  onClick={() => setTheme('light')}
                  className="flex items-center gap-2 cursor-pointer"
                >
                  <Sun className="h-4 w-4" />
                  浅色模式
                  {theme === 'light' && <Check className="ml-auto h-4 w-4" />}
                </MenuItem>
                <MenuItem
                  onClick={() => setTheme('dark')}
                  className="flex items-center gap-2 cursor-pointer"
                >
                  <Moon className="h-4 w-4" />
                  深色模式
                  {theme === 'dark' && <Check className="ml-auto h-4 w-4" />}
                </MenuItem>
                <MenuItem
                  onClick={() => setTheme('system')}
                  className="flex items-center gap-2 cursor-pointer"
                >
                  <CircleHalf className="h-4 w-4" />
                  跟随系统
                  {theme === 'system' && <Check className="ml-auto h-4 w-4" />}
                </MenuItem>
              </MenuPopup>
            </DropdownMenu>
          </nav>

          {/* Mobile Menu Button */}
          <div className="flex items-center gap-2 md:hidden">
            {/* Theme Switcher for Mobile */}
            <DropdownMenu>
              <MenuTrigger
                render={
                  <Button variant="outline" size="icon" className="button-header">
                    {getThemeIcon()}
                    <span className="sr-only">切换主题</span>
                  </Button>
                }
              />
              <MenuPopup className="min-w-[140px] menu-popup-animated" align="end">
                <MenuItem
                  onClick={() => setTheme('light')}
                  className="flex items-center gap-2 cursor-pointer"
                >
                  <Sun className="h-4 w-4" />
                  浅色模式
                  {theme === 'light' && <Check className="ml-auto h-4 w-4" />}
                </MenuItem>
                <MenuItem
                  onClick={() => setTheme('dark')}
                  className="flex items-center gap-2 cursor-pointer"
                >
                  <Moon className="h-4 w-4" />
                  深色模式
                  {theme === 'dark' && <Check className="ml-auto h-4 w-4" />}
                </MenuItem>
                <MenuItem
                  onClick={() => setTheme('system')}
                  className="flex items-center gap-2 cursor-pointer"
                >
                  <CircleHalf className="h-4 w-4" />
                  跟随系统
                  {theme === 'system' && <Check className="ml-auto h-4 w-4" />}
                </MenuItem>
              </MenuPopup>
            </DropdownMenu>

            <Button
              variant="outline"
              size="icon"
              className="button-header"
              onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
            >
              {mobileMenuOpen ? <X className="size-4" /> : <Menu className="size-4" />}
              <span className="sr-only">菜单</span>
            </Button>
          </div>
        </div>

        {/* Mobile Navigation */}
        {mobileMenuOpen && (
          <div className="md:hidden border-t border-border/40">
            <nav className="flex flex-col space-y-2 p-4">
              {navItems.map((item) => (
                <Link
                  key={item.name}
                  to={item.path}
                  className={`text-lg font-medium transition-colors flex items-center py-2 ${
                    isActive(item.path)
                      ? 'text-foreground'
                      : 'text-muted-foreground hover:text-foreground'
                  }`}
                  onClick={() => setMobileMenuOpen(false)}
                >
                  {item.name}
                </Link>
              ))}
              <a
                href="https://github.com/NORMAL-EX/LetRecovery"
                target="_blank"
                rel="noopener noreferrer"
                className="text-lg font-medium text-muted-foreground transition-colors hover:text-foreground flex items-center py-2"
                onClick={() => setMobileMenuOpen(false)}
              >
                Github
              </a>
            </nav>
          </div>
        )}
      </div>
    </header>
  )
}

export default Header
