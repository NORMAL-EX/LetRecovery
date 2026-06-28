import { useLocation } from 'react-router-dom'
import { CornerMarks } from './Frame'
import { useT } from '@/lib/i18n'

const friendLinks = [
  { name: 'Cloud-PE 官网', url: 'https://cloud-pe.cn' },
  { name: 'Cloud-PE 云盘', url: 'https://pan.sysre.cn' },
]

const Footer: React.FC = () => {
  const t = useT()
  const { pathname } = useLocation()
  // 文档页不显示页脚
  if (pathname.startsWith('/docs')) return null
  return (
    <footer className="relative mt-8 py-8 text-muted-foreground before:absolute before:inset-x-0 before:top-0 before:h-px before:bg-border/64">
      <CornerMarks />
      <div className="container flex w-full flex-col items-center gap-4 text-center text-sm">
        {/* 友情链接 */}
        <div className="flex flex-wrap items-center justify-center gap-x-2 gap-y-1">
          <span className="text-muted-foreground/70">{t.footer.friends}</span>
          {friendLinks.map((link, index) => (
            <span key={link.name} className="inline-flex items-center gap-2">
              <a
                href={link.url}
                target="_blank"
                rel="noopener noreferrer"
                className="text-foreground/70 transition-colors hover:text-foreground"
              >
                {link.name}
              </a>
              {index < friendLinks.length - 1 && (
                <span className="text-border" aria-hidden="true">
                  ·
                </span>
              )}
            </span>
          ))}
        </div>

        {/* 备案号 + 版权 */}
        <div className="flex flex-col items-center gap-1.5">
          <a
            href="https://beian.miit.gov.cn/#/Integrated/index"
            target="_blank"
            rel="noopener noreferrer"
            className="transition-colors hover:text-foreground"
          >
            鲁ICP备2023028944号
          </a>
          <p>
            © {new Date().getFullYear()} LetRecovery · {t.footer.crafted}
          </p>
        </div>
      </div>
    </footer>
  )
}

export default Footer
