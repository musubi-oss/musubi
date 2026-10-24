import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'

/** Personalised offer editor (`new` creates one): placement, delivered store item, price override, window, per-player limits, funnel stats. */
export const Route = createFileRoute('/_app/liveops/offers/$id')({ component: OfferPage })

interface Pr { currency: string; amount: string }
const toLocal = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().slice(0, 16) : '')
const fromLocal = (s: string) => (s ? Math.floor(new Date(s).getTime() / 1000) : null)
const pct = (a: unknown, b: unknown) => (Number(b) ? `${((Number(a) / Number(b)) * 100).toFixed(1)}%` : '—')
const BLANK: Row = { name: '', description: '', placement_id: '', store_item_id: '', priority: 0, enabled: true, audience: null, max_purchases: null, max_activations: null, cooldown_secs: 0, duration_secs: null, precursor: null, metadata: {} }

function OfferPage() {
  const { id } = Route.useParams()
  const isNew = id === 'new'
  const nav = useNavigate({ from: Route.fullPath })
  const O = `/v1/server/offers/${encodeURIComponent(id)}`
  const q = useQuery({ queryKey: ['offer', id], enabled: !isNew, queryFn: async ({ signal }) => { const r = await raw<Row>('GET', O, undefined, { signal }); return (r.offer ?? r) as Row } })
  const stats = useSub<Row>(isNew ? null : `${O}/stats`)
  const placements = listOf(useSub('/v1/server/placements').data, 'placements')
  const storeItems = listOf(useSub('/v1/server/store').data, 'items')
  const audiences = listOf(useSub('/v1/server/audiences').data, 'audiences').map((a) => String(a.name))
  const currencies = listOf(useSub('/v1/server/currencies').data, 'currencies')
  const offers = listOf(useSub('/v1/server/offers').data, 'offers').filter((x) => x.id !== id)
  const o = isNew ? BLANK : q.data
  const [f, setF] = useState<Row>(BLANK); const [prices, setPrices] = useState<Pr[]>([]); const [starts, setStarts] = useState(''); const [ends, setEnds] = useState(''); const [meta, setMeta] = useState('{}')
  useEffect(() => { if (!o) return; setF({ ...o }); setPrices(Object.entries((o.prices ?? {}) as Record<string, unknown>).map(([currency, amount]) => ({ currency, amount: String(amount) }))); setStarts(toLocal(o.starts_at)); setEnds(toLocal(o.ends_at)); setMeta(JSON.stringify(o.metadata ?? {}, null, 2)) }, [o])
  const set = (k: string, v: unknown) => setF((x) => ({ ...x, [k]: v }))
  const [dlg, setDlg] = useState(false); const [acc, setAcc] = useState('')
  const { run, pending } = useAct([['offer', id], ['sub']], () => setDlg(false))
  const item = storeItems.find((s) => s.id === f.store_item_id)
  const s = stats.data
  function save() {
    let metadata: unknown; try { metadata = JSON.parse(meta || '{}') } catch { return toast.error('Metadata is not valid JSON') }
    const b: Row = { ...f, prices: prices.length ? Object.fromEntries(prices.filter((p) => p.currency).map((p) => [p.currency, Number(p.amount)])) : null, starts_at: fromLocal(starts), ends_at: fromLocal(ends), metadata, audience: f.audience || null, precursor: f.precursor || null,
      priority: Number(f.priority ?? 0), max_purchases: f.max_purchases ? Number(f.max_purchases) : null, max_activations: f.max_activations ? Number(f.max_activations) : null, cooldown_secs: Number(f.cooldown_secs ?? 0), duration_secs: f.duration_secs ? Number(f.duration_secs) : null }
    run(async () => {
      if (isNew) { const r = await raw<Row>('POST', '/v1/server/offers', b); void nav({ to: '/liveops/offers/$id', params: { id: String(r.id ?? (r.offer as Row | undefined)?.id ?? '') }, replace: true }) }
      else await raw('PUT', O, b)
    }, 'Saved')
  }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'Offers', to: '/liveops/offers' }, { title: isNew ? 'new offer' : String(o?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New offer' : String(o?.name ?? id)}</h1><p className="mt-1 font-mono text-xs text-muted-foreground">{!isNew && id}</p></div>
        {!isNew && o && <>
          <Button size="sm" variant="outline" onClick={() => { setAcc(''); setDlg(true) }}>Force for player</Button>
          <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete this offer?', () => run(async () => { await raw('DELETE', O); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'offers' } }) }, 'Deleted'))}>Delete</Button>
        </>}
        <Button size="sm" disabled={!o || pending || !f.name} onClick={save}>{isNew ? 'Create' : 'Save'}</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {o && (
        <Card><CardContent className="pt-6">
          <KvGrid>
            <Kv k="Name" v={<Input value={String(f.name ?? '')} onChange={(x) => set('name', x.target.value)} />} />
            <Kv k="Description" v={<Input value={String(f.description ?? '')} onChange={(x) => set('description', x.target.value)} />} />
            <Kv k="Enabled" v={<label className="flex items-center gap-2"><Checkbox checked={!!f.enabled} onCheckedChange={(v) => set('enabled', v === true)} /> eligible players see it</label>} />
            <Kv k="Placement" v={<><Input className="font-mono" list="offer-placements" value={String(f.placement_id ?? '')} placeholder="main_menu, shop_banner…" onChange={(x) => set('placement_id', x.target.value)} /><datalist id="offer-placements">{placements.map((p) => <option key={String(p.id ?? p.name)} value={String(p.id ?? p.name)} />)}</datalist></>} />
            <Kv k="Store item delivered" v={<><Input className="font-mono" list="offer-items" value={String(f.store_item_id ?? '')} onChange={(x) => set('store_item_id', x.target.value)} /><datalist id="offer-items">{storeItems.map((i) => <option key={String(i.id)} value={String(i.id)}>{String(i.name ?? '')}</option>)}</datalist>{item && <p className="mt-1 text-xs text-muted-foreground">{String(item.name)} · store price {JSON.stringify(item.prices ?? item.price ?? {})}</p>}</>} />
            <Kv k="Priority" v={<Input type="number" value={String(f.priority ?? 0)} onChange={(x) => set('priority', x.target.value)} />} />
            <Kv k="Audience" v={<Select value={String(f.audience || '*')} onValueChange={(v) => set('audience', v === '*' ? null : v)}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="*">everyone</SelectItem>{audiences.map((a) => <SelectItem key={a} value={a}>{a}</SelectItem>)}</SelectContent></Select>} />
            <Kv k="Precursor offer" v={<Select value={String(f.precursor || '*')} onValueChange={(v) => set('precursor', v === '*' ? null : v)}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="*">none — shown directly</SelectItem>{offers.map((x) => <SelectItem key={String(x.id)} value={String(x.id)}>{String(x.name ?? x.id)}</SelectItem>)}</SelectContent></Select>} />
            <Kv k="Window" v={<div className="flex flex-wrap gap-2"><Input type="datetime-local" value={starts} onChange={(x) => setStarts(x.target.value)} aria-label="starts" /><Input type="datetime-local" value={ends} onChange={(x) => setEnds(x.target.value)} aria-label="ends" /></div>} />
            <Kv k="Per-player limits: purchases / activations" v={<div className="flex gap-2"><Input type="number" min={0} value={f.max_purchases ?? ''} placeholder="∞" onChange={(x) => set('max_purchases', x.target.value)} aria-label="max purchases" /><Input type="number" min={0} value={f.max_activations ?? ''} placeholder="∞" onChange={(x) => set('max_activations', x.target.value)} aria-label="max activations" /></div>} />
            <Kv k="Cooldown / visible for (s)" v={<div className="flex gap-2"><Input type="number" min={0} value={f.cooldown_secs ?? 0} onChange={(x) => set('cooldown_secs', x.target.value)} aria-label="cooldown" /><Input type="number" min={0} value={f.duration_secs ?? ''} placeholder="until window ends" onChange={(x) => set('duration_secs', x.target.value)} aria-label="duration" /></div>} />
          </KvGrid>
          <SubHead title="Offer price"><span className="self-center text-xs text-muted-foreground">{prices.length ? 'overrides the store item price' : 'empty = store item price'}</span><Button size="sm" variant="outline" onClick={() => setPrices((ps) => [...ps, { currency: '', amount: '0' }])}>Add price</Button></SubHead>
          <datalist id="offer-currencies">{currencies.map((c) => <option key={String(c.code ?? c.id)} value={String(c.code ?? c.id)} />)}</datalist>
          <Table><TableHeader><TableRow><TableHead className="w-64">Currency</TableHead><TableHead className="w-40">Amount</TableHead><TableHead className="w-10" /></TableRow></TableHeader><TableBody>
            {prices.map((p, i) => <TableRow key={i}><TableCell><Input className="h-8 font-mono" list="offer-currencies" value={p.currency} onChange={(x) => setPrices((ps) => ps.map((y, j) => (j === i ? { ...y, currency: x.target.value } : y)))} aria-label="currency" /></TableCell><TableCell><Input className="h-8" type="number" min={0} value={p.amount} onChange={(x) => setPrices((ps) => ps.map((y, j) => (j === i ? { ...y, amount: x.target.value } : y)))} aria-label="amount" /></TableCell><TableCell><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setPrices((ps) => ps.filter((_, j) => j !== i))} aria-label="remove">×</Button></TableCell></TableRow>)}
            {!prices.length && <TableRow><TableCell colSpan={3} className="py-4 text-center text-muted-foreground">Store item price applies</TableCell></TableRow>}
          </TableBody></Table>
          {s && <>
            <SubHead title="Funnel" />
            <KvGrid><Kv k="Seen / accounts" v={`${s.seen ?? 0} / ${s.accounts ?? 0}`} /><Kv k="Activations · active now" v={`${s.activations ?? 0} · ${s.active ?? 0}`} /><Kv k="Purchases" v={`${s.purchases ?? 0} (${pct(s.purchases, s.activations)} of activations)`} /><Kv k="Revenue" v={s.revenue ? JSON.stringify(s.revenue) : '—'} /><Kv k="Conversion seen → purchase" v={pct(s.purchases, s.seen)} /></KvGrid>
          </>}
          <SubHead title="Metadata (JSON)" /><Textarea className="min-h-20 font-mono text-xs" value={meta} onChange={(x) => setMeta(x.target.value)} aria-label="metadata" />
          {!isNew && <><SubHead title="Raw" /><JsonView value={o} /></>}
        </CardContent></Card>
      )}
      <FormDialog open={dlg} onOpenChange={setDlg} title="Force offer for a player" description="Activates the offer for this account regardless of audience, window or limits — for support and QA." submit="Force" pending={pending} disabled={!acc}
        onSubmit={() => run(() => raw('POST', `${O}/force`, { account_id: acc.trim() }), 'Offer forced')}>
        <Field label="Account id"><Input className="font-mono" value={acc} onChange={(e) => setAcc(e.target.value)} /></Field>
      </FormDialog>
    </div>
  )
}
