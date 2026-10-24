import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Reward table editor: weighted/fixed entries of items, currencies or nested tables, pity (guaranteed rarity after N rolls), deferred delivery. `new` opens the create form. */
export const Route = createFileRoute('/_app/economy/reward_tables/$id')({ component: RewardTable })

type E = { kind: 'item' | 'currency' | 'table'; ref: string; amount: number | string; rarity: string; weight: number | string }
type D = { t: Row; entries: E[]; deferred: string }
const RAR = ['common', 'uncommon', 'rare', 'epic', 'legendary']
const jsonOk = (s: string) => { if (!s.trim()) return true; try { JSON.parse(s); return true } catch { return false } }

function RewardTable() {
  const { id } = Route.useParams()
  const isNew = id === 'new'
  const nav = useNavigate()
  const all = useSub<Row>('/v1/server/rewards/tables')
  const items = listOf(useSub<Row>('/v1/server/items').data, 'items').map((x) => String(x.id))
  const currencies = listOf(useSub<Row>('/v1/server/currencies').data, 'currencies').map((x) => String(x.code))
  const loaded = isNew ? undefined : listOf(all.data, 'tables').find((x) => String(x.id) === id)
  const [d, setD] = useState<D & { key: string } | null>(null)
  const key = `${id}:${all.dataUpdatedAt}`
  if (!d || (d.key !== key && (isNew || loaded))) {
    const t: Row = isNew ? { id: '', name: '', kind: 'weighted', enabled: true, pity: { after: 0, rarity: '' } } : { ...loaded, pity: (loaded?.pity as Row | undefined) ?? { after: 0, rarity: '' } }
    setD({ key, t, entries: ((t.entries ?? []) as Row[]).map((e) => ({ kind: e.item_id ? 'item' : e.currency ? 'currency' : 'table', ref: String(e.item_id ?? e.currency ?? e.table_id ?? ''), amount: Number(e.amount ?? 1), rarity: String(e.rarity ?? 'common'), weight: Number(e.weight ?? 1) })), deferred: t.deferred ? JSON.stringify(t.deferred) : '' })
  }
  const t = d?.t ?? {}; const entries = d?.entries ?? []
  const setT = (p: Row) => setD((x) => (x ? { ...x, t: { ...x.t, ...p } } : x))
  const setE = (i: number, p: Partial<E>) => setD((x) => (x ? { ...x, entries: x.entries.map((e, j) => (j === i ? { ...e, ...p } : e)) } : x))
  const total = entries.reduce((s, e) => s + (Number(e.weight) || 0), 0)
  const pity = (t.pity ?? {}) as Row
  const [dlg, setDlg] = useState(false)
  const [g, setG] = useState<Row>({ account_id: '', reason: 'console' })
  const [granted, setGranted] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']])
  const defOk = jsonOk(d?.deferred ?? '')
  const save = () => run(async () => {
    const b = { id: t.id || undefined, name: t.name, kind: t.kind, enabled: t.enabled, pity: Number(pity.after) ? { after: Number(pity.after), rarity: pity.rarity } : null, deferred: d?.deferred.trim() ? JSON.parse(d.deferred) : null,
      entries: entries.filter((e) => e.ref).map((e) => ({ item_id: e.kind === 'item' ? e.ref : null, currency: e.kind === 'currency' ? e.ref : null, table_id: e.kind === 'table' ? e.ref : undefined, amount: Number(e.amount), rarity: e.rarity, weight: Number(e.weight) })) }
    const r = (await raw<Row>('POST', '/v1/server/rewards/tables', b)) as Row
    if (isNew) void nav({ to: '/economy/reward_tables/$id', params: { id: String(r.id ?? (r.table as Row | undefined)?.id ?? b.id) } })
  }, 'Reward table saved')
  const notFound = !isNew && all.isSuccess && !loaded
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Reward tables', to: '/economy/reward_tables' }, { title: isNew ? 'New table' : String(loaded?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New reward table' : String(loaded?.name ?? id)}</h1><p className="mt-1 text-sm text-muted-foreground">{isNew ? 'A table is rolled by quests, codes, mail, seasons and store bundles — define it once, reference it by id everywhere.' : `${entries.length} entries · ${t.kind === 'weighted' ? `one roll by weight (total ${total})` : 'every entry granted'}${Number(pity.after) ? ` · pity: ${pity.rarity} after ${pity.after}` : ''}`}</p></div>
        {!isNew && <Button size="sm" variant="outline" onClick={() => { setGranted(null); setDlg(true) }}>Grant to a player</Button>}
      </div>
      {all.isError && <Alert variant="destructive"><AlertDescription>{String(all.error)}</AlertDescription></Alert>}
      {notFound && <Alert variant="destructive"><AlertDescription>Table {id} not found</AlertDescription></Alert>}
      <Card><CardContent className="space-y-3 pt-6">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-4">
          <Field label="Id"><Input className="font-mono" value={String(t.id ?? '')} disabled={!isNew} placeholder="daily_chest" onChange={(e) => setT({ id: e.target.value })} /></Field>
          <Field label="Name"><Input value={String(t.name ?? '')} onChange={(e) => setT({ name: e.target.value })} /></Field>
          <Field label="Kind"><Select value={String(t.kind ?? 'weighted')} onValueChange={(v) => setT({ kind: v })}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="weighted">weighted — one roll</SelectItem><SelectItem value="fixed">fixed — grant all</SelectItem></SelectContent></Select></Field>
          <label className="flex items-center gap-2 pt-6 text-sm"><Checkbox checked={!!t.enabled} onCheckedChange={(v) => setT({ enabled: !!v })} /> enabled</label>
        </div>
        <Field label={`Entries (${entries.length})`}>
          <div className="space-y-2">
            {entries.map((e, i) => <div key={i} className="grid grid-cols-2 gap-2 sm:grid-cols-6">
              <Select value={e.kind} onValueChange={(v) => setE(i, { kind: v as E['kind'], ref: '' })}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="item">item</SelectItem><SelectItem value="currency">currency</SelectItem><SelectItem value="table">table</SelectItem></SelectContent></Select>
              <Input className="font-mono sm:col-span-2" list={e.kind === 'item' ? 'rt-items' : e.kind === 'currency' ? 'rt-cur' : 'rt-tables'} placeholder={`${e.kind} id`} value={e.ref} onChange={(ev) => setE(i, { ref: ev.target.value })} />
              <Input type="number" min={1} title="amount" value={String(e.amount)} onChange={(ev) => setE(i, { amount: ev.target.value })} />
              <Select value={e.rarity} onValueChange={(v) => setE(i, { rarity: v })}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{RAR.map((r) => <SelectItem key={r} value={r}>{r}</SelectItem>)}</SelectContent></Select>
              <div className="flex items-center gap-1"><Input type="number" min={0} title="weight" value={String(e.weight)} disabled={t.kind !== 'weighted'} onChange={(ev) => setE(i, { weight: ev.target.value })} />{t.kind === 'weighted' && total > 0 ? <span className="w-12 text-right text-xs text-muted-foreground">{Math.round((Number(e.weight) / total) * 1000) / 10}%</span> : null}<Button size="sm" variant="ghost" onClick={() => setD((x) => (x ? { ...x, entries: x.entries.filter((_, j) => j !== i) } : x))}>×</Button></div>
            </div>)}
            <Button size="sm" variant="outline" onClick={() => setD((x) => (x ? { ...x, entries: [...x.entries, { kind: 'item', ref: '', amount: 1, rarity: 'common', weight: 1 }] } : x))}>+ entry</Button>
          </div>
        </Field>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <Field label="Pity after N rolls (0 = off)"><Input type="number" min={0} value={String(pity.after ?? 0)} onChange={(e) => setT({ pity: { ...pity, after: e.target.value } })} /></Field>
          <Field label="Pity rarity"><Select value={String(pity.rarity || 'rare')} onValueChange={(v) => setT({ pity: { ...pity, rarity: v } })}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{RAR.map((r) => <SelectItem key={r} value={r}>{r}</SelectItem>)}</SelectContent></Select></Field>
          <Field label={defOk ? 'Deferred delivery (JSON, optional — e.g. {"after_secs":3600})' : 'Deferred — invalid JSON'}><Textarea className={`min-h-9 font-mono text-xs ${defOk ? '' : 'border-destructive'}`} value={d?.deferred ?? ''} onChange={(e) => setD((x) => (x ? { ...x, deferred: e.target.value } : x))} /></Field>
        </div>
        <Button size="sm" disabled={pending || !t.id || !defOk} onClick={save}>{isNew ? 'Create' : 'Save'}</Button>
      </CardContent></Card>
      <datalist id="rt-items">{items.map((x) => <option key={x} value={x} />)}</datalist>
      <datalist id="rt-cur">{currencies.map((x) => <option key={x} value={x} />)}</datalist>
      <datalist id="rt-tables">{listOf(all.data, 'tables').map((x) => <option key={String(x.id)} value={String(x.id)} />)}</datalist>
      <FormDialog open={dlg} onOpenChange={setDlg} title={`Grant ${id}`} submit="Roll & grant" pending={pending} disabled={!g.account_id}
        onSubmit={() => run(async () => { setGranted(await raw('POST', '/v1/server/rewards/grant', { account_id: g.account_id, table_id: id, reference: g.reason || 'console' })) }, 'Granted')}>
        <Field label="Account id"><Input className="font-mono" value={String(g.account_id)} onChange={(e) => setG((x) => ({ ...x, account_id: e.target.value }))} /></Field>
        <Field label="Reference (ledger)"><Input value={String(g.reason)} onChange={(e) => setG((x) => ({ ...x, reason: e.target.value }))} /></Field>
        {granted != null && <JsonView value={granted} />}
      </FormDialog>
    </div>
  )
}
