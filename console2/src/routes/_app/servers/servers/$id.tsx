import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { ts } from '@/lib/format'
import { useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'

/** One game server: placement, allocation, counters, live metrics, reputation/egress and a log tail. */
export const Route = createFileRoute('/_app/servers/servers/$id')({ component: GameServer })

function GameServer() {
  const { id } = Route.useParams()
  const nav = useNavigate({ from: Route.fullPath })
  const G = `/v1/server/gameservers/${encodeURIComponent(id)}`
  const svQ = useQuery({ queryKey: ['gameserver', id], queryFn: ({ signal }) => raw<Row>('GET', G, undefined, { signal }), refetchInterval: 5000 })
  const sv = svQ.data
  const fleetId = sv?.fleet_id ? String(sv.fleet_id) : null
  const FS = fleetId ? `/v1/server/fleets/${encodeURIComponent(fleetId)}/servers/${encodeURIComponent(id)}` : null
  const egress = useSub(`${G}/egress`)
  const rep = useSub(`${G}/reputation`)
  const metrics = useSub(FS ? `${FS}/metrics` : null)
  const logs = useSub<unknown>(FS ? `${FS}/logs?tail=200` : null)
  const { run } = useAct([['gameserver', id], ['sub']])
  const l = logs.data as Row | string | null | undefined
  const logText = !l ? '' : typeof l === 'string' ? l : (((l.lines ?? l.entries ?? []) as unknown[]).map((e) => (typeof e === 'string' ? e : `${(e as Row).at ?? ''} ${(e as Row).stream ?? ''} ${(e as Row).line ?? (e as Row).message ?? JSON.stringify(e)}`)).join('\n'))
  const fleetTo = fleetId ? `/servers/fleets/${fleetId}?tab=servers` : '/servers/fleets'

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Servers', to: '/servers' }, { title: 'Fleets', to: '/servers/fleets' }, { title: String(sv?.fleet ?? sv?.fleet_id ?? '…'), to: fleetTo }, { title: id.slice(0, 8) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="truncate font-mono text-xl font-semibold">{id}</h1>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">{sv ? <><Badge variant={sv.state === 'ready' || sv.state === 'allocated' ? 'secondary' : 'outline'}>{String(sv.state)}</Badge>{sv.address ? <span className="font-mono">{String(sv.address)}:{String(sv.port ?? '')}</span> : null}{sv.reserved_until ? <Badge variant="outline">reserved until {ts(sv.reserved_until)}</Badge> : null}</> : <Skeleton className="h-4 w-40" />}</div>
        </div>
        {sv && (
          <div className="flex flex-wrap gap-2">
            {sv.reserved_until
              ? <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${G}/unreserve`), 'Unreserved')}>Unreserve</Button>
              : <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${G}/reserve`, { secs: 600 }), 'Reserved for 10 min')}>Reserve</Button>}
            {FS && <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${FS}/logs/archive`), 'Log snapshot requested')}>Archive logs</Button>}
            <Button size="sm" variant="destructive" onClick={() => confirmThen('Terminate this game server?', () => run(async () => { await raw('DELETE', G); void nav({ to: '/servers/fleets/$id', params: { id: String(fleetId) }, search: { tab: 'servers' } }) }, 'Terminated'))}>Terminate</Button>
          </div>
        )}
      </div>
      {svQ.isError && <Alert variant="destructive"><AlertDescription>{String(svQ.error)}</AlertDescription></Alert>}
      {sv && (
        <Card><CardContent className="pt-6">
          <KvGrid>
            <Kv k="Server ID" v={sv.id} mono copy /><Kv k="State" v={sv.state} /><Kv k="Address" v={sv.address ? `${sv.address}:${sv.port}` : '—'} mono copy /><Kv k="Ports" v={sv.ports} mono />
            <Kv k="Fleet / region / pool" v={`${sv.fleet || sv.fleet_id} / ${sv.region || '—'} / ${sv.pool || '—'}`} to={fleetTo} /><Kv k="Build" v={sv.build_version} mono /><Kv k="Provider / host" v={`${sv.provider ?? '—'} / ${sv.hostname || sv.host_id || '—'}`} /><Kv k="PID / container" v={`${sv.pid ?? '—'} / ${sv.container_id ?? '—'}`} mono />
            <Kv k="Players / capacity" v={`${sv.players ?? 0} / ${sv.capacity ?? '—'}`} /><Kv k="Created / ready" v={`${ts(sv.created_at)} / ${ts(sv.ready_at)}`} /><Kv k="Reserved until" v={sv.reserved_until ? ts(sv.reserved_until) : '—'} /><Kv k="Last health" v={sv.last_health_at ? `${sv.health_status ?? ''} ${ts(sv.last_health_at)}` : '—'} />
          </KvGrid>
          <SubHead title="Allocation" /><JsonView value={sv.allocation ?? null} empty="Not allocated" />
          <SubHead title="Counters & lists" /><JsonView value={{ counters: sv.counters ?? {}, lists: sv.lists ?? {}, labels: sv.labels ?? {} }} />
          <SubHead title="Metrics" /><JsonView value={metrics.data} empty="No metrics reported yet" />
          <SubHead title="Reputation & egress" /><JsonView value={{ reputation: rep.data, egress: egress.data }} />
          <SubHead title="Logs"><span className="self-center text-xs text-muted-foreground">tail 200</span></SubHead>
          <pre className="max-h-[480px] overflow-auto rounded-md border bg-muted/40 p-3 font-mono text-xs whitespace-pre-wrap">{logText || 'No log lines yet'}</pre>
          <SubHead title="Exit" /><p className="text-sm text-muted-foreground">{String(sv.exit_reason ?? '—')}{sv.terminated_at ? ` · terminated ${ts(sv.terminated_at)}` : ''}</p>
        </CardContent></Card>
      )}
    </div>
  )
}
