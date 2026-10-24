import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { ts } from '@/lib/format'
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
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

const TABS: [string, string][] = [
  ['overview', 'Overview'], ['servers', 'Servers'], ['autoscaler', 'Autoscaler'], ['rollout', 'Rollout & builds'],
  ['health', 'Health & alerts'], ['cost', 'Cost & usage'], ['events', 'Events & metrics'], ['spec', 'Spec'],
]
const METRICS = ['ready_pct', 'ready', 'idle_ready', 'allocated_pct', 'allocated', 'players_pct', 'players', 'live', 'in_game', 'starting', 'capacity', 'queue', 'cpu_pct_avg', 'cpu_pct_max', 'age_secs', 'uptime_secs']
type Dlg = '' | 'scale' | 'allocate' | 'build' | 'rule' | 'spec'
const enc = encodeURIComponent

export const Route = createFileRoute('/_app/servers/fleets/$id')({
  validateSearch: (s: Record<string, unknown>): { tab?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined }),
  component: FleetPage,
})

interface Ctx { F: string; fleet: Row; run: (fn: () => Promise<unknown>, ok: string) => void; open: (d: Dlg) => void }

function Sel({ value, onChange, options, className }: { value: string; onChange: (v: string) => void; options: [string, string][] | string[]; className?: string }) {
  const opts: [string, string][] = options.map((o) => (Array.isArray(o) ? o : [o, o]))
  return <Select value={value} onValueChange={onChange}><SelectTrigger className={className}><SelectValue /></SelectTrigger><SelectContent>{opts.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}</SelectContent></Select>
}

