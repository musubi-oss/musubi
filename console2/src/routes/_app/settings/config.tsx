import { useState } from 'react'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'

/** The node's effective configuration (file + environment + defaults), flattened to dotted keys and grouped by section; secret-looking values are shown redacted by the node. Read-only — change it in musubi.toml or the environment and restart. */
export const Route = createFileRoute('/_app/settings/config')({
  validateSearch: (s: Record<string, unknown>): { q?: string } => ({ q: typeof s.q === 'string' && s.q ? s.q : undefined }),
  component: NodeConfig,
})

function flat(o: unknown, pre = ''): [string, unknown][] { if (o && typeof o === 'object' && !Array.isArray(o)) return Object.entries(o as Row).flatMap(([k, v]) => flat(v, pre ? `${pre}.${k}` : k)); return [[pre, o]] }
const show = (v: unknown) => (v === null ? 'null' : typeof v === 'string' ? (v === '' ? '""' : v) : JSON.stringify(v))
const secret = (k: string) => /secret|key|password|token|license/i.test(k)

function NodeConfig() {
  const { q = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const cq = useSub<Row>('/v1/admin/config')
  const rows = flat((cq.data?.config as Row | undefined) ?? cq.data ?? {}).filter(([k, v]) => !q || k.toLowerCase().includes(q.toLowerCase()) || JSON.stringify(v).toLowerCase().includes(q.toLowerCase()))
  const sections = new Map<string, [string, unknown][]>()
  for (const r of rows) { const s = r[0].split('.')[0]!; sections.set(s, [...(sections.get(s) ?? []), r]) }
  const [open, setOpen] = useState<Record<string, boolean>>({})
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Settings', to: '/settings' }, { title: 'Node config' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Node config</h1><p className="mt-1 text-sm text-muted-foreground">What this node is actually running with, after the config file, environment variables and defaults are merged. Keys that look like secrets are redacted by the node before they get here. Edit the TOML or the environment and restart to change anything.</p></div>
        <Input className="w-56" placeholder="filter keys / values" defaultValue={q} onChange={(e) => void nav({ search: { q: e.target.value || undefined }, replace: true })} />
      </div>
      {cq.isError && <Alert variant="destructive"><AlertDescription>{String(cq.error)}</AlertDescription></Alert>}
      {cq.isPending ? <p className="text-sm text-muted-foreground">Loading…</p> : !sections.size ? <p className="text-sm text-muted-foreground">Nothing matches.</p> : [...sections.entries()].map(([s, items]) => { const o = open[s] ?? !!q; return (
        <Card key={s}><CardContent className="p-0">
          <button className="flex w-full items-center gap-2 px-4 py-3 text-left text-sm font-medium" onClick={() => setOpen((x) => ({ ...x, [s]: !o }))}><span className="text-muted-foreground">{o ? '▾' : '▸'}</span><span className="font-mono">[{s}]</span><Badge variant="outline" className="ml-auto">{items.length}</Badge></button>
          {o && <div className="divide-y border-t">{items.map(([k, v]) => <div key={k} className="grid grid-cols-1 gap-1 px-4 py-2 sm:grid-cols-[minmax(0,20rem)_1fr]"><div className="truncate font-mono text-xs text-muted-foreground">{k.slice(s.length + 1) || k}</div><div className={`break-all font-mono text-xs ${secret(k) ? 'text-muted-foreground' : ''}`}>{secret(k) && v ? '••••••' : show(v)}</div></div>)}</div>}
        </CardContent></Card>) })}
    </div>
  )
}
