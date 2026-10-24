// Catalog-driven list page: ?q (client-side filter), ?cursor, ?<filter>=value all live in the URL;
// the node is queried with limit 50; create = JSON template → POST list (or PUT item for keyed
// resources); row actions come from the catalog.
import { useState } from 'react'
import { Link, createFileRoute, notFound } from '@tanstack/react-router'
import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Plus, RefreshCw } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { ActionButton } from '@/components/action-button'
import { Crumbs } from '@/components/crumbs'
import { DataTable, type Col } from '@/components/data-table'
import { about } from '@/model/about'
import { resource, sectionById } from '@/model/sections'
import { detailPath, featureOff, itemPath, pickRows, type Row } from '@/model/resource'
import { ApiError, qs, raw } from '@/lib/api'

type Search = Record<string, string | undefined>
export const Route = createFileRoute('/_app/$section/$page/')({
  validateSearch: (s: Record<string, unknown>): Search => Object.fromEntries(Object.entries(s).filter(([, v]) => typeof v === 'string' && v !== '').map(([k, v]) => [k, String(v)])),
  beforeLoad: ({ params }) => { if (!sectionById(params.section) || !resource(params.section, params.page)) throw notFound() },
  component: ListPage,
})

export function listQuery(list: string, params: Record<string, string | undefined>) {
  return { queryKey: ['list', list, params], queryFn: ({ signal }: { signal: AbortSignal }) => raw<unknown>('GET', list + qs(params), undefined, { signal }) }
}

