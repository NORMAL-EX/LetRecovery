import { Link } from 'react-router-dom'
import { FileQuestion, Home } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from '@/components/ui/empty'
import { useT } from '@/lib/i18n-hooks'

const NotFound: React.FC = () => {
  const t = useT()
  return (
    <section className="mx-auto flex min-h-[calc(100svh-var(--header-height))] w-full max-w-[1416px] px-6 py-12">
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <FileQuestion />
          </EmptyMedia>
          <EmptyTitle>{t.notFound.title}</EmptyTitle>
          <EmptyDescription>{t.notFound.desc}</EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button render={<Link to="/" />}>
            <Home className="size-4" />
            {t.notFound.home}
          </Button>
        </EmptyContent>
      </Empty>
    </section>
  )
}

export default NotFound
