import { Banner } from '@/components/layout'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { Badge } from '@/components/ui/badge'
import { 
  Scale, 
  Github, 
  XCircle, 
  Heart,
  Info,
  AlertTriangle,
  ExternalLink,
  Globe
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CheckCircle } from '@/components/icons/CheckCircle'

const License: React.FC = () => {
  const allowedItems = [
    '个人学习、研究和非盈利使用',
    '修改源代码并用于非盈利用途',
    '在注明出处的前提下进行非商业性质的分发',
  ]

  const forbiddenItems = [
    '将本软件或其源代码用于任何商业/盈利用途',
    '销售、倒卖本软件或其衍生作品',
    '将本软件整合到商业产品或服务中',
    '个人利用本软件或其代码进行盈利活动',
  ]

  const acknowledgements = [
    '部分系统镜像及 PE 下载服务由 Cloud-PE 云盘提供',
    '感谢 电脑病毒爱好者 提供 WinPE & 制作宣传视频',
    '以及 Cloud-PE 项目的全体贡献人员',
  ]

  const contributors = [
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
      links: [
        { type: 'github', url: 'https://github.com/HelloWin10-19045' },
      ],
    },
    {
      name: 'Hello,World!',
      avatar: 'https://pic1.imgdb.cn/item/6869262058cb8da5c8917549.jpg',
      links: [
        { type: 'github', url: 'https://github.com/hwyyds-skidder-team' },
      ],
    },
    {
      name: '普普通通のNeko',
      avatar: 'https://pic1.imgdb.cn/item/6869266b58cb8da5c8917555.jpg',
      links: [],
    },
  ]

  return (
    <>
      <Banner 
        title="许可证说明" 
        subtitle="了解 LetRecovery 的使用条款和版权信息" 
      />

      <section className="py-16 md:py-24">
        <div className="container mx-auto px-4 max-w-4xl">
          {/* 基本信息卡片 */}
          <Card className="mb-8">
            <CardHeader>
              <div className="flex items-center gap-3">
                <div className="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center">
                  <Info className="size-6 text-primary" />
                </div>
                <div>
                  <CardTitle className="text-2xl">关于 LetRecovery</CardTitle>
                </div>
              </div>
            </CardHeader>
            <CardContent>
              <div className="grid sm:grid-cols-2 gap-6">
                <div className="space-y-4">
                  <div className="flex items-center justify-between">
                    <span className="text-muted-foreground">版本</span>
                    <Badge variant="secondary" size="lg">v2026.2.6</Badge>
                  </div>
                  <div className="flex items-center justify-between">
                    <span className="text-muted-foreground">许可证</span>
                    <Badge variant="outline" size="lg">PolyForm Noncommercial 1.0.0</Badge>
                  </div>
                </div>
                <div className="space-y-4">
                  <div>
                    <span className="text-muted-foreground block mb-2">版权所有</span>
                    <div className="space-y-1">
                      <p className="text-sm">© 2026-present Cloud-PE Dev.</p>
                      <p className="text-sm">© 2026-present NORMAL-EX.</p>
                    </div>
                  </div>
                  <div>
                    <span className="text-muted-foreground block mb-2">开源地址</span>
                    <Button
                      variant="outline"
                      size="sm"
                      render={
                        <a
                          href="https://github.com/NORMAL-EX/LetRecovery"
                          target="_blank"
                          rel="noopener noreferrer"
                        />
                      }
                    >
                      <Github className="size-4 mr-2" />
                      GitHub
                      <ExternalLink className="size-3 ml-1" />
                    </Button>
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* 免费声明 */}
          <Card className="mb-8 border-success/30 bg-success/5">
            <CardHeader>
              <div className="flex items-center gap-3">
                <div className="w-12 h-12 rounded-xl bg-success/10 flex items-center justify-center">
                  <CheckCircle size={24} className="text-success" />
                </div>
                <CardTitle className="text-xl">免费声明</CardTitle>
              </div>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <p className="text-lg font-medium text-success-foreground">
                  本软件完全免费，禁止任何形式的倒卖行为！
                </p>
                <div className="p-4 rounded-lg bg-warning/10 border border-warning/30">
                  <div className="flex gap-3">
                    <AlertTriangle className="size-5 text-warning-foreground shrink-0 mt-0.5" />
                    <p className="text-sm text-warning-foreground">
                      如果您是通过付费渠道获取本软件，您已被骗，请立即举报并申请退款。
                    </p>
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* 使用条款 */}
          <Card className="mb-8">
            <CardHeader>
              <div className="flex items-center gap-3">
                <div className="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center">
                  <Scale className="size-6 text-primary" />
                </div>
                <CardTitle className="text-xl">使用条款</CardTitle>
              </div>
            </CardHeader>
            <CardContent>
              <div className="grid md:grid-cols-2 gap-6">
                {/* 允许 */}
                <div className="space-y-4">
                  <div className="flex items-center gap-2">
                    <CheckCircle size={20} className="text-success" />
                    <h3 className="font-semibold text-success-foreground">允许</h3>
                  </div>
                  <ul className="space-y-3">
                    {allowedItems.map((item, index) => (
                      <li key={index} className="flex items-start gap-3">
                        <span className="w-6 h-6 rounded-full bg-success/10 flex items-center justify-center shrink-0 mt-0.5">
                          <CheckCircle size={14} className="text-success" />
                        </span>
                        <span className="text-sm text-muted-foreground">{item}</span>
                      </li>
                    ))}
                  </ul>
                </div>

                {/* 禁止 */}
                <div className="space-y-4">
                  <div className="flex items-center gap-2">
                    <XCircle className="size-5 text-destructive" />
                    <h3 className="font-semibold text-destructive-foreground">禁止</h3>
                  </div>
                  <ul className="space-y-3">
                    {forbiddenItems.map((item, index) => (
                      <li key={index} className="flex items-start gap-3">
                        <span className="w-6 h-6 rounded-full bg-destructive/10 flex items-center justify-center shrink-0 mt-0.5">
                          <XCircle className="size-3.5 text-destructive" />
                        </span>
                        <span className="text-sm text-muted-foreground">{item}</span>
                      </li>
                    ))}
                  </ul>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* 致谢 */}
          <Card className="mb-8">
            <CardHeader>
              <div className="flex items-center gap-3">
                <div className="w-12 h-12 rounded-xl bg-primary/10 flex items-center justify-center">
                  <Heart className="size-6 text-primary" />
                </div>
                <CardTitle className="text-xl">致谢</CardTitle>
              </div>
            </CardHeader>
            <CardContent>
              <ul className="space-y-3 mb-8">
                {acknowledgements.map((item, index) => (
                  <li key={index} className="flex items-start gap-3">
                    <span className="w-6 h-6 rounded-full bg-primary/10 flex items-center justify-center shrink-0 mt-0.5">
                      <Heart className="size-3.5 text-primary" />
                    </span>
                    <span className="text-muted-foreground">{item}</span>
                  </li>
                ))}
              </ul>

              <Separator className="my-6" />

              {/* 贡献者 */}
              <h4 className="font-semibold text-foreground mb-6">贡献人员</h4>
              <div className="grid grid-cols-2 sm:grid-cols-3 gap-4">
                {contributors.map((contributor) => (
                  <div
                    key={contributor.name}
                    className="flex flex-col items-center p-4 rounded-xl bg-muted/30 border border-border/50"
                  >
                    <img
                      src={contributor.avatar}
                      alt={contributor.name}
                      className="w-16 h-16 rounded-full object-cover mb-3"
                    />
                    <h5 className="font-medium text-foreground text-sm text-center mb-2">
                      {contributor.name}
                    </h5>
                    {contributor.links.length > 0 && (
                      <div className="flex gap-2">
                        {contributor.links.map((link) => (
                          <a
                            key={link.url}
                            href={link.url}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="w-7 h-7 rounded-full bg-muted flex items-center justify-center text-muted-foreground hover:text-foreground hover:bg-accent transition-colors"
                          >
                            {link.type === 'github' ? (
                              <Github className="size-3.5" />
                            ) : (
                              <Globe className="size-3.5" />
                            )}
                          </a>
                        ))}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>
        </div>
      </section>
    </>
  )
}

export default License
