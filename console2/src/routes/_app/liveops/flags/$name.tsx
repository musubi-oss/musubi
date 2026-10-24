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
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'

/** Feature flag editor: default value plus ordered rules (audience → rollout bucket → time window); first matching rule wins. */
export const Route = createFileRoute('/_app/liveops/flags/$name')({ component: FlagPage })

interface Rule { audience: string; rollout_pct: string; value: string; starts_at: string; ends_at: string }
const toLocal = (v: unknown) => (v ? new Date(Number(v) * (Number(v) < 1e12 ? 1000 : 1)).toISOString().slice(0, 16) : '')
const fromLocal = (s: string) => (s ? Math.floor(new Date(s).getTime() / 1000) : null)

function FlagPage() {
  const { name } = Route.useParams()
  const nav = useNavigate({ from: Route.fullPath })
  const F = `/v1/server/flags/${encodeURIComponent(name)}`
  const q = useQuery({ queryKey: ['flag', name], queryFn: async ({ signal }) => { const r = await raw<Row>('GET', F, undefined, { signal }); return (r.flag ?? r) as Row } })
  const audQ = useSub('/v1/server/audiences')
  const audiences = listOf(audQ.data, 'audiences').map((a) => String(a.name))
  const f = q.data
  const [enabled, setEnabled] = useState(true); const [description, setDescription] = useState(''); const [defaultValue, setDefaultValue] = useState('true'); const [labels, setLabels] = useState('')
  const [rules, setRules] = useState<Rule[]>([]); const [dirty, setDirty] = useState(false)
  useEffect(() => {
    if (!f) return
    setEnabled(!!f.enabled); setDescription(String(f.description ?? '')); setDefaultValue(JSON.stringify(f.default_value ?? true)); setLabels(((f.labels ?? []) as string[]).join(', '))
    setRules(((f.rules ?? []) as Row[]).map((r) => ({ audience: r.audience ?? '', rollout_pct: r.rollout_pct == null ? '' : String(r.rollout_pct), value: JSON.stringify(r.value), starts_at: toLocal(r.starts_at), ends_at: toLocal(r.ends_at) })))
    setDirty(false)
  }, [f])
  const { run, pending } = useAct([['flag', name]])
  const upd = (i: number, k: keyof Rule, v: string) => { setRules((rs) => rs.map((r, j) => (j === i ? { ...r, [k]: v } : r))); setDirty(true) }
  const move = (i: number, d: number) => { const j = i + d; if (j < 0 || j >= rules.length) return; setRules((rs) => { const a = [...rs]; const [r] = a.splice(i, 1); a.splice(j, 0, r as Rule); return a }); setDirty(true) }
  function save() {
    let body: Row
    try {
      body = { description, enabled, default_value: JSON.parse(defaultValue), labels: labels.split(',').map((s) => s.trim()).filter(Boolean),
        rules: rules.map((r) => ({ audience: r.audience || null, rollout_pct: r.rollout_pct === '' ? null : Number(r.rollout_pct), value: JSON.parse(r.value || 'null'), starts_at: fromLocal(r.starts_at), ends_at: fromLocal(r.ends_at) })) }
    } catch { return toast.error('A value is not valid JSON') }
    run(() => raw('PUT', F, body), 'Flag saved')
  }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'Feature flags', to: '/liveops/flags' }, { title: name }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate font-mono text-xl font-semibold">{name}</h1><p className="mt-1 text-xs text-muted-foreground">Evaluation: first matching rule wins — audience → rollout bucket → time window; otherwise the default.{dirty && <span className="ml-2 text-amber-500">Unsaved changes</span>}</p></div>
        <Button size="sm" variant="destructive" onClick={() => confirmThen(`Delete flag ${name}?`, () => run(async () => { await raw('DELETE', F); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'flags' } }) }, 'Deleted'))}>Delete</Button>
        <Button size="sm" disabled={!f || pending} onClick={save}>Save</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {f && (
        <Card><CardContent className="pt-6">
          <KvGrid>
            <Kv k="Enabled" v={<label className="flex items-center gap-2"><Checkbox checked={enabled} onCheckedChange={(v) => { setEnabled(v === true); setDirty(true) }} /> {enabled ? 'on — rules evaluate, then default' : 'off — every client gets the default value'}</label>} />
            <Kv k="Default value (JSON)" v={<Input className="font-mono" value={defaultValue} onChange={(e) => { setDefaultValue(e.target.value); setDirty(true) }} />} />
            <Kv k="Description" v={<Input value={description} onChange={(e) => { setDescription(e.target.value); setDirty(true) }} />} />
            <Kv k="Labels (comma-separated)" v={<Input value={labels} onChange={(e) => { setLabels(e.target.value); setDirty(true) }} />} />
            <Kv k="Updated" v={ts(f.updated_at)} />
          </KvGrid>
          <SubHead title="Rules" count={rules.length}><span className="self-center text-xs text-muted-foreground">top first</span><Button size="sm" variant="outline" onClick={() => { setRules((rs) => [...rs, { audience: '', rollout_pct: '', value: 'true', starts_at: '', ends_at: '' }]); setDirty(true) }}>Add rule</Button></SubHead>
          <div className="overflow-x-auto"><Table>
            <TableHeader><TableRow><TableHead className="w-10">#</TableHead><TableHead className="w-52">Audience</TableHead><TableHead className="w-28">Rollout %</TableHead><TableHead>Value (JSON)</TableHead><TableHead className="w-48">Starts</TableHead><TableHead className="w-48">Ends</TableHead><TableHead className="w-28" /></TableRow></TableHeader>
            <TableBody>
              {rules.map((r, i) => (
                <TableRow key={i}>
                  <TableCell className="text-muted-foreground">{i + 1}</TableCell>
                  <TableCell><Select value={r.audience || '*'} onValueChange={(v) => upd(i, 'audience', v === '*' ? '' : v)}><SelectTrigger className="h-8 w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="*">everyone</SelectItem>{audiences.map((a) => <SelectItem key={a} value={a}>{a}</SelectItem>)}{r.audience && !audiences.includes(r.audience) && <SelectItem value={r.audience}>{r.audience} (missing)</SelectItem>}</SelectContent></Select></TableCell>
                  <TableCell><Input className="h-8" type="number" min={0} max={100} value={r.rollout_pct} placeholder="100" onChange={(e) => upd(i, 'rollout_pct', e.target.value)} aria-label="rollout percent" /></TableCell>
                  <TableCell><Input className="h-8 font-mono" value={r.value} onChange={(e) => upd(i, 'value', e.target.value)} aria-label="value json" /></TableCell>
                  <TableCell><Input className="h-8" type="datetime-local" value={r.starts_at} onChange={(e) => upd(i, 'starts_at', e.target.value)} aria-label="starts" /></TableCell>
                  <TableCell><Input className="h-8" type="datetime-local" value={r.ends_at} onChange={(e) => upd(i, 'ends_at', e.target.value)} aria-label="ends" /></TableCell>
                  <TableCell><span className="flex gap-1"><Button size="sm" variant="ghost" onClick={() => move(i, -1)} aria-label="move up">↑</Button><Button size="sm" variant="ghost" onClick={() => move(i, 1)} aria-label="move down">↓</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => { setRules((rs) => rs.filter((_, j) => j !== i)); setDirty(true) }} aria-label="remove">×</Button></span></TableCell>
                </TableRow>
              ))}
              {!rules.length && <TableRow><TableCell colSpan={7} className="py-6 text-center text-muted-foreground">No rules — the default value applies to everyone</TableCell></TableRow>}
            </TableBody>
          </Table></div>
          <SubHead title="Raw" /><JsonView value={f} />
        </CardContent></Card>
      )}
    </div>
  )
}
