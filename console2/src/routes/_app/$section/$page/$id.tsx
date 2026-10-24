// Catalog-driven record page: fields (cross-linked by name), related tabs from the catalog,
// raw JSON; edit/delete only when the resource has a writable item route.
import { useState } from 'react'
import { Link, createFileRoute, notFound } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Copy, Pencil, Trash2 } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'
import { ActionButton } from '@/components/action-button'
import { Crumbs } from '@/components/crumbs'
import { DataTable, type Col } from '@/components/data-table'
import { resource, sectionById } from '@/model/sections'
import { refFor } from '@/model/refs'
import { fill, fmtVal, isMonoKey, pickRows, type Row } from '@/model/resource'
import { qs, raw } from '@/lib/api'
import { cn } from '@/lib/utils'

export const Route = createFileRoute('/_app/$section/$page/$id')({
  validateSearch: (s: Record<string, unknown>) => ({ tab: typeof s.tab === 'string' ? s.tab : undefined }),
  beforeLoad: ({ params }) => { if (!sectionById(params.section) || !resource(params.section, params.page)) throw notFound() },
  component: DetailPage,
})

function unwrap(r: unknown): Row | null {
  if (!r || typeof r !== 'object' || Array.isArray(r)) return null
  const o = r as Row
  const vals = Object.values(o)
  return vals.length === 1 && vals[0] && typeof vals[0] === 'object' && !Array.isArray(vals[0]) ? (vals[0] as Row) : o
}

