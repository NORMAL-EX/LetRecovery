import { Separator } from '@/components/ui/separator'

const Footer: React.FC = () => {
  const friendLinks = [
    { name: 'Cloud-PE官网', url: 'https://cloud-pe.cn' },
    { name: 'Cloud-PE云盘', url: 'https://pan.sysre.cn' },
  ]

  return (
    <footer className="border-t border-border/40 bg-background">
      <div className="container mx-auto px-4 py-10">
        {/* 友情链接 */}
        <div className="max-w-4xl mx-auto mb-8">
          <h3 className="font-semibold text-center text-foreground mb-5 text-base tracking-widest">
            友 情 链 接
          </h3>
          <div className="text-center">
            <span className="text-sm text-muted-foreground mr-3">相关网站</span>
            <div className="inline-flex flex-wrap justify-center gap-x-1 gap-y-2">
              {friendLinks.map((link, index) => (
                <span key={link.name} className="inline-flex items-center">
                  <a
                    href={link.url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-foreground/70 hover:text-primary transition-colors"
                  >
                    {link.name}
                  </a>
                  {index < friendLinks.length - 1 && (
                    <span className="text-border/60 mx-2">|</span>
                  )}
                </span>
              ))}
            </div>
          </div>
        </div>

        {/* 分割线 */}
        <Separator className="max-w-xl mx-auto mb-6" />

        {/* 版权信息 */}
        <div className="text-center space-y-2">
          <a
            href="https://beian.miit.gov.cn/#/Integrated/index"
            target="_blank"
            rel="noopener noreferrer"
            className="text-sm text-muted-foreground hover:text-foreground transition-colors block"
          >
            鲁ICP备2023028944号
          </a>
          <p className="text-sm text-muted-foreground" style={{ fontSize: 14 }}>
            © 2026-Present Cloud-PE Dev / LetRecovery Team
          </p>
        </div>
      </div>
    </footer>
  )
}

export default Footer
