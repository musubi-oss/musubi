import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Matchmaking queues: composition, widening skill / attribute / ping windows, roles, crossplay, external match functions; pause / drain / simulate. */
export const Route = createFileRoute('/_app/social/matchmaker')({ component: Matchmaker })

interface Rule { kind: string; property: string; max: number; step: number; every_secs: number; max_limit: number; min: number; missing: string }
const blank = (): Row => ({ name: '', min_count: 2, max_count: 2, teams: null, mmr: null, rules: [], sort: null, ping: null, roles: {}, no_repeat_secs: 0, crossplay: [], priority_after_secs: 0, ready_check_secs: null, dodge_penalty_secs: null, mmf: null, extensions: {} })
const DEF = { teams: { count: 2, size: 1, kind: 'balanced', property: 'mmr', order: 'desc' }, mmr: { property: 'mmr', initial_delta: 100, step: 50, every_secs: 10, max_delta: 500 }, ping: { prefix: 'ping_', initial_ms: 60, step_ms: 20, every_secs: 10, max_ms: 200 }, sort: { kind: 'property', property: 'mmr', order: 'desc' }, mmf: { kind: 'http', url: '', headers: {}, timeout_ms: 2000, every_ms: 1000, min_tickets: 2, fallback_after_secs: 0, propose: '', score: '' } }
const summary = (x: Row) => { const t = x.teams as Row | null; const m = x.mmr as Row | null; const p = x.ping as Row | null; const f = x.mmf as Row | null; return [t ? `${t.count}×${t.size} teams` : `${x.min_count}–${x.max_count} players`, m ? `mmr ±${m.initial_delta}→${m.max_delta}` : null, ((x.rules ?? []) as unknown[]).length ? `${(x.rules as unknown[]).length} rule(s)` : null, p ? `ping ≤${p.initial_ms}→${p.max_ms}ms` : null, f ? `mmf ${f.kind}` : null].filter(Boolean).join(' · ') }
const stateOf = (x: Row) => (x.draining ? 'draining' : x.paused ? 'paused' : 'open')

function Num({ v, set, label, w = 'w-24', step }: { v: unknown; set: (n: number) => void; label: string; w?: string; step?: string }) {
  return <label className="text-xs"><span className="block text-muted-foreground">{label}</span><Input className={`h-8 ${w}`} type="number" step={step} value={v == null ? '' : String(v)} onChange={(e) => set(Number(e.target.value))} /></label>
}