function DetailPage() {
  const { section, page, id } = Route.useParams()
  const { tab = 'fields' } = Route.useSearch()
  const nav = Route.useNavigate()
  const qc = useQueryClient()
  const res = resource(section, page)!
  const sec = sectionById(section)!
  const key = res.key ?? 'id'
  const path = res.item ? fill(res.item, { [key]: id }, id) : null
  const item = useQuery({
    queryKey: ['item', section, page, id],
    queryFn: async ({ signal }) => {
      if (path) return unwrap(await raw('GET', path, undefined, { signal }))
      const rows = pickRows(await raw('GET', res.list + qs({ limit: 500 }), undefined, { signal }), res.rowsKey, key)
      const row = rows.find((r) => String(r[key] ?? r.id ?? r.name) === id)
      if (!row) throw new Error(`${res.title.replace(/s$/, '')} ${id} is not in the list any more`)
      return row
    },
  })
  const rec = item.data ?? null
  const itemPath = res.item ? fill(res.item, { ...(rec ?? {}), [key]: id }, id) : null
  const tabs = res.detail?.tabs ?? []
  const def = tabs.find((t) => t.id === tab)
  const related = useQuery({
    queryKey: ['item-tab', section, page, id, tab],
    enabled: !!def && !!rec,
    queryFn: ({ signal }) => raw<unknown>('GET', fill(def!.list, rec, id) + (def!.list.includes('?') ? '' : qs({ limit: 100 })), undefined, { signal }),
  })
  const relRows = pickRows(related.data, def?.rowsKey)
  const relCols: Col[] = (def?.cols ?? (relRows[0] ? Object.keys(relRows[0]).slice(0, 7) : ['id'])).map((k) => ({ key: k }))
  const title = String(rec?.name ?? rec?.title ?? rec?.username ?? rec?.display_name ?? id)
  const fields = Object.entries(rec ?? {}).filter(([, v]) => typeof v !== 'object' || v === null)
  const nested = Object.entries(rec ?? {}).filter(([, v]) => typeof v === 'object' && v !== null)
  const reload = () => void qc.invalidateQueries({ queryKey: ['item', section, page, id] })

  const [editing, setEditing] = useState(false)
  const [text, setText] = useState('')
  async function save() {
    let body: unknown
    try { body = JSON.parse(text) } catch { toast.error('not valid JSON'); return }
    try { await raw(res.method ?? 'PUT', itemPath!, body); toast.success('saved'); setEditing(false); reload() } catch (e) { toast.error(e instanceof Error ? e.message : String(e)) }
  }
  async function remove() {
    if (!window.confirm(`Delete ${res.title.replace(/s$/, '').toLowerCase()} ${id}?`)) return
    try { await raw('DELETE', itemPath!); toast.success('deleted'); void qc.invalidateQueries({ queryKey: ['list', res.list] }); void nav({ to: '/$section/$page', params: { section, page }, search: {} }) } catch (e) { toast.error(e instanceof Error ? e.message : String(e)) }
  }
  const acts = rec ? (res.actions ?? []).filter((a) => !a.when || a.when(rec)) : []

  return (
    <div className="grid gap-4">
      <Crumbs items={[{ title: sec.title, to: `/${section}` }, { title: res.title, to: `/${section}/${page}` }, { title }]} />
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <h1 className="truncate text-xl font-semibold">{item.isPending ? <Skeleton className="h-7 w-48" /> : title}</h1>
          {title !== id && <p className="font-mono text-xs text-muted-foreground">{id}</p>}
        </div>
        <div className="flex flex-wrap gap-1">
          {acts.map((a) => <ActionButton key={a.label} act={a} row={rec} id={id} onDone={reload} />)}
          <Button variant="ghost" size="sm" aria-label="Copy JSON" onClick={() => { void navigator.clipboard?.writeText(JSON.stringify(rec, null, 2)); toast.success('copied') }}><Copy /></Button>
          {itemPath && <Button variant="outline" size="sm" onClick={() => { setText(JSON.stringify(rec, null, 2)); setEditing(true) }}><Pencil /> Edit</Button>}
          {itemPath && <Button variant="destructive" size="sm" onClick={remove}><Trash2 /> Delete</Button>}
        </div>
      </div>
      {item.isError && <Alert variant="destructive"><AlertTitle>Could not load</AlertTitle><AlertDescription>{item.error.message}</AlertDescription></Alert>}
      <Tabs value={tab} onValueChange={(v) => void nav({ search: (prev) => ({ ...prev, tab: v === 'fields' ? undefined : v }), replace: true })}>
        <TabsList className="flex-wrap">
          <TabsTrigger value="fields">Fields</TabsTrigger>
          {tabs.map((t) => <TabsTrigger key={t.id} value={t.id}>{t.title}</TabsTrigger>)}
          <TabsTrigger value="json">JSON</TabsTrigger>
        </TabsList>
        <TabsContent value="fields" className="grid gap-4">
          <Card className="py-3"><CardContent className="px-4">
            {item.isPending ? <div className="grid gap-2">{[0, 1, 2, 3].map((i) => <Skeleton key={i} className="h-4 w-full" />)}</div>
            : <dl className="grid gap-x-6 gap-y-1.5 text-sm sm:grid-cols-[max-content_minmax(0,1fr)]">
              {fields.map(([k, v]) => { const to = refFor(k, v, rec); const s = fmtVal(k, v); return (
                <div key={k} className="contents"><dt className="text-muted-foreground">{k.replace(/_/g, ' ')}</dt>
                <dd className={cn('min-w-0 break-all', isMonoKey(k) && 'font-mono text-xs')}>{to ? <Link to={to} className="hover:underline">{s}</Link> : s}</dd></div>) })}
            </dl>}
          </CardContent></Card>
          {nested.map(([k, v]) => (
            <Card key={k} className="py-3"><CardContent className="px-4">
              <div className="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">{k.replace(/_/g, ' ')}</div>
              {Array.isArray(v) && v.length && typeof v[0] === 'object' ? <DataTable id={`${section}.${page}.${k}`} tools={false} cols={Object.keys(v[0] as Row).slice(0, 7).map((c) => ({ key: c }))} rows={v as Row[]} />
              : <pre className="max-h-80 overflow-auto text-xs">{JSON.stringify(v, null, 2)}</pre>}
            </CardContent></Card>
          ))}
        </TabsContent>
        {tabs.map((t) => (
          <TabsContent key={t.id} value={t.id}>
            {related.isError && <Alert variant="destructive" className="mb-2"><AlertDescription>{related.error.message}</AlertDescription></Alert>}
            <DataTable id={`${section}.${page}.${t.id}`} cols={relCols} rows={relRows} loading={related.isPending && !!rec} rowTo={(r) => refFor('session_id', r.session_id, r) ?? null} empty={`No ${t.title.toLowerCase()}.`} />
          </TabsContent>
        ))}
        <TabsContent value="json"><pre className="max-h-[70vh] overflow-auto rounded-md border p-3 text-xs">{JSON.stringify(rec, null, 2)}</pre></TabsContent>
      </Tabs>
      <Dialog open={editing} onOpenChange={setEditing}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader><DialogTitle>Edit · {res.method ?? 'PUT'} <span className="font-mono text-xs">{itemPath}</span></DialogTitle></DialogHeader>
          <Textarea value={text} onChange={(e) => setText(e.target.value)} rows={18} className="font-mono text-xs" />
          <DialogFooter><Button variant="ghost" onClick={() => setEditing(false)}>Cancel</Button><Button onClick={save}>Save</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
