import { Navigate, useLocation } from 'react-router-dom'
import { Card, CardFrame, CardPanel } from '@/components/ui/card'
import DocsSidebar from '@/components/docs/DocsSidebar'
import DocsOutline from '@/components/docs/DocsOutline'
import DocContent from '@/components/docs/DocContent'
import { getDocPage, getSidebar, firstDocLink } from '@/lib/docs'
import { useLang, useT } from '@/lib/i18n'

const Docs: React.FC = () => {
  const { pathname } = useLocation()
  const { lang } = useLang()
  const t = useT()
  const sidebar = getSidebar(lang)

  // /docs 与 /docs/ → 跳到第一篇文档（官网首页已有 Hero，不另做文档主页）
  const normalized = pathname.replace(/\/$/, '') || '/docs'
  if (normalized === '/docs') return <Navigate to={firstDocLink} replace />

  const page = getDocPage(pathname, lang)

  if (!page) {
    return (
      <div className="mx-auto max-w-[1416px] px-6 py-24 text-center">
        <h1 className="font-heading text-3xl font-semibold">{t.docs.notFound}</h1>
        <p className="mt-3 text-muted-foreground">{t.docs.notFoundDesc}</p>
      </div>
    )
  }

  return (
    <div className="mx-auto w-full max-w-[1416px] px-0 lg:px-8">
      <div className="lg:grid lg:min-h-[calc(100svh-var(--header-height))] lg:grid-cols-[240px_minmax(0,1fr)]">
        {/* 左侧目录（手机/平板由页头菜单图标弹出抽屉，桌面端固定显示） */}
        <aside className="hidden lg:block">
          <div className="sticky top-(--header-height) max-h-[calc(100svh-var(--header-height))] overflow-y-auto py-8 pe-2">
            <DocsSidebar items={sidebar} />
          </div>
        </aside>

        {/* 正文 + 右侧大纲 */}
        <div className="flex items-stretch xl:w-full">
          <div className="relative flex w-full min-w-0 flex-1 flex-col mt-6 lg:mt-8 lg:mb-0 lg:ms-6 xl:me-4">
            {/* 桌面端：框线（左右竖线）随卡片撑到页面底部、底部开口（无底边框/底圆角） */}
            <CardFrame className="border-sidebar-border shadow-lg/5 max-lg:border-none lg:flex-1 lg:rounded-b-none lg:border-b-0 dark:bg-background">
              <Card className="gap-0 py-0 dark:bg-background max-lg:rounded-none lg:flex-1 lg:rounded-b-none lg:[clip-path:inset(1px_1px_0_1px_round_calc(var(--radius-2xl)-1px)_calc(var(--radius-2xl)-1px)_0_0)]!">
                <CardPanel className="px-4 py-6 sm:px-6 lg:p-10">
                  <div className="mx-auto w-full max-w-3xl">
                    <DocContent page={page} />
                  </div>
                </CardPanel>
              </Card>
            </CardFrame>
          </div>

          <div className="sticky top-(--header-height) ms-auto hidden max-h-[calc(100svh-var(--header-height))] w-64 shrink-0 self-start overflow-y-auto py-8 ps-4 xl:block">
            <DocsOutline headings={page.headings} />
          </div>
        </div>
      </div>
    </div>
  )
}

export default Docs
