/**
 * Slugify a heading into a URL-safe anchor id.
 * Keeps CJK characters so Chinese headings get readable anchors, mirroring
 * the behaviour of VitePress's default slugify.
 */
const COMBINING_MARKS = new RegExp('[\\u0300-\\u036f]', 'g')
// Drop anything that isn't an ascii word char, a dash, or a CJK ideograph.
const NON_SLUG_CHARS = new RegExp('[^\\w\\u4e00-\\u9fff-]', 'g')

export function slugify(str: string): string {
  return str
    .normalize('NFKD')
    .replace(COMBINING_MARKS, '')
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '-')
    .replace(NON_SLUG_CHARS, '')
    .replace(/-+/g, '-')
    .replace(/^-+|-+$/g, '')
}
