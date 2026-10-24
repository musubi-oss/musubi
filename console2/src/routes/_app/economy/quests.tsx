import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'

/** Quests (fixed objective lists completed from gameplay events) and quest pools (rotating goals drawn by weight per player). Saving posts the whole definition — the node upserts by id. */
export const Route = createFileRoute('/_app/economy/quests')({
  validateSearch: (s: Record<string, unknown>): { tab?: string; q?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined, q: typeof s.q === 'string' ? s.q : undefined }),
  component: Quests,
})

type Obj = { event: string; target: number | string }
type Goal = { id: string; name: string; description: string; objectives: Obj[]; rewards: string; weight: number | string }
const ts = (v: unknown) => (v ? new Date(typeof v === 'number' ? v * 1000 : String(v)).toISOString().replace('T', ' ').slice(0, 16) : '—')
const objs = (a: Row) => ((a.objectives ?? []) as Row[]).map((o) => `${o.event} ×${o.target ?? o.count}`).join(', ')
const clean = (o: Row) => ({ ...o, objectives: ((o.objectives ?? []) as Obj[]).filter((j) => j.event).map((j) => ({ event: j.event, target: Number(j.target) || 1 })) })

function Objectives({ list, onChange }: { list: Obj[]; onChange: (l: Obj[]) => void }) {
  const up = (i: number, p: Partial<Obj>) => onChange(list.map((o, j) => (j === i ? { ...o, ...p } : o)))
  return (
    <div className="space-y-2">
      {list.map((o, i) => <div key={i} className="flex gap-2"><Input className="min-w-0 flex-1 font-mono" placeholder="event name (match_won, kills)" value={o.event} onChange={(e) => up(i, { event: e.target.value })} /><Input className="w-24" type="number" min={1} value={String(o.target)} onChange={(e) => up(i, { target: e.target.value })} /><Button size="sm" variant="ghost" onClick={() => onChange(list.filter((_, j) => j !== i))}>×</Button></div>)}
      <Button size="sm" variant="outline" onClick={() => onChange([...list, { event: '', target: 1 }])}>+ objective</Button>
    </div>
  )
}

