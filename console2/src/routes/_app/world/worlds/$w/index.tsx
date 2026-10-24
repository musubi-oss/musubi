import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { qs, raw } from '@/lib/api'
import { auth } from '@/lib/auth'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { CellMap, type MapCell } from '@/components/cell-map'
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
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

const TABS: [string, string][] = [
  ['overview', 'Overview'], ['cells', 'Cells'], ['sims', 'Simulators & leases'], ['entities', 'Entities'],
  ['handovers', 'Handovers'], ['journal', 'Journal'], ['at', 'Time travel'], ['view', 'Cell view'], ['config', 'Config'],
]
type Dlg = '' | 'host' | 'rewind' | 'spawn'
const OK_PATHS = new Set(['local', 'remote', 'forced', 'accepted', 'completed', 'forced_take', 'have_it'])
const enc = encodeURIComponent

export const Route = createFileRoute('/_app/world/worlds/$w/')({
  validateSearch: (s: Record<string, unknown>): { tab?: string; entity?: string } => ({
    tab: typeof s.tab === 'string' ? s.tab : undefined,
    entity: typeof s.entity === 'string' ? s.entity : undefined,
  }),
  component: WorldPage,
})

export function cellKey(c: unknown): string {
  if (!c) return '—'
  if (Array.isArray(c)) return c.join(',')
  const o = c as Row
  return o.id ?? o.key ?? (o.x != null ? `${o.x},${o.y}${o.z != null ? ',' + o.z : ''}${o.level != null ? ' L' + o.level : ''}` : JSON.stringify(c))
}
const sameCell = (a: Row | undefined, b: Row | undefined) => !!a && !!b && a.x === b.x && a.y === b.y
const fmtMs = (ms: number) => new Date(ms).toISOString().slice(11, 23)

function metric(text: string, name: string, w: string) {
  const re = new RegExp(`^${name}\\{[^}]*world="${w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}"[^}]*\\}\\s+([0-9.e+]+)`, 'm')
  const m = text.match(re)
  return m ? Number(m[1]) : 0
}

/** ticks / handovers / frames per second from the node's tick counter + Prometheus text, sampled every second. */
function useRates(w: string, tick: number | undefined, live: boolean) {
  const prev = useRef<{ at: number; tick: number; ho: number; fr: number } | null>(null)
  const [rates, setRates] = useState<{ ticks: number; handovers: number; frames: number } | null>(null)
  const m = useQuery({
    queryKey: ['metrics-text'],
    queryFn: async ({ signal }) => {
      try {
        const r = await fetch((auth.base || '') + '/metrics', { headers: { 'x-musubi-server-key': auth.key }, signal })
        return r.ok ? await r.text() : ''
      } catch { return '' }
    },
    refetchInterval: live ? 1000 : false,
    retry: false,
  })
  useEffect(() => {
    if (tick === undefined || m.data === undefined) return
    const ho = metric(m.data, 'musubi_world_handovers_total', w), fr = metric(m.data, 'musubi_world_frames_total', w)
    const now = performance.now(), p = prev.current
    if (p && now - p.at > 400) { const dt = (now - p.at) / 1000; setRates({ ticks: (tick - p.tick) / dt, handovers: (ho - p.ho) / dt, frames: (fr - p.fr) / dt }) }
    prev.current = { at: now, tick, ho, fr }
  }, [m.dataUpdatedAt, m.data, tick, w])
  return rates
}

function Stat({ k, v, sub, warn }: { k: string; v: React.ReactNode; sub?: React.ReactNode; warn?: 'warn' | 'bad' }) {
  return (
    <Card><CardContent className="p-4">
      <div className="text-xs text-muted-foreground">{k}</div>
      <div className="mt-1 flex flex-wrap items-baseline gap-2 text-2xl font-semibold tabular-nums">{v}{sub && <span className={`text-xs font-normal ${warn === 'bad' ? 'text-destructive' : warn === 'warn' ? 'text-amber-500' : 'text-muted-foreground'}`}>{sub}</span>}</div>
    </CardContent></Card>
  )
}

