import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'

/** Matches the matchmaker formed (with the matcher's trace per ticket), rejected tickets, queue stats and turn-based matches. */
export const Route = createFileRoute('/_app/social/matches')({
  validateSearch: (s: Record<string, unknown>): { tab?: string; turns?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined, turns: typeof s.turns === 'string' ? s.turns : undefined }),
  component: Matches,
})

const ts = (v: unknown) => (!v ? '—' : (typeof v === 'number' ? new Date(v * (v < 1e12 ? 1000 : 1)) : new Date(String(v))).toISOString().replace('T', ' ').slice(0, 16))
const short = (s: unknown) => (s ? String(s).slice(0, 8) + '…' : '—')
const turnState = (t: Row) => (t.finished_at ? 'finished' : t.current_seat == null ? 'waiting for players' : 'in progress')

function Matches() {
  const { tab = 'matches', turns: tf = 'active' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const matches = useSub<Row>('/v1/server/matchmaker/matches'); const rej = useSub<Row>('/v1/server/matchmaker/rejections'); const stats = useSub<Row>('/v1/server/matchmaker/stats'); const turns = useSub<Row>('/v1/server/turns')
  const ms = listOf(matches.data, 'matches'); const rs = listOf(rej.data, 'rejections'); const allTurns = listOf(turns.data, 'matches')
  const tl = allTurns.filter((t) => tf === 'all' || (tf === 'finished') === !!t.finished_at)
  const [sel, setSel] = useState<Row | null>(null)
  const firstTicket = sel ? String(((sel.tickets ?? []) as unknown[])[0] ?? '') : ''
  const trace = useSub(firstTicket ? `/v1/server/matchmaker/tickets/${encodeURIComponent(firstTicket)}/trace` : null)
  const { run } = useAct([['sub']])
  const waiting = Object.values((stats.data?.waiting ?? {}) as Record<string, number>).reduce((a, b) => a + Number(b), 0)
  const avg = (a: unknown) => (Array.isArray(a) && a.length ? Math.round(a.reduce((x: number, y: unknown) => x + Number(y), 0) / a.length) : 0)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Social', to: '/social' }, { title: 'Matches' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Matches</h1><p className="mt-1 text-sm text-muted-foreground">{ms.length} recent matches · {String(stats.data?.open_matches ?? 0)} open · {rs.length} rejections · {allTurns.length} turn-based</p></div>
        <Button size="sm" variant="outline" asChild><Link to="/social/matchmaker">Queues</Link></Button>
      </div>
      <Tabs value={tab} onValueChange={(v) => void nav({ search: (p) => ({ ...p, tab: v === 'matches' ? undefined : v }), replace: true })}>
        <TabsList className="h-auto max-w-full flex-wrap"><TabsTrigger value="matches">Formed matches</TabsTrigger><TabsTrigger value="rejections">Rejections</TabsTrigger><TabsTrigger value="stats">Queue stats</TabsTrigger><TabsTrigger value="turns">Turn-based</TabsTrigger></TabsList>
        <Card className="mt-3"><CardContent className="pt-6">
          <TabsContent value="matches">
            <div className="grid gap-4 xl:grid-cols-[3fr_2fr]">
              <div className="min-w-0"><DataTable id="mm-matches" rows={ms} loading={matches.isPending} empty="No matches formed recently"
                cols={[{ key: 'created_at', label: 'formed', dim: true, fmt: (m) => ts(m.created_at) }, { key: 'queue', mono: true }, { key: 'region', dim: true }, { key: 'players', fmt: (m) => ((m.accounts ?? []) as unknown[]).length }, { key: 'wait', label: 'avg wait', fmt: (m) => `${avg(m.wait_secs)}s` }]}
                actions={(m) => <Button size="sm" variant={sel?.token === m.token ? 'default' : 'ghost'} onClick={() => setSel(m)}>Trace</Button>} /></div>
              <div className="min-w-0">{sel ? (<>
                <SubHead title={`Match ${short(sel.token)}`} />
                <KvGrid>
                  <Kv k="Players" v={<div className="space-y-0.5">{((sel.accounts ?? []) as unknown[]).map((a) => <Link key={String(a)} to="/players/accounts/$id" params={{ id: String(a) }} className="block font-mono text-xs hover:underline">{String(a)}</Link>)}</div>} />
                  <Kv k="Tickets" v={((sel.tickets ?? []) as unknown[]).map(short).join(', ')} mono />
                  <Kv k="Wait (s)" v={Array.isArray(sel.wait_secs) ? (sel.wait_secs as unknown[]).join(', ') : '—'} />
                  {sel.match_id ? <Kv k="Match" v={<Link to="/$section/$page/$id" params={{ section: 'social', page: 'matches', id: String(sel.match_id) }} search={{ tab: undefined }} className="font-mono text-xs hover:underline">{String(sel.match_id)}</Link>} /> : null}
                </KvGrid>
                <SubHead title="Matcher trace (first ticket)" /><JsonView value={trace.data} empty={trace.isPending ? 'Loading…' : 'No trace kept for this ticket'} />
              </>) : <p className="flex h-full min-h-24 items-center justify-center text-sm text-muted-foreground">Select a match for players and the matcher's trace</p>}</div>
            </div>
          </TabsContent>
          <TabsContent value="rejections">
            <DataTable id="mm-rejections" rows={rs} loading={rej.isPending} empty="No rejected tickets — every ticket satisfied its queue rules" cols={[{ key: 'at', dim: true, fmt: (r) => ts(r.at) }, { key: 'ticket_id', mono: true }, { key: 'queue', mono: true }, { key: 'reason' }]} />
          </TabsContent>
          <TabsContent value="stats">
            <KvGrid><Kv k="Open matches" v={stats.data?.open_matches ?? 0} /><Kv k="Waiting tickets" v={waiting} /><Kv k="Paused queues" v={((stats.data?.paused ?? []) as unknown[]).join(', ') || 'none'} /><Kv k="Draining queues" v={((stats.data?.draining ?? []) as unknown[]).join(', ') || 'none'} /></KvGrid>
            <SubHead title="Raw" /><JsonView value={stats.data} />
          </TabsContent>
          <TabsContent value="turns">
            <div className="mb-3 flex gap-2">
              <Select value={tf} onValueChange={(v) => void nav({ search: (p) => ({ ...p, turns: v === 'active' ? undefined : v }), replace: true })}><SelectTrigger className="h-8 w-32"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="active">active</SelectItem><SelectItem value="finished">finished</SelectItem><SelectItem value="all">all</SelectItem></SelectContent></Select>
              <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', '/v1/server/turns/sweep'), 'Deadlines swept')}>Sweep deadlines</Button>
            </div>
            <DataTable id="turns" rows={tl} loading={turns.isPending} empty="No turn-based matches" rowTo={(t) => `/social/turns/${t.id}`}
              cols={[{ key: 'game', mono: true }, { key: 'id', mono: true, dim: true, fmt: (t) => short(t.id) }, { key: 'players', fmt: (t) => `${((t.players ?? []) as unknown[]).length} / ${t.max_players ?? '?'}` }, { key: 'state', fmt: (t) => `${turnState(t)}${t.result ? ` · ${typeof t.result === 'string' ? t.result : JSON.stringify(t.result)}` : ''}` }, { key: 'turn', fmt: (t) => t.turn ?? t.turn_no ?? '—' }, { key: 'deadline_at', dim: true, fmt: (t) => ts(t.deadline_at ?? t.deadline) }]}
              actions={(t) => <span className="flex justify-end gap-1">{!t.finished_at && <Button size="sm" variant="ghost" onClick={() => confirmThen('Finish this match without a winner?', () => run(() => raw('POST', `/v1/server/turns/${t.id}/finish`, { winner_seat: null }), 'Finished'))}>Finish</Button>}<Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen('Delete match?', () => run(() => raw('DELETE', `/v1/server/turns/${t.id}`), 'Deleted'))}>×</Button></span>} />
          </TabsContent>
        </CardContent></Card>
      </Tabs>
    </div>
  )
}