function FleetPage() {
  const { id } = Route.useParams()
  const { tab = 'overview' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const F = `/v1/server/fleets/${enc(id)}`
  const fleetQ = useQuery({ queryKey: ['fleet', id], queryFn: ({ signal }) => raw<Row>('GET', F, undefined, { signal }) })
  const [dlg, setDlg] = useState<Dlg>('')
  const [form, setForm] = useState<Row>({})
  const [text, setText] = useState('')
  const { run, pending } = useAct([['fleet', id], ['sub']], () => setDlg(''))
  const fleet = fleetQ.data
  const set = (k: string, v: unknown) => setForm((f) => ({ ...f, [k]: v }))

  function open(k: Dlg) {
    setDlg(k)
    if (k === 'scale') setForm({ min_servers: fleet?.min_servers ?? 0, max_servers: fleet?.max_servers ?? 1, buffer: fleet?.buffer ?? 0 })
    else if (k === 'allocate') setForm({ build_version: '', wait_secs: 0, payload: '{}' })
    else if (k === 'build') setForm({ version: '', canary_percent: 10, canary_regions: '', notes: '', spec: JSON.stringify(fleet?.spec ?? {}, null, 2) })
    else if (k === 'rule') setForm({ name: '', metric: 'cpu_pct_avg', op: 'gt', threshold: 90, for_secs: 300, severity: 'warning' })
    else if (k === 'spec') setText(JSON.stringify(fleet?.spec ?? {}, null, 2))
  }
  const parse = (s: string, what: string): unknown | undefined => { try { return JSON.parse(s || '{}') } catch { toast.error(`${what} is not valid JSON`); return undefined } }
  const ctx: Ctx | null = fleet ? { F, fleet, run, open } : null

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Servers', to: '/servers' }, { title: 'Fleets', to: '/servers/fleets' }, { title: String(fleet?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-xl font-semibold">{fleetQ.isPending ? <Skeleton className="h-7 w-48" /> : String(fleet?.name ?? id)}</h1>
          <div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">{id}{fleet && <Badge variant="secondary">{String(fleet.provider ?? 'fleet')}</Badge>}{fleet?.region ? <Badge variant="outline">{String(fleet.region)}</Badge> : null}</div>
        </div>
        {fleet && (
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" onClick={() => open('scale')}>Scale</Button>
            <Button size="sm" variant="outline" onClick={() => open('allocate')}>Allocate</Button>
            <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${F}/reconcile`), 'Reconcile requested')}>Reconcile</Button>
            <Button size="sm" variant="outline" onClick={() => open('spec')}>Edit spec</Button>
            <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete this fleet and terminate its servers?', () => run(async () => { await raw('DELETE', F); void nav({ to: '/$section/$page', params: { section: 'servers', page: 'fleets' } }) }, 'Fleet deleted'))}>Delete</Button>
          </div>
        )}
      </div>
      {fleetQ.isError && <Alert variant="destructive"><AlertDescription>{String(fleetQ.error)}</AlertDescription></Alert>}
      {ctx && (
        <Tabs value={tab} onValueChange={(v) => void nav({ search: { tab: v === 'overview' ? undefined : v }, replace: true })}>
          <div className="overflow-x-auto"><TabsList className="w-max">{TABS.map(([v, l]) => <TabsTrigger key={v} value={v}>{l}</TabsTrigger>)}</TabsList></div>
          <Card className="mt-3"><CardContent className="pt-6">
            <TabsContent value="overview"><OverviewTab ctx={ctx} /></TabsContent>
            <TabsContent value="servers"><ServersTab ctx={ctx} /></TabsContent>
            <TabsContent value="autoscaler"><AutoscalerTab ctx={ctx} /></TabsContent>
            <TabsContent value="rollout"><RolloutTab ctx={ctx} /></TabsContent>
            <TabsContent value="health"><HealthTab ctx={ctx} /></TabsContent>
            <TabsContent value="cost"><CostTab ctx={ctx} /></TabsContent>
            <TabsContent value="events"><EventsTab ctx={ctx} /></TabsContent>
            <TabsContent value="spec"><JsonView value={fleet?.spec} /></TabsContent>
          </CardContent></Card>
        </Tabs>
      )}

      <FormDialog open={dlg === 'scale'} onOpenChange={(o) => !o && setDlg('')} title="Scale fleet" submit="Apply" pending={pending} onSubmit={() => run(() => raw('POST', `${F}/scale`, { min_servers: Number(form.min_servers), max_servers: Number(form.max_servers), buffer: Number(form.buffer) }), 'Scaled')}>
        <div className="grid grid-cols-3 gap-3">
          <Field label="Min servers"><Input type="number" min={0} value={form.min_servers ?? 0} onChange={(e) => set('min_servers', e.target.value)} /></Field>
          <Field label="Max servers"><Input type="number" min={0} value={form.max_servers ?? 1} onChange={(e) => set('max_servers', e.target.value)} /></Field>
          <Field label="Ready buffer"><Input type="number" min={0} value={form.buffer ?? 0} onChange={(e) => set('buffer', e.target.value)} /></Field>
        </div>
      </FormDialog>
      <FormDialog open={dlg === 'allocate'} onOpenChange={(o) => !o && setDlg('')} title="Allocate a server" submit="Allocate" pending={pending}
        onSubmit={() => { const payload = parse(String(form.payload ?? ''), 'Payload'); if (payload === undefined) return; run(async () => { const r = await raw<Row>('POST', `${F}/allocate`, { payload, wait_secs: Number(form.wait_secs ?? 0), selector: form.build_version ? { build_version: form.build_version } : {} }); toast.message(`Allocated ${String(r.server_id ?? r.id ?? '')} ${r.address ? `at ${r.address}:${r.port ?? ''}` : ''}`) }, 'Allocated') }}>
        <Field label="Build version (optional selector)"><Input value={form.build_version ?? ''} onChange={(e) => set('build_version', e.target.value)} /></Field>
        <Field label="Wait for a ready server, seconds (0 = fail fast)"><Input type="number" min={0} value={form.wait_secs ?? 0} onChange={(e) => set('wait_secs', e.target.value)} /></Field>
        <Field label="Payload handed to the server (JSON)"><Textarea className="min-h-24 font-mono text-xs" value={form.payload ?? ''} onChange={(e) => set('payload', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'build'} onOpenChange={(o) => !o && setDlg('')} title="Add build (canary)" submit="Add" pending={pending} disabled={!form.version}
        onSubmit={() => { const spec = parse(String(form.spec ?? ''), 'Spec'); if (spec === undefined) return; run(() => raw('POST', `${F}/builds`, { version: form.version, spec, canary_percent: Number(form.canary_percent ?? 0), canary_regions: String(form.canary_regions ?? '').split(',').map((r) => r.trim()).filter(Boolean), notes: form.notes || undefined }), 'Build added') }}>
        <Field label="Version"><Input value={form.version ?? ''} placeholder="1.2.0" onChange={(e) => set('version', e.target.value)} /></Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Canary share, % (0 = active at once)"><Input type="number" min={0} max={100} value={form.canary_percent ?? 10} onChange={(e) => set('canary_percent', e.target.value)} /></Field>
          <Field label="Canary regions (comma-separated, empty = all)"><Input value={form.canary_regions ?? ''} onChange={(e) => set('canary_regions', e.target.value)} /></Field>
        </div>
        <Field label="Notes"><Input value={form.notes ?? ''} onChange={(e) => set('notes', e.target.value)} /></Field>
        <Field label="Spec (JSON — image / artifact / command / env / vcpu)"><Textarea className="min-h-52 font-mono text-xs" value={form.spec ?? ''} onChange={(e) => set('spec', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'rule'} onOpenChange={(o) => !o && setDlg('')} title="Alert rule" pending={pending} disabled={!form.name}
        onSubmit={() => run(() => raw('PUT', `${F}/alerts/rules/${enc(String(form.name))}`, { metric: form.metric, op: form.op, threshold: Number(form.threshold), for_secs: Number(form.for_secs), severity: form.severity }), 'Rule saved')}>
        <Field label="Name"><Input value={form.name ?? ''} onChange={(e) => set('name', e.target.value)} /></Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Metric"><Sel value={String(form.metric ?? 'cpu_pct_avg')} onChange={(v) => set('metric', v)} options={['cpu_pct_avg', 'cpu_pct_max', 'mem_pct_avg', 'tick_p99_ms_max', 'players', 'live', 'unhealthy', 'crashes']} /></Field>
          <Field label="Operator"><Sel value={String(form.op ?? 'gt')} onChange={(v) => set('op', v)} options={[['gt', '>'], ['gte', '>='], ['lt', '<'], ['lte', '<=']]} /></Field>
          <Field label="Threshold"><Input type="number" value={form.threshold ?? 0} onChange={(e) => set('threshold', e.target.value)} /></Field>
          <Field label="For, seconds"><Input type="number" min={0} value={form.for_secs ?? 0} onChange={(e) => set('for_secs', e.target.value)} /></Field>
        </div>
        <Field label="Severity"><Sel value={String(form.severity ?? 'warning')} onChange={(v) => set('severity', v)} options={['info', 'warning', 'critical']} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'spec'} onOpenChange={(o) => !o && setDlg('')} title="Fleet spec" pending={pending}
        onSubmit={() => { const spec = parse(text, 'Spec'); if (spec === undefined) return; run(() => raw('PUT', F, { spec }), 'Spec saved') }}>
        <Textarea className="min-h-[420px] font-mono text-xs" value={text} onChange={(e) => setText(e.target.value)} aria-label="Fleet spec JSON" />
      </FormDialog>
    </div>
  )
}

function OverviewTab({ ctx: { F, fleet, run } }: { ctx: Ctx }) {
  const counters = useSub<Row>(`${F}/counters`)
  const demand = useSub<Row>(`${F}/demand`)
  const rep = useSub<Row>(`${F}/reputation`)
  const installs = useSub<Row>(`${F}/installs`)
  const spec = (fleet.spec ?? {}) as Row
  const r = rep.data
  const ins = installs.data
  return (
    <>
      <KvGrid>
        <Kv k="Fleet ID" v={fleet.id} mono copy />
        <Kv k="Name" v={fleet.name} />
        <Kv k="Provider" v={fleet.provider} />
        <Kv k="Region / pool" v={`${fleet.region || '—'} / ${fleet.pool || '—'}`} />
        <Kv k="Artifact" v={spec.artifact} mono />
        <Kv k="Image" v={spec.image} mono />
        <Kv k="Servers min / max / buffer" v={`${fleet.min_servers} / ${fleet.max_servers} / ${fleet.buffer}`} />
        <Kv k="Policy" v={(fleet.policy as Row | undefined)?.suspended ? 'autoscaler suspended' : `${((fleet.policy as Row | undefined)?.rules as unknown[] | undefined)?.length ?? 0} rules`} />
        <Kv k="Ports" v={`${fleet.port_start}–${fleet.port_end}`} mono />
        <Kv k="Created" v={ts(fleet.created_at)} />
        <Kv k="Target ready" v={demand.data?.target_ready} />
        <Kv k="Demand / hour" v={demand.data?.per_hour} />
      </KvGrid>
      <SubHead title="Reputation" />
      {r ? <KvGrid><Kv k="Score" v={r.score ?? '—'} /><Kv k="Players served / rounds" v={`${(r.gameplay as Row | undefined)?.players_served ?? 0} / ${(r.gameplay as Row | undefined)?.rounds ?? 0}`} /><Kv k="Crashes / evictions" v={`${r.crashes ?? 0} / ${r.evictions ?? 0}`} /></KvGrid> : <p className="text-sm text-muted-foreground">No reputation data yet</p>}
      <SubHead title="Counters & lists" />
      <JsonView value={counters.data ? { counters: counters.data.counters ?? {}, lists: counters.data.lists ?? {}, counter_target: counters.data.counter_target } : null} empty="No counters" />
      <SubHead title="Install script" count={ins?.total as number | undefined}>
        {!!ins?.total && <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${F}/installs/reset`), 'Install state reset')}>Reset</Button>}
      </SubHead>
      {!!ins?.failed && <p className="mb-2 text-xs text-destructive">{String(ins.failed)} failed runs</p>}
      <DataTable id="fleet-installs" tools={false} rows={listOf(ins, 'runs')} loading={installs.isPending} empty="No install runs — the fleet has no SSH hosts or the script has not run yet"
        cols={[{ key: 'host_id', mono: true }, { key: 'status' }, { key: 'started_at', dim: true, fmt: (x) => ts(x.started_at) }, { key: 'finished_at', dim: true, fmt: (x) => ts(x.finished_at) }, { key: 'error', dim: true }]} />
    </>
  )
}

