import {
  PageHeader,
  PageHeaderHeading,
  PageHeaderDescription,
  FramedSection,
} from '@/components/layout'
import { Card, CardContent } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogTrigger,
  DialogPopup,
  DialogHeader,
  DialogTitle,
  DialogPanel,
} from '@/components/ui/dialog'
import { Users, QrCode, ExternalLink } from 'lucide-react'
import { useT } from '@/lib/i18n'

const groups = [
  {
    number: '1077151966',
    qrcode: '/img/qrcode_1077151966.jpg',
    joinUrl: 'https://qm.qq.com/q/rpaJoZWMLu',
  },
]

const QQGroup: React.FC = () => {
  const t = useT()
  return (
    <>
      <PageHeader>
        <PageHeaderHeading>{t.qqgroup.title}</PageHeaderHeading>
        <PageHeaderDescription>{t.qqgroup.desc}</PageHeaderDescription>
      </PageHeader>

      <FramedSection className="py-12 md:py-16" containerClassName="max-w-3xl">
        <div className="space-y-6">
          {groups.map((group) => (
            <Card key={group.number} className="gap-0 overflow-hidden py-0">
              <CardContent className="p-0">
                <div className="flex flex-col md:flex-row">
                  {/* 左侧图标区 */}
                  <div className="flex items-center justify-center border-b bg-muted/40 p-8 md:w-48 md:border-r md:border-b-0">
                    <div className="flex size-20 items-center justify-center rounded-2xl border bg-background text-foreground">
                      <Users className="size-9" />
                    </div>
                  </div>

                  {/* 右侧内容区 */}
                  <div className="flex-1 p-6 md:p-8">
                    <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
                      <div>
                        <h3 className="mb-1 text-xl font-semibold text-foreground">
                          {t.qqgroup.groupName}
                        </h3>
                        <p className="text-muted-foreground">{group.number}</p>
                      </div>
                      <div className="flex gap-2">
                        <Badge variant="secondary">{t.qqgroup.capacity}</Badge>
                        <Badge variant="success">{t.qqgroup.status}</Badge>
                      </div>
                    </div>

                    <div className="flex flex-wrap gap-3">
                      <Dialog>
                        <DialogTrigger render={<Button variant="outline" />}>
                          <QrCode className="size-4" />
                          {t.qqgroup.viewQr}
                        </DialogTrigger>
                        <DialogPopup>
                          <DialogHeader>
                            <DialogTitle>{t.qqgroup.groupName}</DialogTitle>
                          </DialogHeader>
                          <DialogPanel>
                            <div className="flex flex-col items-center">
                              <img
                                src={group.qrcode}
                                alt={`${t.qqgroup.groupName} ${t.qqgroup.qrAlt}`}
                                className="max-h-[32rem] max-w-full rounded-lg border"
                              />
                            </div>
                          </DialogPanel>
                        </DialogPopup>
                      </Dialog>
                      <Button
                        render={
                          <a href={group.joinUrl} target="_blank" rel="noopener noreferrer" />
                        }
                      >
                        <ExternalLink className="size-4" />
                        {t.qqgroup.join}
                      </Button>
                    </div>
                  </div>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      </FramedSection>
    </>
  )
}

export default QQGroup
