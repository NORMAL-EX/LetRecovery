import type * as React from 'react'
import { cn } from '@/lib/utils'

interface PageHeaderProps extends React.ComponentProps<'section'> {
  /** 对齐方式：居中（默认，内页用）或左对齐（首页 Hero 用） */
  align?: 'center' | 'start'
}

/** coss 风格英雄/页眉区：大号 font-heading 标题 + muted 描述 + 按钮行。 */
export function PageHeader({
  className,
  align = 'center',
  children,
  ...props
}: PageHeaderProps) {
  return (
    <section className={cn('relative', className)} {...props}>
      <div
        className={cn(
          'container flex flex-col gap-3 px-4 py-12 md:py-16 lg:py-20 xl:gap-4',
          align === 'center'
            ? 'items-center text-center'
            : 'items-start text-left',
        )}
      >
        {children}
      </div>
    </section>
  )
}

export function PageHeaderHeading({
  className,
  ...props
}: React.ComponentProps<'h1'>) {
  return (
    <h1
      className={cn(
        'font-heading text-3xl leading-tight tracking-tight md:text-4xl lg:text-5xl',
        className,
      )}
      {...props}
    />
  )
}

export function PageHeaderDescription({
  className,
  ...props
}: React.ComponentProps<'p'>) {
  return (
    <p
      className={cn(
        'max-w-2xl text-balance text-muted-foreground md:text-lg',
        className,
      )}
      {...props}
    />
  )
}
