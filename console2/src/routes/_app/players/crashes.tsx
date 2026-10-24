import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { qs, raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { ts } from '@/lib/format'
import { listOf } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Crash reports grouped by stack fingerprint, with triage (assignee, notes, resolve/reopen) and the latest sample. */
export const Route = createFileRoute('/_app/players/crashes')({
  validateSearch: (s: Record<string, unknown>): { status?: string; q?: string; fp?: string } => ({ status: typeof s.status === 'string' ? s.status : undefined, q: typeof s.q === 'string' ? s.q : undefined, fp: typeof s.fp === 'string' ? s.fp : undefined }),
  component: Crashes,
})

function Crashes() {
  const { status = 'open', q = '', fp } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const go = (patch: Record<string, string | undefined>) => void nav({ search: (p) => ({ ...p, ...patch }), replace: true })
  const groupsQ = useQuery({ queryKey: ['crashes', status], queryFn: ({ signal }) => raw<Row>('GET', '/v1/server/crashes' + qs({ status: status === 'all' ? undefined : status, limit: 200 }), undefined, { signal }) })
  const groups = listOf(groupsQ.data, 'groups').filter((g) => !q || JSON.stringify(g).toLowerCase().includes(q.toLowerCase()))
  const sel = groups.find((g) => g.fingerprint === fp) ?? null
  const sampleQ = useQuery({ queryKey: ['crash-sample', fp, sel?.sample_id], enabled: !!sel, queryFn: async ({ signal }) => { const base = `/v1/server/crashes/${encodeURIComponent(String(fp))}`; try { return sel?.sample_id ? await raw('GET', `${base}/samples/${sel.sample_id}`, undefined, { signal }) : await raw('GET', base, undefined, { signal }) } catch { return null } } })
  const [notes, setNotes] = useState(''); const [assignee, setAssignee] = useState('')
  useEffect(() => { setNotes(String(sel?.notes ?? '')); setAssignee(String(sel?.assignee ?? '')) }, [sel?.fingerprint]) // eslint-disable-line react-hooks/exhaustive-deps
  const { run } = useAct([['crashes']])
  const patch = (b: Row, ok: string) => run(() => raw('PATCH', `/v1/server/crashes/${encodeURIComponent(String(fp))}`, b), ok)

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Players', to: '/players' }, { title: 'Crash reports' }]} />
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Crash reports</h1><p className="mt-1 text-xs text-muted-foreground">{String(groupsQ.data?.total ?? groups.length)} groups · grouped by stack fingerprint. Pick a group to see its sample, stack and triage controls.</p></div>
        <Input className="w-56" value={q} placeholder="filter title / version" onChange={(e) => go({ q: e.target.value || undefined })} />
        <Select value={status} onValueChange={(v) => go({ status: v === 'open' ? undefined : v })}><SelectTrigger className="w-32"><SelectValue /></SelectTrigger><SelectContent>{['open', 'resolved', 'ignored', 'all'].map((s) => <SelectItem key={s} value={s}>{s}</SelectItem>)}</SelectContent></Select>
      </div>
      <div className="grid gap-4 xl:grid-cols-5">
        <Card className="xl:col-span-3"><CardContent className="pt-6">
          <DataTable id="crash-groups" rows={groups} loading={groupsQ.isPending} empty="No crash groups" rowTo={(g) => `/players/crashes?status=${status}&fp=${encodeURIComponent(String(g.fingerprint))}`}
            cols={[{ key: 'title', fmt: (g) => <span className={g.fingerprint === fp ? 'font-semibold' : ''}>{g.title}<span className="block font-mono text-[11px] text-muted-foreground">{String(g.fingerprint).slice(0, 16)}{g.assignee ? ` · ${g.assignee}` : ''}</span></span> }, { key: 'kind' }, { key: 'count', label: 'seen' }, { key: 'last_version', label: 'version' }, { key: 'last_seen', label: 'last', dim: true, fmt: (g) => ts(g.last_seen) }, { key: 'status' }]} />
        </CardContent></Card>
        <Card className="xl:col-span-2"><CardContent className="pt-6">
          {sel ? (
            <>
              <h3 className="mb-3 text-sm font-semibold">{sel.title}</h3>
              <KvGrid className="sm:grid-cols-1 xl:grid-cols-1">
                <Kv k="Fingerprint" v={sel.fingerprint} mono copy />
                <Kv k="Seen" v={`${sel.count}× · first ${ts(sel.first_seen)} (${sel.first_version ?? '?'}) · last ${ts(sel.last_seen)} (${sel.last_version ?? '?'})`} />
                <Kv k="Assignee" v={<Input className="h-8" value={assignee} placeholder="who owns this" onChange={(e) => setAssignee(e.target.value)} onBlur={() => assignee !== (sel.assignee ?? '') && patch({ assignee }, 'Assignee saved')} />} />
                <Kv k="Notes" v={<div className="space-y-1.5"><Textarea className="min-h-16" value={notes} onChange={(e) => setNotes(e.target.value)} /><Button size="sm" variant="outline" onClick={() => patch({ notes }, 'Notes saved')}>Save notes</Button></div>} />
              </KvGrid>
              <div className="my-3 flex flex-wrap gap-2">
                {sel.status === 'open' ? <Button size="sm" onClick={() => patch({ status: 'resolved', resolved_in_version: sel.last_version }, 'Resolved')}>Resolve (fixed in {String(sel.last_version ?? '?')})</Button> : <Button size="sm" variant="outline" onClick={() => patch({ status: 'open' }, 'Reopened')}>Reopen</Button>}
                {sel.status !== 'ignored' && <Button size="sm" variant="outline" onClick={() => patch({ status: 'ignored' }, 'Ignored')}>Ignore</Button>}
                <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete this crash group and its reports?', () => { run(() => raw('DELETE', `/v1/server/crashes/${encodeURIComponent(String(fp))}`), 'Deleted'); go({ fp: undefined }) })}>Delete</Button>
              </div>
              <h3 className="mb-2 text-sm font-semibold">Latest sample</h3>
              <JsonView value={sampleQ.data ?? { sample_id: sel.sample_id, note: 'sample detail not available' }} />
            </>
          ) : <p className="py-12 text-center text-sm text-muted-foreground">Select a group to see the sample, stack and triage controls</p>}
        </CardContent></Card>
      </div>
    </div>
  )
}
