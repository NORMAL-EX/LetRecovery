import { createContext } from 'react'

export type Lang = 'zh' | 'en'

interface LangContextValue {
  lang: Lang
  setLang: (lang: Lang) => void
}

export const LangContext = createContext<LangContextValue>({
  lang: 'zh',
  setLang: () => {},
})
