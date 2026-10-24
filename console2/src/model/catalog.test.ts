import { describe, expect, it } from 'vitest'
import { catalog } from './catalog'
import spec from '../../openapi.json'

const known = new Set(Object.keys((spec as { paths: Record<string, unknown> }).paths).map((p) => p.replace(/\{[^}]+\}/g, '{}')))
const norm = (p: string) => p.split('?')[0]!.replace(/\{[^}]+\}/g, '{}')
const all = Object.entries(catalog).flatMap(([section, rs]) => rs.map((r) => ({ section, ...r })))

describe('catalog', () => {
  it('has unique resource ids within a section', () => { for (const [s, rs] of Object.entries(catalog)) { const ids = rs.map((r) => r.id); expect(new Set(ids).size, s).toBe(ids.length) } })
  it('only points at paths the node serves', () => {
    const bad = all.flatMap((r) => [r.list, r.item, ...(r.actions ?? []).map((a) => a.path), ...(r.pageActions ?? []).map((a) => a.path)].filter((p): p is string => !!p && p.startsWith('/')).filter((p) => !known.has(norm(p))).map((p) => `${r.section}/${r.id}: ${p}`))
    expect(bad).toEqual([])
  })
  it('names a title for every resource', () => { for (const r of all) expect(r.title, `${r.section}/${r.id}`).toBeTruthy() })
})
