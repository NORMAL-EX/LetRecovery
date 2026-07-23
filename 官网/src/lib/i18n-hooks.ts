import { useContext } from 'react'

import { LangContext } from './i18n-context'
import { translations, type Dict } from './translations'

export function useLang() {
  return useContext(LangContext)
}

export function useT(): Dict {
  return translations[useLang().lang]
}