function Quests() {
  const { tab = 'quests', q = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const quests = useSub<Row>('/v1/server/quests')
  const pools = useSub<Row>('/v1/server/quests/pools')
  const tables = listOf(useSub<Row>('/v1/server/rewards/tables').data, 'tables')
  const match = (a: Row) => !q || JSON.stringify(a).toLowerCase().includes(q.toLowerCase())
  const vq = listOf(quests.data, 'quests').filter(match); const vp = listOf(pools.data, 'pools').filter(match)
  const [dlg, setDlg] = useState<'' | 'quest' | 'pool' | 'preview' | 'event'>('')
  const [isNew, setIsNew] = useState(false)
  const [x, setX] = useState<Row>({})
  const [ev, setEv] = useState<Row>({ account_id: '', event: '', amount: 1 })
  const [acc, setAcc] = useState('')
  const [preview, setPreview] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const side = useAct([['sub']])
  const editQuest = (a?: Row) => { setIsNew(!a); setX(structuredClone(a ?? { id: '', name: '', kind: 'daily', description: '', objectives: [{ event: '', target: 1 }], rewards: '', enabled: true, rerollable: false, rerolls_per_period: 1 })); setDlg('quest') }
  const editPool = (p?: Row) => { setIsNew(!p); setX(structuredClone(p ?? { id: '', name: '', description: '', enabled: true, ends_at: '', size: 3, rotation: 'daily', goals: [{ id: 'g1', name: '', description: '', objectives: [{ event: '', target: 1 }], rewards: '', weight: 1 }] })); setDlg('pool') }
  const goals = (x.goals ?? []) as Goal[]
  const setGoal = (gi: number, p: Partial<Goal>) => setX((s) => ({ ...s, goals: ((s.goals ?? []) as Goal[]).map((g, j) => (j === gi ? { ...g, ...p } : g)) }))
  const P = (id: unknown) => `/v1/server/quests/pools/${encodeURIComponent(String(id))}`
  const bind = (k: string) => (e: React.ChangeEvent<HTMLInputElement>) => setX((s) => ({ ...s, [k]: e.target.value }))
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Quests' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Quests</h1><p className="mt-1 text-sm text-muted-foreground">{listOf(quests.data, 'quests').length} quests · {listOf(pools.data, 'pools').length} pools. A quest completes when every objective reaches its target from gameplay events (<code>POST /v1/quests/event</code> or server hooks); a pool hands each player <i>size</i> goals drawn by weight every rotation.</p></div>
        <Button size="sm" variant="outline" onClick={() => { setEv({ account_id: '', event: '', amount: 1 }); setDlg('event') }}>Emit event</Button>
        <Button size="sm" onClick={() => (tab === 'pools' ? editPool() : editQuest())}>New {tab === 'pools' ? 'pool' : 'quest'}</Button>
      </div>
      {quests.isError && <Alert variant="destructive"><AlertDescription>{String(quests.error)}</AlertDescription></Alert>}
      <div className="flex flex-wrap items-center gap-2">
        <Tabs value={tab} onValueChange={(v) => void nav({ search: (p) => ({ ...p, tab: v === 'quests' ? undefined : v }), replace: true })}><TabsList className="h-auto max-w-full flex-wrap"><TabsTrigger value="quests">Quests</TabsTrigger><TabsTrigger value="pools">Pools (rotating goals)</TabsTrigger></TabsList></Tabs>
        <Input className="w-full sm:w-56" placeholder="filter" value={q} onChange={(e) => void nav({ search: (p) => ({ ...p, q: e.target.value || undefined }), replace: true })} />
      </div>
      <Card><CardContent className="pt-6">
        {tab === 'quests' ? (
          <DataTable id="quests" rows={vq} loading={quests.isPending} empty="No quests" cols={[{ key: 'id', mono: true }, { key: 'name' }, { key: 'kind' }, { key: 'objectives', fmt: objs, dim: true }, { key: 'rewards', mono: true, dim: true }, { key: 'rerollable', label: 'reroll', fmt: (a) => (a.rerollable ? `${a.rerolls_per_period ?? 1}/period` : 'no'), dim: true }, { key: 'enabled', fmt: (a) => <Badge variant={a.enabled ? 'default' : 'secondary'}>{a.enabled ? 'on' : 'off'}</Badge> }]}
            actions={(a) => <Button size="sm" variant="ghost" onClick={() => editQuest(a)}>Edit</Button>} />
        ) : (
          <DataTable id="quest-pools" rows={vp} loading={pools.isPending} empty="No pools" cols={[{ key: 'id', mono: true }, { key: 'name' }, { key: 'goals', fmt: (p) => ((p.goals ?? []) as unknown[]).length }, { key: 'rotation', fmt: (p) => `${p.size ?? '?'} per ${p.rotation ?? 'period'}`, dim: true }, { key: 'generation', label: 'gen', dim: true }, { key: 'ends_at', label: 'ends', fmt: (p) => ts(p.ends_at), dim: true }, { key: 'enabled', fmt: (p) => <Badge variant={p.enabled ? 'default' : 'secondary'}>{p.enabled ? 'on' : 'off'}</Badge> }]}
            actions={(p) => <span className="flex justify-end gap-1"><Button size="sm" variant="ghost" onClick={() => editPool(p)}>Edit</Button><Button size="sm" variant="ghost" onClick={() => { setX(p); setPreview(null); setDlg('preview') }}>Preview</Button><Button size="sm" variant="ghost" onClick={() => confirmThen(`Retire pool ${p.id}? Players keep current goals, no new rotation.`, () => run(() => raw('POST', `${P(p.id)}/retire`), 'Pool retired'))}>Retire</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen('Delete pool?', () => run(() => raw('DELETE', P(p.id)), 'Deleted'))}>×</Button></span>} />
        )}
      </CardContent></Card>
      <FormDialog open={dlg === 'quest'} onOpenChange={(o) => !o && setDlg('')} title={isNew ? 'New quest' : `Quest ${x.id}`} submit="Save" pending={pending} disabled={!x.id}
        onSubmit={() => run(() => raw('POST', '/v1/server/quests', { ...clean(x), rerolls_per_period: Number(x.rerolls_per_period) || 1 }), 'Quest saved')}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Id"><Input className="font-mono" value={String(x.id ?? '')} disabled={!isNew} onChange={bind('id')} /></Field><Field label="Name"><Input value={String(x.name ?? '')} onChange={bind('name')} /></Field>
          <Field label="Kind"><Select value={String(x.kind ?? 'daily')} onValueChange={(v) => setX((s) => ({ ...s, kind: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{['daily', 'weekly', 'story', 'event', 'achievement'].map((k) => <SelectItem key={k} value={k}>{k}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Rewards (reward table)"><Input className="font-mono" list="rtables" value={String(x.rewards ?? '')} onChange={bind('rewards')} /></Field>
        </div>
        <Field label="Description"><Input value={String(x.description ?? '')} onChange={bind('description')} /></Field>
        <Field label="Objectives"><Objectives list={(x.objectives ?? []) as Obj[]} onChange={(l) => setX((s) => ({ ...s, objectives: l }))} /></Field>
        <div className="flex flex-wrap items-center gap-4 text-sm">
          <label className="flex items-center gap-2"><Checkbox checked={!!x.enabled} onCheckedChange={(v) => setX((s) => ({ ...s, enabled: !!v }))} /> enabled</label>
          <label className="flex items-center gap-2"><Checkbox checked={!!x.rerollable} onCheckedChange={(v) => setX((s) => ({ ...s, rerollable: !!v }))} /> rerollable</label>
          {x.rerollable ? <span className="flex items-center gap-2"><Input className="w-20" type="number" min={1} value={String(x.rerolls_per_period ?? 1)} onChange={bind('rerolls_per_period')} /> per period</span> : null}
        </div>
      </FormDialog>
      <FormDialog wide open={dlg === 'pool'} onOpenChange={(o) => !o && setDlg('')} title={isNew ? 'New quest pool' : `Pool ${x.id}`} submit="Save" pending={pending} disabled={!x.id}
        onSubmit={() => run(() => raw('POST', '/v1/server/quests/pools', { ...x, size: Number(x.size) || 1, ends_at: x.ends_at === '' || x.ends_at == null ? null : Number(x.ends_at), goals: goals.map((g) => ({ ...clean(g as unknown as Row), weight: Number(g.weight) || 1 })) }), 'Pool saved')}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Id"><Input className="font-mono" value={String(x.id ?? '')} disabled={!isNew} onChange={bind('id')} /></Field><Field label="Name"><Input value={String(x.name ?? '')} onChange={bind('name')} /></Field>
        </div>
        <Field label="Description"><Input value={String(x.description ?? '')} onChange={bind('description')} /></Field>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Field label="Goals per player"><Input type="number" min={1} value={String(x.size ?? '')} onChange={bind('size')} /></Field>
          <Field label="Rotation"><Select value={String(x.rotation ?? 'daily')} onValueChange={(v) => setX((s) => ({ ...s, rotation: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{['daily', 'weekly', 'monthly'].map((k) => <SelectItem key={k} value={k}>{k}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Ends at (unix s, empty = open)"><Input type="number" value={String(x.ends_at ?? '')} onChange={bind('ends_at')} /></Field>
          <label className="flex items-center gap-2 pt-6 text-sm"><Checkbox checked={!!x.enabled} onCheckedChange={(v) => setX((s) => ({ ...s, enabled: !!v }))} /> enabled</label>
        </div>
        <Field label={`Goals (${goals.length})`}>
          <div className="space-y-3">
            {goals.map((g, gi) => <div key={gi} className="space-y-2 rounded-md border p-3">
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-5">
                <Input className="font-mono" placeholder="id" value={g.id} onChange={(e) => setGoal(gi, { id: e.target.value })} /><Input className="sm:col-span-2" placeholder="name" value={g.name} onChange={(e) => setGoal(gi, { name: e.target.value })} /><Input className="font-mono" list="rtables" placeholder="reward table" value={g.rewards} onChange={(e) => setGoal(gi, { rewards: e.target.value })} /><div className="flex gap-1"><Input type="number" min={1} title="weight" value={String(g.weight)} onChange={(e) => setGoal(gi, { weight: e.target.value })} /><Button size="sm" variant="ghost" onClick={() => setX((s) => ({ ...s, goals: goals.filter((_, j) => j !== gi) }))}>×</Button></div>
              </div>
              <Objectives list={g.objectives ?? []} onChange={(l) => setGoal(gi, { objectives: l })} />
            </div>)}
            <Button size="sm" variant="outline" onClick={() => setX((s) => ({ ...s, goals: [...goals, { id: `g${goals.length + 1}`, name: '', description: '', objectives: [{ event: '', target: 1 }], rewards: '', weight: 1 }] }))}>Add goal</Button>
          </div>
        </Field>
      </FormDialog>
      <FormDialog open={dlg === 'preview'} onOpenChange={(o) => !o && setDlg('')} title={`Pool ${x.id} — what a player gets`} submit="Preview" pending={side.pending} disabled={!acc}
        onSubmit={() => side.run(async () => { setPreview(await raw('GET', `${P(x.id)}/preview/${encodeURIComponent(acc)}`)) }, 'Previewed')}>
        <Field label="Account id"><Input className="font-mono" value={acc} onChange={(e) => setAcc(e.target.value)} /></Field>
        <Button size="sm" variant="outline" disabled={!acc || side.pending} onClick={() => side.run(() => raw('POST', `${P(x.id)}/accelerate`, { account_id: acc }), 'Rotation accelerated for this player')}>Accelerate rotation for this player</Button>
        {preview != null && <JsonView value={preview} />}
      </FormDialog>
      <FormDialog open={dlg === 'event'} onOpenChange={(o) => !o && setDlg('')} title="Emit a quest event" submit="Emit" pending={pending} disabled={!ev.account_id || !ev.event}
        onSubmit={() => run(() => raw('POST', '/v1/server/quests/event', { ...ev, amount: Number(ev.amount) || 1 }), 'Event emitted')}>
        <p className="text-sm text-muted-foreground">Advances every active quest objective listening to this event for the player — handy to test a quest or compensate a player.</p>
        <Field label="Account id"><Input className="font-mono" value={String(ev.account_id)} onChange={(e) => setEv((s) => ({ ...s, account_id: e.target.value }))} /></Field>
        <div className="grid grid-cols-3 gap-3"><div className="col-span-2"><Field label="Event"><Input className="font-mono" value={String(ev.event)} onChange={(e) => setEv((s) => ({ ...s, event: e.target.value }))} /></Field></div><Field label="Amount"><Input type="number" min={1} value={String(ev.amount)} onChange={(e) => setEv((s) => ({ ...s, amount: e.target.value }))} /></Field></div>
      </FormDialog>
      <datalist id="rtables">{tables.map((t) => <option key={String(t.id)} value={String(t.id)} />)}</datalist>
    </div>
  )
}
