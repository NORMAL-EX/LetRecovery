import { ImageWithLoading } from '@/components/common'
import {
  PageHeader,
  PageHeaderHeading,
  PageHeaderDescription,
  FramedSection,
} from '@/components/layout'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { Github, Globe, ExternalLink } from 'lucide-react'
import { useT } from '@/lib/i18n'

interface Contributor {
  name: string
  avatar: string
  links: { type: 'blog' | 'github'; url: string }[]
}

const contributors: Contributor[] = [
  {
    name: 'dddffgg',
    avatar: 'https://pic1.imgdb.cn/item/6a3f5b6b2546dff76d1b82b9.png',
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
]

const About: React.FC = () => {
  const t = useT()
  return (
    <>
      <PageHeader>
        <PageHeaderHeading>{t.about.title}</PageHeaderHeading>
        <PageHeaderDescription>{t.about.desc}</PageHeaderDescription>
      </PageHeader>

      <FramedSection className="py-12 md:py-16" containerClassName="max-w-3xl">
        <div className="space-y-6">
          {/* 关于卡片 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-lg">{t.about.cardTitle}</CardTitle>
            </CardHeader>
            <CardContent>
              <dl className="grid grid-cols-[6rem_1fr] gap-y-3 text-sm">
                <dt className="text-muted-foreground">{t.about.version}</dt>
                <dd className="text-foreground">{__APP_VERSION__}</dd>

                <dt className="text-muted-foreground">{t.about.license}</dt>
                <dd className="text-foreground">PolyForm Noncommercial 1.0.0</dd>

                <dt className="text-muted-foreground">{t.about.copyright}</dt>
                <dd className="leading-relaxed text-foreground">
                  © 2026–present Cloud-PE Dev.
                  <br />
                  © 2026–present NORMAL-EX.
                </dd>

                <dt className="text-muted-foreground">{t.about.source}</dt>
                <dd>
                  <Button
                    variant="link"
                    size="sm"
                    className="h-auto px-0"
                    render={
                      <a
                        href="https://github.com/NORMAL-EX/LetRecovery"
                        target="_blank"
                        rel="noopener noreferrer"
                      />
                    }
                  >
                    <Github className="size-4" />
                    github.com/NORMAL-EX/LetRecovery
                    <ExternalLink className="size-3" />
                  </Button>
                </dd>
              </dl>
            </CardContent>
          </Card>

          {/* 致谢卡片 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-lg">{t.about.ackTitle}</CardTitle>
            </CardHeader>
            <CardContent className="space-y-6">
              <ul className="space-y-2 text-sm text-muted-foreground">
                {t.about.acks.map((item) => (
                  <li key={item} className="flex gap-2">
                    <span className="select-none text-muted-foreground/60">•</span>
                    <span>{item}</span>
                  </li>
                ))}
              </ul>

              <Separator />

              <div>
                <h3 className="mb-4 text-sm font-medium text-foreground">{t.about.contributors}</h3>
                <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
                  {contributors.map((c) => (
                    <Card key={c.name} className="gap-0 py-0 transition-colors hover:border-foreground/20">
                      <CardContent className="flex items-center gap-3 px-4 py-4">
                        <ImageWithLoading
                          src={c.avatar}
                          alt={c.name}
                          className="size-11 rounded-full object-cover"
                          wrapperClassName="shrink-0"
                          rounded="rounded-full"
                          loading="lazy"
                        />
                        <span className="flex-1 truncate text-sm font-medium text-foreground">
                          {c.name}
                        </span>
                        {c.links.length > 0 && (
                          <div className="flex shrink-0 items-center gap-1">
                            {c.links.map((link) => (
                              <a
                                key={link.url}
                                href={link.url}
                                target="_blank"
                                rel="noopener noreferrer"
                                aria-label={`${c.name} · ${link.type}`}
                                className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
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
                      </CardContent>
                    </Card>
                  ))}
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      </FramedSection>
    </>
  )
}

export default About
