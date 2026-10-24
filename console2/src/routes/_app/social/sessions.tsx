import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'

/** Game sessions (lobbies / rooms backed by a match or a fleet server) across the cluster: search, stop one or many (dry run first), and the templates players create their own rooms from. */
export const Route = createFileRoute('/_app/social/sessions')({
  validateSearch: (s: Record<string, unknown>): { kind?: string; name?: string } => ({ kind: typeof s.kind === 'string' ? s.kind : undefined, name: typeof s.name === 'string' ? s.name : undefined }),
  component: Sessions,
})

const ts = (v: unknown) => (v ? new Date(Number(v) * (Number(v) < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—')

function Sessions() {
  const { kind = '', name = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const [draft, setDraft] = useState({ kind, name })
  const q = useQuery({ queryKey: ['sessions-search', kind, name], queryFn: ({ signal }) => raw<Row>('POST', '/v1/server/sessions/search', { ...(kind ? { kind } : {}), ...(name ? { name } : {}), limit: 200 }, { signal }) })
  const tpl = useSub<Row>('/v1/server/session_templates')
  const sessions = listOf(q.data, 'sessions'); const templates = listOf(tpl.data, 'templates')
  const [sel, setSel] = useState<Set<string>>(new Set())
  const [stopOut, setStopOut] = useState<unknown>(null)
  const [dlg, setDlg] = useState<'' | 'tpl'>(''); const [form, setForm] = useState({ id: '', text: '', isNew: true })
  const { run, pending } = useAct([['sessions-search'], ['sub']], () => setDlg(''))
  const toggle = (id: string) => setSel((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n })
  const stopSel = (dry: boolean) => run(async () => { const r = await raw('POST', '/v1/server/sessions/stop', { ids: [...sel], reason: 'console', dry_run: dry }); setStopOut(r); if (!dry) setSel(new Set()) }, dry ? 'Dry run done' : 'Stopped')
  const openTemplate = (t?: Row) => { setForm({ id: String(t?.id ?? ''), isNew: !t, text: JSON.stringify(t ? { ...t, id: undefined } : { name: '', kind: 'custom', max_players: 4, join_policy: 'open', properties: {}, data: {} }, null, 2) }); setDlg('tpl') }
  const players = sessions.reduce((a, s) => a + (Number(s.max_players ?? 0) - Number(s.free_slots ?? 0)), 0)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Social', to: '/social' }, { title: 'Game sessions' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Game sessions</h1><p className="mt-1 text-sm text-muted-foreground">{sessions.length} sessions · {players} players in them</p></div>
        <Button size="sm" variant="outline" disabled={!sel.size || pending} onClick={() => stopSel(true)}>Dry-run stop ({sel.size})</Button>
        <Button size="sm" variant="destructive" disabled={!sel.size || pending} onClick={() => confirmThen(`Stop ${sel.size} session(s)?`, () => stopSel(false))}>Stop selected</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      <Card><CardContent className="pt-6">
        <form className="mb-3 flex flex-wrap gap-2" onSubmit={(e) => { e.preventDefault(); void nav({ search: { kind: draft.kind || undefined, name: draft.name || undefined }, replace: true }) }}>
          <Input className="h-8 w-48" value={draft.name} placeholder="name" onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))} aria-label="name" />
          <Input className="h-8 w-36" value={draft.kind} placeholder="kind" onChange={(e) => setDraft((d) => ({ ...d, kind: e.target.value }))} aria-label="kind" />
          <Button type="submit" size="sm" variant="outline">Search</Button>
        </form>
        <DataTable id="sessions" rows={sessions} loading={q.isPending} empty="No sessions match" rowTo={(s) => `/social/live_sessions/${s.id}`}
          cols={[{ key: 'sel', label: '', fmt: (s) => <Checkbox checked={sel.has(String(s.id))} onCheckedChange={() => toggle(String(s.id))} aria-label="select" onClick={(e) => e.stopPropagation()} /> }, { key: 'name', fmt: (s) => <span>{String(s.name ?? '')}<span className="block font-mono text-[11px] text-muted-foreground">{String(s.id)}</span></span> }, { key: 'kind' }, { key: 'players', fmt: (s) => `${Number(s.max_players ?? 0) - Number(s.free_slots ?? 0)} / ${s.max_players ?? '?'}` }, { key: 'join_policy' }, { key: 'backing', dim: true, fmt: (s) => (s.match_id ? `match ${String(s.match_id).slice(0, 8)}…` : s.server_id ? `server ${String(s.server_id).slice(0, 8)}…` : 'none') }, { key: 'created_at', dim: true, fmt: (s) => ts(s.created_at) }]}
          actions={(s) => <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen('Stop this session?', () => run(() => raw('POST', '/v1/server/sessions/stop', { ids: [s.id], reason: 'console' }), 'Stopped'))}>Stop</Button>} />
        {stopOut != null && <><SubHead title="Stop result" /><JsonView value={stopOut} /></>}
      </CardContent></Card>
      <Card><CardContent className="pt-6">
        <SubHead title="Session templates" count={templates.length}><Button size="sm" variant="outline" onClick={() => openTemplate()}>Add template</Button></SubHead>
        <DataTable id="session-templates" rows={templates} loading={tpl.isPending} empty="No templates — players can only join sessions the server creates" cols={[{ key: 'id', mono: true }, { key: 'name' }, { key: 'kind' }, { key: 'max_players' }, { key: 'join_policy' }]}
          actions={(t) => <span className="flex justify-end gap-1"><Button size="sm" variant="ghost" onClick={() => openTemplate(t)}>Edit</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Delete template ${t.id}?`, () => run(() => raw('DELETE', `/v1/server/session_templates/${encodeURIComponent(String(t.id))}`), 'Deleted'))}>×</Button></span>} />
        <p className="mt-3 text-xs text-muted-foreground">A template is what a player's "create room" button offers: kind, size, join policy and default properties; the server fills in fleet or match. Live sessions open in <Link to="/$section/$page" params={{ section: 'social', page: 'live_sessions' }} className="underline">Live sessions</Link>.</p>
      </CardContent></Card>
      <FormDialog open={dlg === 'tpl'} onOpenChange={(o) => !o && setDlg('')} title={form.isNew ? 'New template' : `Template ${form.id}`} submit="Save" pending={pending} disabled={!form.id}
        onSubmit={() => { let b: unknown; try { b = JSON.parse(form.text) } catch { return toast.error('Template is not valid JSON') }; run(() => raw('PUT', `/v1/server/session_templates/${encodeURIComponent(form.id)}`, b), 'Saved') }}>
        <Field label="Id"><Input className="font-mono" value={form.id} disabled={!form.isNew} placeholder="duel" onChange={(e) => setForm((f) => ({ ...f, id: e.target.value }))} /></Field>
        <Field label="Template (JSON)"><Textarea className="min-h-44 font-mono text-xs" value={form.text} onChange={(e) => setForm((f) => ({ ...f, text: e.target.value }))} /></Field>
      </FormDialog>
    </div>
  )
}
