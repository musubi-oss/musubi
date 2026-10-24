import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

/** A/B experiment: audience, rollout, window and weighted variants with flag overrides; start / pause / end with a winner. */
export const Route = createFileRoute('/_app/liveops/experiments/$id')({ component: ExperimentPage })

interface V { name: string; weight: string; flags: string }
const toLocal = (v: unknown) => (v ? new Date(Number(v) * (Number(v) < 1e12 ? 1000 : 1)).toISOString().slice(0, 16) : '')
const fromLocal = (s: string) => (s ? Math.floor(new Date(s).getTime() / 1000) : null)

function ExperimentPage() {
  const { id } = Route.useParams()
  const nav = useNavigate({ from: Route.fullPath })
  const E = `/v1/server/experiments/${encodeURIComponent(id)}`
  const q = useQuery({ queryKey: ['experiment', id], queryFn: async ({ signal }) => { const r = await raw<Row>('GET', E, undefined, { signal }); return (r.experiment ?? r) as Row } })
  const audiences = listOf(useSub('/v1/server/audiences').data, 'audiences').map((a) => String(a.name))
  const e = q.data
  const [f, setF] = useState<Row>({}); const [variants, setVariants] = useState<V[]>([]); const [labels, setLabels] = useState(''); const [starts, setStarts] = useState(''); const [ends, setEnds] = useState(''); const [winner, setWinner] = useState('')
  useEffect(() => { if (!e) return; setF({ ...e }); setVariants(((e.variants ?? []) as Row[]).map((v) => ({ name: String(v.name), weight: String(v.weight ?? 50), flags: JSON.stringify(v.flags ?? {}) }))); setLabels(((e.labels ?? []) as string[]).join(', ')); setStarts(toLocal(e.starts_at)); setEnds(toLocal(e.ends_at)) }, [e])
  const set = (k: string, v: unknown) => setF((x) => ({ ...x, [k]: v }))
  const { run, pending } = useAct([['experiment', id]])
  const total = variants.reduce((s, v) => s + Number(v.weight || 0), 0)
  function save() {
    let vs: Row[]
    try { vs = variants.map((v) => ({ name: v.name, weight: Number(v.weight), flags: JSON.parse(v.flags || '{}') })) } catch { return toast.error('A flag override is not valid JSON') }
    run(() => raw('PUT', E, { name: f.name, description: f.description ?? '', audience: f.audience || null, exclusion_group: f.exclusion_group || null, rollout_pct: Number(f.rollout_pct ?? 100), labels: labels.split(',').map((s) => s.trim()).filter(Boolean), starts_at: fromLocal(starts), ends_at: fromLocal(ends), variants: vs }), 'Saved')
  }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'Experiments', to: '/liveops/experiments' }, { title: String(e?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{String(e?.name ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">{id}{e && <Badge variant={e.status === 'running' ? 'default' : 'secondary'}>{String(e.status)}</Badge>}</div></div>
        {e && (
          <div className="flex flex-wrap items-center gap-2">
            {e.status !== 'running' && e.status !== 'ended' && <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${E}/start`), 'Started')}>Start</Button>}
            {e.status === 'running' && <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${E}/pause`), 'Paused')}>Pause</Button>}
            {e.status !== 'ended' && <>
              <Select value={winner || '-'} onValueChange={(v) => setWinner(v === '-' ? '' : v)}><SelectTrigger className="w-44"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="-">no winner</SelectItem>{variants.map((v) => <SelectItem key={v.name} value={v.name}>winner: {v.name}</SelectItem>)}</SelectContent></Select>
              <Button size="sm" variant="outline" onClick={() => confirmThen(`End the experiment${winner ? ` with winner ${winner}` : ''}?`, () => run(() => raw('POST', `${E}/end`, { winner: winner || null }), 'Ended'))}>End</Button>
            </>}
            <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete experiment?', () => run(async () => { await raw('DELETE', E); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'experiments' } }) }, 'Deleted'))}>Delete</Button>
            <Button size="sm" disabled={pending} onClick={save}>Save</Button>
          </div>
        )}
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {e && (
        <Card><CardContent className="pt-6">
          <KvGrid>
            <Kv k="Name" v={<Input value={String(f.name ?? '')} onChange={(x) => set('name', x.target.value)} />} />
            <Kv k="Description" v={<Input value={String(f.description ?? '')} onChange={(x) => set('description', x.target.value)} />} />
            <Kv k="Audience" v={<Select value={String(f.audience || '*')} onValueChange={(v) => set('audience', v === '*' ? null : v)}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="*">everyone</SelectItem>{audiences.map((a) => <SelectItem key={a} value={a}>{a}</SelectItem>)}</SelectContent></Select>} />
            <Kv k="Rollout % of eligible" v={<Input type="number" min={0} max={100} value={String(f.rollout_pct ?? 100)} onChange={(x) => set('rollout_pct', x.target.value)} />} />
            <Kv k="Exclusion group" v={<Input value={String(f.exclusion_group ?? '')} placeholder="players are in at most one experiment per group" onChange={(x) => set('exclusion_group', x.target.value)} />} />
            <Kv k="Labels" v={<Input value={labels} onChange={(x) => setLabels(x.target.value)} />} />
            <Kv k="Starts" v={<Input type="datetime-local" value={starts} onChange={(x) => setStarts(x.target.value)} />} />
            <Kv k="Ends" v={<Input type="datetime-local" value={ends} onChange={(x) => setEnds(x.target.value)} />} />
          </KvGrid>
          <SubHead title="Variants" count={variants.length}><span className="self-center text-xs text-muted-foreground">weights sum {total}</span><Button size="sm" variant="outline" onClick={() => setVariants((vs) => [...vs, { name: `v${vs.length + 1}`, weight: '50', flags: '{}' }])}>Add variant</Button></SubHead>
          <div className="overflow-x-auto"><Table>
            <TableHeader><TableRow><TableHead className="w-48">Name</TableHead><TableHead className="w-28">Weight</TableHead><TableHead className="w-36">Share</TableHead><TableHead>Flag overrides (JSON: flag → value)</TableHead><TableHead className="w-10" /></TableRow></TableHeader>
            <TableBody>
              {variants.map((v, i) => (
                <TableRow key={i}>
                  <TableCell><Input className="h-8" value={v.name} onChange={(x) => setVariants((vs) => vs.map((y, j) => (j === i ? { ...y, name: x.target.value } : y)))} aria-label="variant name" /></TableCell>
                  <TableCell><Input className="h-8" type="number" min={0} value={v.weight} onChange={(x) => setVariants((vs) => vs.map((y, j) => (j === i ? { ...y, weight: x.target.value } : y)))} aria-label="weight" /></TableCell>
                  <TableCell><span className="flex items-center gap-2"><span className="h-1.5 w-20 overflow-hidden rounded bg-muted"><span className="block h-full bg-primary" style={{ width: `${total ? (Number(v.weight) / total) * 100 : 0}%` }} /></span><span className="text-xs text-muted-foreground">{total ? Math.round((Number(v.weight) / total) * 100) : 0}%</span></span></TableCell>
                  <TableCell><Input className="h-8 font-mono" value={v.flags} onChange={(x) => setVariants((vs) => vs.map((y, j) => (j === i ? { ...y, flags: x.target.value } : y)))} aria-label="flag overrides" /></TableCell>
                  <TableCell><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setVariants((vs) => vs.filter((_, j) => j !== i))} aria-label="remove">×</Button></TableCell>
                </TableRow>
              ))}
              {!variants.length && <TableRow><TableCell colSpan={5} className="py-6 text-center text-muted-foreground">No variants — add at least two</TableCell></TableRow>}
            </TableBody>
          </Table></div>
          <SubHead title="Raw" /><JsonView value={e} />
        </CardContent></Card>
      )}
    </div>
  )
}