function ServersTab({ ctx: { F, run } }: { ctx: Ctx }) {
  const servers = useSub(`${F}/servers`)
  return (
    <DataTable id="fleet-servers" rows={listOf(servers.data, 'servers')} loading={servers.isPending} empty="No game servers in this fleet — scale it up or wait for the autoscaler" rowTo={(s) => `/servers/servers/${enc(String(s.id))}`}
      cols={[{ key: 'id', mono: true }, { key: 'state' }, { key: 'region' }, { key: 'address', mono: true, fmt: (s) => (s.address ? `${s.address}:${s.port ?? ''}` : '—') }, { key: 'players', fmt: (s) => `${s.players ?? 0} / ${s.capacity ?? s.max_players ?? '—'}` }, { key: 'build_version', label: 'build', mono: true }, { key: 'reserved_until', fmt: (s) => (s.reserved_until ? ts(s.reserved_until) : '') }, { key: 'started_at', dim: true, fmt: (s) => ts(s.started_at) }]}
      actions={(s) => (
        <span className="flex justify-end gap-1">
          {s.reserved_until
            ? <Button size="sm" variant="ghost" onClick={() => run(() => raw('POST', `/v1/server/gameservers/${enc(String(s.id))}/unreserve`), 'Unreserved')}>Unreserve</Button>
            : <Button size="sm" variant="ghost" onClick={() => run(() => raw('POST', `/v1/server/gameservers/${enc(String(s.id))}/reserve`, { secs: 600 }), 'Reserved for 10 min')}>Reserve</Button>}
          <Button size="sm" variant="ghost" onClick={() => run(() => raw('POST', `${F}/servers/${enc(String(s.id))}/logs/archive`), 'Log snapshot requested')}>Archive logs</Button>
          <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen('Terminate this game server?', () => run(() => raw('DELETE', `/v1/server/gameservers/${enc(String(s.id))}`), 'Terminated'))}>Terminate</Button>
        </span>
      )} />
  )
}