function ListPage() {
  const { section, page } = Route.useParams()
  const search = Route.useSearch()
  const nav = Route.useNavigate()
  const qc = useQueryClient()
  const res = resource(section, page)!
  const sec = sectionById(section)!
  const filters = Object.fromEntries((res.filters ?? []).map((f) => [f.key, search[f.key] ?? f.def ?? '']))
  const params: Record<string, string | undefined> = { cursor: search.cursor, limit: '50', ...Object.fromEntries(Object.entries(filters).filter(([, v]) => v && v !== 'all')) }
  const list = useQuery({ ...listQuery(res.list, params), placeholderData: keepPreviousData, enabled: res.custom !== 'zanshin' })
  const data = list.data as Row | Row[] | undefined
  const rows = pickRows(data, res.rowsKey)
  const next = !Array.isArray(data) ? (data?.next_cursor ?? data?.cursor ?? null) as string | null : null
  const off = list.error instanceof ApiError ? featureOff(list.error) : (!Array.isArray(data) && data?.enabled === false && !rows.length) ? `${res.title} is disabled in this node's config.` : null
  const q = (search.q ?? '').trim().toLowerCase()
  const shown = q ? rows.filter((r) => { const hay = JSON.stringify(r).toLowerCase(); return q.split(/\s+/).every((t) => hay.includes(t)) }) : rows
  const keys = res.cols ?? (rows[0] ? Object.keys(rows[0]).slice(0, 7) : ['id'])
  const cols: Col[] = keys.map((k) => ({ key: k }))
  const [stack, setStack] = useState<string[]>([])
  const set = (patch: Record<string, string | undefined>) => void nav({ search: (s: Search) => { const n: Search = { ...s }; for (const [k, v] of Object.entries(patch)) { if (v) n[k] = v; else delete n[k] } return n }, replace: true })
  const reload = () => void qc.invalidateQueries({ queryKey: ['list', res.list] })

  const [creating, setCreating] = useState(false)
  const [text, setText] = useState('')
  function startCreate() {
    setText(JSON.stringify({ ...(res.key ? { [res.key]: '' } : {}), ...(res.template ?? {}) }, null, 2)); setCreating(true)
  }
  async function create() {
    let body: Row
    try { body = JSON.parse(text) } catch { toast.error('body is not valid JSON'); return }
    try {
      if (res.method === 'PUT' && res.item) { const p = itemPath(res, body)!; const b = { ...body }; delete b[res.key ?? 'id']; await raw('PUT', p, b) }
      else await raw('POST', res.list, body)
      toast.success('created'); setCreating(false); reload()
    } catch (e) { toast.error(e instanceof Error ? e.message : String(e)) }
  }
  const canCreate = !!(res.create || res.template || res.newPath || (res.method === 'PUT' && res.item))
  const rowActions = (r: Row) => {
    const acts = (res.actions ?? []).filter((a) => !a.when || a.when(r))
    return acts.length ? <span className="inline-flex gap-1">{acts.map((a) => <ActionButton key={a.label} act={a} row={r} onDone={reload} />)}</span> : null
  }

  return (
    <div className="grid gap-4">
      <Crumbs items={[{ title: sec.title, to: `/${section}` }, { title: res.title }]} />
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <h1 className="text-xl font-semibold">{res.title}</h1>
          {about[`${section}/${page}`] && <p className="mt-1 max-w-3xl text-sm text-muted-foreground">{about[`${section}/${page}`]}</p>}
        </div>
        <div className="flex flex-wrap gap-1">
          {(res.pageActions ?? []).map((a) => <ActionButton key={a.label} act={a} row={null} onDone={reload} />)}
          <Button variant="ghost" size="sm" aria-label="Refresh" onClick={reload} disabled={list.isFetching}><RefreshCw className={list.isFetching ? 'animate-spin' : ''} /></Button>
          {canCreate && (res.newPath ? <Button size="sm" asChild><Link to={res.newPath}><Plus /> New</Link></Button> : <Button size="sm" onClick={startCreate}><Plus /> New</Button>)}
        </div>
      </div>
      {off ? (
        <Alert><AlertTitle>Feature is off</AlertTitle><AlertDescription>{off} — enable it in the node config (<Link to="/$section/$page" params={{ section: 'settings', page: 'config' }} className="underline">Settings → Config</Link>).</AlertDescription></Alert>
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-2">
            <Input value={search.q ?? ''} onChange={(e) => set({ q: e.target.value })} placeholder="Filter rows…" className="w-full sm:w-64" aria-label="Filter rows" />
            {(res.filters ?? []).map((f) => (
              <Select key={f.key} value={filters[f.key] || 'all'} onValueChange={(v) => { setStack([]); set({ [f.key]: v === (f.def ?? '') || v === 'all' ? undefined : v, cursor: undefined }) }}>
                <SelectTrigger size="sm" className="w-40" aria-label={f.label ?? f.key}><SelectValue placeholder={f.label ?? f.key} /></SelectTrigger>
                <SelectContent>{!f.options.includes('all') && <SelectItem value="all">{f.label ?? f.key}: all</SelectItem>}{f.options.map((o) => <SelectItem key={o} value={o}>{o}</SelectItem>)}</SelectContent>
              </Select>
            ))}
            <span className="ml-auto text-xs text-muted-foreground">{list.isPending ? '' : `${shown.length}${q ? ` of ${rows.length}` : ''} rows`}</span>
          </div>
          {list.isError && !off && <Alert variant="destructive"><AlertTitle>Could not load</AlertTitle><AlertDescription>{list.error.message}</AlertDescription></Alert>}
          <DataTable id={`${section}.${page}`} cols={cols} rows={shown} loading={list.isPending} rowTo={(r) => detailPath(section, res, r)}
            actions={res.actions?.length ? rowActions : undefined}
            empty={list.isError ? 'Nothing to show.' : q ? 'No rows match the filter.' : `No ${res.title.toLowerCase()} yet${canCreate ? ' — create one above.' : '.'}`} />
          {(next || stack.length > 0) && (
            <div className="flex justify-end gap-1">
              <Button variant="outline" size="sm" disabled={!stack.length} onClick={() => { const s = [...stack]; s.pop(); setStack(s); set({ cursor: s[s.length - 1] }) }}>Previous</Button>
              <Button variant="outline" size="sm" disabled={!next} onClick={() => { if (next) { setStack([...stack, next]); set({ cursor: next }) } }}>Next</Button>
            </div>
          )}
        </>
      )}
      <Dialog open={creating} onOpenChange={setCreating}>
        <DialogContent>
          <DialogHeader><DialogTitle>New {res.title.replace(/s$/, '').toLowerCase()}</DialogTitle><DialogDescription className="font-mono text-xs">{res.method === 'PUT' && res.item ? `PUT ${res.item}` : `POST ${res.list}`}</DialogDescription></DialogHeader>
          <Textarea value={text} onChange={(e) => setText(e.target.value)} rows={12} className="font-mono text-xs" />
          <DialogFooter><Button variant="ghost" onClick={() => setCreating(false)}>Cancel</Button><Button onClick={create}>Create</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
