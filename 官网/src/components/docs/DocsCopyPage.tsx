import { useState } from 'react'
import { Check, Copy } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useT } from '@/lib/i18n'

/** "复制 Markdown" —— 把当前文档的原始 markdown 复制到剪贴板（仿 coss.com/ui） */
export default function DocsCopyPage({ raw }: { raw: string }) {
  const [copied, setCopied] = useState(false)
  const t = useT()

  const onCopy = () => {
    if (!navigator.clipboard || !raw) return
    void navigator.clipboard.writeText(raw).then(() => {
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2000)
    })
  }

  return (
    <Button variant="outline" size="sm" onClick={onCopy} className="gap-1.5">
      {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
      {copied ? t.docs.copied : t.docs.copyMarkdown}
    </Button>
  )
}
