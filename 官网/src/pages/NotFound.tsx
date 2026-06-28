import { Link } from 'react-router-dom'
import { Home } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useT } from '@/lib/i18n'

const NotFound: React.FC = () => {
  const t = useT()
  return (
    <section className="mx-auto flex min-h-[60vh] max-w-[1416px] flex-col items-center justify-center px-6 py-24 text-center">
      <p className="font-heading text-7xl font-semibold leading-none text-foreground sm:text-8xl">
        404
      </p>
      <h1 className="mt-6 font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
        {t.notFound.title}
      </h1>
      <p className="mt-3 max-w-md text-muted-foreground">{t.notFound.desc}</p>
      <Button className="mt-8 gap-1.5" render={<Link to="/" />}>
        <Home className="size-4" />
        {t.notFound.home}
      </Button>
    </section>
  )
}

export default NotFound
