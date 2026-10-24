import { useState } from 'react'
import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import type { Row } from '@/model/resource'
import { Bars } from '@/components/bars'
import { Crumbs } from '@/components/crumbs'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'

/** The KPI sheet: active users and stickiness, retention cohorts, revenue per day, top events, geography, attribution, crash-free rate and a funnel of your choosing — all computed by the node from its own event store. */
export const Route = createFileRoute('/_app/analytics/')({
  validateSearch: (s: Record<string, unknown>): { days?: number; funnel?: string } => ({ days: [7, 14, 30, 90].includes(Number(s.days)) && Number(s.days) !== 30 ? Number(s.days) : undefined, funnel: typeof s.funnel === 'string' && s.funnel ? s.funnel : undefined }),
  component: Overview,
})

const A = '/v1/server/analytics'
const pct = (v: unknown) => (v == null || v === '' ? '—' : (Number(v) * 100).toFixed(Number(v) >= 0.1 ? 0 : 1) + '%')
const num = (v: unknown) => (v == null ? '—' : Number(v).toLocaleString())
const useKpi = (path: string | null) => useQuery({ queryKey: ['sub', path], queryFn: () => raw<Row>('GET', path!), enabled: !!path, staleTime: 30_000 })

function Stat({ k, v, sub, to, warn }: { k: string; v: string; sub?: string; to?: string; warn?: boolean }) {
  const body = <><div className="text-xs text-muted-foreground">{k}</div><div className={`text-2xl font-semibold tabular-nums ${warn ? 'text-destructive' : ''}`}>{v}</div>{sub && <div className="truncate text-xs text-muted-foreground">{sub}</div>}</>
  return <Card><CardContent className="p-4">{to ? <Link to={to} className="block hover:underline">{body}</Link> : body}</CardContent></Card>
}

