import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
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
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

/** Live event editor (`new` creates one): window, recurrence, participation, phases relative to each occurrence, participants. */
export const Route = createFileRoute('/_app/liveops/live_events/$id')({
  validateSearch: (s: Record<string, unknown>): { tab?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined }),
  component: LiveEventPage,
})

interface Ph { name: string; offset_secs: string; duration_secs: string; metadata: string }
const toLocal = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().slice(0, 16) : '')
const fromLocal = (s: string) => (s ? Math.floor(new Date(s).getTime() / 1000) : null)
const tsu = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—')
const fmt = (s: number) => (s >= 86400 ? `${(s / 86400).toFixed(s % 86400 ? 1 : 0)}d` : s >= 3600 ? `${(s / 3600).toFixed(s % 3600 ? 1 : 0)}h` : s >= 60 ? `${Math.round(s / 60)}m` : `${s}s`)
const BLANK: Row = { name: '', description: '', enabled: true, participation: 'implicit', audience: null, local_time: false, max_participants: null, participation_secs: null, phases: [], metadata: {} }

function LiveEventPage() {
  const { id } = Route.useParams()
  const { tab = 'phases' } = Route.useSearch()
  const isNew = id === 'new'
  const nav = useNavigate({ from: Route.fullPath })
  const E = `/v1/server/live_events/${encodeURIComponent(id)}`
  const q = useQuery({ queryKey: ['live_event', id], enabled: !isNew, queryFn: async ({ signal }) => { const r = await raw<Row>('GET', E, undefined, { signal }); return (r.live_event ?? r.event ?? r) as Row } })
  const parts = useSub(isNew ? null : `${E}/participants`)
  const audiences = listOf(useSub('/v1/server/audiences').data, 'audiences').map((a) => String(a.name))
  const e = isNew ? BLANK : q.data
  const [f, setF] = useState<Row>(BLANK); const [phases, setPhases] = useState<Ph[]>([]); const [labels, setLabels] = useState(''); const [starts, setStarts] = useState(''); const [ends, setEnds] = useState(''); const [meta, setMeta] = useState('{}'); const [recur, setRecur] = useState('none'); const [recurSecs, setRecurSecs] = useState('0')
  useEffect(() => {
    if (!e) return
    setF({ ...e }); setPhases(((e.phases ?? []) as Row[]).map((p) => ({ name: String(p.name), offset_secs: String(p.offset_secs ?? 0), duration_secs: String(p.duration_secs ?? 0), metadata: p.metadata ? JSON.stringify(p.metadata) : '' })))
    setLabels(((e.labels ?? []) as string[]).join(', ')); setStarts(toLocal(e.starts_at)); setEnds(toLocal(e.ends_at)); setMeta(JSON.stringify(e.metadata ?? {}, null, 2))
    const rs = e.recurrence_secs as number | null | undefined; setRecur(!rs ? 'none' : rs === 3600 ? 'hourly' : rs === 86400 ? 'daily' : rs === 604800 ? 'weekly' : 'custom'); setRecurSecs(String(rs ?? 0))
  }, [e])
  const set = (k: string, v: unknown) => setF((x) => ({ ...x, [k]: v }))
  const { run, pending } = useAct([['live_event', id], ['sub']])
  const total = phases.reduce((m, p) => Math.max(m, Number(p.offset_secs) + Number(p.duration_secs)), 0)
  const now = Date.now() / 1000
  const state = !e?.starts_at ? 'draft' : !e.enabled ? 'disabled' : now < Number(e.starts_at) ? 'upcoming' : e.ends_at && now > Number(e.ends_at) ? 'ended' : 'live'
  function body(): Row | null {
    try {
      const rs = recur === 'none' ? null : recur === 'hourly' ? 3600 : recur === 'daily' ? 86400 : recur === 'weekly' ? 604800 : Number(recurSecs) || null
      return { ...f, starts_at: fromLocal(starts) ?? 0, ends_at: fromLocal(ends), recurrence_secs: rs, labels: labels.split(',').map((s) => s.trim()).filter(Boolean), metadata: JSON.parse(meta || '{}'), audience: f.audience || null,
        max_participants: f.max_participants ? Number(f.max_participants) : null, participation_secs: f.participation_secs ? Number(f.participation_secs) : null,
        phases: phases.map((p) => ({ name: p.name, offset_secs: Number(p.offset_secs), duration_secs: Number(p.duration_secs), ...(p.metadata.trim() ? { metadata: JSON.parse(p.metadata) } : {}) })) }
    } catch { toast.error('Metadata is not valid JSON'); return null }
  }
  function save() {
    const b = body(); if (!b) return
    run(async () => {
      if (isNew) { const r = await raw<Row>('POST', '/v1/server/live_events', b); void nav({ to: '/liveops/live_events/$id', params: { id: String(r.id ?? (r.live_event as Row | undefined)?.id ?? '') }, replace: true }) }
      else await raw('PUT', E, b)
    }, 'Saved')
  }
  const updPh = (i: number, k: keyof Ph, v: string) => setPhases((ps) => ps.map((p, j) => (j === i ? { ...p, [k]: v } : p)))
  const addPhase = () => setPhases((ps) => { const last = ps[ps.length - 1]; return [...ps, { name: `phase ${ps.length + 1}`, offset_secs: last ? String(Number(last.offset_secs) + Number(last.duration_secs)) : '0', duration_secs: last ? last.duration_secs : '3600', metadata: '' }] })
  const move = (i: number, d: number) => { const j = i + d; if (j < 0 || j >= phases.length) return; setPhases((ps) => { const a = [...ps]; const t = a[i] as Ph; a[i] = a[j] as Ph; a[j] = t; return a }) }

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'Live events', to: '/liveops/live_events' }, { title: isNew ? 'new event' : String(e?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New live event' : String(e?.name ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">{!isNew && id}{e && !isNew && <Badge variant={state === 'live' ? 'default' : 'secondary'}>{state}</Badge>}</div></div>
        {!isNew && e && <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete this live event?', () => run(async () => { await raw('DELETE', E); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'live_events' } }) }, 'Deleted'))}>Delete</Button>}
        <Button size="sm" disabled={!e || pending || !f.name} onClick={save}>{isNew ? 'Create' : 'Save'}</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {e && (
        <>
          <Card><CardContent className="pt-6"><KvGrid>
            <Kv k="Name" v={<Input value={String(f.name ?? '')} onChange={(x) => set('name', x.target.value)} />} />
            <Kv k="Description" v={<Input value={String(f.description ?? '')} onChange={(x) => set('description', x.target.value)} />} />
            <Kv k="Enabled" v={<label className="flex items-center gap-2"><Checkbox checked={!!f.enabled} onCheckedChange={(v) => set('enabled', v === true)} /> scheduled occurrences run</label>} />
            <Kv k="Starts" v={<Input type="datetime-local" value={starts} onChange={(x) => setStarts(x.target.value)} />} />
            <Kv k="Ends" v={<Input type="datetime-local" value={ends} placeholder="open-ended" onChange={(x) => setEnds(x.target.value)} />} />
            <Kv k="Recurrence" v={<div className="flex gap-2"><Select value={recur} onValueChange={setRecur}><SelectTrigger className="w-40"><SelectValue /></SelectTrigger><SelectContent>{[['none', 'one-off'], ['hourly', 'every hour'], ['daily', 'every day'], ['weekly', 'every week'], ['custom', 'custom (s)']].map(([v, l]) => <SelectItem key={v} value={v as string}>{l}</SelectItem>)}</SelectContent></Select>{recur === 'custom' && <Input className="w-28" type="number" min={60} value={recurSecs} onChange={(x) => setRecurSecs(x.target.value)} aria-label="recurrence seconds" />}</div>} />
            <Kv k="Local time" v={<label className="flex items-center gap-2"><Checkbox checked={!!f.local_time} onCheckedChange={(v) => set('local_time', v === true)} /> schedule in each player's local time zone</label>} />
            <Kv k="Participation" v={<Select value={String(f.participation ?? 'implicit')} onValueChange={(v) => set('participation', v)}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="implicit">implicit — every eligible player is in</SelectItem><SelectItem value="opt_in">opt-in — player joins explicitly</SelectItem></SelectContent></Select>} />
            <Kv k="Audience" v={<Select value={String(f.audience || '*')} onValueChange={(v) => set('audience', v === '*' ? null : v)}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="*">everyone</SelectItem>{audiences.map((a) => <SelectItem key={a} value={a}>{a}</SelectItem>)}</SelectContent></Select>} />
            <Kv k="Max participants / join window (s)" v={<div className="flex gap-2"><Input type="number" min={1} value={f.max_participants ?? ''} placeholder="unlimited" onChange={(x) => set('max_participants', x.target.value)} /><Input type="number" min={0} value={f.participation_secs ?? ''} placeholder="whole event" onChange={(x) => set('participation_secs', x.target.value)} /></div>} />
            <Kv k="Labels" v={<Input value={labels} onChange={(x) => setLabels(x.target.value)} />} />
          </KvGrid></CardContent></Card>
          <Tabs value={tab} onValueChange={(v) => void nav({ search: { tab: v === 'phases' ? undefined : v }, replace: true })}>
            <TabsList><TabsTrigger value="phases">Phases ({phases.length})</TabsTrigger>{!isNew && <TabsTrigger value="participants">Participants ({listOf(parts.data, 'participants').length})</TabsTrigger>}{!isNew && <TabsTrigger value="raw">Raw</TabsTrigger>}</TabsList>
            <Card className="mt-3"><CardContent className="pt-6">
              <TabsContent value="phases">
                <SubHead title="Phases"><span className="self-center text-xs text-muted-foreground">relative to each occurrence start · total {fmt(total)}</span><Button size="sm" variant="outline" onClick={addPhase}>Add phase</Button></SubHead>
                {total > 0 && <div className="relative mb-3 h-2.5 w-full overflow-hidden rounded bg-muted">{phases.map((p, i) => <span key={i} title={p.name} className="absolute top-0 h-full border-r border-background bg-primary/70" style={{ left: `${(Number(p.offset_secs) / total) * 100}%`, width: `${(Number(p.duration_secs) / total) * 100}%` }} />)}</div>}
                <div className="overflow-x-auto"><Table>
                  <TableHeader><TableRow><TableHead className="w-10">#</TableHead><TableHead>Name</TableHead><TableHead className="w-32">Offset (s)</TableHead><TableHead className="w-32">Duration (s)</TableHead><TableHead className="w-32">Window</TableHead><TableHead>Metadata (JSON)</TableHead><TableHead className="w-28" /></TableRow></TableHeader>
                  <TableBody>
                    {phases.map((p, i) => (
                      <TableRow key={i}>
                        <TableCell className="text-muted-foreground">{i + 1}</TableCell>
                        <TableCell><Input className="h-8" value={p.name} onChange={(x) => updPh(i, 'name', x.target.value)} aria-label="phase name" /></TableCell>
                        <TableCell><Input className="h-8" type="number" min={0} value={p.offset_secs} onChange={(x) => updPh(i, 'offset_secs', x.target.value)} aria-label="offset" /></TableCell>
                        <TableCell><Input className="h-8" type="number" min={0} value={p.duration_secs} onChange={(x) => updPh(i, 'duration_secs', x.target.value)} aria-label="duration" /></TableCell>
                        <TableCell className="text-xs text-muted-foreground">{fmt(Number(p.offset_secs))} → {fmt(Number(p.offset_secs) + Number(p.duration_secs))}</TableCell>
                        <TableCell><Input className="h-8 font-mono" value={p.metadata} placeholder="{}" onChange={(x) => updPh(i, 'metadata', x.target.value)} aria-label="phase metadata" /></TableCell>
                        <TableCell><span className="flex gap-1"><Button size="sm" variant="ghost" onClick={() => move(i, -1)} aria-label="up">↑</Button><Button size="sm" variant="ghost" onClick={() => move(i, 1)} aria-label="down">↓</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setPhases((ps) => ps.filter((_, j) => j !== i))} aria-label="remove">×</Button></span></TableCell>
                      </TableRow>
                    ))}
                    {!phases.length && <TableRow><TableCell colSpan={7} className="py-6 text-center text-muted-foreground">No phases — the event is a single window</TableCell></TableRow>}
                  </TableBody>
                </Table></div>
                <SubHead title="Metadata (JSON)" /><Textarea className="min-h-24 font-mono text-xs" value={meta} onChange={(x) => setMeta(x.target.value)} aria-label="metadata" />
              </TabsContent>
              <TabsContent value="participants">
                <DataTable id="event-participants" rows={listOf(parts.data, 'participants')} loading={parts.isPending} empty="No participants yet" rowTo={(r) => `/players/accounts/${r.account_id}`}
                  cols={[{ key: 'account_id', mono: true }, { key: 'joined_at', dim: true, fmt: (r) => tsu(r.joined_at ?? r.entered_at) }, { key: 'occurrence', fmt: (r) => (r.occurrence != null ? tsu(r.occurrence) : '—') }, { key: 'progress', dim: true, fmt: (r) => (r.progress ? JSON.stringify(r.progress) : '') }]} />
              </TabsContent>
              <TabsContent value="raw"><JsonView value={e} /></TabsContent>
            </CardContent></Card>
          </Tabs>
        </>
      )}
    </div>
  )
}
