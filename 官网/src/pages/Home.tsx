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
  AlertTriangle,
  Info,
  ExternalLink,
  Cloud,
  HardDrive
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '@/components/ui/card'
import {
  Dialog,
  DialogTrigger,
  DialogPopup,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogPanel,
  DialogFooter,
  DialogClose
} from '@/components/ui/dialog'
import { Alert, AlertTitle, AlertDescription } from '@/components/ui/alert'
import Artplayer from 'artplayer'

const Home: React.FC = () => {
  const artRef = useRef<HTMLDivElement>(null)
  const playerRef = useRef<Artplayer | null>(null)

  useEffect(() => {
    if (artRef.current && !playerRef.current) {
      playerRef.current = new Artplayer({
        container: artRef.current,
        url: 'https://p1.cloud-pe.cn/LetRecovery.mp4',
        poster: 'https://pic1.imgdb.cn/item/6975ba3d6deeadc41a3395a4.jpg',
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
        theme: '#3b82f6',
        lang: 'zh-cn',
      })
    }

    return () => {
      if (playerRef.current) {
        playerRef.current.destroy()
        playerRef.current = null
      }
    }
  }, [])

  const features = [
    {
      icon: Zap,
      title: '极致高效',
      description: '基于 Rust 语言开发，享有卓越的性能表现和极低的资源占用，系统重装快人一步。',
    },
    {
      icon: Shield,
      title: '纯净无捆绑',
      description: '完全开源透明，不附带任何广告或捆绑软件，给您一个清爽干净的使用体验。',
    },
    {
      icon: Sparkles,
      title: '简单易用',
      description: '精心设计的现代化界面，操作直观明了，即使是电脑小白也能轻松驾驭。',
    },
    {
      icon: Rocket,
      title: '快速部署',
      description: '一键式操作流程，从启动到完成系统重装仅需几步，大幅节省您的宝贵时间。',
    },
    {
      icon: Gauge,
      title: '功能强大',
      description: '支持多种系统镜像格式，提供丰富的自定义选项，满足各类重装需求。',
    },
    {
      icon: BadgeCheck,
      title: '安全可靠',
      description: '采用先进的安全机制，确保数据传输和系统安装过程的稳定与安全。',
    },
  ]

  const downloadLinks = [
    {
      name: 'OneDrive',
      url: 'https://ruzex-my.sharepoint.cn/:u:/g/personal/ruz-ex_ruzex_partner_onmschina_cn1/IQBzSTykKOWNRoT3afEq1KiiAewece1PUpbKCFQNxygSwqk?e=IU37rX',
      icon: Cloud,
    },
    {
      name: '123云盘',
      url: 'https://www.123865.com/s/5ZD9-OZ2fd',
      icon: HardDrive,
    },
    {
      name: 'Cloud-PE 云盘',
      url: 'https://pan.sysre.cn/s/N3iW',
      icon: Cloud,
    }
  ]

  return (
    <>
      {/* Hero Section */}
      <section className="relative overflow-hidden bg-gradient-to-br from-primary/5 via-background to-accent/5 py-16 md:py-24 lg:py-32">
        <div className="absolute inset-0 bg-grid-pattern opacity-5" />
        <div className="container mx-auto px-4 relative z-10">
          <div className="grid lg:grid-cols-2 gap-12 items-center">
            {/* Left Content */}
            <div className="text-center lg:text-left">
              <h1 className="text-3xl md:text-4xl lg:text-5xl font-bold text-foreground mb-6 leading-tight">
                一款{' '}
                <span className="gradient-highlight">
                  纯净的系统重装工具
                </span>
              </h1>
              <p className="text-lg md:text-xl text-muted-foreground mb-8 max-w-xl mx-auto lg:mx-0">
                采用 Rust + egui 精心打造，拥有极致的运行效率，零广告零捆绑的纯净体验，
                简洁直观的操作界面让电脑小白也能轻松上手！
              </p>
              <div className="flex flex-col sm:flex-row gap-4 justify-center lg:justify-start">
                <Dialog>
                  <DialogTrigger
                    render={<Button size="lg" />}
                  >
                    <Download className="mr-2 size-5" />
                    立即下载
                  </DialogTrigger>
                  <DialogPopup className="max-w-2xl">
                    <DialogHeader>
                      <DialogTitle className="flex items-center gap-2">
                        <Download className="size-5" />
                        下载 LetRecovery
                      </DialogTitle>
                      <DialogDescription>
                        获取最新版本的 LetRecovery 系统重装工具
                      </DialogDescription>
                    </DialogHeader>
                    <DialogPanel>
                      <div className="space-y-4">
                        {/* 警告通知 */}
                        <Alert variant="warning">
                          <AlertTriangle className="size-4" />
                          <AlertTitle>下载服务可能不稳定</AlertTitle>
                          <AlertDescription>
                            近期我们的下载服务遭受了来自数百个境外 IP 的恶意请求攻击，累计产生了超过 33TB 的异常流量，
                            导致上游服务商对我们实施了严格的流量限制。因此，下载功能可能会出现间歇性不可用的情况，敬请谅解。
                          </AlertDescription>
                        </Alert>

                        {/* 说明信息 */}
                        <Alert variant="info">
                          <Info className="size-4" />
                          <AlertTitle>关于此下载</AlertTitle>
                          <AlertDescription>
                            以下提供的是已内置 WinPE 环境的最新版 LetRecovery 完整包。
                            如需不含 PE 的精简版本，请前往{' '}
                            <a
                              href="https://github.com/NORMAL-EX/LetRecovery/releases"
                              target="_blank"
                              rel="noopener noreferrer"
                              className="text-primary hover:underline inline-flex items-center gap-1"
                            >
                              GitHub Releases
                              <ExternalLink className="size-3" />
                            </a>{' '}
                            获取。
                            <br></br>
                            （精简版本的 LetRecovery 在正常系统下对系统盘进行重装操作时，需要下载 WinPE 环境，
                            由于我们的下载服务不稳定，可能出现下载失败的情况，因此我们不推荐您下载精简版本的
                            LetRecovery，除非您是在 WinPE 环境下使用）
                          </AlertDescription>
                        </Alert>

                        {/* 下载链接按钮组 */}
                        <div className="pt-2">
                          <p className="text-sm text-muted-foreground mb-3">选择下载源：</p>
                          <div className="flex flex-wrap gap-2">
                            {downloadLinks.map((link) => (
                              <Button
                                key={link.name}
                                variant="outline"
                                size="sm"
                                render={
                                  <a
                                    href={link.url}
                                    target="_blank"
                                    rel="noopener noreferrer"
                                  />
                                }
                              >
                                <link.icon className="mr-1.5 size-4" />
                                {link.name}
                              </Button>
                            ))}
                          </div>
                        </div>
                      </div>
                    </DialogPanel>
                    <DialogFooter>
                      <Button
                        variant="outline"
                        render={
                          <a
                            href="https://github.com/NORMAL-EX/LetRecovery/releases"
                            target="_blank"
                            rel="noopener noreferrer"
                          />
                        }
                      >
                        <ExternalLink className="mr-2 size-4" />
                        GitHub Releases
                      </Button>
                      <DialogClose render={<Button variant="outline" />}>
                        关闭
                      </DialogClose>
                    </DialogFooter>
                  </DialogPopup>
                </Dialog>
                <Button
                  variant="outline"
                  size="lg"
                  render={<Link to="/qqg" />}
                >
                  <Users className="mr-2 size-5" />
                  加入社区
                </Button>
              </div>
            </div>

            {/* Right Image */}
            <div className="relative">
              <div className="relative overflow-hidden shadow-2xl border border-border/50 bg-card" style={{ borderRadius: 6 }}>
                <img
                  src="https://pic1.imgdb.cn/item/69613b1d14866864fecdc7dc.png"
                  alt="LetRecovery"
                  className="w-full h-auto"
                  loading="lazy"
                />
              </div>
              {/* Decorative elements */}
              <div className="absolute -z-10 -top-4 -right-4 w-72 h-72 bg-primary/10 rounded-full blur-3xl" />
              <div className="absolute -z-10 -bottom-4 -left-4 w-72 h-72 bg-accent/10 rounded-full blur-3xl" />
            </div>
          </div>
        </div>
      </section>

      {/* Features Section */}
      <section className="py-16 md:py-24 bg-background">
        <div className="container mx-auto px-4">
          <div className="text-center mb-12">
            <h2 className="text-2xl md:text-3xl lg:text-4xl font-bold text-foreground mb-4">
              产品特性
            </h2>
            <p className="text-lg text-muted-foreground max-w-2xl mx-auto">
              探索 LetRecovery 的核心功能，让系统重装变得前所未有的简单
            </p>
          </div>

          <div className="grid sm:grid-cols-2 lg:grid-cols-3 gap-6">
            {features.map((feature) => (
              <Card key={feature.title}>
                <CardHeader>
                  <div className="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center mb-4">
                    <feature.icon className="size-6 text-primary" />
                  </div>
                  <CardTitle className="text-xl">{feature.title}</CardTitle>
                </CardHeader>
                <CardContent>
                  <CardDescription className="text-base leading-relaxed">
                    {feature.description}
                  </CardDescription>
                </CardContent>
              </Card>
            ))}
          </div>
        </div>
      </section>

      {/* Video Demo Section */}
      <section className="py-16 md:py-24 bg-muted/30">
        <div className="container mx-auto px-4">
          <div className="text-center mb-12">
            <h2 className="text-2xl md:text-3xl lg:text-4xl font-bold text-foreground mb-4">
              操作演示
            </h2>
            <p className="text-lg text-muted-foreground max-w-2xl mx-auto">
              观看视频了解如何使用 LetRecovery 快速完成系统重装<br></br>
              （该视频由 <strong>电脑病毒爱好者</strong> 制作）
            </p>
          </div>

          <div className="max-w-4xl mx-auto">
            <div className="relative rounded-2xl overflow-hidden shadow-2xl border border-border/50 bg-card">
              <div
                ref={artRef}
                className="w-full aspect-video"
              />
            </div>
          </div>
        </div>
      </section>
    </>
  )
}

export default Home