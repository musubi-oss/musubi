import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { FormDialog, Field } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'

/** Season / battle pass: a progression track plus a free and premium reward table per level and a time window. `new` opens the create form. */
export const Route = createFileRoute('/_app/economy/seasons/$id')({ component: Season })

type L = { level: number; free: string; premium: string }
const toIso = (v: unknown) => (v ? new Date(String(v)).toISOString().slice(0, 16) : '')

function Season() {
  const { id } = Route.useParams()
  const isNew = id === 'new'
  const nav = useNavigate()
  const all = useSub<Row>('/v1/server/seasons')
  const tracks = listOf(useSub<Row>('/v1/server/progression/tracks').data, 'tracks')
  const tables = listOf(useSub<Row>('/v1/server/rewards/tables').data, 'tables')
  const loaded = isNew ? undefined : listOf(all.data, 'seasons').find((x) => String(x.id) === id)
  const [d, setD] = useState<{ key: string; s: Row; levels: L[]; starts: string; ends: string } | null>(null)
  const key = `${id}:${all.dataUpdatedAt}`
  if (!d || (d.key !== key && (isNew || loaded))) {
    const s: Row = isNew ? { id: '', name: '', track_id: '' } : { ...loaded }
    setD({ key, s, levels: ((s.levels ?? []) as Row[]).map((l) => ({ level: Number(l.level), free: String(l.free ?? ''), premium: String(l.premium ?? '') })), starts: toIso(s.starts_at), ends: toIso(s.ends_at) })
  }
  const s = d?.s ?? {}; const levels = d?.levels ?? []
  const track = tracks.find((t) => String(t.id) === String(s.track_id))
  const trackN = ((track?.levels ?? []) as unknown[]).length
  const setX = (p: Partial<NonNullable<typeof d>>) => setD((x) => (x ? { ...x, ...p } : x))
  const set = (p: Row) => setD((x) => (x ? { ...x, s: { ...x.s, ...p } } : x))
  const setL = (i: number, p: Partial<L>) => setX({ levels: levels.map((l, j) => (j === i ? { ...l, ...p } : l)) })
  const sync = () => { const out = [...levels]; while (out.length < trackN) out.push({ level: out.length + 1, free: '', premium: '' }); setX({ levels: out }) }
  const [dlg, setDlg] = useState(false)
  const [acc, setAcc] = useState('')
  const { run, pending } = useAct([['sub']], () => setDlg(false))
  const state = !s.starts_at ? 'draft' : Date.now() < +new Date(String(s.starts_at)) ? 'upcoming' : s.ends_at && Date.now() > +new Date(String(s.ends_at)) ? 'ended' : 'live'
  const save = () => run(async () => {
    const b = { ...s, id: s.id || undefined, starts_at: d?.starts ? new Date(d.starts).toISOString() : null, ends_at: d?.ends ? new Date(d.ends).toISOString() : null, levels: levels.map((l, i) => ({ level: i + 1, free: l.free || null, premium: l.premium || null })) }
    const r = (await raw<Row>('POST', '/v1/server/seasons', b)) as Row
    if (isNew) void nav({ to: '/economy/seasons/$id', params: { id: String(r.id ?? (r.season as Row | undefined)?.id ?? b.id) } })
  }, 'Season saved')
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Seasons', to: '/economy/seasons' }, { title: isNew ? 'New season' : String(loaded?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New season' : String(loaded?.name ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">{!isNew && <><span className="font-mono">{id}</span><Badge variant={state === 'live' ? 'default' : 'secondary'}>{state}</Badge></>}<span>Players earn XP on the season's track; each level unlocks its free reward and, with premium, the premium one.</span></div></div>
        {!isNew && <Button size="sm" variant="outline" onClick={() => setDlg(true)}>Grant premium</Button>}
      </div>
      {all.isError && <Alert variant="destructive"><AlertDescription>{String(all.error)}</AlertDescription></Alert>}
      {!isNew && all.isSuccess && !loaded && <Alert variant="destructive"><AlertDescription>Season {id} not found</AlertDescription></Alert>}
      <Card><CardContent className="space-y-3 pt-6">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-5">
          <Field label="Id"><Input className="font-mono" value={String(s.id ?? '')} disabled={!isNew} placeholder="s1" onChange={(e) => set({ id: e.target.value })} /></Field>
          <Field label="Name"><Input value={String(s.name ?? '')} onChange={(e) => set({ name: e.target.value })} /></Field>
          <Field label="Progression track"><Select value={String(s.track_id || '_')} onValueChange={(v) => set({ track_id: v === '_' ? '' : v })}><SelectTrigger className="w-full max-w-full [&>span]:truncate"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="_">— pick —</SelectItem>{tracks.map((t) => <SelectItem key={String(t.id)} value={String(t.id)}>{String(t.name || t.id)} ({((t.levels ?? []) as unknown[]).length} lvl)</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Starts"><Input type="datetime-local" value={d?.starts ?? ''} onChange={(e) => setX({ starts: e.target.value })} /></Field>
          <Field label="Ends"><Input type="datetime-local" value={d?.ends ?? ''} onChange={(e) => setX({ ends: e.target.value })} /></Field>
        </div>
        <Field label={`Level rewards (${levels.length}${trackN ? ` / track has ${trackN}` : ''})`}>
          <div className="space-y-2">
            <div className="hidden grid-cols-12 gap-2 text-xs text-muted-foreground sm:grid"><span className="col-span-1">lvl</span><span className="col-span-5">free reward table</span><span className="col-span-5">premium reward table</span></div>
            {levels.map((l, i) => <div key={i} className="grid grid-cols-6 items-center gap-2 sm:grid-cols-12">
              <span className="font-mono text-sm">{i + 1}</span>
              <Input className="col-span-2 font-mono sm:col-span-5" list="se-tables" placeholder="none" value={l.free} onChange={(e) => setL(i, { free: e.target.value })} />
              <Input className="col-span-2 font-mono sm:col-span-5" list="se-tables" placeholder="none" value={l.premium} onChange={(e) => setL(i, { premium: e.target.value })} />
              <Button size="sm" variant="ghost" onClick={() => setX({ levels: levels.filter((_, j) => j !== i) })}>×</Button>
            </div>)}
            <div className="flex gap-2"><Button size="sm" variant="outline" onClick={() => setX({ levels: [...levels, { level: levels.length + 1, free: '', premium: '' }] })}>+ level</Button>{trackN > levels.length && <Button size="sm" variant="outline" onClick={sync}>Fill to track ({trackN})</Button>}</div>
          </div>
        </Field>
        <Button size="sm" disabled={pending || !s.id || !s.track_id} onClick={save}>{isNew ? 'Create' : 'Save'}</Button>
      </CardContent></Card>
      <datalist id="se-tables">{tables.map((x) => <option key={String(x.id)} value={String(x.id)} />)}</datalist>
      <FormDialog open={dlg} onOpenChange={setDlg} title={`Grant premium pass — ${id}`} submit="Grant" pending={pending} disabled={!acc}
        onSubmit={() => run(() => raw('POST', `/v1/server/seasons/${encodeURIComponent(id)}/premium`, { account_id: acc }), 'Premium granted')}>
        <p className="text-sm text-muted-foreground">Unlocks the premium reward lane for this player (what an IAP purchase does automatically). Already-reached levels pay out immediately.</p>
        <Field label="Account id"><Input className="font-mono" value={acc} onChange={(e) => setAcc(e.target.value)} /></Field>
      </FormDialog>
    </div>
  )
}
