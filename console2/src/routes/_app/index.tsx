// Landing page: is the cluster healthy, who is online, where worlds and matches run, what needs
// attention, what changed last. Every card is its own query and fails on its own; numbers link to
// the filtered page behind them; the period lives in ?days=.
import { Link, createFileRoute } from '@tanstack/react-router'
import { useQueries, useQuery, type UseQueryResult } from '@tanstack/react-query'
import { z } from 'zod'
import { AlertTriangle, CircleAlert, Info } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { ApiError, raw } from '@/lib/api'
import { nodeQuery } from '@/lib/node'
import { ago, pct, short, uptime } from '@/lib/format'
import { cn } from '@/lib/utils'

const search = z.object({ days: z.union([z.literal(7), z.literal(14), z.literal(30)]).default(7) })
export const Route = createFileRoute('/_app/')({ validateSearch: search, component: Dashboard })

type Any = Record<string, any>
type Q = UseQueryResult<Any>
const q = (key: string, path: string) => ({
  queryKey: ['dash', key, path],
  queryFn: ({ signal }: { signal: AbortSignal }) => raw<Any>('GET', path, undefined, { signal }),
  refetchInterval: 15_000,
})

function failText(e: unknown): string {
  return e instanceof ApiError && e.status === 403 ? 'not visible to your role' : e instanceof Error ? e.message : String(e)
}

function Stat({ to, search, label, value, sub, bad }: { to: string; search?: Any; label: string; value: React.ReactNode; sub?: React.ReactNode; bad?: boolean }) {
  return (
    <Link to={to} search={search} className="block">
      <Card className="h-full min-w-0 gap-1 py-4 transition-colors hover:bg-accent/40">
        <CardHeader className="px-4"><CardTitle className="text-xs font-medium uppercase tracking-wide text-muted-foreground">{label}</CardTitle></CardHeader>
        <CardContent className="px-4">
          <div className={cn('text-2xl font-semibold tabular-nums', bad && 'text-destructive')}>{value}</div>
          {sub && <div className="mt-0.5 truncate text-xs text-muted-foreground">{sub}</div>}
        </CardContent>
      </Card>
    </Link>
  )
}

