import type * as React from 'react'
import { cn } from '@/lib/utils'

/**
 * coss 招牌"框线网格"：在居中的 1416 宽栏左右两侧绘制全高竖直导轨。
 * 放在 App 外壳里渲染一次即可。仅在视口宽于内容栏时（留有边距）可见。
 */
export function GridRails() {
  return (
    <div
      aria-hidden="true"
      className="container pointer-events-none absolute inset-0 z-30 before:absolute before:inset-y-0 before:-left-3 before:w-px before:bg-border/64 after:absolute after:inset-y-0 after:-right-3 after:w-px after:bg-border/64"
    />
  )
}

/**
 * 区块交界处的四角小方块（与竖直导轨相交形成"准星"标记）。
 * 默认贴在所属相对定位容器的顶部。
 */
export function CornerMarks({ className }: { className?: string }) {
  return (
    <div
      aria-hidden="true"
      className={cn(
        'container pointer-events-none absolute inset-0 z-30 before:absolute before:top-[-3.5px] before:-left-[11.5px] before:z-1 before:-ml-1 before:size-2 before:rounded-[2px] before:border before:border-border before:bg-popover before:bg-clip-padding before:shadow-xs after:absolute after:top-[-3.5px] after:-right-[11.5px] after:z-1 after:-mr-1 after:size-2 after:rounded-[2px] after:border after:border-border after:bg-background after:bg-clip-padding after:shadow-xs dark:before:bg-clip-border dark:after:bg-clip-border',
        className,
      )}
    />
  )
}

interface FramedSectionProps extends React.ComponentProps<'section'> {
  /** 内层 container 的额外类名 */
  containerClassName?: string
  /** 是否在顶部绘制分隔细线 + 四角方块（默认开启） */
  divider?: boolean
}

/**
 * 一个带顶部分隔细线和四角方块的内容区块——coss 文档站的核心版式单元。
 */
export function FramedSection({
  className,
  containerClassName,
  divider = true,
  children,
  ...props
}: FramedSectionProps) {
  return (
    <section
      className={cn(
        'relative',
        divider &&
          'before:absolute before:inset-x-0 before:top-0 before:h-px before:bg-border/64',
        className,
      )}
      {...props}
    >
      {divider && <CornerMarks />}
      <div className={cn('container w-full', containerClassName)}>{children}</div>
    </section>
  )
}
