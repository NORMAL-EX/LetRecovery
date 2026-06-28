import { useEffect, useRef } from 'react'
import { Link } from 'react-router-dom'
import {
  Download,
  Users,
  Zap,
  Shield,
  Sparkles,
  Rocket,
  Gauge,
  BadgeCheck,
  Info,
  Cloud,
  HardDrive,
  Github,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { ImageWithLoading } from '@/components/common'
import { FramedSection } from '@/components/layout'
import { Card, CardHeader, CardTitle, CardContent, CardDescription } from '@/components/ui/card'
import {
  Dialog,
  DialogTrigger,
  DialogPopup,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogPanel,
  DialogFooter,
  DialogClose,
} from '@/components/ui/dialog'
import { Alert, AlertTitle, AlertDescription } from '@/components/ui/alert'
import { useLang, useT } from '@/lib/i18n'
import Artplayer from 'artplayer'

const featureIcons = [Zap, Shield, Sparkles, Rocket, Gauge, BadgeCheck]

const downloadLinks = [
  { name: '123云盘', url: 'https://www.123865.com/s/5ZD9-OZ2fd', icon: HardDrive },
  { name: 'Cloud-PE 云盘', url: 'https://pan.sysre.cn/s/N3iW', icon: Cloud },
  { name: 'GitHub', url: 'https://github.com/NORMAL-EX/LetRecovery/releases', icon: Github },
]

const DownloadDialog: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const t = useT()
  return (
    <Dialog>
      <DialogTrigger render={<Button size="lg" />}>{children}</DialogTrigger>
      <DialogPopup className="max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Download className="size-5" />
            {t.home.dialogTitle}
          </DialogTitle>
          <DialogDescription>{t.home.dialogDesc}</DialogDescription>
        </DialogHeader>
        <DialogPanel>
          <div className="space-y-4">
            <Alert variant="info">
              <Info className="size-4" />
              <AlertTitle>{t.home.alertTitle}</AlertTitle>
              <AlertDescription>{t.home.alertDesc}</AlertDescription>
            </Alert>

            <div className="pt-2">
              <p className="mb-3 text-sm text-muted-foreground">{t.home.sourceLabel}</p>
              <div className="flex flex-wrap gap-2">
                {downloadLinks.map((link) => (
                  <Button
                    key={link.name}
                    variant="outline"
                    size="sm"
                    render={<a href={link.url} target="_blank" rel="noopener noreferrer" />}
                  >
                    <link.icon className="size-4" />
                    {link.name}
                  </Button>
                ))}
              </div>
            </div>
          </div>
        </DialogPanel>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>{t.home.close}</DialogClose>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  )
}

const Home: React.FC = () => {
  const t = useT()
  const { lang } = useLang()
  const artRef = useRef<HTMLDivElement>(null)
  const playerRef = useRef<Artplayer | null>(null)

  useEffect(() => {
    if (artRef.current && !playerRef.current) {
      playerRef.current = new Artplayer({
        container: artRef.current,
        url: 'https://p1.cloud-pe.cn/LetRecovery.mp4',
        poster: 'https://pic1.imgdb.cn/item/6a33aa9a91b65c4475abaa73.jpg',
        volume: 0.5,
        isLive: false,
        muted: false,
        autoplay: false,
        pip: false,
        autoSize: false,
        autoMini: false,
        screenshot: true,
        setting: true,
        loop: false,
        flip: true,
        playbackRate: true,
        aspectRatio: true,
        fullscreen: true,
        fullscreenWeb: true,
        subtitleOffset: true,
        miniProgressBar: false,
        mutex: true,
        backdrop: true,
        playsInline: true,
        autoPlayback: true,
        airplay: true,
        theme: '#262626',
        lang: lang === 'en' ? 'en' : 'zh-cn',
      })
    }

    return () => {
      if (playerRef.current) {
        playerRef.current.destroy()
        playerRef.current = null
      }
    }
  }, [lang])

  return (
    <>
      {/* Hero */}
      <section className="relative">
        <div className="container grid items-center gap-10 py-12 md:py-16 lg:grid-cols-2 lg:gap-14 lg:py-24">
          {/* 文案 */}
          <div className="flex flex-col items-start gap-6 text-left">
            <h1 className="font-heading text-4xl leading-[1.08] tracking-tight md:text-5xl lg:text-6xl">
              {t.home.heroTitleLines[0]}
              <br />
              {t.home.heroTitleLines[1]}
            </h1>
            <p className="max-w-xl text-balance text-muted-foreground md:text-lg">
              {t.home.heroDesc}
            </p>
            <div className="flex flex-wrap gap-3">
              <DownloadDialog>
                <Download className="size-5" />
                {t.home.download}
              </DownloadDialog>
              <Button variant="outline" size="lg" render={<Link to="/qqg" />}>
                <Users className="size-5" />
                {t.home.joinCommunity}
              </Button>
            </div>
          </div>

          {/* 产品截图 */}
          <div className="relative w-full">
            <div className="overflow-hidden rounded-xl border bg-card bg-clip-padding shadow-xs dark:bg-clip-border">
              <ImageWithLoading
                src="https://pic1.imgdb.cn/item/6a339a3591b65c4475ab67b2.png"
                alt={t.home.screenshotAlt}
                className="h-auto w-full"
                wrapperClassName="w-full"
                rounded="rounded-none"
                loadingMinHeight="260px"
                loading="lazy"
              />
            </div>
          </div>
        </div>
      </section>

      {/* 产品特性 */}
      <FramedSection className="py-16 md:py-24">
        <div className="mx-auto mb-12 max-w-2xl text-center">
          <h2 className="font-heading text-3xl tracking-tight md:text-4xl">
            {t.home.featuresTitle}
          </h2>
          <p className="mt-3 text-muted-foreground md:text-lg">{t.home.featuresDesc}</p>
        </div>

        <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
          {t.home.features.map((feature, i) => {
            const Icon = featureIcons[i]
            return (
              <Card key={feature.title} className="gap-4 transition-colors hover:border-foreground/20">
                <CardHeader>
                  <div className="mb-1 flex size-10 items-center justify-center rounded-lg border bg-muted/40 text-foreground">
                    <Icon className="size-5" />
                  </div>
                  <CardTitle className="text-base">{feature.title}</CardTitle>
                </CardHeader>
                <CardContent>
                  <CardDescription className="leading-relaxed">{feature.desc}</CardDescription>
                </CardContent>
              </Card>
            )
          })}
        </div>
      </FramedSection>

      {/* 操作演示 */}
      <FramedSection className="py-16 md:py-24">
        <div className="mx-auto mb-12 max-w-2xl text-center">
          <h2 className="font-heading text-3xl tracking-tight md:text-4xl">{t.home.demoTitle}</h2>
          <p className="mt-3 text-muted-foreground md:text-lg">
            {t.home.demoDescLine}
            <br />
            {t.home.demoCreditPrefix}
            <strong className="font-medium text-foreground">电脑病毒爱好者</strong>
            {t.home.demoCreditSuffix}
          </p>
        </div>

        <div className="mx-auto max-w-4xl">
          <div className="overflow-hidden rounded-xl border bg-card bg-clip-padding shadow-xs dark:bg-clip-border">
            <div ref={artRef} className="aspect-video w-full" />
          </div>
        </div>
      </FramedSection>
    </>
  )
}

export default Home