function Matchmaker() {
  const q = useQuery({ queryKey: ['mm-queues'], queryFn: ({ signal }) => raw<Row>('GET', '/v1/server/matchmaker/queues', undefined, { signal }) })
  const stats = useSub<Row>('/v1/server/matchmaker/stats')
  const mmf = useSub<Row>('/v1/server/matchmaker/mmf')
  const queues = (q.data?.queues ?? []) as Row[]
  const waiting = (stats.data?.waiting ?? {}) as Record<string, number>
  const ages = (stats.data?.waiting_ages ?? {}) as Record<string, Row | number>
  const age = (n: string) => { const a = ages[n]; return a == null ? '—' : typeof a === 'object' ? `${a.p50 ?? a.avg ?? '?'}s / max ${a.max ?? '?'}s` : `${a}s` }
  const [dlg, setDlg] = useState<'' | 'edit' | 'sim'>('')
  const [isNew, setIsNew] = useState(false)
  const [f, setF] = useState<Row>(blank())
  const [rules, setRules] = useState<Rule[]>([]); const [roles, setRoles] = useState<{ name: string; count: number }[]>([]); const [crossplay, setCrossplay] = useState(''); const [ext, setExt] = useState('{}'); const [mmfHeaders, setMmfHeaders] = useState('{}')
  const [sim, setSim] = useState<{ queue: string; tickets: number }>({ queue: '', tickets: 100 }); const [simOut, setSimOut] = useState<unknown>(null); const [simBusy, setSimBusy] = useState(false)
  const [showRaw, setShowRaw] = useState(false)
  const { run, pending } = useAct([['mm-queues'], ['sub']], () => setDlg(''))
  const set = (k: string, v: unknown) => setF((x) => ({ ...x, [k]: v }))
  const sub = (k: string, kk: string, v: unknown) => setF((x) => ({ ...x, [k]: { ...((x[k] ?? {}) as Row), [kk]: v } }))
  const toggle = (k: keyof typeof DEF) => set(k, f[k] ? null : { ...DEF[k] })
  function edit(x: Row | null) {
    const v = x ? (JSON.parse(JSON.stringify(x)) as Row) : blank()
    setIsNew(!x); setF(v)
    setRules(((v.rules ?? []) as Row[]).map((r) => ({ kind: String(r.kind ?? 'difference'), property: String(r.property ?? ''), max: Number(r.max ?? 0), step: Number(r.step ?? 0), every_secs: Number(r.every_secs ?? 0), max_limit: Number(r.max_limit ?? 0), min: Number(r.min ?? 0), missing: String(r.missing ?? 'reject') })))
    setRoles(Object.entries((v.roles ?? {}) as Record<string, unknown>).map(([name, count]) => ({ name, count: Number(count) })))
    setCrossplay(((v.crossplay ?? []) as string[][]).map((g) => g.join('+')).join(', ')); setExt(JSON.stringify(v.extensions ?? {}, null, 2)); setMmfHeaders(JSON.stringify((v.mmf as Row | null)?.headers ?? {}))
    setDlg('edit')
  }
  function body(): Row | null {
    let extensions: unknown, headers: unknown
    try { extensions = JSON.parse(ext || '{}'); headers = JSON.parse(mmfHeaders || '{}') } catch { toast.error('Extensions / MMF headers must be valid JSON'); return null }
    const { paused: _p, draining: _d, status_reason: _s, ...rest } = f
    const b: Row = { ...rest, rules, roles: Object.fromEntries(roles.filter((r) => r.name).map((r) => [r.name, Number(r.count)])), crossplay: crossplay.split(',').map((g) => g.trim()).filter(Boolean).map((g) => g.split('+').map((s) => s.trim()).filter(Boolean)), extensions }
    if (b.mmf) b.mmf = { ...(b.mmf as Row), headers }
    return b
  }
  const T = f.teams as Row | null; const M = f.mmr as Row | null; const P = f.ping as Row | null; const S = f.sort as Row | null; const F = f.mmf as Row | null
  const totalWaiting = Object.values(waiting).reduce((a, b) => a + Number(b), 0)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Social', to: '/social' }, { title: 'Matchmaking' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Matchmaking</h1><p className="mt-1 text-sm text-muted-foreground">{queues.length} queues · {totalWaiting} waiting · {String(stats.data?.open_matches ?? 0)} open matches</p></div>
        <Button size="sm" variant="outline" onClick={() => setShowRaw((v) => !v)}>{showRaw ? 'Hide raw' : 'Raw'}</Button>
        <Button size="sm" variant="outline" disabled={!queues.length} onClick={() => { setSim({ queue: String(queues[0]?.name ?? ''), tickets: 100 }); setSimOut(null); setDlg('sim') }}>Simulate</Button>
        <Button size="sm" onClick={() => edit(null)}>New queue</Button>
      </div>
      <p className="text-sm text-muted-foreground">A ticket enters one queue; the matcher widens MMR / attribute / ping windows over time, then forms teams. Paused queues accept tickets but do not match; draining queues cancel waiting tickets.</p>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="pt-6">
        <DataTable id="mm-queues" rows={queues} loading={q.isPending} empty="No queues — players cannot matchmake until one exists"
          cols={[{ key: 'name', mono: true }, { key: 'state', fmt: (x) => <span className="inline-flex items-center gap-1"><Badge variant={x.draining ? 'destructive' : x.paused ? 'secondary' : 'outline'}>{stateOf(x)}</Badge>{x.status_reason ? <span className="text-xs text-muted-foreground">{String(x.status_reason)}</span> : null}</span> }, { key: 'composition', dim: true, fmt: summary }, { key: 'waiting', fmt: (x) => waiting[String(x.name)] ?? 0 }, { key: 'wait', label: 'wait p50 / max', dim: true, fmt: (x) => age(String(x.name)) }]}
          actions={(x) => { const n = encodeURIComponent(String(x.name)); return <span className="flex justify-end gap-1"><Button size="sm" variant="ghost" onClick={() => edit(x)}>Edit</Button>{x.paused ? <Button size="sm" variant="ghost" onClick={() => run(() => raw('POST', `/v1/server/matchmaker/queues/${n}/resume`), 'Resumed')}>Resume</Button> : <Button size="sm" variant="ghost" onClick={() => run(() => raw('POST', `/v1/server/matchmaker/queues/${n}/pause`), 'Paused')}>Pause</Button>}<Button size="sm" variant="ghost" disabled={!!x.draining} onClick={() => confirmThen(`Drain ${x.name}? Every waiting ticket is cancelled.`, () => run(() => raw('POST', `/v1/server/matchmaker/queues/${n}/drain`), 'Draining'))}>Drain</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Delete queue ${x.name}?`, () => run(() => raw('DELETE', `/v1/server/matchmaker/queues/${n}`), 'Deleted'))}>×</Button></span> }} />
        {showRaw && <><SubHead title="Stats" /><JsonView value={stats.data} /><SubHead title="Match functions" /><JsonView value={mmf.data} empty="No external match functions registered" /></>}
      </CardContent></Card>

      <FormDialog open={dlg === 'edit'} onOpenChange={(o) => !o && setDlg('')} title={isNew ? 'New queue' : `Queue ${f.name}`} submit="Save" pending={pending} disabled={!f.name} wide
        onSubmit={() => { const b = body(); if (b) run(() => raw('PUT', '/v1/server/matchmaker/queues', b), 'Queue saved') }}>
        <div className="grid grid-cols-3 gap-3">
          <Field label="Name"><Input className="font-mono" value={String(f.name ?? '')} disabled={!isNew} onChange={(e) => set('name', e.target.value)} /></Field>
          <Num label="Min players" v={f.min_count} set={(n) => set('min_count', n)} w="w-full" /><Num label="Max players" v={f.max_count} set={(n) => set('max_count', n)} w="w-full" />
        </div>
        <SubHead title={<label className="flex items-center gap-2"><Checkbox checked={!!T} onCheckedChange={() => toggle('teams')} /> Teams</label>} />
        {T && <div className="flex flex-wrap gap-2"><Num label="Count" v={T.count} set={(n) => sub('teams', 'count', n)} /><Num label="Size" v={T.size} set={(n) => sub('teams', 'size', n)} /><label className="text-xs"><span className="block text-muted-foreground">Fill</span><Select value={String(T.kind)} onValueChange={(v) => sub('teams', 'kind', v)}><SelectTrigger className="h-8 w-32"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="balanced">balanced</SelectItem><SelectItem value="random">random</SelectItem><SelectItem value="party">keep parties</SelectItem></SelectContent></Select></label><label className="text-xs"><span className="block text-muted-foreground">Balance by</span><Input className="h-8 w-28 font-mono" value={String(T.property ?? '')} onChange={(e) => sub('teams', 'property', e.target.value)} /></label></div>}
        <SubHead title={<label className="flex items-center gap-2"><Checkbox checked={!!M} onCheckedChange={() => toggle('mmr')} /> Skill window</label>} />
        {M && <div className="flex flex-wrap gap-2"><label className="text-xs"><span className="block text-muted-foreground">Property</span><Input className="h-8 w-28 font-mono" value={String(M.property ?? '')} onChange={(e) => sub('mmr', 'property', e.target.value)} /></label><Num label="Initial ±" v={M.initial_delta} set={(n) => sub('mmr', 'initial_delta', n)} /><Num label="+ step" v={M.step} set={(n) => sub('mmr', 'step', n)} /><Num label="every (s)" v={M.every_secs} set={(n) => sub('mmr', 'every_secs', n)} /><Num label="Max ±" v={M.max_delta} set={(n) => sub('mmr', 'max_delta', n)} /></div>}
        <SubHead title="Attribute rules" count={rules.length}><Button type="button" size="sm" variant="outline" onClick={() => setRules((r) => [...r, { kind: 'difference', property: '', max: 0, step: 0, every_secs: 0, max_limit: 0, min: 0, missing: 'reject' }])}>Add</Button></SubHead>
        {rules.map((r, i) => { const u = (k: keyof Rule, v: unknown) => setRules((rs) => rs.map((x, j) => (j === i ? { ...x, [k]: v } : x))); return (
          <div key={i} className="flex flex-wrap items-end gap-2 rounded border p-2">
            <label className="text-xs"><span className="block text-muted-foreground">Kind</span><Select value={r.kind} onValueChange={(v) => u('kind', v)}><SelectTrigger className="h-8 w-28"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="difference">difference</SelectItem><SelectItem value="equal">equal</SelectItem><SelectItem value="count">count</SelectItem></SelectContent></Select></label>
            <label className="text-xs"><span className="block text-muted-foreground">Property</span><Input className="h-8 w-28 font-mono" value={r.property} onChange={(e) => u('property', e.target.value)} /></label>
            <Num label="Max" v={r.max} set={(n) => u('max', n)} w="w-20" /><Num label="Step" v={r.step} set={(n) => u('step', n)} w="w-20" /><Num label="Every (s)" v={r.every_secs} set={(n) => u('every_secs', n)} w="w-20" /><Num label="Max limit" v={r.max_limit} set={(n) => u('max_limit', n)} w="w-20" /><Num label="Min" v={r.min} set={(n) => u('min', n)} w="w-20" />
            <label className="text-xs"><span className="block text-muted-foreground">Missing</span><Select value={r.missing} onValueChange={(v) => u('missing', v)}><SelectTrigger className="h-8 w-24"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="reject">reject</SelectItem><SelectItem value="ignore">ignore</SelectItem></SelectContent></Select></label>
            <Button type="button" size="sm" variant="ghost" onClick={() => setRules((rs) => rs.filter((_, j) => j !== i))}>×</Button>
          </div>) })}
        <SubHead title={<label className="flex items-center gap-2"><Checkbox checked={!!P} onCheckedChange={() => toggle('ping')} /> Ping window</label>} />
        {P && <div className="flex flex-wrap gap-2"><label className="text-xs"><span className="block text-muted-foreground">Property prefix</span><Input className="h-8 w-28 font-mono" value={String(P.prefix ?? '')} onChange={(e) => sub('ping', 'prefix', e.target.value)} /></label><Num label="Initial ms" v={P.initial_ms} set={(n) => sub('ping', 'initial_ms', n)} /><Num label="+ ms" v={P.step_ms} set={(n) => sub('ping', 'step_ms', n)} /><Num label="every (s)" v={P.every_secs} set={(n) => sub('ping', 'every_secs', n)} /><Num label="Max ms" v={P.max_ms} set={(n) => sub('ping', 'max_ms', n)} /></div>}
        <SubHead title={<label className="flex items-center gap-2"><Checkbox checked={!!S} onCheckedChange={() => toggle('sort')} /> Sort tickets</label>} />
        {S && <div className="flex flex-wrap gap-2"><Input className="h-8 w-28" value={String(S.kind ?? '')} placeholder="kind" onChange={(e) => sub('sort', 'kind', e.target.value)} /><Input className="h-8 w-36 font-mono" value={String(S.property ?? '')} placeholder="property" onChange={(e) => sub('sort', 'property', e.target.value)} /><Select value={String(S.order)} onValueChange={(v) => sub('sort', 'order', v)}><SelectTrigger className="h-8 w-24"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="desc">desc</SelectItem><SelectItem value="asc">asc</SelectItem></SelectContent></Select></div>}
        <SubHead title="Roles (required per match)"><Button type="button" size="sm" variant="outline" onClick={() => setRoles((r) => [...r, { name: '', count: 1 }])}>Add</Button></SubHead>
        {roles.map((r, i) => <div key={i} className="flex gap-2"><Input className="h-8 w-40 font-mono" value={r.name} placeholder="healer" onChange={(e) => setRoles((rs) => rs.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))} /><Input className="h-8 w-20" type="number" min={1} value={r.count} onChange={(e) => setRoles((rs) => rs.map((x, j) => (j === i ? { ...x, count: Number(e.target.value) } : x)))} /><Button type="button" size="sm" variant="ghost" onClick={() => setRoles((rs) => rs.filter((_, j) => j !== i))}>×</Button></div>)}
        <Field label='Crossplay groups (platforms that may meet: "+" inside a group, "," between)'><Input value={crossplay} placeholder="pc+xbox+psn, ios+android" onChange={(e) => setCrossplay(e.target.value)} /></Field>
        <div className="flex flex-wrap gap-2"><Num label="No repeat opponents (s)" v={f.no_repeat_secs} set={(n) => set('no_repeat_secs', n)} w="w-32" /><Num label="Priority after (s)" v={f.priority_after_secs} set={(n) => set('priority_after_secs', n)} w="w-32" /><Num label="Ready check (s)" v={f.ready_check_secs} set={(n) => set('ready_check_secs', n || null)} w="w-32" /><Num label="Dodge penalty (s)" v={f.dodge_penalty_secs} set={(n) => set('dodge_penalty_secs', n || null)} w="w-32" /></div>
        <SubHead title={<label className="flex items-center gap-2"><Checkbox checked={!!F} onCheckedChange={() => toggle('mmf')} /> External match function</label>} />
        {F && <>
          <div className="flex gap-2"><Select value={String(F.kind)} onValueChange={(v) => sub('mmf', 'kind', v)}><SelectTrigger className="h-8 w-56"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="http">http — POST tickets, get proposals</SelectItem><SelectItem value="wasm">wasm — runtime module</SelectItem></SelectContent></Select><Input className="h-8 flex-1 font-mono" value={String(F.url ?? '')} placeholder={F.kind === 'http' ? 'https://mmf.example/propose' : 'module name'} onChange={(e) => sub('mmf', 'url', e.target.value)} /></div>
          <div className="flex flex-wrap gap-2"><Num label="Timeout ms" v={F.timeout_ms} set={(n) => sub('mmf', 'timeout_ms', n)} /><Num label="Every ms" v={F.every_ms} set={(n) => sub('mmf', 'every_ms', n)} /><Num label="Min tickets" v={F.min_tickets} set={(n) => sub('mmf', 'min_tickets', n)} /><Num label="Fallback after (s)" v={F.fallback_after_secs} set={(n) => sub('mmf', 'fallback_after_secs', n)} w="w-32" /></div>
          {F.kind === 'wasm' && <div className="flex gap-2"><Input className="h-8 font-mono" value={String(F.propose ?? '')} placeholder="propose fn" onChange={(e) => sub('mmf', 'propose', e.target.value)} /><Input className="h-8 font-mono" value={String(F.score ?? '')} placeholder="score fn" onChange={(e) => sub('mmf', 'score', e.target.value)} /></div>}
          <Field label="Headers (JSON)"><Input className="font-mono" value={mmfHeaders} onChange={(e) => setMmfHeaders(e.target.value)} /></Field>
          {!isNew && <Button type="button" size="sm" variant="outline" onClick={() => run(() => raw('POST', `/v1/server/matchmaker/queues/${encodeURIComponent(String(f.name))}/mmf/test`, { tickets: [] }), 'Match function reachable')}>Test match function</Button>}
        </>}
        <Field label="Extensions (JSON, passed to game servers with the match)"><Textarea className="min-h-16 font-mono text-xs" value={ext} onChange={(e) => setExt(e.target.value)} /></Field>
      </FormDialog>

      <FormDialog open={dlg === 'sim'} onOpenChange={(o) => !o && setDlg('')} title="Simulate matchmaking" submit="Run" pending={simBusy} disabled={!sim.queue}
        onSubmit={async () => { setSimBusy(true); try { setSimOut(await raw('POST', '/v1/server/matchmaker/simulate', { queue: sim.queue, tickets: Number(sim.tickets) })) } catch (e) { toast.error(String(e)) } finally { setSimBusy(false) } }}>
        <p className="text-xs text-muted-foreground">Generates synthetic tickets against a queue's rules and reports match sizes and wait distribution — nothing is persisted.</p>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Queue"><Select value={sim.queue} onValueChange={(v) => setSim((s) => ({ ...s, queue: v }))}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{queues.map((x) => <SelectItem key={String(x.name)} value={String(x.name)}>{String(x.name)}</SelectItem>)}</SelectContent></Select></Field>
          <Field label="Tickets"><Input type="number" min={1} value={sim.tickets} onChange={(e) => setSim((s) => ({ ...s, tickets: Number(e.target.value) }))} /></Field>
        </div>
        {simOut != null && <JsonView value={simOut} />}
      </FormDialog>
    </div>
  )
}
