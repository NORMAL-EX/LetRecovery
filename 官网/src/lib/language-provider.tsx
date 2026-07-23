import { useEffect, useState, type ReactNode } from 'react'

import { LangContext, type Lang } from './i18n-context'

const STORAGE_KEY = 'letrecovery-lang'

function detectInitialLanguage(): Lang {
  try {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved === 'zh' || saved === 'en') return saved
  } catch {
    // Storage can be unavailable in hardened or private browsing contexts.
  }

  if (
    typeof navigator !== 'undefined' &&
    !navigator.language.toLowerCase().startsWith('zh')
  ) {
    return 'en'
  }
  return 'zh'
}

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectInitialLanguage)

  useEffect(() => {
    document.documentElement.lang = lang === 'en' ? 'en' : 'zh-CN'
  }, [lang])

  const setLang = (nextLanguage: Lang) => {
    setLangState(nextLanguage)
    try {
      localStorage.setItem(STORAGE_KEY, nextLanguage)
    } catch {
      // Keep the in-memory selection when persistent storage is unavailable.
    }
  }

  return (
    <LangContext.Provider value={{ lang, setLang }}>
      {children}
    </LangContext.Provider>
  )
}
