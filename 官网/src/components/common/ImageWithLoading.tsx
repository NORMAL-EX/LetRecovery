import React, { useState } from 'react'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

interface ImageWithLoadingProps
  extends Omit<React.ImgHTMLAttributes<HTMLImageElement>, 'src' | 'alt'> {
  src: string
  alt: string
  /** classes applied to the <img> (size / shape) */
  className?: string
  /** extra classes for the wrapper */
  wrapperClassName?: string
  /** rounding used by the skeleton / fallback, should match the image */
  rounded?: string
  /** min-height kept while loading, useful for fluid (w-full h-auto) images */
  loadingMinHeight?: string
}

const ImageWithLoading: React.FC<ImageWithLoadingProps> = ({
  src,
  alt,
  className,
  wrapperClassName,
  rounded = 'rounded-lg',
  loadingMinHeight,
  ...rest
}) => {
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(false)

  if (error) {
    // 加载失败 → 显示灰色占位图（响应式，填满图片应占的宽度）
    return (
      <div className={cn('overflow-hidden', rounded, wrapperClassName)}>
        <img
          src="/img-placeholder.svg"
          alt={alt}
          className={cn('select-none', className)}
        />
      </div>
    )
  }

  return (
    <div
      className={cn('relative', wrapperClassName)}
      style={loading && loadingMinHeight ? { minHeight: loadingMinHeight } : undefined}
    >
      {loading && (
        <Skeleton className={cn('absolute inset-0 h-full w-full', rounded)} />
      )}
      <img
        src={src}
        alt={alt}
        onLoad={() => setLoading(false)}
        onError={() => {
          setLoading(false)
          setError(true)
        }}
        className={cn(
          'transition-opacity duration-300',
          loading ? 'opacity-0' : 'opacity-100',
          className,
        )}
        {...rest}
      />
    </div>
  )
}

export default ImageWithLoading
