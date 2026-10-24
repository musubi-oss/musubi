import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Field } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Store listing: what is sold (item or bundle), prices per currency, discount, availability window, audience/platform/level gating, rotation. `new` opens the create form. */
export const Route = createFileRoute('/_app/economy/store/$id')({ component: StoreItem })

type P = { currency: string; amount: number | string }
const toIso = (v: unknown) => (v ? new Date(typeof v === 'number' ? v * (v < 1e12 ? 1000 : 1) : String(v)).toISOString().slice(0, 16) : '')
const fromIso = (x: string) => (x ? Math.floor(new Date(x).getTime() / 1000) : null)
const jsonOk = (s: string) => { try { JSON.parse(s || '[]'); return true } catch { return false } }
const DEF: Row = { id: '', item_id: '', quantity: 1, enabled: true, discount_percent: 0, max_per_account: '', min_level: '', level_track: '', rotation: '', rotation_slots: 0 }

function StoreItem() {
  const { id } = Route.useParams()
  const isNew = id === 'new'
  const nav = useNavigate()
  const all = useSub<Row>('/v1/server/store')
  const items = listOf(useSub<Row>('/v1/server/items').data, 'items').map((x) => String(x.id))
  const currencies = listOf(useSub<Row>('/v1/server/currencies').data, 'currencies').map((x) => String(x.code))
  const audiences = listOf(useSub<Row>('/v1/server/audiences').data, 'audiences').map((x) => String(x.name))
  const loaded = isNew ? undefined : listOf(all.data, 'items').find((x) => String(x.id) === id)
  const [d, setD] = useState<{ key: string; s: Row; prices: P[]; bundle: string; aud: string[]; plat: string; from: string; until: string; discUntil: string } | null>(null)
  const key = `${id}:${all.dataUpdatedAt}`
  if (!d || (d.key !== key && (isNew || loaded))) {
    const s: Row = isNew ? { ...DEF } : { ...DEF, ...loaded }
    setD({ key, s, prices: Object.entries((s.prices ?? {}) as Record<string, unknown>).map(([currency, a]) => ({ currency, amount: Number(a) })), bundle: JSON.stringify(s.bundle ?? []), aud: [...((s.audiences ?? []) as string[])], plat: ((s.platforms ?? []) as string[]).join(', '), from: toIso(s.available_from), until: toIso(s.available_until), discUntil: toIso(s.discount_until) })
  }
  const s = d?.s ?? DEF
  const set = (p: Row) => setD((x) => (x ? { ...x, s: { ...x.s, ...p } } : x))
  const setX = (p: Partial<NonNullable<typeof d>>) => setD((x) => (x ? { ...x, ...p } : x))
  const bind = (k: string) => (e: React.ChangeEvent<HTMLInputElement>) => set({ [k]: e.target.value })
  const { run, pending } = useAct([['sub']])
  const bundleOk = jsonOk(d?.bundle ?? '[]')
  const num = (v: unknown) => (v === '' || v == null ? null : Number(v))
  const save = () => run(async () => {
    const b = { ...s, id: s.id || undefined, prices: Object.fromEntries((d?.prices ?? []).filter((p) => p.currency).map((p) => [p.currency, Number(p.amount)])), bundle: JSON.parse(d?.bundle || '[]'), audiences: d?.aud ?? [], platforms: (d?.plat ?? '').split(',').map((x) => x.trim()).filter(Boolean), available_from: fromIso(d?.from ?? ''), available_until: fromIso(d?.until ?? ''), discount_until: fromIso(d?.discUntil ?? ''), quantity: Number(s.quantity) || 1, discount_percent: Number(s.discount_percent) || 0, max_per_account: num(s.max_per_account), min_level: num(s.min_level), level_track: s.level_track || null, rotation: s.rotation || null, rotation_slots: Number(s.rotation_slots) || 0 }
    const r = (await raw<Row>('POST', '/v1/server/store', b)) as Row
    if (isNew) void nav({ to: '/economy/store/$id', params: { id: String(r.id ?? (r.item as Row | undefined)?.id ?? b.id) } })
  }, 'Listing saved')
  const live = (() => { const n = Date.now() / 1000; const f = Number(s.available_from), u = Number(s.available_until); return !s.enabled ? 'disabled' : f && n < f ? 'scheduled' : u && n > u ? 'expired' : 'live' })()
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Store', to: '/economy/store' }, { title: isNew ? 'New listing' : String(loaded?.name ?? loaded?.item_id ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New store listing' : String(loaded?.name ?? loaded?.item_id ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">{!isNew && <><span className="font-mono">{id}</span><Badge variant={live === 'live' ? 'default' : 'secondary'}>{live}</Badge></>}<span>Purchase is <code>POST /v1/store/buy</code>: price is debited, item or bundle granted, coupons and discounts applied, receipts for IAP verified separately.</span></div></div>
      </div>
      {all.isError && <Alert variant="destructive"><AlertDescription>{String(all.error)}</AlertDescription></Alert>}
      {!isNew && all.isSuccess && !loaded && <Alert variant="destructive"><AlertDescription>Listing {id} not found</AlertDescription></Alert>}
      <Card><CardContent className="space-y-3 pt-6">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-4">
          <Field label="Id"><Input className="font-mono" value={String(s.id ?? '')} disabled={!isNew} placeholder="starter_pack" onChange={bind('id')} /></Field>
          <Field label="Name"><Input value={String(s.name ?? '')} onChange={bind('name')} /></Field>
          <Field label="Item id (empty for bundle)"><Input className="font-mono" list="st-items" value={String(s.item_id ?? '')} onChange={bind('item_id')} /></Field>
          <Field label="Quantity"><Input type="number" min={1} value={String(s.quantity ?? '')} onChange={bind('quantity')} /></Field>
        </div>
        <Field label={bundleOk ? 'Bundle (JSON: [{item_id, quantity}] or [{currency, amount}])' : 'Bundle — invalid JSON'}><Textarea className={`min-h-9 font-mono text-xs ${bundleOk ? '' : 'border-destructive'}`} value={d?.bundle ?? ''} onChange={(e) => setX({ bundle: e.target.value })} /></Field>
        <Field label={`Prices (${(d?.prices ?? []).length})`}>
          <div className="space-y-2">
            {(d?.prices ?? []).map((p, i) => <div key={i} className="flex min-w-0 gap-2"><Input className="min-w-0 flex-1 font-mono sm:flex-none sm:w-40" list="st-cur" placeholder="currency" value={p.currency} onChange={(e) => setX({ prices: d!.prices.map((q, j) => (j === i ? { ...q, currency: e.target.value } : q)) })} /><Input className="w-24 shrink-0 sm:w-32" type="number" min={0} value={String(p.amount)} onChange={(e) => setX({ prices: d!.prices.map((q, j) => (j === i ? { ...q, amount: e.target.value } : q)) })} /><Button size="sm" variant="ghost" onClick={() => setX({ prices: d!.prices.filter((_, j) => j !== i) })}>×</Button></div>)}
            <Button size="sm" variant="outline" onClick={() => setX({ prices: [...(d?.prices ?? []), { currency: currencies[0] ?? '', amount: 100 }] })}>+ price</Button>
          </div>
        </Field>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <Field label="Discount %"><Input type="number" min={0} max={100} value={String(s.discount_percent ?? 0)} onChange={bind('discount_percent')} /></Field>
          <Field label="Discount until"><Input type="datetime-local" value={d?.discUntil ?? ''} onChange={(e) => setX({ discUntil: e.target.value })} /></Field>
          <Field label="Max per account (empty = unlimited)"><Input type="number" min={1} value={String(s.max_per_account ?? '')} onChange={bind('max_per_account')} /></Field>
          <Field label="Available from"><Input type="datetime-local" value={d?.from ?? ''} onChange={(e) => setX({ from: e.target.value })} /></Field>
          <Field label="Available until"><Input type="datetime-local" value={d?.until ?? ''} onChange={(e) => setX({ until: e.target.value })} /></Field>
          <Field label="Platforms (csv, empty = all)"><Input className="font-mono" placeholder="steam, ios" value={d?.plat ?? ''} onChange={(e) => setX({ plat: e.target.value })} /></Field>
          <Field label="Min level"><Input type="number" min={0} value={String(s.min_level ?? '')} onChange={bind('min_level')} /></Field>
          <Field label="…on progression track"><Input className="font-mono" value={String(s.level_track ?? '')} onChange={bind('level_track')} /></Field>
          <Field label="Rotation"><Select value={String(s.rotation || 'none')} onValueChange={(v) => set({ rotation: v === 'none' ? '' : v })}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{['none', 'daily', 'weekly'].map((k) => <SelectItem key={k} value={k}>{k}</SelectItem>)}</SelectContent></Select></Field>
          {s.rotation ? <Field label="Rotation slots"><Input type="number" min={0} value={String(s.rotation_slots ?? 0)} onChange={bind('rotation_slots')} /></Field> : null}
        </div>
        <Field label="Audiences (empty = everyone)">
          <div className="flex flex-wrap gap-2">{audiences.length ? audiences.map((a) => <label key={a} className="flex items-center gap-1.5 rounded-md border px-2 py-1 text-sm"><Checkbox checked={(d?.aud ?? []).includes(a)} onCheckedChange={(v) => setX({ aud: v ? [...(d?.aud ?? []), a] : (d?.aud ?? []).filter((x) => x !== a) })} /> {a}</label>) : <span className="text-xs text-muted-foreground">No audiences defined yet (LiveOps → Audiences).</span>}</div>
        </Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!s.enabled} onCheckedChange={(v) => set({ enabled: !!v })} /> enabled</label>
        <Button size="sm" disabled={pending || !s.id || !bundleOk || !(s.item_id || (d?.bundle ?? '[]') !== '[]')} onClick={save}>{isNew ? 'Create' : 'Save'}</Button>
      </CardContent></Card>
      <datalist id="st-items">{items.map((x) => <option key={x} value={x} />)}</datalist>
      <datalist id="st-cur">{currencies.map((x) => <option key={x} value={x} />)}</datalist>
    </div>
  )
}
