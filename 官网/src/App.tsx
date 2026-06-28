import { HashRouter as Router, Routes, Route } from 'react-router-dom'
import { ThemeProvider } from '@/hooks/useTheme'
import { LanguageProvider } from '@/lib/i18n'
import { Header, Footer, GridRails } from '@/components/layout'
import { Home, QQGroup, About, Docs, NotFound } from '@/pages'

function App() {
  return (
    <ThemeProvider>
      <LanguageProvider>
        <Router>
        <div className="relative isolate flex min-h-svh flex-col overflow-clip bg-sidebar text-foreground [--header-height:4rem]">
          {/* coss 招牌框线网格：左右竖直导轨 */}
          <GridRails />
          {/* 页头底线与导轨相交处的四角准星（仅宽屏可见） */}
          <div
            aria-hidden="true"
            className="container pointer-events-none fixed inset-x-0 top-[calc(var(--header-height)-3.5px)] z-40 before:absolute before:top-0 before:-left-[11.5px] before:-ml-1 before:size-2 before:rounded-[2px] before:border before:border-border before:bg-popover before:bg-clip-padding before:shadow-xs after:absolute after:top-0 after:-right-[11.5px] after:-mr-1 after:size-2 after:rounded-[2px] after:border after:border-border after:bg-background after:bg-clip-padding after:shadow-xs dark:before:bg-clip-border dark:after:bg-clip-border"
          />
          <Header />
          <main className="relative flex-1">
            <Routes>
              <Route path="/" element={<Home />} />
              <Route path="/qqg" element={<QQGroup />} />
              <Route path="/about" element={<About />} />
              <Route path="/docs/*" element={<Docs />} />
              <Route path="*" element={<NotFound />} />
            </Routes>
          </main>
          <Footer />
        </div>
        </Router>
      </LanguageProvider>
    </ThemeProvider>
  )
}

export default App
