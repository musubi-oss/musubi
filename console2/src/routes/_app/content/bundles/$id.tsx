import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { JsonView } from '@/components/json-view'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'

/** One published content version: every table in the bundle diffed against the previous version (added / removed / changed rows, changed fields), with rollback that publishes the old bundle as a new version. */
export const Route = createFileRoute('/_app/content/bundles/$id')({
  validateSearch: (s: Record<string, unknown>): { table?: string; all?: string } => ({ table: typeof s.table === 'string' ? s.table : undefined, all: typeof s.all === 'string' ? s.all : undefined }),
  component: Version,
})

const ts = (x: unknown) => (x ? String(x).replace('T', ' ').slice(0, 19) : '—')
const keyOf = (r: Row) => String(r.id ?? r.code ?? r.name ?? r.key ?? JSON.stringify(r))
type St = 'added' | 'removed' | 'changed' | 'same'
function diffTable(a: Row[], b: Row[]) {
  const bm = new Map(b.map((r) => [keyOf(r), r])), am = new Map(a.map((r) => [keyOf(r), r]))
  const rows: { k: string; st: St; a?: Row; b?: Row }[] = []
  for (const [k, r] of am) { const o = bm.get(k); rows.push({ k, st: !o ? 'added' : JSON.stringify(o) === JSON.stringify(r) ? 'same' : 'changed', a: r, b: o }) }
  for (const [k, r] of bm) if (!am.has(k)) rows.push({ k, st: 'removed', b: r })
  return rows.sort((x, y) => x.k.localeCompare(y.k))
}
const fields = (a?: Row, b?: Row) => [...new Set([...Object.keys(a ?? {}), ...Object.keys(b ?? {})])].filter((k) => JSON.stringify(a?.[k]) !== JSON.stringify(b?.[k]))
const COLOR: Record<St, string> = { added: 'text-emerald-600', removed: 'text-destructive', changed: 'text-amber-600', same: 'text-muted-foreground' }

function Version() {
  const { id } = Route.useParams()
  const { table = '', all = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const v = useSub<Row>(`/v1/admin/content/versions/${encodeURIComponent(id)}`)
  const list = useSub<Row>('/v1/admin/content/versions')
  const versions = listOf(list.data, 'versions')
  const i = versions.findIndex((x) => String(x.id) === id)
  const prevId = i >= 0 ? versions[i + 1]?.id : undefined
  const prev = useSub<Row>(prevId != null ? `/v1/admin/content/versions/${encodeURIComponent(String(prevId))}` : null)
  const tables = ((v.data?.bundle as Row | undefined)?.tables ?? {}) as Record<string, Row[]>
  const ptables = ((prev.data?.bundle as Row | undefined)?.tables ?? {}) as Record<string, Row[]>
  const names = [...new Set([...Object.keys(tables), ...Object.keys(ptables)])].sort()
  const summary = names.map((n) => { const d = diffTable(tables[n] ?? [], ptables[n] ?? []); return { n, rows: (tables[n] ?? []).length, added: d.filter((x) => x.st === 'added').length, removed: d.filter((x) => x.st === 'removed').length, changed: d.filter((x) => x.st === 'changed').length } })
  const cur = table && names.includes(table) ? table : names[0] ?? ''
  const diff = cur ? diffTable(tables[cur] ?? [], ptables[cur] ?? []) : []
  const shown = all ? diff : diff.filter((r) => r.st !== 'same')
  const { run, pending } = useAct([['sub']], () => void nav({ to: '/$section/$page', params: { section: 'content', page: 'bundles' } }))
  const total = Object.values(tables).reduce((s, t) => s + t.length, 0)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Content', to: '/content' }, { title: 'Versions', to: '/content/bundles' }, { title: `v${id}` }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Content version {id}{i === 0 && <Badge className="ml-2 align-middle">current</Badge>}</h1><p className="mt-1 text-sm text-muted-foreground">{v.data ? <>{ts(v.data.created_at)}{v.data.note ? ` · ${v.data.note}` : ''} · {names.length} tables, {total} rows{prevId != null ? ` · compared with v${prevId}` : ' · first version, nothing to compare'}</> : 'Loading…'}</p></div>
        {i > 0 && <Button size="sm" variant="outline" disabled={pending} onClick={() => confirmThen(`Publish v${id} again as the newest version? Clients will receive this bundle on next content check.`, () => run(() => raw('POST', `/v1/admin/content/versions/${encodeURIComponent(id)}/rollback`), 'Rolled back — new version created'))}>Roll back to this</Button>}
      </div>
      {v.isError && <Alert variant="destructive"><AlertDescription>{String(v.error)}</AlertDescription></Alert>}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-[18rem_1fr]">
        <Card><CardContent className="pt-6">
          <ul className="divide-y text-sm">{summary.map((s) => <li key={s.n}><button className={`flex w-full items-center justify-between gap-2 py-1.5 text-left ${s.n === cur ? 'font-semibold' : ''}`} onClick={() => void nav({ search: (p) => ({ ...p, table: s.n }), replace: true })}><span className="truncate font-mono">{s.n}</span><span className="shrink-0 text-xs text-muted-foreground">{s.rows}{s.added ? <span className="text-emerald-600"> +{s.added}</span> : null}{s.removed ? <span className="text-destructive"> −{s.removed}</span> : null}{s.changed ? <span className="text-amber-600"> ~{s.changed}</span> : null}</span></button></li>)}{!summary.length && !v.isPending && <li className="py-2 text-muted-foreground">Empty bundle</li>}</ul>
        </CardContent></Card>
        <Card className="min-w-0"><CardContent className="pt-6">
          <div className="mb-3 flex flex-wrap items-center gap-2"><span className="font-mono text-sm font-semibold">{cur || '—'}</span><span className="text-xs text-muted-foreground">{shown.length} of {diff.length} rows</span><Tabs className="ml-auto" value={all ? 'all' : 'changes'} onValueChange={(x) => void nav({ search: (p) => ({ ...p, all: x === 'all' ? '1' : undefined }), replace: true })}><TabsList className="h-8"><TabsTrigger value="changes">Changes</TabsTrigger><TabsTrigger value="all">All rows</TabsTrigger></TabsList></Tabs></div>
          {!shown.length ? <p className="text-sm text-muted-foreground">{diff.length ? 'No changes in this table versus the previous version.' : 'No rows.'}</p> :
            <ul className="divide-y">{shown.map((r) => <li key={r.k} className="py-2 text-sm"><div className="flex flex-wrap items-center gap-2"><span className={`w-16 shrink-0 text-xs uppercase ${COLOR[r.st]}`}>{r.st}</span><span className="font-mono">{r.k}</span>{r.st === 'changed' && <span className="text-xs text-muted-foreground">{fields(r.a, r.b).join(', ')}</span>}</div>
              {r.st === 'changed' ? <div className="mt-1 grid grid-cols-1 gap-2 pl-0 sm:grid-cols-2 sm:pl-[4.5rem]"><div><div className="text-[10px] uppercase text-muted-foreground">before</div><JsonView value={Object.fromEntries(fields(r.a, r.b).map((k) => [k, r.b?.[k]]))} /></div><div><div className="text-[10px] uppercase text-muted-foreground">after</div><JsonView value={Object.fromEntries(fields(r.a, r.b).map((k) => [k, r.a?.[k]]))} /></div></div>
                : r.st !== 'same' ? <div className="mt-1 sm:pl-[4.5rem]"><JsonView value={r.a ?? r.b} /></div> : null}</li>)}</ul>}
        </CardContent></Card>
      </div>
    </div>
  )
}