function Overview() {
  const { days = 30, funnel = 'login,match_start,match_end' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const au = useKpi(`${A}/kpi/active_users?days=${days}`), ret = useKpi(`${A}/kpi/retention?days=${days}`), rev = useKpi(`${A}/kpi/revenue?days=${days}`), ev = useKpi(`${A}/kpi/events?days=${days}`)
  const geo = useKpi(`${A}/geo`), attr = useKpi(`${A}/attribution`), cr = useKpi('/v1/server/crashes/summary'), samp = useKpi(`${A}/sampling`), rep = useKpi(`${A}/reports/latest`)
  const fun = useKpi(`${A}/kpi/funnel?steps=${encodeURIComponent(funnel)}&days=${days}`)
  const [funnelTxt, setFunnelTxt] = useState(funnel)
  const [pctTxt, setPctTxt] = useState<string | null>(null)
  const { run, pending } = useAct([['sub']])
  const daily = ((au.data?.daily as Row[] | undefined) ?? []).map((d) => ({ label: String(d.day), v: Number(d.users ?? 0) }))
  const revDay = (() => { const m = new Map<string, number>(); for (const d of (rev.data?.daily as Row[] | undefined) ?? []) m.set(String(d.day), (m.get(String(d.day)) ?? 0) + Number(d.amount ?? 0)); return [...m.entries()].sort().map(([label, v]) => ({ label, v })) })()
  const cohorts = (ret.data?.cohorts as Row[] | undefined) ?? []
  const steps = (fun.data?.steps as Row[] | undefined) ?? []
  const countries = (geo.data?.countries as Row[] | undefined) ?? []
  const groups = (attr.data?.groups as Row[] | undefined) ?? []
  const events = (ev.data?.events as Row[] | undefined) ?? []
  const errs = [au, ret, rev, ev, fun].filter((q) => q.isError).map((q) => String(q.error))
  const crPct = cr.data?.crash_free_users_pct as number | undefined
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Analytics' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Analytics</h1><p className="mt-1 text-sm text-muted-foreground">Computed by the node from the events your game sends — nothing leaves the cluster. Sampling below {samp.data ? `${String(samp.data.global_pct ?? 100)}%` : '100%'} is applied at ingest; the KPIs are scaled accordingly.</p></div>
        <Tabs value={String(days)} onValueChange={(v) => void nav({ search: (p) => ({ ...p, days: Number(v) === 30 ? undefined : Number(v) }), replace: true })}><TabsList>{[7, 14, 30, 90].map((d) => <TabsTrigger key={d} value={String(d)}>{d}d</TabsTrigger>)}</TabsList></Tabs>
        <Button size="sm" variant="outline" disabled={pending} onClick={() => run(() => raw('POST', `${A}/flush`), 'Buffered events flushed to the store')}>Flush events</Button>
      </div>
      {errs.length > 0 && <Alert variant="destructive"><AlertDescription>{errs[0]}</AlertDescription></Alert>}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
        <Stat k="DAU" v={num(au.data?.dau)} sub={`WAU ${num(au.data?.wau)} · MAU ${num(au.data?.mau)}`} to="/analytics/active_users" />
        <Stat k="Stickiness (DAU/MAU)" v={pct(au.data?.stickiness)} />
        <Stat k="D1 / D7 retention" v={`${pct(cohorts[0]?.d1)} / ${pct(cohorts[0]?.d7)}`} sub="newest cohort" />
        <Stat k={`Revenue, ${days}d`} v={num(rev.data?.total ?? revDay.reduce((s, d) => s + d.v, 0))} sub={`ARPDAU ${num(rev.data?.arpdau)} · ARPPU ${num(rev.data?.arppu)}`} to="/analytics/revenue" />
        <Stat k="Crash-free users (24 h)" v={crPct != null ? `${Number(crPct).toFixed(1)}%` : '—'} sub={`${num(cr.data?.crashes ?? 0)} crashes · ${num(cr.data?.groups ?? 0)} groups`} warn={crPct != null && crPct < 99} to="/players/crashes" />
        <Stat k="Last daily report" v={rep.data?.for_date ? String(rep.data.for_date) : '—'} sub={rep.data?.generated_at ? `generated ${String(rep.data.generated_at).replace('T', ' ').slice(0, 16)}` : 'none yet'} to="/analytics/reports" />
      </div>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Daily active users</CardTitle></CardHeader><CardContent><Bars data={daily} /><p className="mt-1 text-xs text-muted-foreground">{daily.length} days with activity in the period</p></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Revenue per day</CardTitle></CardHeader><CardContent><Bars data={revDay} format={(v) => v.toLocaleString()} /><p className="mt-1 text-xs text-muted-foreground">{(rev.data?.daily as Row[] | undefined)?.length ? `currencies: ${[...new Set(((rev.data!.daily as Row[])).map((d) => String(d.currency ?? '')))].filter(Boolean).join(', ') || 'n/a'}` : 'no purchases in the period'}</p></CardContent></Card>
      </div>
      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Retention cohorts</CardTitle><p className="text-sm text-muted-foreground">Share of accounts created on a day that came back 1 / 3 / 7 / 14 / 30 days later. Cells further right are empty until the cohort is old enough.</p></CardHeader><CardContent className="p-0">
        <Table><TableHeader><TableRow><TableHead>Cohort</TableHead><TableHead className="text-right">Size</TableHead>{['d1', 'd3', 'd7', 'd14', 'd30'].map((d) => <TableHead key={d} className="text-right">{d.toUpperCase()}</TableHead>)}</TableRow></TableHeader>
          <TableBody>{!cohorts.length ? <TableRow><TableCell colSpan={7} className="text-muted-foreground">{ret.isPending ? 'Loading…' : 'No cohorts yet — accounts need to be created in the period.'}</TableCell></TableRow> : cohorts.map((c) => <TableRow key={String(c.cohort)}><TableCell className="font-mono text-xs">{String(c.cohort)}</TableCell><TableCell className="text-right tabular-nums">{num(c.size)}</TableCell>{['d1', 'd3', 'd7', 'd14', 'd30'].map((d) => <TableCell key={d} className="text-right tabular-nums">{pct(c[d])}</TableCell>)}</TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Funnel</CardTitle><p className="text-sm text-muted-foreground">Accounts that performed each event at least once in the period, in order.</p></CardHeader><CardContent>
        <form className="mb-3 flex flex-wrap gap-2" onSubmit={(e) => { e.preventDefault(); void nav({ search: (p) => ({ ...p, funnel: funnelTxt.trim() || undefined }), replace: true }) }}><Input className="min-w-0 flex-1 font-mono" value={funnelTxt} onChange={(e) => setFunnelTxt(e.target.value)} placeholder="event names, comma-separated" /><Button type="submit" size="sm">Run</Button></form>
        <Table><TableHeader><TableRow><TableHead>Step</TableHead><TableHead className="text-right">Users</TableHead><TableHead className="text-right">From previous</TableHead><TableHead className="text-right">From start</TableHead></TableRow></TableHeader>
          <TableBody>{!steps.length ? <TableRow><TableCell colSpan={4} className="text-muted-foreground">{fun.isPending ? 'Loading…' : 'No users passed the first step.'}</TableCell></TableRow> : steps.map((s) => <TableRow key={String(s.step ?? s.name)}><TableCell className="font-mono text-xs">{String(s.step ?? s.name)}</TableCell><TableCell className="text-right tabular-nums">{num(s.users ?? s.count)}</TableCell><TableCell className="text-right tabular-nums">{pct(s.conversion_from_previous)}</TableCell><TableCell className="text-right tabular-nums">{pct(s.conversion_from_start)}</TableCell></TableRow>)}</TableBody></Table>
      </CardContent></Card>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Top events, {days}d</CardTitle></CardHeader><CardContent className="p-0"><Table><TableBody>{!events.length ? <TableRow><TableCell className="text-muted-foreground">No events.</TableCell></TableRow> : events.slice(0, 15).map((e) => <TableRow key={String(e.name)}><TableCell><Link className="font-mono text-xs underline" to="/analytics/events" search={{ name: String(e.name) }}>{String(e.name)}</Link></TableCell><TableCell className="text-right tabular-nums">{num(e.count)}</TableCell></TableRow>)}</TableBody></Table></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Geography <span className="text-sm font-normal text-muted-foreground">({num(geo.data?.total ?? 0)} accounts)</span></CardTitle></CardHeader><CardContent className="p-0"><Table><TableBody>{!countries.length ? <TableRow><TableCell className="text-muted-foreground">No geo data — the node resolves it from the login IP when the GeoIP database is configured.</TableCell></TableRow> : countries.slice(0, 15).map((c) => <TableRow key={String(c.country)}><TableCell className="font-mono text-xs">{String(c.country)}</TableCell><TableCell className="text-right tabular-nums">{num(c.accounts)}</TableCell><TableCell className="text-right tabular-nums text-muted-foreground">{pct(Number(c.accounts) / Math.max(1, Number(geo.data?.total ?? 1)))}</TableCell></TableRow>)}</TableBody></Table></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Attribution <span className="text-sm font-normal text-muted-foreground">({String(attr.data?.touch ?? 'first')} touch by {String(attr.data?.group_by ?? 'campaign')})</span></CardTitle></CardHeader><CardContent className="p-0"><Table><TableBody>{!groups.length ? <TableRow><TableCell className="text-muted-foreground">No install attribution recorded yet.</TableCell></TableRow> : groups.slice(0, 15).map((g) => <TableRow key={String(g.group)}><TableCell className="text-xs">{String(g.group ?? '(organic)')}</TableCell><TableCell className="text-right tabular-nums">{num(g.installs ?? g.users ?? g.count)}</TableCell><TableCell className="text-right tabular-nums text-muted-foreground">{g.revenue != null ? num(g.revenue) : ''}</TableCell></TableRow>)}</TableBody></Table></CardContent></Card>
      </div>
      <Card><CardHeader className="pb-2"><CardTitle className="text-base">Sampling</CardTitle><p className="text-sm text-muted-foreground">Keep only this share of incoming events (per account, so funnels stay consistent). Event names listed as always-enabled are never sampled.</p></CardHeader><CardContent>
        <form className="flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); run(() => raw('PUT', `${A}/sampling`, { global_pct: Number(pctTxt ?? samp.data?.global_pct ?? 100), always_enabled: samp.data?.always_enabled ?? [] }), 'Sampling saved') }}>
          <Input type="number" min={1} max={100} className="w-24" value={pctTxt ?? String(samp.data?.global_pct ?? 100)} onChange={(e) => setPctTxt(e.target.value)} /><span className="text-sm">% of events</span><Button type="submit" size="sm" variant="outline" disabled={pending}>Save</Button>
          <span className="text-xs text-muted-foreground">always: {((samp.data?.always_enabled as string[] | undefined) ?? []).join(', ') || 'none'} — edit on the <Link className="underline" to="/analytics/events">Events</Link> page</span>
        </form>
      </CardContent></Card>
    </div>
  )
}
