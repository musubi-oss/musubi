import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Player stat definitions — every stat is also a leaderboard unless private; cycles add daily / weekly / monthly / season boards. Set or bump a player's value from here. */
export const Route = createFileRoute('/_app/social/stats')({
  validateSearch: (s: Record<string, unknown>): { q?: string; top?: string } => ({ q: typeof s.q === 'string' ? s.q : undefined, top: typeof s.top === 'string' ? s.top : undefined }),
  component: Stats,
})

const KINDS: [string, string][] = [['int', 'int — counter / score'], ['float', 'float'], ['max', 'max — keeps the highest value'], ['min', 'min — keeps the lowest'], ['avgrate', 'avgrate — rolling average per window'], ['set', 'set — last written value']]
const DEF = (): Row => ({ id: '', name: '', kind: 'int', global: false, visibility: 'public', increment_only: false, client_writable: false, default_value: 0, min_value: null, max_value: null, max_change: null, window_secs: null, metadata: {} })

function Stats() {
  const { q = '', top = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const defs = useSub<Row>('/v1/server/stats/defs')
  const rows = listOf(defs.data, 'defs').filter((d) => !q || `${d.id} ${d.name ?? ''}`.toLowerCase().includes(q.toLowerCase()))
  const board = useQuery({ queryKey: ['stat-top', top], enabled: !!top, retry: false, queryFn: async ({ signal }) => { try { return await raw<Row>('GET', `/v1/server/leaderboards/${encodeURIComponent(top)}?limit=20`, undefined, { signal }) } catch { return raw<Row>('GET', `/v1/server/stats/top?stat=${encodeURIComponent(top)}&limit=20`, undefined, { signal }) } } })
  const [dlg, setDlg] = useState<'' | 'edit' | 'upd'>(''); const [isNew, setIsNew] = useState(true); const [d, setD] = useState<Row>(DEF()); const [cycles, setCycles] = useState<{ name: string; period: string }[]>([]); const [meta, setMeta] = useState('{}')
  const [upd, setUpd] = useState({ account_id: '', stat: '', op: 'set', value: 0 })
  const { run, pending } = useAct([['sub'], ['stat-top']], () => setDlg(''))
  const set = (k: string, v: unknown) => setD((x) => ({ ...x, [k]: v }))
  const num = (v: unknown) => (v == null || v === '' ? null : Number(v))
  const edit = (x?: Row) => { setIsNew(!x); setD(x ? { ...DEF(), ...x } : DEF()); setCycles(((x?.cycles ?? []) as Row[]).map((c) => ({ name: String(c.name), period: String(c.period) }))); setMeta(JSON.stringify(x?.metadata ?? {}, null, 2)); setDlg('edit') }
  const N = ({ k, label }: { k: string; label: string }) => <Field label={label}><Input type="number" value={d[k] == null ? '' : String(d[k])} onChange={(e) => set(k, num(e.target.value))} /></Field>
  const B = ({ k, label }: { k: string; label: string }) => <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!d[k]} onCheckedChange={(v) => set(k, !!v)} /> {label}</label>
  const entries = (listOf(board.data, 'records').length ? listOf(board.data, 'records') : listOf(board.data, 'top')).map((r, i) => ({ rank: i + 1, ...r }))
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Social', to: '/social' }, { title: 'Stats & leaderboards' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Stats & leaderboards</h1><p className="mt-1 text-sm text-muted-foreground">{rows.length} definitions · every public / friends stat ranks players; cycles add periodic boards</p></div>
        <Button size="sm" variant="outline" onClick={() => { setUpd({ account_id: '', stat: String(rows[0]?.id ?? ''), op: 'set', value: 0 }); setDlg('upd') }}>Update a player</Button>
        <Button size="sm" onClick={() => edit()}>New stat</Button>
      </div>
      {defs.isError && <Alert variant="destructive"><AlertDescription>{String(defs.error)}</AlertDescription></Alert>}
      <div className="grid gap-4 xl:grid-cols-[3fr_2fr]">
        <Card className="min-w-0"><CardContent className="pt-6">
          <Input className="mb-3 h-8 max-w-xs" value={q} placeholder="filter by id or name" onChange={(e) => void nav({ search: (p) => ({ ...p, q: e.target.value || undefined }), replace: true })} />
          <DataTable id="stat-defs" rows={rows} loading={defs.isPending} empty={q ? 'Nothing matches' : 'No stats defined — servers cannot record scores until one exists'}
            cols={[{ key: 'id', mono: true, fmt: (x) => <span>{String(x.id)}{x.name ? <span className="block text-xs text-muted-foreground">{String(x.name)}</span> : null}</span> }, { key: 'kind' }, { key: 'scope', fmt: (x) => (x.global ? 'global' : 'per player') }, { key: 'visibility' }, { key: 'flags', dim: true, fmt: (x) => [x.increment_only ? 'incr-only' : null, x.client_writable ? 'client-writable' : null, ((x.cycles ?? []) as unknown[]).length ? `${(x.cycles as unknown[]).length} cycle(s)` : null].filter(Boolean).join(', ') || '—' }, { key: 'bounds', dim: true, fmt: (x) => (x.min_value != null || x.max_value != null || x.max_change != null ? `${x.min_value ?? '−∞'}…${x.max_value ?? '∞'}${x.max_change != null ? ` Δ≤${x.max_change}` : ''}` : '—') }]}
            actions={(x) => <span className="flex justify-end gap-1"><Button size="sm" variant={top === x.id ? 'default' : 'ghost'} disabled={x.visibility === 'private'} onClick={() => void nav({ search: (p) => ({ ...p, top: String(x.id) }), replace: true })}>Top 20</Button><Button size="sm" variant="ghost" onClick={() => edit(x)}>Edit</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Delete stat ${x.id} and its values?`, () => run(() => raw('DELETE', `/v1/server/stats/defs/${encodeURIComponent(String(x.id))}`), 'Deleted'))}>×</Button></span>} />
        </CardContent></Card>
        <Card className="min-w-0"><CardContent className="pt-6">
          {top ? (<>
            <SubHead title={`Top 20 — ${top}`}><Button size="sm" variant="ghost" asChild><Link to="/$section/$page/$id" params={{ section: 'social', page: 'leaderboards', id: top }} search={{ tab: undefined }}>Full board</Link></Button></SubHead>
            {board.isError ? <Alert variant="destructive"><AlertDescription>{String(board.error)}</AlertDescription></Alert> : entries.length ? <DataTable id="stat-top" rows={entries} loading={board.isPending} empty="" rowTo={(r) => (r.owner_id ?? r.account_id ? `/players/accounts/${r.owner_id ?? r.account_id}` : null)} cols={[{ key: 'rank' }, { key: 'owner_id', mono: true, fmt: (r) => r.owner_id ?? r.account_id }, { key: 'username' }, { key: 'score', fmt: (r) => r.score ?? r.value }]} /> : board.isPending ? <p className="text-sm text-muted-foreground">Loading…</p> : <><p className="text-sm text-muted-foreground">Nobody has a value yet.</p><JsonView value={board.data} /></>}
          </>) : <p className="flex min-h-24 items-center justify-center text-sm text-muted-foreground">Pick «Top 20» on a stat to see its leaderboard</p>}
        </CardContent></Card>
      </div>
      <FormDialog open={dlg === 'edit'} onOpenChange={(o) => !o && setDlg('')} title={isNew ? 'New stat' : `Stat ${d.id}`} submit="Save" pending={pending} disabled={!d.id} wide
        onSubmit={() => { let metadata: unknown; try { metadata = JSON.parse(meta || '{}') } catch { return toast.error('Metadata must be valid JSON') }; run(() => raw('PUT', '/v1/server/stats/defs', { ...d, metadata, cycles: cycles.filter((c) => c.name) }), 'Saved') }}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Id"><Input className="font-mono" value={String(d.id)} disabled={!isNew} placeholder="kills" onChange={(e) => set('id', e.target.value)} /></Field>
          <Field label="Display name"><Input value={String(d.name ?? '')} onChange={(e) => set('name', e.target.value)} /></Field>
          <Field label="Kind"><Select value={String(d.kind)} onValueChange={(v) => set('kind', v)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{KINDS.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Visibility"><Select value={String(d.visibility)} onValueChange={(v) => set('visibility', v)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="public">public — ranked, visible to all</SelectItem><SelectItem value="friends">friends — ranked among friends</SelectItem><SelectItem value="private">private — owner only, no board</SelectItem></SelectContent></Select></Field>
        </div>
        <div className="flex flex-wrap gap-4"><B k="global" label="Global (one value for the whole game)" /><B k="increment_only" label="Increment only" /><B k="client_writable" label="Clients may write it" /></div>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-5"><N k="default_value" label="Default" /><N k="min_value" label="Min" /><N k="max_value" label="Max" /><N k="max_change" label="Max change per write" />{d.kind === 'avgrate' && <N k="window_secs" label="Window (s)" />}</div>
        <SubHead title="Cycles (periodic boards)"><Button type="button" size="sm" variant="outline" onClick={() => setCycles((c) => [...c, { name: 'weekly', period: 'weekly' }])}>Add</Button></SubHead>
        {cycles.map((c, i) => <div key={i} className="flex gap-2"><Input className="h-8 w-40 font-mono" value={c.name} onChange={(e) => setCycles((cs) => cs.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))} /><Select value={c.period} onValueChange={(v) => setCycles((cs) => cs.map((x, j) => (j === i ? { ...x, period: v } : x)))}><SelectTrigger className="h-8 w-32"><SelectValue /></SelectTrigger><SelectContent>{['daily', 'weekly', 'monthly', 'season'].map((p) => <SelectItem key={p} value={p}>{p}</SelectItem>)}</SelectContent></Select><Button type="button" size="sm" variant="ghost" onClick={() => setCycles((cs) => cs.filter((_, j) => j !== i))}>×</Button></div>)}
        <Field label="Metadata (JSON)"><Textarea className="min-h-16 font-mono text-xs" value={meta} onChange={(e) => setMeta(e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'upd'} onOpenChange={(o) => !o && setDlg('')} title="Update a player's stat" submit="Apply" pending={pending} disabled={!upd.account_id || !upd.stat}
        onSubmit={() => run(() => raw('POST', '/v1/server/stats/update', { account_id: upd.account_id, stat: upd.stat, op: upd.op, value: Number(upd.value) }), 'Updated')}>
        <Field label="Account id"><Input className="font-mono" value={upd.account_id} onChange={(e) => setUpd((u) => ({ ...u, account_id: e.target.value }))} /></Field>
        <div className="grid grid-cols-3 gap-3">
          <Field label="Stat"><Select value={upd.stat} onValueChange={(v) => setUpd((u) => ({ ...u, stat: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{rows.map((x) => <SelectItem key={String(x.id)} value={String(x.id)}>{String(x.id)}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Operation"><Select value={upd.op} onValueChange={(v) => setUpd((u) => ({ ...u, op: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="set">set</SelectItem><SelectItem value="incr">increment</SelectItem><SelectItem value="max">max</SelectItem><SelectItem value="min">min</SelectItem></SelectContent></Select></Field>
          <Field label="Value"><Input type="number" value={upd.value} onChange={(e) => setUpd((u) => ({ ...u, value: Number(e.target.value) }))} /></Field>
        </div>
      </FormDialog>
    </div>
  )
}
