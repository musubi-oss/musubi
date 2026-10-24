import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { FormDialog, Field } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'

/** Progression track: XP needed per level and the reward table granted on reaching it; seasons attach to a track. `new` opens the create form. */
export const Route = createFileRoute('/_app/economy/progression/$id')({ component: Track })

type L = { xp: number | string; rewards: string }

function Track() {
  const { id } = Route.useParams()
  const isNew = id === 'new'
  const nav = useNavigate()
  const all = useSub<Row>('/v1/server/progression/tracks')
  const tables = listOf(useSub<Row>('/v1/server/rewards/tables').data, 'tables')
  const loaded = isNew ? undefined : listOf(all.data, 'tracks').find((x) => String(x.id) === id)
  const [d, setD] = useState<{ key: string; t: Row; levels: L[] } | null>(null)
  const key = `${id}:${all.dataUpdatedAt}`
  if (!d || (d.key !== key && (isNew || loaded))) {
    const t: Row = isNew ? { id: '', name: '' } : { ...loaded }
    setD({ key, t, levels: ((t.levels ?? []) as Row[]).map((l) => ({ xp: Number(l.xp), rewards: l.rewards ? (typeof l.rewards === 'string' ? l.rewards : JSON.stringify(l.rewards)) : '' })) })
  }
  const t = d?.t ?? {}; const levels = d?.levels ?? []
  let s = 0; const cum = levels.map((l) => (s += Number(l.xp) || 0))
  const setL = (i: number, p: Partial<L>) => setD((x) => (x ? { ...x, levels: x.levels.map((l, j) => (j === i ? { ...l, ...p } : l)) } : x))
  const [dlg, setDlg] = useState(false)
  const [g, setG] = useState<Row>({ account_id: '', xp: 100, reason: 'console' })
  const { run, pending } = useAct([['sub']], () => setDlg(false))
  const parseRewards = (x: string) => { if (!x.trim()) return null; try { return JSON.parse(x) } catch { return x } }
  const save = () => run(async () => {
    const b = { ...t, id: t.id || undefined, levels: levels.map((l) => ({ xp: Number(l.xp), rewards: parseRewards(l.rewards) })) }
    const r = (await raw<Row>('POST', '/v1/server/progression/tracks', b)) as Row
    if (isNew) void nav({ to: '/economy/progression/$id', params: { id: String(r.id ?? (r.track as Row | undefined)?.id ?? b.id) } })
  }, 'Track saved')
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Progression', to: '/economy/progression' }, { title: isNew ? 'New track' : String(loaded?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New progression track' : String(loaded?.name ?? id)}</h1><p className="mt-1 text-sm text-muted-foreground">{levels.length} levels · {cum.at(-1) ?? 0} XP to max. XP arrives from gameplay (<code>POST /v1/server/progression/xp</code>); crossing a level grants its reward table and fires a <code>level_up</code> event.</p></div>
        {!isNew && <Button size="sm" variant="outline" onClick={() => setDlg(true)}>Grant XP</Button>}
      </div>
      {all.isError && <Alert variant="destructive"><AlertDescription>{String(all.error)}</AlertDescription></Alert>}
      {!isNew && all.isSuccess && !loaded && <Alert variant="destructive"><AlertDescription>Track {id} not found</AlertDescription></Alert>}
      <Card><CardContent className="space-y-3 pt-6">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label="Id"><Input className="font-mono" value={String(t.id ?? '')} disabled={!isNew} placeholder="main" onChange={(e) => setD((x) => (x ? { ...x, t: { ...x.t, id: e.target.value } } : x))} /></Field>
          <Field label="Name"><Input value={String(t.name ?? '')} onChange={(e) => setD((x) => (x ? { ...x, t: { ...x.t, name: e.target.value } } : x))} /></Field>
        </div>
        <Field label="Levels">
          <div className="space-y-2">
            <div className="hidden grid-cols-12 gap-2 text-xs text-muted-foreground sm:grid"><span className="col-span-1">lvl</span><span className="col-span-3">XP for this level</span><span className="col-span-2">cumulative</span><span className="col-span-5">reward table id (or JSON)</span></div>
            {levels.map((l, i) => <div key={i} className="grid grid-cols-6 items-center gap-2 sm:grid-cols-12">
              <span className="font-mono text-sm">{i + 1}</span>
              <Input className="col-span-2 sm:col-span-3" type="number" min={0} value={String(l.xp)} onChange={(e) => setL(i, { xp: e.target.value })} />
              <span className="hidden font-mono text-xs text-muted-foreground sm:col-span-2 sm:block">{cum[i]}</span>
              <Input className="col-span-2 font-mono sm:col-span-5" list="pt-tables" value={l.rewards} onChange={(e) => setL(i, { rewards: e.target.value })} />
              <Button size="sm" variant="ghost" onClick={() => setD((x) => (x ? { ...x, levels: x.levels.filter((_, j) => j !== i) } : x))}>×</Button>
            </div>)}
            <Button size="sm" variant="outline" onClick={() => setD((x) => (x ? { ...x, levels: [...x.levels, { xp: Number(x.levels.at(-1)?.xp) || 100, rewards: '' }] } : x))}>+ level</Button>
          </div>
        </Field>
        <Button size="sm" disabled={pending || !t.id} onClick={save}>{isNew ? 'Create' : 'Save'}</Button>
      </CardContent></Card>
      <datalist id="pt-tables">{tables.map((x) => <option key={String(x.id)} value={String(x.id)} />)}</datalist>
      <FormDialog open={dlg} onOpenChange={setDlg} title={`Grant XP on ${id}`} submit="Grant" pending={pending} disabled={!g.account_id || !Number(g.xp)}
        onSubmit={() => run(() => raw('POST', '/v1/server/progression/xp', { account_id: g.account_id, track_id: id, delta: Number(g.xp), reason: g.reason || 'console' }), 'XP granted')}>
        <Field label="Account id"><Input className="font-mono" value={String(g.account_id)} onChange={(e) => setG((x) => ({ ...x, account_id: e.target.value }))} /></Field>
        <div className="grid grid-cols-2 gap-3"><Field label="XP"><Input type="number" value={String(g.xp)} onChange={(e) => setG((x) => ({ ...x, xp: e.target.value }))} /></Field><Field label="Reason"><Input value={String(g.reason)} onChange={(e) => setG((x) => ({ ...x, reason: e.target.value }))} /></Field></div>
      </FormDialog>
    </div>
  )
}
