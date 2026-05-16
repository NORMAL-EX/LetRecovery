import { Banner } from '@/components/layout'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { Github, Globe, ExternalLink } from 'lucide-react'

interface Contributor {
  name: string
  avatar: string
  links: { type: 'blog' | 'github'; url: string }[]
}

const acknowledgements = [
  '部分系统镜像及 PE 下载服务由 Cloud-PE 云盘提供',
  '感谢 电脑病毒爱好者 提供 WinPE 及制作宣传视频',
  '以及 Cloud-PE 项目的全体贡献人员',
]

const contributors: Contributor[] = [
  {
    name: 'dddffgg',
    avatar: 'https://pic1.imgdb.cn/item/6906fb8f3203f7be00c2cbc7.png',
    links: [
      { type: 'blog', url: 'https://blog.cloud-pe.cn' },
      { type: 'github', url: 'https://github.com/NORMAL-EX' },
    ],
  },
  {
    name: '电脑病毒爱好者',
    avatar: 'https://pic1.imgdb.cn/item/6961e0d97488ce4061907c41.jpg',
    links: [{ type: 'github', url: 'https://github.com/HelloWin10-19045' }],
  },
  {
    name: 'Hello,World!',
    avatar: 'https://pic1.imgdb.cn/item/6869262058cb8da5c8917549.jpg',
    links: [{ type: 'github', url: 'https://github.com/hwyyds-skidder-team' }],
  },
  {
    name: '普普通通のNeko',
    avatar: 'https://pic1.imgdb.cn/item/6869266b58cb8da5c8917555.jpg',
    links: [],
  },
]

const About: React.FC = () => {
  return (
    <>
      <Banner title="关于" subtitle="了解 LetRecovery 的项目信息与贡献人员" />

      <section className="pb-16 md:pb-24">
        <div className="container mx-auto px-4 max-w-3xl space-y-6">
          {/* 关于卡片 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-xl">关于 LetRecovery</CardTitle>
            </CardHeader>
            <CardContent>
              <dl className="grid grid-cols-[6rem_1fr] gap-y-3 text-sm">
                <dt className="text-muted-foreground">版本</dt>
                <dd className="text-foreground">v2026.2.6</dd>

                <dt className="text-muted-foreground">许可证</dt>
                <dd className="text-foreground">PolyForm Noncommercial 1.0.0</dd>

                <dt className="text-muted-foreground">版权所有</dt>
                <dd className="text-foreground leading-relaxed">
                  © 2026–present Cloud-PE Dev.
                  <br />
                  © 2026–present NORMAL-EX.
                </dd>

                <dt className="text-muted-foreground">开源地址</dt>
                <dd>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-auto px-0 text-primary hover:bg-transparent hover:underline"
                    render={
                      <a
                        href="https://github.com/NORMAL-EX/LetRecovery"
                        target="_blank"
                        rel="noopener noreferrer"
                      />
                    }
                  >
                    <Github className="size-4 mr-1.5" />
                    github.com/NORMAL-EX/LetRecovery
                    <ExternalLink className="size-3 ml-1" />
                  </Button>
                </dd>
              </dl>
            </CardContent>
          </Card>

          {/* 致谢卡片 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-xl">致谢</CardTitle>
            </CardHeader>
            <CardContent className="space-y-6">
              <ul className="space-y-2 text-sm text-muted-foreground">
                {acknowledgements.map((item) => (
                  <li key={item} className="flex gap-2">
                    <span className="text-muted-foreground/60 select-none">•</span>
                    <span>{item}</span>
                  </li>
                ))}
              </ul>

              <Separator />

              <div>
                <h3 className="text-sm font-medium text-foreground mb-4">贡献人员</h3>
                <ul className="space-y-3">
                  {contributors.map((c) => (
                    <li key={c.name} className="flex items-center gap-3">
                      <img
                        src={c.avatar}
                        alt={c.name}
                        className="w-9 h-9 rounded-full object-cover"
                        loading="lazy"
                      />
                      <span className="text-sm text-foreground flex-1">{c.name}</span>
                      {c.links.length > 0 && (
                        <div className="flex items-center gap-1">
                          {c.links.map((link) => (
                            <a
                              key={link.url}
                              href={link.url}
                              target="_blank"
                              rel="noopener noreferrer"
                              aria-label={`${c.name} 的 ${link.type}`}
                              className="p-1.5 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent transition-colors"
                            >
                              {link.type === 'github' ? (
                                <Github className="size-4" />
                              ) : (
                                <Globe className="size-4" />
                              )}
                            </a>
                          ))}
                        </div>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            </CardContent>
          </Card>
        </div>
      </section>
    </>
  )
}

export default About