function WorldPage() {
  const { w } = Route.useParams()
  const { tab = 'overview', entity: entityParam } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const S = `/v1/server/worlds/${enc(w)}`, A = `/v1/admin/worlds/${enc(w)}`
  const [live, setLive] = useState(true)
  const [dlg, setDlg] = useState<Dlg>('')
  const [form, setForm] = useState<Row>({})
  const [verify, setVerify] = useState<Row | null>(null)
  const [entQ, setEntQ] = useState(''); const [entApplied, setEntApplied] = useState('')
  const [hoEntity, setHoEntity] = useState(entityParam ?? ''); const [hoApplied, setHoApplied] = useState(entityParam ?? '')
  const [hoLimit, setHoLimit] = useState(500); const [hoGroup, setHoGroup] = useState(!entityParam)
  const [jFrom, setJFrom] = useState(''); const [jApplied, setJApplied] = useState<string | null>(null)
  const [atTick, setAtTick] = useState(''); const [atApplied, setAtApplied] = useState<string | null>(null)
  const [cx, setCx] = useState(0); const [cy, setCy] = useState(0); const [viewApplied, setViewApplied] = useState<[number, number] | null>(null)
  const set = (k: string, v: unknown) => setForm((f) => ({ ...f, [k]: v }))
  const poll = live ? 1000 : false

  const worldQ = useQuery({ queryKey: ['world', w], queryFn: ({ signal }) => raw<Row>('GET', S, undefined, { signal }), refetchInterval: poll })
  const world = worldQ.data
  const needEnts = ['overview', 'cells', 'sims', 'entities'].includes(tab)
  const entsQ = useQuery({
    queryKey: ['world-ents', w, tab === 'entities' ? entApplied : ''],
    queryFn: ({ signal }) => raw<Row>('GET', A + '/entities' + qs({ q: tab === 'entities' && entApplied ? entApplied : undefined, limit: 5000 }), undefined, { signal }),
    enabled: !!world && needEnts, refetchInterval: tab === 'entities' ? false : poll,
  })
  const hoQ = useQuery({
    queryKey: ['world-ho', w, hoApplied, hoLimit],
    queryFn: ({ signal }) => raw<Row>('GET', A + '/handovers' + qs({ entity: hoApplied.trim() || undefined, limit: hoLimit }), undefined, { signal }),
    enabled: tab === 'handovers', refetchInterval: tab === 'handovers' ? poll : false,
  })
  const histQ = useQuery({
    queryKey: ['world-hist', w, jApplied],
    queryFn: ({ signal }) => raw<Row>('GET', A + '/history' + qs({ from_tick: jApplied || undefined, limit: 200 }), undefined, { signal }),
    enabled: tab === 'journal',
  })
  const atQ = useQuery({
    queryKey: ['world-at', w, atApplied],
    queryFn: ({ signal }) => raw<Row>('GET', A + '/at' + qs({ tick: atApplied }), undefined, { signal }),
    enabled: tab === 'at' && atApplied !== null,
  })
  const viewQ = useQuery({
    queryKey: ['world-view', w, viewApplied],
    queryFn: ({ signal }) => raw<Row>('GET', `${S}/view/${viewApplied?.[0]}/${viewApplied?.[1]}`, undefined, { signal }),
    enabled: tab === 'view' && !!viewApplied,
  })
  const nodesQ = useSub<Row>('/v1/server/cluster/nodes')
  const names = useMemo(() => { const m: Record<string, string> = {}; for (const n of listOf(nodesQ.data, 'nodes')) m[String(n.id)] = String(n.name ?? n.id); return m }, [nodesQ.data])
  const simName = (id?: string | null) => (id ? (names[id] ?? '…' + id.slice(-8)) : '—')
  const rates = useRates(w, world?.tick as number | undefined, live)
  const { run, pending } = useAct([['world', w], ['world-ents', w], ['world-ho', w], ['world-hist', w]], () => setDlg(''))

  const entities = useMemo(() => listOf(entsQ.data, 'entities'), [entsQ.data])
  const leases = useMemo(() => listOf(entsQ.data, 'leases'), [entsQ.data])
  const cells = useMemo(() => {
    const counts = new Map<string, { n: number; transit: number }>()
    for (const e of entities) { const k = cellKey(e.cell); const c = counts.get(k) ?? { n: 0, transit: 0 }; c.n++; if (e.transit || e.remote_to) c.transit++; counts.set(k, c) }
    const orphans = new Set(((world?.orphaned ?? []) as unknown[]).map(cellKey))
    const hot = ((world?.cells ?? []) as unknown[]).map((c) => {
      const [cell, owner] = Array.isArray(c) ? (c as [Row, string | null]) : [c as Row, (c as Row).owner as string | null]
      const k = cellKey(cell); const lease = leases.find((l) => sameCell(l.cell, cell))
      return { ...cell, id: k, owner: owner ?? lease?.sim ?? null, lease: lease?.sim ?? null, orphaned: orphans.has(k), cold: false, entities: counts.get(k)?.n ?? 0, transit: counts.get(k)?.transit ?? 0 }
    })
    const cold = ((world?.cold_cells ?? []) as Row[]).map((c) => ({ ...c, id: cellKey(c), owner: null, lease: null, orphaned: false, cold: true, entities: 0, transit: 0 }))
    return [...hot, ...cold] as (MapCell & Row & { id: string; entities: number; transit: number; lease: string | null })[]
  }, [world, entities, leases])
  const mapEntities = useMemo(() => entities.map((e) => ({ x: Number(e.pos?.x ?? e.x ?? 0), y: Number(e.pos?.y ?? e.y ?? 0), transit: !!(e.transit || e.remote_to), cell: e.cell })), [entities])
  const simRows = useMemo(() => ((world?.sims ?? []) as unknown[]).map((s) => {
    const o = typeof s === 'string' ? { id: s } : (s as Row); const id = String(o.id)
    const mine = cells.filter((c) => c.owner === id)
    return { ...o, cells: mine.length, entities: mine.reduce((a, c) => a + c.entities, 0), lag: o.acked_tick != null && world?.tick != null ? Number(world.tick) - Number(o.acked_tick) : undefined }
  }), [world, cells])
  const simIds = useMemo(() => { const ids = ((world?.sims ?? []) as unknown[]).map((x) => (typeof x === 'string' ? x : String((x as Row).id))); for (const l of leases) if (l.sim && !ids.includes(String(l.sim))) ids.push(String(l.sim)); return ids }, [world, leases])
  const simCount = simIds.length
  const homeNote = (() => { const e = entsQ.data; if (!e) return 'home unknown'; if (e.hosted_only) return e.home_node ? `home is ${simName(String(e.home_node))}` : 'home pending adoption'; return 'this node is home' })()
  const steps = useMemo(() => listOf(hoQ.data, 'steps'), [hoQ.data])
  const trails = useMemo(() => {
    const open = new Map<string, Row[]>(); const all: [string, Row[]][] = []
    for (const s of steps) { let a = open.get(s.entity); if (!a || s.event === 'started') { a = []; open.set(s.entity, a); all.push([s.entity, a]) } a.push(s) }
    return all.map(([entity, ss]) => { const first = ss[0] as Row, last = ss[ss.length - 1] as Row; return { entity, steps: ss, first, last, bad: ss.some((s) => !OK_PATHS.has(s.path)), open: last.event !== 'ended', ms: Number(last.ms) - Number(first.ms) } }).sort((a, b) => Number(b.last.ms) - Number(a.last.ms))
  }, [steps])
  const hoStats = { entities: trails.length, open: trails.filter((t) => t.open).length, bad: trails.filter((t) => t.bad).length }
  const cfg = (world?.config ?? {}) as Row

  const go = (t: string, extra: Record<string, string | undefined> = {}) => void nav({ search: { tab: t === 'overview' ? undefined : t, ...extra }, replace: true })
  function open(k: Dlg) {
    setDlg(k)
    setForm(k === 'host' ? { x: 0, y: 0 } : k === 'rewind' ? { tick: Math.max(0, Number(world?.tick ?? 0) - 100) } : { sim: simIds[0] ?? '', x: 0, y: 0, persistent: true, global: false, owner: '', components: '{}' })
  }
  const entityTo = (id: string) => `/world/worlds/${enc(w)}/entities/${enc(id)}`

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'World', to: '/world' }, { title: 'Worlds', to: '/world/worlds' }, { title: w }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="truncate font-mono text-xl font-semibold">{w}</h1>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            {world ? <>tick {String(world.tick)} · {String(world.entities ?? entities.length)} entities · {simCount} simulators</> : <Skeleton className="h-4 w-40" />}
            {worldQ.isFetching && live && <Badge variant="outline">live</Badge>}
          </div>
        </div>
        {world && (
          <div className="flex flex-wrap items-center gap-2">
            <label className="mr-1 flex items-center gap-1.5 text-xs text-muted-foreground"><Checkbox checked={live} onCheckedChange={(v) => setLive(v === true)} /> live</label>
            <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${A}/snapshot`), 'Snapshot written')}>Snapshot</Button>
            <Button size="sm" variant="outline" onClick={() => run(async () => setVerify(await raw<Row>('POST', `${A}/verify`)), 'Verified')}>Verify</Button>
            <Button size="sm" variant="outline" onClick={() => open('host')}>Host cell here</Button>
            <Button size="sm" variant="outline" onClick={() => open('spawn')}>Spawn entity</Button>
            <Button size="sm" variant="outline" onClick={() => open('rewind')}>Rewind</Button>
            <Button size="sm" variant="outline" onClick={() => confirmThen('Detach every simulator from this world?', () => run(() => raw('DELETE', `${S}/sims`), 'Simulators detached'))}>Detach sims</Button>
            <Button size="sm" variant="destructive" onClick={() => confirmThen(`Delete world ${w}?`, () => run(async () => { await raw('DELETE', S); void nav({ to: '/$section/$page', params: { section: 'world', page: 'worlds' } }) }, 'Deleted'))}>Delete</Button>
          </div>
        )}
      </div>
      {worldQ.isError && <Alert variant="destructive"><AlertDescription>{String(worldQ.error)}</AlertDescription></Alert>}
      {world && (
        <Tabs value={tab} onValueChange={(v) => go(v)}>
          <div className="overflow-x-auto"><TabsList className="w-max">{TABS.map(([v, l]) => <TabsTrigger key={v} value={v}>{l}</TabsTrigger>)}</TabsList></div>
          <Card className="mt-3"><CardContent className="pt-6">
            <TabsContent value="overview">
              <div className="mb-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-6">
                <Stat k="Tick" v={String(world.tick)} sub={rates ? `${rates.ticks.toFixed(1)} /s` : undefined} />
                <Stat k="Entities" v={String(world.entities ?? entities.length)} sub={mapEntities.some((e) => e.transit) ? `${mapEntities.filter((e) => e.transit).length} in transit` : undefined} warn="warn" />
                <Stat k="Cells hot / cold" v={((world.cells ?? []) as unknown[]).length} sub={<>/ {((world.cold_cells ?? []) as unknown[]).length} cold{((world.orphaned ?? []) as unknown[]).length ? <span className="ml-1 text-destructive">{((world.orphaned ?? []) as unknown[]).length} orphaned</span> : null}</>} />
                <Stat k="Simulators" v={simCount} sub={`${leases.length} leases`} />
                <Stat k="Handovers" v={rates ? rates.handovers.toFixed(1) : '—'} sub="/s on this node" />
                <Stat k="Players" v={String(world.subscribers ?? 0)} sub={rates ? `${rates.frames.toFixed(0)} frames/s` : undefined} />
              </div>
              {entsQ.isPending ? <Skeleton className="h-40 w-full" /> : <CellMap cells={cells} entities={mapEntities} cellSize={Number(cfg.cell_size ?? 1)} names={names} selected={null} onPick={(c) => { setCx(c.x); setCy(c.y); setViewApplied([c.x, c.y]); go('view') }} />}
              <p className="mt-2 text-xs text-muted-foreground">Hot cells tinted by simulator, hibernated cells dimmed, orphaned outlined red; dots are entities (amber while changing cells). Entities shown are this node's shard ({homeNote}). Click a cell for its view.</p>
              <KvGrid className="mt-5">
                <Kv k="Cell size" v={cfg.cell_size} />
                <Kv k="Partition" v={cfg.three_d ? 'octree' : Number(cfg.max_depth) > 0 ? `quadtree, depth ≤ ${cfg.max_depth}` : 'fixed grid'} />
                <Kv k="Ghost margin / hysteresis" v={`${cfg.ghost_margin ?? '—'} / ${cfg.hysteresis ?? '—'}`} />
                <Kv k="Lease TTL / handover timeout" v={`${cfg.lease_ttl_ticks ?? '—'} / ${cfg.handover_timeout_ticks ?? '—'} ticks`} />
                <Kv k="Hibernate after" v={cfg.hibernate_after_ticks ? `${cfg.hibernate_after_ticks} ticks` : 'never'} />
                <Kv k="Snapshot every" v={cfg.snapshot_every_ticks ? `${cfg.snapshot_every_ticks} ticks` : '—'} />
                <Kv k="Client budget" v={`${cfg.client_budget_bytes ?? '—'} B, min ${cfg.client_min_delta_ticks ?? '—'} ticks`} />
                <Kv k="Keyframe every" v={cfg.keyframe_ticks ? `${cfg.keyframe_ticks} ticks` : '—'} />
                <Kv k="Contacts co-locate" v={cfg.contact_colocate_after_ticks ? `after ${cfg.contact_colocate_after_ticks} ticks` : 'off'} />
                <Kv k="Region" v={cfg.region} />
              </KvGrid>
              {verify && <><SubHead title="Last verify"><Badge variant={verify.ok === false ? 'destructive' : 'secondary'}>{verify.ok === false ? 'problems found' : 'ok'}</Badge></SubHead><JsonView value={verify} /></>}
            </TabsContent>

            <TabsContent value="cells">
              <SubHead title="Hot cells" count={cells.filter((c) => !c.cold).length} />
              <DataTable id="world-cells" rows={cells.filter((c) => !c.cold)} loading={entsQ.isPending} empty="No hot cells on this node"
                actions={(r) => <Button size="sm" variant="ghost" onClick={() => { setCx(r.x); setCy(r.y); setViewApplied([r.x, r.y]); go('view') }}>View</Button>}
                cols={[{ key: 'id', label: 'cell', mono: true }, { key: 'owner', label: 'simulator', mono: true, fmt: (r) => (r.owner ? simName(r.owner) : r.orphaned ? 'orphaned' : '—') }, { key: 'entities' }, { key: 'transit', label: 'in transit' }, { key: 'lease', mono: true, fmt: (r) => (r.lease ? simName(r.lease) : '—') }]} />
              <SubHead title="Hibernated cells" count={((world.cold_cells ?? []) as unknown[]).length} />
              {((world.cold_cells ?? []) as unknown[]).length ? <div className="flex flex-wrap gap-1.5">{((world.cold_cells ?? []) as unknown[]).map((c) => <Badge key={cellKey(c)} variant="outline" className="font-mono">{cellKey(c)}</Badge>)}</div> : <p className="text-sm text-muted-foreground">None — every cell with persistent entities is in memory</p>}
              <SubHead title="Replicas"><span className="text-xs text-muted-foreground">standby copies of cells hosted here</span></SubHead>
              <DataTable id="world-replicas" tools={false} rows={(world.replicas ?? []) as Row[]} empty="No replicas"
                cols={[{ key: 'cell', mono: true, fmt: (r) => cellKey(r.cell) }, { key: 'node', mono: true, fmt: (r) => simName(r.node) }, { key: 'tick', label: 'acked tick' }, { key: 'lag', fmt: (r) => String(Number(world.tick) - Number(r.tick)) }]} />
              <SubHead title="Mirrors"><span className="text-xs text-muted-foreground">cells hosted elsewhere that this node follows</span></SubHead>
              <DataTable id="world-mirrors" tools={false} rows={(world.mirrors ?? []) as Row[]} empty="No mirrors"
                cols={[{ key: 'cell', mono: true, fmt: (r) => cellKey(r.cell) }, { key: 'node', label: 'host', mono: true, fmt: (r) => simName(r.node) }, { key: 'tick', label: 'host tick' }]} />
            </TabsContent>

            <TabsContent value="sims">
              <SubHead title="Simulators" count={simRows.length} />
              <DataTable id="world-sims" rows={simRows} empty="No simulators attached"
                cols={[{ key: 'id', mono: true, fmt: (r) => simName(r.id) }, { key: 'cells' }, { key: 'entities' }, { key: 'capacity', fmt: (r) => (r.capacity ? String(r.capacity) : 'unbounded') }, { key: 'last_seen_tick', label: 'last seen (tick)' }, { key: 'acked_tick', label: 'acked tick' }, { key: 'lag', fmt: (r) => (r.lag == null ? '—' : `${r.lag} ticks`) }]} />
              <SubHead title="Leases" count={leases.length} />
              <DataTable id="world-leases" tools={false} rows={leases} loading={entsQ.isPending} empty="No leases"
                cols={[{ key: 'cell', mono: true, fmt: (r) => cellKey(r.cell) }, { key: 'sim', mono: true, fmt: (r) => simName(r.sim) }, { key: 'entities', fmt: (r) => String(cells.find((c) => sameCell(c, r.cell))?.entities ?? 0) }]} />
              <p className="mt-3 text-xs text-muted-foreground">{homeNote} · this shard from node {simName(entsQ.data?.node_id ? String(entsQ.data.node_id) : null)}{entsQ.data?.hosted_only ? ' · hosted-only (home is elsewhere)' : ''}</p>
            </TabsContent>

            <TabsContent value="entities">
              <form className="mb-3 flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); setEntApplied(entQ) }}>
                <Input className="max-w-xs" value={entQ} placeholder="filter id" onChange={(e) => setEntQ(e.target.value)} />
                <Button type="submit" size="sm" variant="outline">Refresh</Button>
                <span className="text-xs text-muted-foreground">tick {String(entsQ.data?.tick ?? '—')} · {entities.length} on this node</span>
              </form>
              <DataTable id="world-entities" rows={entities} loading={entsQ.isPending} empty="No entities on this node" rowTo={(r) => entityTo(String(r.id))}
                actions={(r) => { const sim = r.sim ?? leases.find((l) => sameCell(l.cell, r.cell))?.sim ?? simIds[0]; return sim ? <Button size="sm" variant="ghost" onClick={() => confirmThen('Despawn this entity?', () => run(() => raw('POST', `${S}/despawn`, { sim, entity: r.id }), 'Despawned'))}>Despawn</Button> : null }}
                cols={[{ key: 'id', mono: true }, { key: 'cell', mono: true, fmt: (r) => cellKey(r.cell) }, { key: 'pos', mono: true, fmt: (r) => (r.pos ? `${Number(r.pos.x).toFixed(1)}, ${Number(r.pos.y).toFixed(1)}${r.pos.z != null ? ', ' + Number(r.pos.z).toFixed(1) : ''}` : '—') }, { key: 'owner', mono: true }, { key: 'kind' }, { key: 'persistent', fmt: (r) => (r.persistent ? 'journaled' : 'volatile') }, { key: 'transit', label: 'in transit', fmt: (r) => (r.remote_to ? `→ node ${simName(String(r.remote_to))}` : r.transit ? `→ ${cellKey(r.transit.to ?? r.transit)}` : '') }]} />
            </TabsContent>

            <TabsContent value="handovers">
              <form className="mb-2 flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); setHoApplied(hoEntity) }}>
                <Input className="min-w-72 max-w-md" value={hoEntity} placeholder="entity id (empty = all)" onChange={(e) => setHoEntity(e.target.value)} />
                <Select value={String(hoLimit)} onValueChange={(v) => setHoLimit(Number(v))}><SelectTrigger className="w-32"><SelectValue /></SelectTrigger><SelectContent>{[200, 500, 2000, 10000].map((n) => <SelectItem key={n} value={String(n)}>last {n}</SelectItem>)}</SelectContent></Select>
                <Button type="submit" size="sm" variant="outline">Apply</Button>
                <label className="flex items-center gap-1.5 text-xs text-muted-foreground"><Checkbox checked={hoGroup} onCheckedChange={(v) => setHoGroup(v === true)} /> group by entity</label>
                <span className="ml-auto text-xs text-muted-foreground">{steps.length} steps · {hoStats.entities} handovers · <span className={hoStats.open ? 'text-amber-500' : ''}>{hoStats.open} in flight</span> · <span className={hoStats.bad ? 'text-destructive' : ''}>{hoStats.bad} off the happy path</span></span>
              </form>
              <p className="mb-3 text-xs text-muted-foreground">Every step of a change of owner, oldest first, from every node's durable journal plus this node's unflushed tail: <code>started</code> (the cell border crossing), <code>shipped</code> (remote / forced on offload), <code>received</code> (what the new owner did with it), <code>ended</code>. Paths outside the happy set are highlighted.</p>
              {hoQ.isPending ? <Skeleton className="h-24 w-full" /> : hoGroup ? (
                <div className="overflow-x-auto"><Table>
                  <TableHeader><TableRow><TableHead className="w-56">Entity</TableHead><TableHead className="w-24">Ticks</TableHead><TableHead>Trail</TableHead><TableHead className="w-24">Took</TableHead><TableHead className="w-32">Last step</TableHead></TableRow></TableHeader>
                  <TableBody>
                    {trails.map((t) => (
                      <TableRow key={t.entity + t.first.ms} className="cursor-pointer" onClick={() => { setHoEntity(t.entity); setHoApplied(t.entity); setHoGroup(false) }}>
                        <TableCell className="font-mono text-xs">…{String(t.entity).slice(-12)}</TableCell>
                        <TableCell className="text-xs text-muted-foreground">{t.first.tick}{t.last.tick !== t.first.tick && <> → {t.last.tick}</>}</TableCell>
                        <TableCell className="space-x-1">{t.steps.map((s, i) => <span key={i} title={s.detail} className={`inline-block rounded border px-1 text-xs ${OK_PATHS.has(s.path) ? '' : 'border-destructive text-destructive'}`}>{s.event}<small className="text-muted-foreground">/{s.path}</small></span>)}{t.open && <span className="text-xs text-muted-foreground">…</span>}</TableCell>
                        <TableCell className="text-xs text-muted-foreground">{t.ms} ms</TableCell>
                        <TableCell className="text-xs text-muted-foreground">{fmtMs(Number(t.last.ms))}</TableCell>
                      </TableRow>
                    ))}
                    {!trails.length && <TableRow><TableCell colSpan={5} className="py-8 text-center text-muted-foreground">No handovers recorded{hoApplied ? ' for this entity' : ''}</TableCell></TableRow>}
                  </TableBody>
                </Table></div>
              ) : (
                <DataTable id="world-ho-steps" rows={steps} empty={`No handovers recorded${hoApplied ? ' for this entity' : ''}`}
                  cols={[{ key: 'ms', label: 'time', dim: true, fmt: (r) => fmtMs(Number(r.ms)) }, { key: 'tick', dim: true }, { key: 'entity', mono: true, fmt: (r) => '…' + String(r.entity).slice(-12) }, { key: 'event' }, { key: 'path', fmt: (r) => <span className={OK_PATHS.has(r.path) ? '' : 'text-destructive'}>{r.path}</span> }, { key: 'node', mono: true, dim: true, fmt: (r) => simName(r.node) }, { key: 'detail', dim: true }]} />
              )}
            </TabsContent>

            <TabsContent value="journal">
              <form className="mb-3 flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); setJApplied(jFrom) }}>
                <Input className="w-36" type="number" value={jFrom} placeholder="from tick" onChange={(e) => setJFrom(e.target.value)} />
                <Button type="submit" size="sm" variant="outline">Load</Button>
                {histQ.data && <span className="text-xs text-muted-foreground">{String(histQ.data.total ?? listOf(histQ.data, 'ops').length)} ops · ticks {String(histQ.data.from_tick ?? '—')} → {String(histQ.data.to_tick ?? '—')}</span>}
              </form>
              <DataTable id="world-journal" rows={listOf(histQ.data, 'ops')} loading={histQ.isPending} empty="No journal ops in range"
                cols={[{ key: 'tick', dim: true }, { key: 'op', fmt: (r) => r.op ?? r.kind }, { key: 'id', label: 'entity', mono: true, fmt: (r) => r.id ?? r.entity ?? '—' }, { key: 'data', label: 'payload', dim: true, fmt: (r) => <span className="font-mono text-xs break-all">{JSON.stringify(r.data ?? r.payload ?? r)}</span> }]} />
            </TabsContent>

            <TabsContent value="at">
              <form className="mb-3 flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (atTick !== '') setAtApplied(atTick) }}>
                <Input className="w-36" type="number" value={atTick} placeholder="tick" onChange={(e) => setAtTick(e.target.value)} />
                <Button type="submit" size="sm">Reconstruct</Button>
                {atQ.data && <span className="text-xs text-muted-foreground">snapshots {((atQ.data.from_snapshots ?? []) as Row[]).map((s) => `${s.tick}@${s.node ?? s.cell ?? ''}`).join(', ') || 'none'} · {String(atQ.data.ops_replayed ?? 0)} ops replayed</span>}
              </form>
              <p className="mb-3 text-xs text-muted-foreground">Rebuilds the world state at a past tick from the nearest snapshot plus the journal — nothing is changed.</p>
              <DataTable id="world-at" rows={listOf(atQ.data, 'entities')} loading={atApplied !== null && atQ.isPending} empty={atApplied === null ? 'Enter a tick to reconstruct' : 'No entities at that tick'}
                cols={[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'pos', mono: true, fmt: (r) => (r.pos ? `${r.pos.x}, ${r.pos.y}` : '—') }, { key: 'data', dim: true, fmt: (r) => JSON.stringify(r.data ?? r.state ?? {}) }]} />
            </TabsContent>

            <TabsContent value="view">
              <form className="mb-3 flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); setViewApplied([cx, cy]) }}>
                <Input className="w-24" type="number" value={cx} onChange={(e) => setCx(Number(e.target.value))} aria-label="cell x" />
                <Input className="w-24" type="number" value={cy} onChange={(e) => setCy(Number(e.target.value))} aria-label="cell y" />
                <Button type="submit" size="sm">View cell</Button>
                {viewQ.data && <span className="text-xs text-muted-foreground">tick {String(viewQ.data.tick ?? '—')}</span>}
              </form>
              {viewApplied ? (viewQ.isPending ? <Skeleton className="h-24 w-full" /> : <>
                <SubHead title="Owned" count={listOf(viewQ.data, 'owned').length} /><JsonView value={listOf(viewQ.data, 'owned')} />
                <SubHead title="Ghosts" count={listOf(viewQ.data, 'ghosts').length} /><JsonView value={listOf(viewQ.data, 'ghosts')} />
                <SubHead title="Globals" count={listOf(viewQ.data, 'globals').length} /><JsonView value={listOf(viewQ.data, 'globals')} />
              </>) : <p className="text-sm text-muted-foreground">Pick a cell on the overview map or enter its coordinates.</p>}
            </TabsContent>

            <TabsContent value="config"><JsonView value={cfg} /></TabsContent>
          </CardContent></Card>
        </Tabs>
      )}

      <FormDialog open={dlg === 'host'} onOpenChange={(o) => !o && setDlg('')} title="Host a cell on this node" submit="Host" pending={pending} onSubmit={() => run(() => raw('POST', `${A}/cells/${form.x}/${form.y}/host`), 'Cell hosted here')}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Cell x"><Input type="number" value={form.x ?? 0} onChange={(e) => set('x', Number(e.target.value))} /></Field>
          <Field label="Cell y"><Input type="number" value={form.y ?? 0} onChange={(e) => set('y', Number(e.target.value))} /></Field>
        </div>
      </FormDialog>
      <FormDialog open={dlg === 'rewind'} onOpenChange={(o) => !o && setDlg('')} title="Rewind world" submit="Rewind" destructive pending={pending} description="Replays the journal up to the given tick and discards everything after it. Simulators must re-attach."
        onSubmit={() => confirmThen(`Rewind ${w} to tick ${form.tick}?`, () => run(() => raw('POST', `${S}/rewind`, { tick: Number(form.tick) }), 'Rewound'))}>
        <Field label="Tick"><Input type="number" value={form.tick ?? 0} onChange={(e) => set('tick', Number(e.target.value))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'spawn'} onOpenChange={(o) => !o && setDlg('')} title="Spawn entity" submit="Spawn" pending={pending} disabled={!form.sim}
        description={simIds.length ? 'Spawned on behalf of a simulator attached to this world; the node places it in the cell under x / y.' : 'No simulator is attached to this world — attach a game server first, spawning needs one to own the entity.'}
        onSubmit={() => {
          let components: unknown
          try { components = JSON.parse(String(form.components || '{}')) } catch { return toast.error('Components is not valid JSON') }
          run(() => raw('POST', `${S}/spawn`, { sim: form.sim, x: Number(form.x), y: Number(form.y), components, persistent: !!form.persistent, global: !!form.global, owner: form.owner ? String(form.owner).trim() : undefined }), 'Spawned')
        }}>
        <Field label="Simulator">
          <Select value={String(form.sim ?? '')} onValueChange={(v) => set('sim', v)} disabled={!simIds.length}>
            <SelectTrigger><SelectValue placeholder="no simulators attached" /></SelectTrigger>
            <SelectContent>{simIds.map((id) => <SelectItem key={id} value={id}>{simName(id)} <span className="font-mono text-xs text-muted-foreground">{id}</span></SelectItem>)}</SelectContent>
          </Select>
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="x"><Input type="number" value={form.x ?? 0} onChange={(e) => set('x', Number(e.target.value))} /></Field>
          <Field label="y"><Input type="number" value={form.y ?? 0} onChange={(e) => set('y', Number(e.target.value))} /></Field>
        </div>
        <Field label="Owner account (optional)" hint="Only sessions of this account may drive the entity; empty = unowned (NPC, prop)."><Input value={form.owner ?? ''} placeholder="account uuid" className="font-mono" onChange={(e) => set('owner', e.target.value)} /></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!form.persistent} onCheckedChange={(v) => set('persistent', v === true)} /> persistent (journaled)</label>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!form.global} onCheckedChange={(v) => set('global', v === true)} /> global (delivered to every subscriber)</label>
        <Field label="Components (JSON)"><Textarea className="min-h-32 font-mono text-xs" value={form.components ?? ''} onChange={(e) => set('components', e.target.value)} /></Field>
      </FormDialog>
    </div>
  )
}
