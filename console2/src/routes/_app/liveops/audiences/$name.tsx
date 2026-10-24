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
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

/** Audience editor: segment rules (lists, numeric bounds, time bounds, nested any/all/not algebra), tracking and on-enter / on-exit actions, plus the member table. */
export const Route = createFileRoute('/_app/liveops/audiences/$name')({
  validateSearch: (s: Record<string, unknown>): { tab?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined }),
  component: AudiencePage,
})

const LIST_KEYS = ['platform', 'lang', 'location', 'age_bracket', 'include', 'exclude']
const NUM_KEYS: [string, string][] = [['min_level', 'Min level'], ['max_level', 'Max level'], ['min_account_age_days', 'Min account age (days)'], ['max_account_age_days', 'Max account age (days)'], ['active_within_days', 'Active within (days)'], ['inactive_for_days', 'Inactive for (days)'], ['min_spend', 'Min spend'], ['max_spend', 'Max spend']]
const toLocal = (v: unknown) => (v ? new Date(Number(v) * (Number(v) < 1e12 ? 1000 : 1)).toISOString().slice(0, 16) : '')
const fromLocal = (s: string) => (s ? Math.floor(new Date(s).getTime() / 1000) : null)

function AudiencePage() {
  const { name } = Route.useParams()
  const { tab = 'rules' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const U = `/v1/server/audiences/${encodeURIComponent(name)}`
  const q = useQuery({ queryKey: ['audience', name], queryFn: async ({ signal }) => { const r = await raw<Row>('GET', U, undefined, { signal }); return (r.audience ?? r) as Row } })
  const members = useSub<Row>(`${U}/members`)
  const a = q.data
  const [r, setR] = useState<Row>({}); const [lists, setLists] = useState<Record<string, string>>({}); const [nested, setNested] = useState({ any: '[]', all: '[]', not: '' }); const [actions, setActions] = useState('{}'); const [tracked, setTracked] = useState(false); const [created, setCreated] = useState({ after: '', before: '' })
  useEffect(() => {
    if (!a) return
    const rules = (a.rules ?? {}) as Row
    setR({ ...rules }); setLists(Object.fromEntries(LIST_KEYS.map((k) => [k, ((rules[k] ?? []) as string[]).join(', ')])))
    setNested({ any: JSON.stringify(rules.any ?? [], null, 2), all: JSON.stringify(rules.all ?? [], null, 2), not: rules.not ? JSON.stringify(rules.not, null, 2) : '' })
    setCreated({ after: toLocal(rules.created_after), before: toLocal(rules.created_before) }); setTracked(!!a.tracked); setActions(JSON.stringify(a.actions ?? { on_enter: [], on_exit: [] }, null, 2))
  }, [a])
  const { run, pending } = useAct([['audience', name], ['sub']])
  function save() {
    const rules: Row = {}
    try {
      for (const [k] of NUM_KEYS) if (r[k] !== null && r[k] !== '' && r[k] !== undefined) rules[k] = Number(r[k])
      for (const k of LIST_KEYS) { const v = (lists[k] ?? '').split(',').map((s) => s.trim()).filter(Boolean); if (v.length) rules[k] = v }
      if (r.bucket_salt) rules.bucket_salt = r.bucket_salt
      if (r.track) rules.track = r.track
      const any = JSON.parse(nested.any || '[]'); if (any.length) rules.any = any
      const all = JSON.parse(nested.all || '[]'); if (all.length) rules.all = all
      if (nested.not.trim()) rules.not = JSON.parse(nested.not)
      const ca = fromLocal(created.after), cb = fromLocal(created.before); if (ca) rules.created_after = ca; if (cb) rules.created_before = cb
      const acts = JSON.parse(actions || '{}')
      run(() => raw('PUT', U, { rules, tracked, actions: acts }), 'Audience saved')
    } catch { toast.error('A nested rule or the actions block is not valid JSON') }
  }
  const mrows = listOf(members.data, 'members').map((x) => (typeof x === 'string' ? { account_id: x } : x))
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'Audiences', to: '/liveops/audiences' }, { title: name }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate font-mono text-xl font-semibold">{name}</h1><p className="mt-1 text-xs text-muted-foreground">A segment of players used by flags, experiments, offers and news. Untracked audiences are evaluated per request; tracked ones keep a member table and run actions on enter / exit.</p></div>
        <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${U}/refresh`), 'Membership refreshed')}>Refresh members</Button>
        <Button size="sm" variant="destructive" onClick={() => confirmThen(`Delete audience ${name}?`, () => run(async () => { await raw('DELETE', U); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'audiences' } }) }, 'Deleted'))}>Delete</Button>
        <Button size="sm" disabled={!a || pending} onClick={save}>Save</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {a && (
        <Tabs value={tab} onValueChange={(v) => void nav({ search: { tab: v === 'rules' ? undefined : v }, replace: true })}>
          <TabsList><TabsTrigger value="rules">Rules</TabsTrigger><TabsTrigger value="members">Members ({String(members.data?.total ?? mrows.length)})</TabsTrigger><TabsTrigger value="raw">Raw</TabsTrigger></TabsList>
          <Card className="mt-3"><CardContent className="pt-6">
            <TabsContent value="rules">
              <KvGrid>
                <Kv k="Tracked" v={<label className="flex items-center gap-2"><Checkbox checked={tracked} onCheckedChange={(v) => setTracked(v === true)} /> keep a member table and run actions on enter / exit</label>} />
                {LIST_KEYS.map((k) => <Kv key={k} k={`${k.replace(/_/g, ' ')} (comma-separated)`} v={<Input value={lists[k] ?? ''} onChange={(e) => setLists((l) => ({ ...l, [k]: e.target.value }))} />} />)}
                {NUM_KEYS.map(([k, l]) => <Kv key={k} k={l} v={<Input type="number" value={r[k] ?? ''} placeholder="any" onChange={(e) => setR((x) => ({ ...x, [k]: e.target.value }))} />} />)}
                <Kv k="Bucket salt" v={<Input value={String(r.bucket_salt ?? '')} placeholder="default" onChange={(e) => setR((x) => ({ ...x, bucket_salt: e.target.value }))} />} />
                <Kv k="Created after" v={<Input type="datetime-local" value={created.after} onChange={(e) => setCreated((c) => ({ ...c, after: e.target.value }))} />} />
                <Kv k="Created before" v={<Input type="datetime-local" value={created.before} onChange={(e) => setCreated((c) => ({ ...c, before: e.target.value }))} />} />
              </KvGrid>
              <SubHead title="Rule algebra"><span className="self-center text-xs text-muted-foreground">nested rule objects with the same fields</span></SubHead>
              <div className="grid gap-3 xl:grid-cols-3">
                {(['any', 'all', 'not'] as const).map((k) => <div key={k}><div className="mb-1 text-xs text-muted-foreground">{k} — {k === 'any' ? 'at least one matches' : k === 'all' ? 'every one matches' : 'must not match'}</div><Textarea className="min-h-28 font-mono text-xs" value={nested[k]} placeholder={k === 'not' ? '{ }' : '[ ]'} onChange={(e) => setNested((n) => ({ ...n, [k]: e.target.value }))} /></div>)}
              </div>
              <SubHead title="Actions on change"><span className="self-center text-xs text-muted-foreground">tracked only: reward_table, flag, webhook, …</span></SubHead>
              <Textarea className="min-h-28 font-mono text-xs" value={actions} onChange={(e) => setActions(e.target.value)} aria-label="actions json" />
            </TabsContent>
            <TabsContent value="members">
              {!a.tracked ? <p className="text-sm text-muted-foreground">Not tracked — membership is evaluated per request, no member table. Enable “tracked” and save to materialise it.</p>
                : <DataTable id="audience-members" rows={mrows} loading={members.isPending} empty="No members yet" rowTo={(x) => `/players/accounts/${x.account_id}`}
                  cols={[{ key: 'account_id', mono: true }, { key: 'kind' }, { key: 'entered_at', dim: true, fmt: (x) => ts(x.entered_at ?? x.since) }]} />}
            </TabsContent>
            <TabsContent value="raw"><JsonView value={a} /></TabsContent>
          </CardContent></Card>
        </Tabs>
      )}
    </div>
  )
}