function AutoscalerTab({ ctx: { F, fleet, run } }: { ctx: Ctx }) {
  const as = useSub<Row>(`${F}/autoscaler`)
  const demand = useSub<Row>(`${F}/demand`)
  const [policy, setPolicy] = useState<Row>({})
  const [rules, setRules] = useState<Row[]>([])
  const [dirty, setDirty] = useState(false)
  useEffect(() => {
    const p = { suspended: false, rules: [], webhook: null, wasm: null, forecast: false, ...((fleet.policy ?? as.data?.policy ?? {}) as Row) }
    setPolicy(p); setRules(((p.rules ?? []) as Row[]).map((r) => ({ ...r }))); setDirty(false)
  }, [fleet.policy, as.data])
  const upd = (i: number, k: string, v: unknown) => { setRules((rs) => rs.map((r, j) => (j === i ? { ...r, [k]: v } : r))); setDirty(true) }
  const last = as.data?.last as Row | undefined
  const g = (last?.gauges ?? {}) as Row
  const save = () => run(() => raw('POST', `${F}/scale`, { policy: { ...policy, rules: rules.map((r) => ({ ...r, threshold: Number(r.threshold), for_secs: Number(r.for_secs), step: Number(r.step), cooldown_secs: Number(r.cooldown_secs) })) } }), 'Autoscaler rules saved')
  return (
    <>
      <div className="mb-4 flex flex-wrap items-center gap-2">
        {as.data?.suspended
          ? <><Button size="sm" onClick={() => run(() => raw('POST', `${F}/autoscaler/resume`), 'Autoscaler resumed')}>Resume</Button><Badge variant="destructive">suspended</Badge></>
          : <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${F}/autoscaler/suspend`), 'Autoscaler suspended')}>Suspend</Button>}
      </div>
      {last && (
        <KvGrid>
          <Kv k="Last decision" v={ts(last.at)} /><Kv k="Able to scale" v={String(last.able_to_scale)} /><Kv k="Applied" v={`start ${(last.applied as Row | undefined)?.start ?? 0} · stop ${(last.applied as Row | undefined)?.stop ?? 0}`} />
          <Kv k="Live / in game" v={`${g.live ?? 0} / ${g.in_game ?? 0}`} /><Kv k="Allocated / capacity" v={`${g.allocated ?? 0} / ${g.capacity ?? 0}`} /><Kv k="Ready / idle" v={`${g.ready ?? 0} / ${g.idle_ready ?? 0}`} />
          <Kv k="CPU avg / max" v={`${g.cpu_pct_avg ?? '—'} / ${g.cpu_pct_max ?? '—'}`} /><Kv k="Base + buffer" v={`${last.base ?? 0} + ${last.buffer ?? 0}`} /><Kv k="Target" v={last.target} />
        </KvGrid>
      )}
      <SubHead title="Rules" count={rules.length}>
        <span className="self-center text-xs text-muted-foreground">evaluated top-down each cycle; first matching rule acts</span>
        <Button size="sm" variant="outline" onClick={() => { setRules((rs) => [...rs, { name: `rule-${rs.length + 1}`, metric: 'ready_pct', op: 'lt', threshold: 20, for_secs: 60, action: 'scale_up', step: 1, cooldown_secs: 120 }]); setDirty(true) }}>Add rule</Button>
        <Button size="sm" disabled={!dirty} onClick={save}>Save</Button>
      </SubHead>
      <div className="overflow-x-auto"><Table>
        <TableHeader><TableRow><TableHead>Name</TableHead><TableHead>Metric</TableHead><TableHead className="w-20">Op</TableHead><TableHead className="w-24">Threshold</TableHead><TableHead className="w-24">For (s)</TableHead><TableHead className="w-32">Action</TableHead><TableHead className="w-20">Step</TableHead><TableHead className="w-28">Cooldown (s)</TableHead><TableHead className="w-10" /></TableRow></TableHeader>
        <TableBody>
          {rules.map((r, i) => (
            <TableRow key={i}>
              <TableCell><Input className="h-8" value={String(r.name ?? '')} onChange={(e) => upd(i, 'name', e.target.value)} aria-label="rule name" /></TableCell>
              <TableCell><Sel className="h-8" value={String(r.metric ?? 'ready_pct')} onChange={(v) => upd(i, 'metric', v)} options={METRICS} /></TableCell>
              <TableCell><Sel className="h-8" value={String(r.op ?? 'lt')} onChange={(v) => upd(i, 'op', v)} options={['gt', 'gte', 'lt', 'lte']} /></TableCell>
              <TableCell><Input className="h-8" type="number" value={String(r.threshold ?? 0)} onChange={(e) => upd(i, 'threshold', e.target.value)} aria-label="threshold" /></TableCell>
              <TableCell><Input className="h-8" type="number" min={0} value={String(r.for_secs ?? 0)} onChange={(e) => upd(i, 'for_secs', e.target.value)} aria-label="for seconds" /></TableCell>
              <TableCell><Sel className="h-8" value={String(r.action ?? 'scale_up')} onChange={(v) => upd(i, 'action', v)} options={[['scale_up', 'scale up'], ['scale_down', 'scale down']]} /></TableCell>
              <TableCell><Input className="h-8" type="number" min={1} value={String(r.step ?? 1)} onChange={(e) => upd(i, 'step', e.target.value)} aria-label="step" /></TableCell>
              <TableCell><Input className="h-8" type="number" min={0} value={String(r.cooldown_secs ?? 0)} onChange={(e) => upd(i, 'cooldown_secs', e.target.value)} aria-label="cooldown seconds" /></TableCell>
              <TableCell><Button size="sm" variant="ghost" onClick={() => { setRules((rs) => rs.filter((_, j) => j !== i)); setDirty(true) }} aria-label="remove rule">×</Button></TableCell>
            </TableRow>
          ))}
          {!rules.length && <TableRow><TableCell colSpan={9} className="py-6 text-center text-muted-foreground">No rules — only min / buffer / max keep servers up</TableCell></TableRow>}
        </TableBody>
      </Table></div>
      <KvGrid className="mt-3">
        <Kv k="Forecast" v={<label className="flex items-center gap-2"><Checkbox checked={!!policy.forecast} onCheckedChange={(v) => { setPolicy((p) => ({ ...p, forecast: v === true })); setDirty(true) }} /> pre-scale from demand history</label>} />
        <Kv k="Webhook scaler" v={(policy.webhook as Row | null | undefined)?.url ?? 'none'} mono />
        <Kv k="WASM scaler" v={policy.wasm ? `${(policy.wasm as Row).module}.${(policy.wasm as Row).function}` : 'none'} />
      </KvGrid>
      <SubHead title="Decision chain" /><JsonView value={last?.chain ?? []} />
      <SubHead title="Demand forecast" />
      <DataTable id="fleet-demand" tools={false} rows={listOf(demand.data, 'slots')} loading={demand.isPending} empty="No demand history yet" cols={[{ key: 'slot' }, { key: 'expected' }, { key: 'observed' }]} />
      <SubHead title="Raw" /><JsonView value={as.data ?? {}} />
    </>
  )
}

function RolloutTab({ ctx: { F, run, open } }: { ctx: Ctx }) {
  const ro = useSub<Row>(`${F}/rollout`)
  const ros = useSub(`${F}/rollouts`)
  const builds = useSub(`${F}/builds`)
  const r = ro.data
  const pol = (r?.policy ?? {}) as Row
  const act = r?.rollout as Row | undefined
  return (
    <>
      {r && (
        <KvGrid>
          <Kv k="Strategy" v={pol.strategy} /><Kv k="Surge / unavailable" v={`${pol.max_surge ?? '—'} / ${pol.max_unavailable ?? '—'}`} /><Kv k="Auto rollback" v={String(pol.auto_rollback ?? false)} />
          <Kv k="Progress" v={<span className="flex items-center gap-2"><span className="h-1.5 w-36 overflow-hidden rounded bg-muted"><span className="block h-full bg-primary" style={{ width: `${Number(r.progress_pct ?? 0)}%` }} /></span>{String(r.progress_pct ?? 0)}%</span>} />
          <Kv k="Updated / outdated / live" v={`${r.updated ?? 0} / ${r.outdated ?? 0} / ${r.live ?? 0}`} />
          <Kv k="Active rollout" v={act?.state ?? 'none'} /><Kv k="Target version" v={act?.version} mono />
        </KvGrid>
      )}
      {act && (
        <div className="mt-3 flex flex-wrap gap-2">
          {act.state !== 'paused' ? <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${F}/rollout/pause`), 'Rollout paused')}>Pause</Button> : <Button size="sm" onClick={() => run(() => raw('POST', `${F}/rollout/resume`), 'Rollout resumed')}>Resume</Button>}
          <Button size="sm" variant="destructive" onClick={() => confirmThen('Roll back to the previous version?', () => run(() => raw('POST', `${F}/rollout/rollback`), 'Rolling back'))}>Rollback</Button>
        </div>
      )}
      <SubHead title="Builds" count={listOf(builds.data, 'builds').length}><Button size="sm" onClick={() => open('build')}>Add build / canary</Button></SubHead>
      <DataTable id="fleet-builds" rows={listOf(builds.data, 'builds')} loading={builds.isPending} empty="No builds registered — servers run the fleet spec"
        cols={[{ key: 'version', mono: true }, { key: 'state' }, { key: 'canary_percent', label: 'canary %' }, { key: 'servers' }, { key: 'notes', dim: true }, { key: 'created_at', dim: true, fmt: (b) => ts(b.created_at) }]}
        actions={(b) => b.state === 'live' ? null : (
          <span className="flex justify-end gap-1">
            <Button size="sm" variant="ghost" onClick={() => confirmThen(`Promote ${b.version} to the whole fleet?`, () => run(() => raw('POST', `${F}/builds/${enc(String(b.version))}/promote`), 'Promoted'))}>Promote</Button>
            <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Remove ${b.version}?`, () => run(() => raw('DELETE', `${F}/builds/${enc(String(b.version))}`), 'Removed'))}>Remove</Button>
          </span>
        )} />
      <SubHead title="Rollout history" />
      <DataTable id="fleet-rollouts" tools={false} rows={listOf(ros.data, 'rollouts')} loading={ros.isPending} empty="No rollouts yet"
        cols={[{ key: 'id', mono: true }, { key: 'from' }, { key: 'to' }, { key: 'state' }, { key: 'started_at', dim: true, fmt: (x) => ts(x.started_at) }, { key: 'finished_at', dim: true, fmt: (x) => ts(x.finished_at) }]} />
    </>
  )
}

function HealthTab({ ctx: { F, run, open } }: { ctx: Ctx }) {
  const h = useSub<Row>(`${F}/health`)
  const alerts = useSub(`${F}/alerts`)
  const rules = useSub(`${F}/alerts/rules`)
  const pol = (h.data?.policy ?? {}) as Row
  const counts = (h.data?.counts ?? {}) as Record<string, number>
  return (
    <>
      {h.data && (
        <KvGrid>
          <Kv k="Probe" v={pol.disabled ? 'disabled' : `every ${pol.period_secs}s after ${pol.initial_delay_secs}s`} /><Kv k="Failure threshold" v={pol.failure_threshold} />
          <Kv k="Eviction" v={h.data.eviction} /><Kv k="Timeout" v={h.data.timeout_secs != null ? `${h.data.timeout_secs}s` : undefined} />
          <Kv k="Counts" v={Object.keys(counts).length ? <span className="space-x-3">{Object.entries(counts).map(([k, n]) => <span key={k}>{k} <b>{n}</b></span>)}</span> : '—'} />
        </KvGrid>
      )}
      <SubHead title="Server health" />
      <DataTable id="fleet-health" tools={false} rows={listOf(h.data, 'servers')} loading={h.isPending} empty="No servers probed"
        cols={[{ key: 'id', mono: true }, { key: 'status' }, { key: 'failures' }, { key: 'last_probe_at', dim: true, fmt: (x) => ts(x.last_probe_at) }, { key: 'message', dim: true }]} />
      <SubHead title="Firing alerts" count={listOf(alerts.data, 'alerts').length}><Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${F}/alerts/sweep`), 'Alerts evaluated')}>Evaluate now</Button></SubHead>
      <DataTable id="fleet-alerts" tools={false} rows={listOf(alerts.data, 'alerts')} loading={alerts.isPending} empty="Nothing is firing"
        cols={[{ key: 'rule' }, { key: 'severity' }, { key: 'value' }, { key: 'since', dim: true, fmt: (x) => ts(x.since ?? x.started_at) }, { key: 'message', dim: true }]} />
      <SubHead title="Alert rules" count={listOf(rules.data, 'rules').length}><Button size="sm" onClick={() => open('rule')}>Add rule</Button></SubHead>
      <DataTable id="fleet-alert-rules" tools={false} rows={listOf(rules.data, 'rules')} loading={rules.isPending} empty="No alert rules"
        cols={[{ key: 'name' }, { key: 'metric' }, { key: 'op' }, { key: 'threshold' }, { key: 'for_secs', label: 'for (s)' }, { key: 'severity' }]}
        actions={(x) => <Button size="sm" variant="ghost" className="text-destructive" onClick={() => run(() => raw('DELETE', `${F}/alerts/rules/${enc(String(x.name))}`), 'Rule deleted')}>Delete</Button>} />
    </>
  )
}

