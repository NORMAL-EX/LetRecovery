import { useEffect, useRef } from 'react'

interface Track {
  default?: boolean
  html: string
  url: string
}

interface VideoPlayerProps {
  src?: string
  poster?: string
  title?: string
  qualities?: Track[] | string
  audioTracks?: Track[] | string
}

function parseList(v: Track[] | string | undefined): Track[] | undefined {
  if (!v) return undefined
  if (typeof v === 'string') {
    try {
      return JSON.parse(v) as Track[]
    } catch {
      return undefined
    }
  }
  return v
}

/**
 * ArtPlayer 封装（懒加载、仅客户端）。支持多清晰度源，以及一条可选的、与视频同步的
 * 独立音轨（如背景音乐）。文档里用 `<div data-player="video" …>` 媒体岛挂载。
 */
export default function VideoPlayer({
  src,
  poster,
  title,
  qualities,
  audioTracks,
}: VideoPlayerProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let art: any = null
    let audioEl: HTMLAudioElement | null = null
    let cancelled = false
    const el = containerRef.current
    if (!el) return

    const qs = parseList(qualities)
    const tracks = parseList(audioTracks)

    void import('artplayer').then(({ default: Artplayer }) => {
      if (cancelled || !el) return
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const options: any = {
        container: el,
        poster: poster ?? '',
        title: title ?? '',
        volume: 1,
        theme: '#737373',
        setting: true,
        playbackRate: true,
        aspectRatio: true,
        fullscreen: true,
        fullscreenWeb: true,
        pip: true,
        autoSize: false,
        screenshot: true,
        flip: true,
        miniProgressBar: true,
        playsInline: true,
        mutex: true,
        moreVideoAttr: { crossOrigin: 'anonymous' },
      }
      if (qs && qs.length) {
        options.quality = qs
        options.url = (qs.find((q) => q.default) ?? qs[0]).url
      } else {
        options.url = src ?? ''
      }

      art = new Artplayer(options)

      if (tracks && tracks.length) {
        const track = tracks.find((t) => t.default) ?? tracks[0]
        art.on('ready', () => {
          art.video.volume = 0
          audioEl = document.createElement('audio')
          audioEl.src = track.url
          audioEl.crossOrigin = 'anonymous'
          audioEl.style.display = 'none'
          document.body.appendChild(audioEl)
          const v = art.video as HTMLVideoElement
          const sync = () => {
            if (audioEl && !Number.isNaN(audioEl.duration)) {
              audioEl.currentTime = v.currentTime
            }
          }
          v.addEventListener('play', () => {
            sync()
            void audioEl?.play().catch(() => {})
          })
          v.addEventListener('pause', () => audioEl?.pause())
          v.addEventListener('seeked', sync)
          v.addEventListener('volumechange', () => {
            if (audioEl) {
              audioEl.volume = art.volume
              audioEl.muted = art.muted
            }
          })
          audioEl.volume = 1
        })
      }
    })

    return () => {
      cancelled = true
      audioEl?.pause()
      audioEl?.remove()
      art?.destroy(false)
    }
  }, [src, poster, title, qualities, audioTracks])

  return (
    <div
      ref={containerRef}
      className="isolate my-5 aspect-video w-full overflow-hidden rounded-xl border bg-black"
    />
  )
}
