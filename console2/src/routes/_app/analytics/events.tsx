import { useState } from 'react'
import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Field, FormDialog } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'

/** Raw event explorer (filter by name, account, platform) plus the schemas that validate events on ingest and the list of events exempt from sampling. */
export const Route = createFileRoute('/_app/analytics/events')({
  validateSearch: (s: Record<string, unknown>): { name?: string; account_id?: string; platform?: string; limit?: number } => ({
    name: typeof s.name === 'string' && s.name ? s.name : undefined, account_id: typeof s.account_id === 'string' && s.account_id ? s.account_id : undefined,
    platform: typeof s.platform === 'string' && s.platform ? s.platform : undefined, limit: Number(s.limit) > 0 && Number(s.limit) !== 200 ? Number(s.limit) : undefined,
  }),
  component: Events,
})

const A = '/v1/server/analytics'
const ts = (v: unknown) => { const n = Number(v); return v ? (Number.isFinite(n) && n > 0 ? new Date(n * (n < 1e12 ? 1000 : 1)) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 19) : '—' }
const DEFAULT_SCHEMA = { properties: { level: { type: 'integer' } }, required: ['level'], strict: false }

function Events() {
  const s = Route.useSearch()
  const { limit = 200 } = s
  const nav = useNavigate({ from: Route.fullPath })
  const qs = new URLSearchParams({ limit: String(limit), ...(s.name ? { name: s.name } : {}), ...(s.account_id ? { account_id: s.account_id } : {}), ...(s.platform ? { platform: s.platform } : {}) })
  const eq = useSub<Row>(`${A}/events?${qs}`)
  const events = listOf(eq.data, 'events')
  const sq = useSub<Row>(`${A}/schemas`)
  const schemas = listOf(sq.data, 'schemas')
  const samp = useSub<Row>(`${A}/sampling`)
  const always = (samp.data?.always_enabled as string[] | undefined) ?? []
  const [f, setF] = useState<{ name: string; account_id: string; platform: string }>({ name: s.name ?? '', account_id: s.account_id ?? '', platform: s.platform ?? '' })
  const [dlg, setDlg] = useState('')
  const [form, setForm] = useState<{ name: string; text: string; existing: boolean }>({ name: '', text: '', existing: false })
  const [alwaysTxt, setAlwaysTxt] = useState<string | null>(null)
  const [sel, setSel] = useState<Row | null>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  let schemaErr = ''
  try { JSON.parse(form.text || '{}') } catch (e) { schemaErr = (e as Error).message }
  const openSchema = (x?: Row) => { setForm({ name: String(x?.name ?? ''), text: JSON.stringify(x ? Object.fromEntries(Object.entries(x).filter(([k]) => k !== 'name')) : DEFAULT_SCHEMA, null, 2), existing: !!x }); setDlg('schema') }
  const saveSchema = () => { const body = JSON.parse(form.text || '{}') as Row; run(() => (form.existing ? raw('PUT', `${A}/schemas/${encodeURIComponent(form.name)}`, body) : raw('POST', `${A}/schemas`, { name: form.name, ...body })), 'Schema saved') }
  const apply = () => void nav({ search: (p) => ({ ...p, name: f.name || undefined, account_id: f.account_id || undefined, platform: f.platform || undefined }), replace: true })
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Analytics', to: '/analytics' }, { title: 'Events' }]} />
      <div><h1 className="text-xl font-semibold">Events</h1><p className="mt-1 text-sm text-muted-foreground">Newest events as the node stored them. Filters are in the URL, so a view can be shared. A schema makes ingest reject events of that name whose properties don't match; <i>strict</i> also rejects unknown properties.</p></div>
      <form className="flex flex-wrap gap-2" onSubmit={(e) => { e.preventDefault(); apply() }}>
        <Input className="w-44 font-mono" placeholder="event name" value={f.name} onChange={(e) => setF((x) => ({ ...x, name: e.target.value }))} list="ev-names" /><datalist id="ev-names">{schemas.map((x) => <option key={String(x.name)} value={String(x.name)} />)}</datalist>
        <Input className="w-64 font-mono" placeholder="account id" value={f.account_id} onChange={(e) => setF((x) => ({ ...x, account_id: e.target.value }))} />
        <Input className="w-32" placeholder="platform" value={f.platform} onChange={(e) => setF((x) => ({ ...x, platform: e.target.value }))} />
        <Button type="submit" size="sm">Filter</Button>{(s.name || s.account_id || s.platform) && <Button type="button" size="sm" variant="ghost" onClick={() => { setF({ name: '', account_id: '', platform: '' }); void nav({ search: { limit: s.limit }, replace: true }) }}>Clear</Button>}
      </form>
      {eq.isError && <Alert variant="destructive"><AlertDescription>{String(eq.error)}</AlertDescription></Alert>}
      <Card><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>At</TableHead><TableHead>Event</TableHead><TableHead>Account</TableHead><TableHead>Platform</TableHead><TableHead>Properties</TableHead></TableRow></TableHeader>
          <TableBody>{eq.isPending ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">Loading…</TableCell></TableRow> : !events.length ? <TableRow><TableCell colSpan={5} className="text-muted-foreground">No events match. Events are buffered on the node for a few seconds before they appear — use Flush on the overview to force it.</TableCell></TableRow> :
            events.map((e, i) => <TableRow key={String(e.id ?? i)} className="cursor-pointer" onClick={() => setSel(e)}><TableCell className="whitespace-nowrap font-mono text-xs text-muted-foreground">{ts(e.at ?? e.ts ?? e.created_at)}</TableCell><TableCell><button className="font-mono text-xs underline" onClick={(ev) => { ev.stopPropagation(); setF((x) => ({ ...x, name: String(e.name) })); void nav({ search: (p) => ({ ...p, name: String(e.name) }), replace: true }) }}>{String(e.name)}</button></TableCell><TableCell onClick={(ev) => ev.stopPropagation()}>{e.account_id ? <Link className="font-mono text-xs underline" to="/players/accounts/$id" params={{ id: String(e.account_id) }}>{String(e.account_id).slice(0, 8)}…</Link> : '—'}</TableCell><TableCell className="text-xs">{String(e.platform ?? '—')}</TableCell><TableCell className="max-w-[24rem] truncate font-mono text-xs text-muted-foreground">{JSON.stringify(e.properties ?? e.props ?? {})}</TableCell></TableRow>)}</TableBody></Table>
        {events.length >= limit && <div className="border-t p-2 text-right"><Button size="sm" variant="ghost" onClick={() => void nav({ search: (p) => ({ ...p, limit: limit * 2 }), replace: true })}>Show more</Button></div>}
      </CardContent></Card>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-[1fr_20rem]">
        <Card><CardHeader className="flex-row items-center justify-between pb-2"><CardTitle className="text-base">Event schemas</CardTitle><Button size="sm" variant="outline" onClick={() => openSchema()}>New schema</Button></CardHeader><CardContent className="p-0">
          <Table><TableHeader><TableRow><TableHead>Event</TableHead><TableHead>Properties</TableHead><TableHead>Required</TableHead><TableHead /></TableRow></TableHeader>
            <TableBody>{!schemas.length ? <TableRow><TableCell colSpan={4} className="text-muted-foreground">{sq.isPending ? 'Loading…' : 'No schemas — every event is accepted as sent.'}</TableCell></TableRow> : schemas.map((x) => <TableRow key={String(x.name)}><TableCell><button className="font-mono text-xs underline" onClick={() => openSchema(x)}>{String(x.name)}</button>{x.strict ? <Badge variant="outline" className="ml-2">strict</Badge> : null}</TableCell><TableCell className="max-w-[18rem] truncate text-xs text-muted-foreground">{Object.keys((x.properties as Row | undefined) ?? {}).join(', ')}</TableCell><TableCell className="text-xs">{((x.required as string[] | undefined) ?? []).join(', ') || '—'}</TableCell><TableCell className="text-right"><Button size="sm" variant="ghost" disabled={pending} onClick={() => confirmThen(`Delete schema for ${String(x.name)}? Events of that name will be accepted unvalidated.`, () => run(() => raw('DELETE', `${A}/schemas/${encodeURIComponent(String(x.name))}`), 'Schema deleted'))}>Delete</Button></TableCell></TableRow>)}</TableBody></Table>
        </CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Never sampled</CardTitle><p className="text-sm text-muted-foreground">Event names kept at 100% regardless of the global sampling rate (purchases, logins…).</p></CardHeader><CardContent>
          <form className="space-y-2" onSubmit={(e) => { e.preventDefault(); run(() => raw('PUT', `${A}/sampling`, { global_pct: Number(samp.data?.global_pct ?? 100), always_enabled: (alwaysTxt ?? always.join('\n')).split(/[\n,]/).map((x) => x.trim()).filter(Boolean) }), 'Sampling exemptions saved') }}>
            <Textarea rows={5} className="font-mono text-xs" value={alwaysTxt ?? always.join('\n')} onChange={(e) => setAlwaysTxt(e.target.value)} placeholder={'purchase\nlogin'} /><Button type="submit" size="sm" variant="outline" disabled={pending}>Save</Button>
          </form>
        </CardContent></Card>
      </div>
      <FormDialog open={dlg === 'schema'} onOpenChange={(o) => !o && setDlg('')} title={form.existing ? `Schema · ${form.name}` : 'New event schema'} pending={pending} disabled={!form.name || !!schemaErr} wide onSubmit={saveSchema}>
        <Field label="Event name"><Input className="font-mono" value={form.name} disabled={form.existing} onChange={(e) => setForm((x) => ({ ...x, name: e.target.value }))} /></Field>
        <Field label="Schema" hint={schemaErr || 'properties: {name: {type}}, required: [...], strict: bool'}><Textarea rows={12} className="font-mono text-xs" value={form.text} onChange={(e) => setForm((x) => ({ ...x, text: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={!!sel} onOpenChange={(o) => !o && setSel(null)} title={`Event · ${String(sel?.name ?? '')}`} submit="Close" onSubmit={() => setSel(null)} wide>{sel && <JsonView value={sel} />}</FormDialog>
    </div>
  )
}