function CostTab({ ctx: { F } }: { ctx: Ctx }) {
  const cost = useSub<Row>(`${F}/cost`)
  const usage = useSub<Row>(`${F}/usage`)
  const egress = useSub(`${F}/egress`)
  const c = cost.data
  const u = usage.data
  return (
    <>
      {c && <KvGrid><Kv k="Cost model" v={c.configured ? 'configured' : 'not configured (spec.cost)'} /><Kv k="Live servers" v={c.live} /><Kv k="Hourly estimate" v={c.hourly != null ? `${c.hourly} ${c.currency ?? ''}` : '—'} /><Kv k="Monthly estimate" v={c.monthly != null ? `${c.monthly} ${c.currency ?? ''}` : '—'} /></KvGrid>}
      <SubHead title="Usage">{u && <span className="self-center text-xs text-muted-foreground">{ts(u.from)} → {ts(u.to)}, {Math.max(0, Number(u.vcpu_hours ?? 0)).toFixed(1)} vCPU·h</span>}</SubHead>
      <DataTable id="fleet-usage" tools={false} rows={listOf(u, 'rows')} loading={usage.isPending} empty="No usage recorded yet"
        cols={[{ key: 'day', fmt: (x) => x.day ?? x.date ?? ts(x.at) }, { key: 'servers' }, { key: 'server_hours', label: 'server·h' }, { key: 'vcpu_hours', label: 'vCPU·h' }, { key: 'cost' }]} />
      <SubHead title="Egress" /><JsonView value={egress.data ?? {}} />
    </>
  )
}

