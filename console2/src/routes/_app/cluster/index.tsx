import { Link, createFileRoute } from '@tanstack/react-router'
import { queryOptions, useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { auth } from '@/lib/auth'
import { useAct, confirmThen } from '@/lib/act'
import { ago, ts } from '@/lib/format'
import { fmtN, mb, series, sum, useMetrics } from '@/lib/metrics'
import { listOf } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'

/** Cluster at a glance: membership + raft + bus/fan-out rates of this node, from the node's own
 * cluster endpoints and its Prometheus text. Replaces the plain resource card grid for this section. */
export const Route = createFileRoute('/_app/cluster/')({ component: ClusterOverview })

function Stat({ k, v, sub, bad, to }: { k: string; v: React.ReactNode; sub?: React.ReactNode; bad?: boolean; to?: string }) {
  const body = (
    <Card className={to ? 'h-full transition-colors hover:bg-accent/40' : 'h-full'}><CardContent className="p-4">
      <div className="text-xs text-muted-foreground">{k}</div>
      <div className={`mt-1 flex flex-wrap items-baseline gap-2 text-2xl font-semibold tabular-nums ${bad ? 'text-destructive' : ''}`}>{v}{sub && <span className="text-xs font-normal text-muted-foreground">{sub}</span>}</div>
    </CardContent></Card>
  )
  return to ? <Link to={to} className="block">{body}</Link> : body
}

function ClusterOverview() {
  const [live, setLive] = useState(true)
  const poll = live ? 1000 : false
  const q = (key: string, path: string) => queryOptions({ queryKey: ['cluster', key], queryFn: ({ signal }) => raw<Row>('GET', path, undefined, { signal }), refetchInterval: poll })
  const nodesQ = useQuery(q('nodes', '/v1/server/cluster/nodes'))
  const raftQ = useQuery(q('raft', '/v1/server/cluster/raft'))
  const meshQ = useQuery({ ...q('mesh', '/v1/server/cluster/mesh_net'), retry: false })
  const worldsQ = useQuery(q('worlds', '/v1/admin/cluster/worlds'))
  const matchesQ = useQuery(q('matches', '/v1/admin/cluster/matches'))
  const { tot, rate } = useMetrics(live)
  const { run } = useAct([['cluster']])
  const [dirKey, setDirKey] = useState('')
  const [dir, setDir] = useState<unknown>(null)
  const [profiling, setProfiling] = useState(false)

  const raft = (raftQ.data ?? {}) as Row
  const voters = ((raft.voters ?? []) as unknown[]).map(String)
  const rawNodes = listOf(nodesQ.data, 'nodes')
  const worlds = listOf(worldsQ.data, 'worlds')
  const matches = listOf(matchesQ.data, 'matches')
  const byRaft = useMemo(() => { const m: Record<string, Row> = {}; for (const n of rawNodes) if (n.raft_id != null) m[String(n.raft_id)] = n; return m }, [rawNodes])
  const raftRole = (n: Row) => (!raft.enabled ? '—' : String(n.raft_id) === String(raft.leader) ? 'leader' : voters.includes(String(n.raft_id)) ? 'voter' : 'learner')
  const nodes = useMemo(() => rawNodes.map((n): Row => ({ ...n, me: n.id === nodesQ.data?.self, raft: raftRole(n), worlds: worlds.filter((w) => w.node_id === n.id).length, matches: matches.filter((m) => m.node_id === n.id).length })), [rawNodes, nodesQ.data, worlds, matches, raft]) // eslint-disable-line react-hooks/exhaustive-deps
  const alive = nodes.filter((n) => n.alive).length
  const leaderName = raft.leader != null ? (byRaft[String(raft.leader)]?.name ?? '…' + String(raft.leader).slice(-6)) : '—'
  const unplaced = worlds.filter((w) => !w.node_id || !nodes.some((n) => n.id === w.node_id && n.alive))

  const bus = useMemo(() => {
    const rows: Row[] = []
    for (const s of series(tot, 'musubi_bus_messages_total')) {
      const { transport = '', dir = '', ok = '' } = s.labels
      let r = rows.find((x) => x.transport === transport && x.dir === dir)
      if (!r) { r = { transport, dir, msgs: 0, failed: 0, bytes: sum(tot, 'musubi_bus_bytes_total', { transport, dir }), msgs_s: 0, bytes_s: sum(rate, 'musubi_bus_bytes_total', { transport, dir }) }; rows.push(r) }
      if (ok === 'false') r.failed += s.v; else r.msgs += s.v
      r.msgs_s += sum(rate, 'musubi_bus_messages_total', { transport, dir, ok })
    }
    return rows
  }, [tot, rate])
  const busTx = bus.filter((b) => b.dir === 'tx').reduce((a, b) => ({ msgs: a.msgs + b.msgs_s, bytes: a.bytes + b.bytes_s }), { msgs: 0, bytes: 0 })
  const busRx = bus.filter((b) => b.dir === 'rx').reduce((a, b) => ({ msgs: a.msgs + b.msgs_s, bytes: a.bytes + b.bytes_s }), { msgs: 0, bytes: 0 })
  const lanes = useMemo(() => {
    const names = new Set<string>()
    for (const s of [...series(tot, 'musubi_fanout_published_total'), ...series(tot, 'musubi_fanout_queue_depth')]) names.add(s.labels.lane ?? '')
    return [...names].sort().map((lane) => {
      const kinds = series(rate, 'musubi_fanout_published_total').filter((s) => s.labels.lane === lane).sort((a, b) => b.v - a.v)
      const ms = (phase: string) => { const c = sum(rate, 'musubi_fanout_batch_ms_count', { lane, phase }); return c ? sum(rate, 'musubi_fanout_batch_ms_sum', { lane, phase }) / c : null }
      return { lane, published: sum(tot, 'musubi_fanout_published_total', { lane }), rate: sum(rate, 'musubi_fanout_published_total', { lane }), depth: sum(tot, 'musubi_fanout_queue_depth', { lane }), serialize: ms('serialize'), send: ms('send'), top: kinds.filter((k) => k.v > 0).slice(0, 3).map((k) => `${k.labels.kind} ${fmtN(k.v)}/s`).join(' · ') }
    })
  }, [tot, rate])
  const sessions = sum(tot, 'musubi_realtime_sessions')

  async function lookup() {
    const k = dirKey.trim(); if (!k) return
    try { setDir(await raw('GET', `/v1/server/cluster/directory/${encodeURIComponent(k)}`)) } catch (e) { setDir({ key: k, error: String(e) }) }
  }
  async function profile() {
    setProfiling(true)
    try {
      const r = await fetch((auth.base || '') + '/v1/admin/debug/profile', { headers: { 'x-musubi-server-key': auth.key } })
      if (!r.ok) throw new Error(`HTTP ${r.status}`)
      window.open(URL.createObjectURL(await r.blob()), '_blank'); toast.success('Flame graph opened')
    } catch (e) { toast.error(String(e)) } finally { setProfiling(false) }
  }

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Cluster' }]} />
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Cluster</h1><p className="mt-1 text-xs text-muted-foreground">Membership, raft directory and the bus of this node. Sampled every second while live.</p></div>
        <label className="flex items-center gap-1.5 text-xs text-muted-foreground"><Checkbox checked={live} onCheckedChange={(v) => setLive(v === true)} /> live</label>
      </div>
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-5">
        <Stat k="Nodes alive" v={alive} bad={alive < nodes.length} sub={`of ${nodes.length} · ${new Set(nodes.map((n) => n.region)).size} regions`} to="/cluster/nodes" />
        <Stat k="Raft directory" v={raft.enabled ? leaderName : 'off'} sub={raft.enabled ? `${raft.is_leader ? 'this node leads' : 'leader'} · ${voters.length} voters` : 'single node / no bus'} />
        <Stat k="Bus out / in" v={fmtN(busTx.msgs)} sub={`msg/s · ${mb(busTx.bytes)}/s out · ${fmtN(busRx.msgs)} msg/s in`} />
        <Stat k="Realtime sessions" v={fmtN(sessions)} sub="on this node" to="/players/sessions" />
        <Stat k="Worlds / matches" v={worlds.length} sub={`${listOf(worldsQ.data, 'local').length} homed here · ${matches.length} matches, ${String(matchesQ.data?.local ?? 0)} here`} to="/cluster/worlds" />
      </div>

      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Nodes <span className="font-normal text-muted-foreground">({nodes.length})</span></CardTitle></CardHeader><CardContent>
        <DataTable id="cluster-nodes" rows={nodes} loading={nodesQ.isPending} empty="No nodes registered" rowTo={(r) => `/cluster/nodes/${r.id}`}
          cols={[{ key: 'name', fmt: (r) => <>{r.name}{r.me ? <Badge variant="outline" className="ml-1.5">this</Badge> : null}</> }, { key: 'state', fmt: (r) => (r.alive ? r.state : <span className="text-destructive">dead</span>) }, { key: 'raft' }, { key: 'roles', fmt: (r) => ((r.roles ?? []) as string[]).join(' · ') }, { key: 'region', fmt: (r) => `${r.region}${r.group && r.group !== 'default' ? ' / ' + r.group : ''}` }, { key: 'worlds', label: 'worlds homed' }, { key: 'matches' }, { key: 'heartbeat_at', label: 'heartbeat', dim: true, fmt: (r) => ago(r.heartbeat_at) }, { key: 'started_at', label: 'up since', dim: true, fmt: (r) => ts(r.started_at) }, { key: 'version' }, { key: 'advertise', label: 'http / bus', mono: true, fmt: (r) => `${r.advertise}${r.bus_addr ? ' · ' + r.bus_addr : ' · no QUIC bus (PG notify)'}` }]}
          actions={(r) => (r.me ? null : <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Drain ${r.name}? Its worlds and leases move to other nodes.`, () => run(() => raw('POST', `/v1/admin/cluster/nodes/${r.id}/drain`), 'Drain requested'))}>Drain</Button>)} />
        <p className="mt-3 text-xs text-muted-foreground">State is the SWIM verdict (alive · suspect · dead) confirmed by the heartbeat row; a dead node's worlds and matches are adopted by the others. A node without a bus address falls back to Postgres NOTIFY for cluster traffic.</p>
        {unplaced.length > 0 && <><SubHead title="Worlds awaiting adoption" count={unplaced.length} /><div className="flex flex-wrap gap-1.5">{unplaced.map((w) => <Link key={String(w.name)} to="/world/worlds/$w" params={{ w: String(w.name) }}><Badge variant="destructive" className="font-mono">{String(w.name)}</Badge></Link>)}</div></>}
      </CardContent></Card>

      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Bus & fan-out <span className="font-normal text-muted-foreground">(this node)</span></CardTitle></CardHeader><CardContent>
        <SubHead title="Transport" />
        <DataTable id="cluster-bus" tools={false} rows={bus} empty="No bus traffic yet"
          cols={[{ key: 'transport' }, { key: 'dir', fmt: (r) => (r.dir === 'tx' ? 'out' : 'in') }, { key: 'msgs_s', label: 'msg/s', fmt: (r) => fmtN(r.msgs_s) }, { key: 'bytes_s', label: 'bytes/s', fmt: (r) => mb(r.bytes_s) }, { key: 'msgs', label: 'total', dim: true, fmt: (r) => fmtN(r.msgs) }, { key: 'failed', fmt: (r) => (r.failed ? <span className="text-destructive">{fmtN(r.failed)}</span> : '0') }, { key: 'bytes', label: 'total bytes', dim: true, fmt: (r) => mb(r.bytes) }]} />
        <SubHead title="Lanes" />
        <DataTable id="cluster-lanes" tools={false} rows={lanes} empty="No lanes active"
          cols={[{ key: 'lane' }, { key: 'rate', label: 'published/s', fmt: (r) => fmtN(r.rate) }, { key: 'depth', label: 'queue depth', fmt: (r) => String(r.depth) }, { key: 'serialize', label: 'serialize ms', fmt: (r) => (r.serialize == null ? '—' : Number(r.serialize).toFixed(2)) }, { key: 'send', label: 'send ms', fmt: (r) => (r.send == null ? '—' : Number(r.send).toFixed(2)) }, { key: 'top', label: 'top kinds now', dim: true, fmt: (r) => r.top || 'idle' }, { key: 'published', label: 'total', dim: true, fmt: (r) => fmtN(r.published) }]} />
        <p className="mt-3 text-xs text-muted-foreground">Lanes are FIFO per priority (control before session before user); a growing queue depth means the bus cannot drain this node's publications — look at send ms and the peer's state first.</p>
      </CardContent></Card>

      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Raft directory</CardTitle></CardHeader><CardContent>
        {raft.enabled ? (
          <KvGrid>
            <Kv k="Leader" v={leaderName} /><Kv k="This node" v={raft.is_leader ? 'leader' : voters.includes(String(raft.raft_id)) ? 'voter' : 'learner'} />
            <Kv k="Voters" v={voters.map((v) => byRaft[v]?.name ?? '…' + v.slice(-6)).join(', ')} /><Kv k="Replication" v={raft.is_leader ? 'this node replicates the directory to the voters' : 'follows the leader'} />
            <Kv k="Quorum" v={`${Math.floor(voters.length / 2) + 1} of ${voters.length}`} /><Kv k="Dead voters" v={voters.filter((v) => byRaft[v] && !byRaft[v]?.alive).length || 'none'} />
          </KvGrid>
        ) : <p className="text-sm text-muted-foreground">Raft is off: one node, or <code>[cluster]</code> without a bus. World cells and their hosts are then recorded in Postgres (<code>musubi.world_cell_hosts</code>) instead of the replicated directory.</p>}
        <form className="mt-4 flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); void lookup() }}>
          <Input className="min-w-72 max-w-md font-mono" value={dirKey} placeholder="directory key, e.g. world:<name>" onChange={(e) => setDirKey(e.target.value)} />
          <Button type="submit" size="sm" variant="outline" disabled={!dirKey.trim()}>Look up</Button>
        </form>
        {dir != null && <div className="mt-3"><JsonView value={dir} /></div>}
      </CardContent></Card>

      <Card><CardHeader className="pb-2"><CardTitle className="text-base">This node</CardTitle></CardHeader><CardContent className="flex flex-wrap items-center gap-2">
        <Button size="sm" variant="outline" disabled={profiling} onClick={() => void profile()}>{profiling ? 'profiling ~10 s…' : 'CPU flame graph'}</Button>
        <Button size="sm" variant="outline" asChild><Link to="/$section/$page" params={{ section: 'cluster', page: 'logs' }}>Logs</Link></Button>
        <Button size="sm" variant="outline" asChild><Link to="/$section/$page" params={{ section: 'cluster', page: 'loadtests' }}>Load tests</Link></Button>
        <span className="text-xs text-muted-foreground">mesh network: {meshQ.data?.enabled ? `on · ${listOf(meshQ.data, 'peers').length} peers` : 'off'} · <Link to="/$section/$page" params={{ section: 'cluster', page: 'mesh_net' }} className="underline">details</Link></span>
      </CardContent></Card>
    </div>
  )
}