interface Col<R> { key: string; label?: string; fmt?: (r: R) => React.ReactNode; dim?: boolean; mono?: boolean }
function Panel<R extends Any>({ title, count, to, cols, rows, q, onRow, empty }: { title: string; count?: number; to: string; cols: Col<R>[]; rows: R[]; q: UseQueryResult<Any>; onRow: (r: R) => string; empty: string }) {
  return (
    <Card className="min-w-0 gap-2 py-4">
      <CardHeader className="flex-row items-baseline justify-between px-4">
        <CardTitle className="text-sm">{title} {count != null && <span className="font-normal text-muted-foreground">({count})</span>}</CardTitle>
        <Link to={to} className="text-xs text-muted-foreground hover:underline">all →</Link>
      </CardHeader>
      <CardContent className="px-0">
        {q.isError ? <p className="px-4 text-sm text-destructive">{failText(q.error)}</p>
        : q.isPending ? <div className="grid gap-2 px-4">{[0, 1, 2].map((i) => <Skeleton key={i} className="h-5 w-full" />)}</div>
        : !rows.length ? <p className="px-4 text-sm text-muted-foreground">{empty}</p>
        : (
          <div className="overflow-x-auto">
            <Table>
              <TableHeader><TableRow>{cols.map((c) => <TableHead key={c.key} className="h-8 text-xs">{c.label ?? c.key.replace(/_/g, ' ')}</TableHead>)}</TableRow></TableHeader>
              <TableBody>
                {rows.map((r, i) => (
                  <TableRow key={i} className="cursor-pointer">
                    {cols.map((c, j) => (
                      <TableCell key={c.key} className={cn('py-1.5 text-sm', c.dim && 'text-muted-foreground', c.mono && 'font-mono text-xs')}>
                        {j === 0 ? <Link to={onRow(r)} className="block hover:underline">{c.fmt ? c.fmt(r) : String(r[c.key] ?? '')}</Link> : (c.fmt ? c.fmt(r) : String(r[c.key] ?? ''))}
                      </TableCell>
                    ))}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function Dashboard() {
  const { days } = Route.useSearch()
  const nav = Route.useNavigate()
  const node = useQuery({ ...nodeQuery, refetchInterval: 15_000 })
  const [nodes, raft, worlds, matches, fleets, mm, au, rev, cr, audit, sanctions, hooks] = useQueries({
    queries: [
      q('nodes', '/v1/server/cluster/nodes'), q('raft', '/v1/server/cluster/raft'), q('worlds', '/v1/admin/cluster/worlds'),
      q('matches', '/v1/admin/cluster/matches'), q('fleets', '/v1/server/fleets'), q('mm', '/v1/server/matchmaker/stats'),
      q('au', `/v1/server/analytics/kpi/active_users?days=${days}`), q('rev', `/v1/server/analytics/kpi/revenue?days=${days}`),
      q('cr', '/v1/server/crashes/summary'), q('audit', '/v1/admin/audit?limit=12'), q('sanctions', '/v1/admin/sanctions?limit=6'), q('hooks', '/v1/admin/webhooks'),
    ],
  }) as unknown as [Q, Q, Q, Q, Q, Q, Q, Q, Q, Q, Q, Q]
  const n = node.data as Any | undefined
  const nodeRows: Any[] = (nodes.data?.nodes ?? []).map((x: Any) => ({ ...x, me: x.id === nodes.data?.self }))
  const alive = nodeRows.filter((x) => x.alive).length
  const worldRows: Any[] = worlds.data?.worlds ?? []
  const localWorlds: string[] = worlds.data?.local ?? []
  const unplaced = worldRows.filter((w) => !w.node_id || !nodeRows.some((x) => x.id === w.node_id && x.alive))
  const matchRows: Any[] = matches.data?.matches ?? []
  const fleetRows: Any[] = fleets.data?.fleets ?? []
  const m = mm.data ?? {}
  const queueNames = [...new Set<string>([...(m.queues ?? []), ...Object.keys(m.waiting ?? {})])]
  const queues = queueNames.map((qn) => ({ queue: qn, waiting: m.waiting?.[qn] ?? 0, paused: !!m.paused?.[qn] }))
  const waiting = Object.values<number>(m.waiting ?? {}).reduce((a, b) => a + b, 0)
  const daily: Any[] = rev.data?.daily ?? []
  const revenue = daily.reduce((a, d) => a + (d.amount ?? 0), 0)
  const purchases = daily.reduce((a, d) => a + (d.purchases ?? 0), 0)
  const hooksBad: Any[] = (hooks.data?.webhooks ?? []).filter((h: Any) => h.disabled || (h.failures ?? h.consecutive_failures ?? 0) > 0)
  const nodeName = (id?: string | null) => (id ? nodeRows.find((x) => x.id === id)?.name ?? short(id) : '—')

  const attention: { level: 'bad' | 'warn' | 'dim'; text: string; to: string }[] = []
  for (const x of nodeRows.filter((x) => !x.alive)) attention.push({ level: 'bad', text: `node ${x.name} is not heartbeating (last ${ago(x.heartbeat_at)})`, to: `/cluster/nodes/${encodeURIComponent(x.id)}` })
  for (const w of unplaced) attention.push({ level: 'bad', text: `world ${w.name ?? w.world} has no live node`, to: `/world/worlds/${encodeURIComponent(w.name ?? w.world)}` })
  if (cr.data && cr.data.crash_free_users_pct < 99) attention.push({ level: 'warn', text: `crash-free users at ${Number(cr.data.crash_free_users_pct).toFixed(1)}% over 24 h (${cr.data.crashes} crashes)`, to: '/players/crashes?status=open' })
  for (const h of hooksBad) attention.push({ level: 'warn', text: `webhook ${h.url ?? h.id} ${h.disabled ? 'is disabled' : `failing (${h.failures ?? h.consecutive_failures} in a row)`}`, to: `/liveops/webhooks/${encodeURIComponent(h.id)}` })
  for (const qq of queues) if (qq.paused) attention.push({ level: 'warn', text: `matchmaking queue "${qq.queue}" is paused`, to: `/social/matchmaker?queue=${encodeURIComponent(qq.queue)}` })
  if (nodeRows.length > 1 && raft.data && !raft.data.enabled) attention.push({ level: 'warn', text: 'several nodes but Raft is off — no shared directory / leader', to: '/cluster' })
  const named: [string, UseQueryResult<Any>][] = [['nodes', nodes], ['raft', raft], ['worlds', worlds], ['matches', matches], ['fleets', fleets], ['matchmaker', mm], ['active users', au], ['revenue', rev], ['crashes', cr], ['audit', audit], ['sanctions', sanctions], ['webhooks', hooks]]
  for (const [k, r] of named) if (r.isError && !(r.error instanceof ApiError && r.error.status === 403)) attention.push({ level: 'dim', text: `could not load ${k}: ${failText(r.error)}`, to: '/settings/config' })
  const anyPending = named.some(([, r]) => r.isPending)
  const updatedAt = Math.max(...named.map(([, r]) => r.dataUpdatedAt))

  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-end justify-between gap-2">
        <div>
          <h1 className="text-xl font-semibold">{n?.project ?? 'musubi'} {n?.environment && <span className="text-base font-normal text-muted-foreground">{n.environment}</span>}</h1>
          <div className="text-sm text-muted-foreground">{n ? `${n.name ?? ''} · ${n.region ?? ''} · v${n.version ?? '?'} · up ${uptime(n.uptime_secs)}` : node.isError ? failText(node.error) : <Skeleton className="inline-block h-4 w-64" />}</div>
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span>period</span>
          <Select value={String(days)} onValueChange={(v) => void nav({ search: { days: Number(v) as 7 | 14 | 30 } })}>
            <SelectTrigger size="sm" className="w-28"><SelectValue /></SelectTrigger>
            <SelectContent>{[7, 14, 30].map((d) => <SelectItem key={d} value={String(d)}>{d} days</SelectItem>)}</SelectContent>
          </Select>
          <span>{anyPending ? 'loading…' : updatedAt > 0 ? `updated ${ago(updatedAt)}` : ''}</span>
        </div>
      </div>

      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <Stat to="/cluster" label="Nodes" bad={nodeRows.length > 0 && alive < nodeRows.length} value={nodes.isPending ? <Skeleton className="h-7 w-10" /> : alive} sub={`/ ${nodeRows.length} alive · ${raft.data?.enabled ? 'raft on' : 'single-node directory'}`} />
        <Stat to="/social/sessions" label="Online now" value={n?.realtime_sessions ?? '—'} sub={`realtime sessions on this node · ${n?.parties ?? 0} parties`} />
        <Stat to="/world" label="Worlds" value={worlds.isPending ? <Skeleton className="h-7 w-10" /> : worldRows.length || localWorlds.length} sub={<>{localWorlds.length} on this node{unplaced.length > 0 && <> · <b className="text-destructive">{unplaced.length} unplaced</b></>}</>} />
        <Stat to="/cluster/matches" label="Matches" value={matches.isPending ? <Skeleton className="h-7 w-10" /> : matchRows.length} sub={`${n?.authoritative_matches ?? 0} authoritative · ${n?.relayed_matches ?? 0} relayed on this node`} />
        <Stat to="/social/matchmaker" label="Matchmaking" value={mm.isPending ? <Skeleton className="h-7 w-10" /> : waiting} sub={`waiting in ${queues.length} queue${queues.length === 1 ? '' : 's'} · ${m.matched_total ?? m.matched ?? 0} matched`} />
        <Stat to="/analytics" search={{ days }} label="DAU / MAU" value={au.isPending ? <Skeleton className="h-7 w-10" /> : au.data?.dau ?? '—'} sub={`/ ${au.data?.mau ?? '—'} · stickiness ${pct(au.data?.stickiness)}`} />
        <Stat to="/analytics/revenue" search={{ days }} label={`Revenue ${days} d`} value={rev.isPending ? <Skeleton className="h-7 w-10" /> : revenue.toLocaleString()} sub={`${purchases} purchases · all currencies`} />
        <Stat to="/players/crashes" search={{ status: 'open' }} label="Crash-free 24 h" bad={cr.data && cr.data.crash_free_users_pct < 99} value={cr.isPending ? <Skeleton className="h-7 w-10" /> : cr.data?.crash_free_users_pct != null ? `${Number(cr.data.crash_free_users_pct).toFixed(1)}%` : '—'} sub={`${cr.data?.crashes ?? 0} crashes · ${cr.data?.active_users ?? 0} users seen`} />
      </div>

      <Card className="gap-2 py-4">
        <CardHeader className="px-4"><CardTitle className="text-sm">Needs attention {attention.length > 0 && <span className="font-normal text-muted-foreground">({attention.length})</span>}</CardTitle></CardHeader>
        <CardContent className="px-4">
          {anyPending && !attention.length ? <Skeleton className="h-5 w-72" />
          : !attention.length ? <p className="text-sm text-muted-foreground">Nothing — nodes heartbeat, worlds are placed, webhooks deliver, queues run.</p>
          : <ul className="grid gap-1 text-sm">{attention.map((a, i) => (
              <li key={i}><Link to={a.to} className={cn('inline-flex items-center gap-2 hover:underline', a.level === 'bad' && 'text-destructive', a.level === 'dim' && 'text-muted-foreground')}>
                {a.level === 'bad' ? <CircleAlert className="size-4" /> : a.level === 'warn' ? <AlertTriangle className="size-4" /> : <Info className="size-4" />}{a.text}</Link></li>
            ))}</ul>}
        </CardContent>
      </Card>

      <div className="grid min-w-0 gap-4 xl:grid-cols-2">
        <Panel title="Cluster" count={nodeRows.length} to="/cluster" q={nodes} rows={nodeRows} empty="No nodes registered." onRow={(r) => `/cluster/nodes/${encodeURIComponent(r.id)}`}
          cols={[{ key: 'name', fmt: (r) => r.name + (r.me ? ' (this)' : '') }, { key: 'region', dim: true }, { key: 'roles', fmt: (r) => (r.roles ?? []).join(', '), dim: true }, { key: 'alive', fmt: (r) => (r.alive ? 'alive' : 'down') }, { key: 'heartbeat_at', label: 'heartbeat', fmt: (r) => ago(r.heartbeat_at), dim: true }]} />
        <Panel title="Fleets" count={fleetRows.length} to="/servers" q={fleets} rows={fleetRows.slice(0, 8)} empty="No fleets — create one under Game Servers." onRow={(r) => `/servers/fleets/${encodeURIComponent(r.id)}`}
          cols={[{ key: 'name' }, { key: 'region', dim: true }, { key: 'provider', dim: true }, { key: 'min_servers', label: 'min / max', fmt: (r) => `${r.min_servers} / ${r.max_servers}` }, { key: 'buffer' }, { key: 'created_at', fmt: (r) => ago(r.created_at), dim: true }]} />
        <Panel title="Recent audit" to="/settings/audit" q={audit} rows={audit.data?.entries ?? []} empty="No operator actions yet." onRow={(r) => `/settings/audit?q=${encodeURIComponent(r.actor ?? '')}`}
          cols={[{ key: 'at', fmt: (r) => ago(r.at), dim: true }, { key: 'actor' }, { key: 'method', mono: true }, { key: 'path', mono: true }]} />
        <Panel title="Sanctions" to="/settings/sanctions" q={sanctions} rows={sanctions.data?.sanctions ?? []} empty="No sanctions." onRow={(r) => `/settings/sanctions/${encodeURIComponent(r.id)}`}
          cols={[{ key: 'created_at', fmt: (r) => ago(r.created_at), dim: true }, { key: 'action' }, { key: 'account_id', fmt: (r) => short(r.account_id), mono: true }, { key: 'message', fmt: (r) => r.message || r.reason || '' }]} />
      </div>
      <Panel title="Live matches" count={matchRows.length} to="/cluster/matches" q={matches} rows={matchRows.slice(0, 10)} empty="No authoritative matches running." onRow={(r) => `/cluster/matches/${encodeURIComponent(r.match_id)}`}
        cols={[{ key: 'label' }, { key: 'module', mono: true }, { key: 'match_id', fmt: (r) => short(r.match_id), mono: true }, { key: 'node_id', label: 'node', fmt: (r) => nodeName(r.node_id) }, { key: 'tick' }, { key: 'updated_at', fmt: (r) => ago(r.updated_at), dim: true }]} />
    </div>
  )
}
