import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'

/** Skill-rating queues: which system (Elo / Glicko-2 / OpenSkill) rates results reported for a queue, placement games, inactivity decay and seasons. */
export const Route = createFileRoute('/_app/social/rating')({ component: Rating })

const SYSTEMS: [string, string][] = [['elo', 'Elo — simple, 1v1 and teams'], ['glicko2', 'Glicko-2 — rating + confidence'], ['openskill', 'OpenSkill — free-for-all / asymmetric teams']]
const DEF = (): Row => ({ id: '', system: 'glicko2', params: { elo_k: 32, glicko_tau: 0.5, openskill_beta: 4.17, placement_games: 10, placement_k_mult: 2, decay_after_secs: 14 * 86400, decay_every_secs: 86400, decay_points: 10, decay_floor: 1000, season_carry: 0.5 } })

function Rating() {
  const q = useSub<Row>('/v1/server/rating/queues')
  const rows = listOf(q.data, 'queues')
  const [open, setOpen] = useState(false); const [isNew, setIsNew] = useState(true); const [f, setF] = useState<Row>(DEF())
  const [season, setSeason] = useState<{ id: string; soft: boolean } | null>(null)
  const { run, pending } = useAct([['sub']], () => { setOpen(false); setSeason(null) })
  const P = (f.params ?? {}) as Row
  const sp = (k: string, v: unknown) => setF((x) => ({ ...x, params: { ...((x.params ?? {}) as Row), [k]: v } }))
  const edit = (r?: Row) => { setIsNew(!r); setF(r ? { ...DEF(), ...r, params: { ...(DEF().params as Row), ...((r.params ?? {}) as Row) } } : DEF()); setOpen(true) }
  const N = ({ k, label, step }: { k: string; label: string; step?: string }) => <Field label={label}><Input type="number" step={step} value={P[k] == null ? '' : String(P[k])} onChange={(e) => sp(k, Number(e.target.value))} /></Field>
  const days = (s: unknown) => (s ? `${Math.round(Number(s) / 86400)} d` : 'off')
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Social', to: '/social' }, { title: 'Rating' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Rating queues</h1><p className="mt-1 text-sm text-muted-foreground">{rows.length} queues · a queue's rating updates when a game server reports a result for it</p></div>
        <Button size="sm" onClick={() => edit()}>New queue</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="pt-6">
        <DataTable id="rating-queues" rows={rows} loading={q.isPending} empty="No rating queues — results reported by servers are ignored until one exists" rowTo={(r) => `/social/rating/${encodeURIComponent(String(r.id))}`}
          cols={[{ key: 'id', mono: true }, { key: 'system' }, { key: 'season', fmt: (r) => r.season ?? 1 }, { key: 'placement', fmt: (r) => `${((r.params ?? {}) as Row).placement_games ?? 0} games` }, { key: 'decay', dim: true, fmt: (r) => { const p = (r.params ?? {}) as Row; return p.decay_after_secs ? `−${p.decay_points} every ${days(p.decay_every_secs)} after ${days(p.decay_after_secs)} idle, floor ${p.decay_floor}` : 'off' } }, { key: 'players', fmt: (r) => r.players ?? r.rated ?? '—' }]}
          actions={(r) => { const id = encodeURIComponent(String(r.id)); return <span className="flex justify-end gap-1"><Button size="sm" variant="ghost" onClick={() => edit(r)}>Edit</Button><Button size="sm" variant="ghost" onClick={() => setSeason({ id: String(r.id), soft: true })}>New season</Button><Button size="sm" variant="ghost" onClick={() => confirmThen(`Apply inactivity decay to ${r.id} now?`, () => run(() => raw('POST', `/v1/server/rating/queues/${id}/decay`), 'Decay applied'))}>Decay</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Delete queue ${r.id} and all its ratings?`, () => run(() => raw('DELETE', `/v1/server/rating/queues/${id}`), 'Deleted'))}>×</Button></span> }} />
      </CardContent></Card>
      <FormDialog open={open} onOpenChange={setOpen} title={isNew ? 'New rating queue' : `Queue ${f.id}`} submit="Save" pending={pending} disabled={!f.id}
        onSubmit={() => run(() => raw('PUT', '/v1/server/rating/queues', { id: f.id, system: f.system, params: f.params }), 'Saved')}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Id (matchmaker queue name is the usual choice)"><Input className="font-mono" value={String(f.id)} disabled={!isNew} onChange={(e) => setF((x) => ({ ...x, id: e.target.value }))} /></Field>
          <Field label="System"><Select value={String(f.system)} onValueChange={(v) => setF((x) => ({ ...x, system: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{SYSTEMS.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}</SelectContent></Select></Field>
        </div>
        <div className="grid grid-cols-2 gap-3">
          {f.system === 'elo' && <N k="elo_k" label="K factor" />}
          {f.system === 'glicko2' && <N k="glicko_tau" label="τ (volatility change)" step="0.01" />}
          {f.system === 'openskill' && <N k="openskill_beta" label="β (skill spread)" step="0.01" />}
          <N k="placement_games" label="Placement games" /><N k="placement_k_mult" label="Placement gain multiplier" step="0.1" />
        </div>
        <p className="text-xs text-muted-foreground">Decay pulls idle players' rating down toward the floor so they re-place when they return.</p>
        <div className="grid grid-cols-2 gap-3"><N k="decay_after_secs" label="Decay after idle (s, 0 = off)" /><N k="decay_every_secs" label="Decay every (s)" /><N k="decay_points" label="Points per step" /><N k="decay_floor" label="Floor" /></div>
        <N k="season_carry" label="Season carry (0 = full reset, 1 = keep rating)" step="0.05" />
      </FormDialog>
      <FormDialog open={!!season} onOpenChange={(o) => !o && setSeason(null)} title={`New season — ${season?.id ?? ''}`} submit="Start season" pending={pending}
        onSubmit={() => season && run(() => raw('POST', `/v1/server/rating/queues/${encodeURIComponent(season.id)}/season`, { soft_reset: season.soft }), 'Season started')}>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={season?.soft ?? true} onCheckedChange={(v) => setSeason((s) => (s ? { ...s, soft: !!v } : s))} /> Soft reset — carry part of the rating (season_carry) instead of a full reset</label>
      </FormDialog>
    </div>
  )
}
