import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** One tournament: leaderboard records plus an optional bracket (single / double elimination, swiss, round robin) with rounds, reported results and forfeits. */
export const Route = createFileRoute('/_app/social/tournaments/$id')({ component: Tournament })

const ts = (v: unknown) => { if (!v) return '—'; const d = typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(String(v)); return isNaN(d.getTime()) ? String(v) : d.toISOString().replace('T', ' ').slice(0, 16) }
const stateOf = (t: Row) => (t.ended ? 'ended' : t.end_time && Number(t.end_time) * 1000 < Date.now() ? 'over' : t.start_time && Number(t.start_time) * 1000 > Date.now() ? 'upcoming' : 'running')

function Tournament() {
  const { id } = Route.useParams()
  const nav = useNavigate({ from: Route.fullPath })
  const T = `/v1/server/tournaments/${encodeURIComponent(id)}`
  const list = useQuery({ queryKey: ['tournaments'], queryFn: ({ signal }) => raw<Row>('GET', '/v1/server/tournaments?limit=200', undefined, { signal }) })
  const t = (listOf(list.data, 'tournaments').find((x) => String(x.id) === id) ?? (list.data ? { id } : undefined)) as Row | undefined
  const bracket = useSub<Row>(`${T}/bracket`)
  const records = useSub<Row>(`${T}/records?limit=50`)
  const b = bracket.data
  const matches = ((b?.matches ?? []) as Row[]).filter((m) => !m.bye)
  const names = new Map(((b?.participants ?? []) as Row[]).map((x) => [String(x.id), `${x.name || String(x.id).slice(0, 8)} (#${x.seed})`]))
  const [dlg, setDlg] = useState<'' | 'bracket' | 'report' | 'forfeit'>('')
  const [f, setF] = useState<Row>({})
  const { run, pending } = useAct([['tournaments'], ['sub']], () => setDlg(''))
  const slotId = (v: unknown) => (v && typeof v === 'object' && 'player' in (v as Row) ? String((v as Row).player) : null)
  const who = (v: unknown) => { const id = slotId(v); return id ? (names.get(id) ?? id) : v === 'bye' ? 'bye' : 'tbd' }
  const players = (m: Row) => [slotId(m.a), slotId(m.b)].filter((x): x is string => !!x)
  const mid = (m: Row) => String(m.id)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Social', to: '/social' }, { title: 'Tournaments', to: '/social/tournaments' }, { title: String(t?.title ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{String(t?.title ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">{id}{t && <Badge variant="secondary">{stateOf(t)}</Badge>}</div></div>
        {!b && !bracket.isFetching && <Button size="sm" onClick={() => { setF({ format: 'single', seeding: 'score', participants: '', size: 8, swiss_rounds: '', deadline_secs: 3600 }); setDlg('bracket') }}>Create bracket</Button>}
        {b && <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${T}/bracket/rounds`), 'Next round generated')}>Next round</Button>}
        {b && <Button size="sm" variant="outline" onClick={() => confirmThen('Delete the bracket? Reported results are lost.', () => run(() => raw('DELETE', `${T}/bracket`), 'Bracket deleted'))}>Delete bracket</Button>}
        {t && !t.ended && <Button size="sm" variant="outline" onClick={() => confirmThen('End the tournament now? Rewards are distributed.', () => run(() => raw('POST', `${T}/end`), 'Ended'))}>End</Button>}
        <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete this tournament?', () => run(async () => { await raw('DELETE', T); void nav({ to: '/$section/$page', params: { section: 'social', page: 'tournaments' } }) }, 'Deleted'))}>Delete</Button>
      </div>
      {list.isError && <Alert variant="destructive"><AlertDescription>{String(list.error)}</AlertDescription></Alert>}
      {t && <Card><CardContent className="pt-6"><KvGrid>
        <Kv k="Window" v={`${ts(t.start_time)} → ${ts(t.end_time)}`} /><Kv k="Scoring" v={`${t.operator ?? '—'} · ${t.sort_order ?? '—'} · ${t.max_num_score ?? '—'} scores`} /><Kv k="Size" v={`${t.size ?? t.participants ?? 0} / ${t.max_size ?? '∞'}`} /><Kv k="Category" v={t.category} /><Kv k="Join required" v={t.join_required ? 'yes' : 'no'} /><Kv k="Duration" v={t.duration ? `${Math.round(Number(t.duration) / 3600)} h` : '—'} />
      </KvGrid>{t.description ? <p className="mt-3 text-sm text-muted-foreground">{String(t.description)}</p> : null}</CardContent></Card>}
      <Card><CardContent className="pt-6">
        {bracket.isError && !/no bracket|not found|404/i.test(String(bracket.error)) ? <Alert variant="destructive"><AlertDescription>{String(bracket.error)}</AlertDescription></Alert> : b ? (<>
          <SubHead title="Bracket"><span className="self-center text-xs text-muted-foreground">{String(b.format)} · round {String(b.round)} of {String(b.rounds)}{b.champion ? ` · champion ${names.get(String(b.champion)) ?? String(b.champion)}` : ''}</span></SubHead>
          <DataTable id="bracket" rows={matches} empty="No matches yet — generate the next round" cols={[{ key: 'side', dim: true }, { key: 'round' }, { key: 'slot', dim: true }, { key: 'a', fmt: (m) => who(m.a) }, { key: 'b', fmt: (m) => who(m.b) }, { key: 'winner', fmt: (m) => (m.winner ? names.get(String(m.winner)) ?? String(m.winner) : '') }, { key: 'score', mono: true, fmt: (m) => (m.score == null ? '' : typeof m.score === 'object' ? JSON.stringify(m.score) : String(m.score)) }, { key: 'match_id', label: 'live match', mono: true, dim: true }]}
            actions={(m) => !m.winner && players(m).length === 2 ? <span className="flex justify-end gap-1"><Button size="sm" variant="ghost" onClick={() => { setF({ mid: mid(m), players: players(m), winner: players(m)[0], score: '' }); setDlg('report') }}>Report</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => { setF({ mid: mid(m), players: players(m), participant: players(m)[0] }); setDlg('forfeit') }}>Forfeit</Button></span> : null} />
          <SubHead title="Standings" /><JsonView value={b.standings ?? b.participants} />
        </>) : <p className="text-sm text-muted-foreground">{bracket.isPending ? 'Loading…' : 'No bracket — the tournament is a leaderboard only until you create one.'}</p>}
        <SubHead title="Records" count={listOf(records.data, 'records').length} />
        <DataTable id="t-records" rows={listOf(records.data, 'records')} loading={records.isPending} empty="Nobody has scored yet" rowTo={(r) => (r.owner_id ? `/players/accounts/${r.owner_id}` : null)} cols={[{ key: 'rank' }, { key: 'owner_id', mono: true }, { key: 'username' }, { key: 'score' }, { key: 'subscore' }, { key: 'num_score', label: 'submissions' }, { key: 'updated_at', dim: true, fmt: (r) => ts(r.updated_at ?? r.update_time) }]} />
      </CardContent></Card>
      <FormDialog open={dlg === 'bracket'} onOpenChange={(o) => !o && setDlg('')} title="Create bracket" submit="Create" pending={pending}
        onSubmit={() => run(() => raw('POST', `${T}/bracket`, { format: f.format, seeding: f.seeding, participants: f.seeding === 'list' ? String(f.participants).split(/[\s,]+/).filter(Boolean) : [], size: Number(f.size) || undefined, swiss_rounds: f.format === 'swiss' && Number(f.swiss_rounds) ? Number(f.swiss_rounds) : undefined, deadline_secs: Number(f.deadline_secs) || undefined }), 'Bracket created')}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Format"><Select value={String(f.format)} onValueChange={(v) => setF((x) => ({ ...x, format: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="single">single elimination</SelectItem><SelectItem value="double">double elimination</SelectItem><SelectItem value="swiss">swiss</SelectItem><SelectItem value="round_robin">round robin</SelectItem></SelectContent></Select></Field>
          <Field label="Seeding"><Select value={String(f.seeding)} onValueChange={(v) => setF((x) => ({ ...x, seeding: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="score">by current standings</SelectItem><SelectItem value="join">by arrival order</SelectItem><SelectItem value="random">random</SelectItem><SelectItem value="list">explicit list below</SelectItem></SelectContent></Select></Field>
        </div>
        <Field label={f.seeding === 'list' ? 'Participants — account or team ids, in seed order' : 'Participants are taken from the standings (switch seeding to “explicit list” to paste ids)'}><Textarea className="min-h-16 font-mono text-xs" disabled={f.seeding !== 'list'} value={String(f.participants ?? '')} onChange={(e) => setF((x) => ({ ...x, participants: e.target.value }))} /></Field>
        <div className="grid grid-cols-3 gap-3"><Field label="Size"><Input type="number" value={String(f.size ?? '')} onChange={(e) => setF((x) => ({ ...x, size: e.target.value }))} /></Field><Field label="Swiss rounds (default ⌈log₂ n⌉)"><Input type="number" disabled={f.format !== 'swiss'} value={String(f.swiss_rounds ?? '')} onChange={(e) => setF((x) => ({ ...x, swiss_rounds: e.target.value }))} /></Field><Field label="Match deadline (s)"><Input type="number" value={String(f.deadline_secs ?? '')} onChange={(e) => setF((x) => ({ ...x, deadline_secs: e.target.value }))} /></Field></div>
      </FormDialog>
      <FormDialog open={dlg === 'report'} onOpenChange={(o) => !o && setDlg('')} title="Report result" submit="Report" pending={pending}
        onSubmit={() => run(() => raw('POST', `${T}/bracket/matches/${encodeURIComponent(String(f.mid))}`, { winner: f.winner, score: f.score ? String(f.score) : null }), 'Reported')}>
        <Field label="Winner"><Select value={String(f.winner)} onValueChange={(v) => setF((x) => ({ ...x, winner: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{((f.players ?? []) as string[]).map((id) => <SelectItem key={id} value={id}>{names.get(id) ?? id}</SelectItem>)}</SelectContent></Select></Field>
        <Field label="Score (free text, e.g. 2-1)"><Input value={String(f.score ?? '')} onChange={(e) => setF((x) => ({ ...x, score: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'forfeit'} onOpenChange={(o) => !o && setDlg('')} title="Forfeit" submit="Forfeit" destructive pending={pending}
        onSubmit={() => run(() => raw('POST', `${T}/bracket/matches/${encodeURIComponent(String(f.mid))}/forfeit`, { participant: f.participant }), 'Forfeited')}>
        <Field label="Participant who forfeits (the other side advances)"><Select value={String(f.participant)} onValueChange={(v) => setF((x) => ({ ...x, participant: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{((f.players ?? []) as string[]).map((id) => <SelectItem key={id} value={id}>{names.get(id) ?? id}</SelectItem>)}</SelectContent></Select></Field>
      </FormDialog>
    </div>
  )
}