function EventsTab({ ctx: { F } }: { ctx: Ctx }) {
  const events = useSub(`${F}/events?limit=200`)
  const metrics = useSub<Row>(`${F}/metrics`)
  const m = metrics.data
  const a = (m?.aggregate ?? {}) as Row
  return (
    <>
      {m && (
        <KvGrid>
          <Kv k="Live servers / players" v={`${m.live_servers ?? 0} / ${m.players ?? 0}`} /><Kv k="Reporting" v={`${a.servers_reporting ?? 0} servers, ${a.samples ?? 0} samples`} />
          <Kv k="CPU avg / max %" v={`${a.cpu_pct_avg ?? '—'} / ${a.cpu_pct_max ?? '—'}`} /><Kv k="Memory avg / max" v={`${a.mem_bytes_avg ?? '—'} / ${a.mem_bytes_max ?? '—'}`} />
          <Kv k="Tick avg / p99 max (ms)" v={`${a.tick_ms_avg ?? '—'} / ${a.tick_p99_ms_max ?? '—'}`} /><Kv k="Firing alerts" v={m.firing_alerts} />
        </KvGrid>
      )}
      <SubHead title="Per-server metrics" />
      <DataTable id="fleet-metrics" rows={listOf(m, 'servers')} loading={metrics.isPending} empty="No metrics reported — servers report through the SDK heartbeat"
        cols={[{ key: 'id', mono: true }, { key: 'cpu_pct' }, { key: 'mem_bytes' }, { key: 'tick_ms' }, { key: 'tick_p99_ms' }, { key: 'players' }, { key: 'at', dim: true, fmt: (x) => ts(x.at) }]} />
      <SubHead title="Events" count={listOf(events.data, 'events').length} />
      <DataTable id="fleet-events" rows={listOf(events.data, 'events')} loading={events.isPending} empty="No events yet"
        cols={[{ key: 'at', dim: true, fmt: (e) => ts(e.at ?? e.created_at) }, { key: 'kind', mono: true }, { key: 'payload', dim: true, fmt: (e) => <span className="font-mono text-xs break-all">{JSON.stringify(e.payload ?? e.data ?? {})}</span> }]} />
    </>
  )
}
