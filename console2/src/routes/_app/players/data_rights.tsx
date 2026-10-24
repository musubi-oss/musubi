import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { auth } from '@/lib/auth'
import { useAct, confirmThen } from '@/lib/act'
import { ts } from '@/lib/format'
import { listOf } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'

/** GDPR / CCPA export and erasure requests, with downloads of finished exports. */
export const Route = createFileRoute('/_app/players/data_rights')({
  validateSearch: (s: Record<string, unknown>): { filter?: string } => ({ filter: typeof s.filter === 'string' ? s.filter : undefined }),
  component: DataRights,
})

function DataRights() {
  const { filter = 'all' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const q = useQuery({ queryKey: ['data-rights'], queryFn: ({ signal }) => raw<Row>('GET', '/v1/server/data_rights', undefined, { signal }) })
  const [dlg, setDlg] = useState(false)
  const [form, setForm] = useState<Row>({ account_id: '', kind: 'export', include_game_data: true })
  const set = (k: string, v: unknown) => setForm((f) => ({ ...f, [k]: v }))
  const { run, pending } = useAct([['data-rights']], () => setDlg(false))
  const done = ['done', 'failed', 'cancelled']
  const rows = listOf(q.data, 'requests').filter((r) => filter === 'all' || r.status === filter || (filter === 'active' && !done.includes(r.status)))
  async function download(r: Row) {
    const res = await fetch((auth.base || '') + `/v1/server/data_rights/${r.id}/download`, { headers: { 'x-musubi-server-key': auth.key } })
    if (!res.ok) return toast.error(`Download failed: ${res.status}`)
    const u = URL.createObjectURL(await res.blob()); const a = document.createElement('a'); a.href = u; a.download = `data-rights-${r.id}.zip`; a.click(); URL.revokeObjectURL(u)
  }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Players', to: '/players' }, { title: 'Data rights' }]} />
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Data rights</h1><p className="mt-1 text-xs text-muted-foreground">GDPR / CCPA export &amp; erasure · {String(q.data?.pending ?? 0)} pending · worker every {String(q.data?.poll_minutes ?? '?')} min. Export bundles every table that references the account into a downloadable archive; erase anonymises the account and deletes personal data while keeping aggregate economics intact.</p></div>
        <Select value={filter} onValueChange={(v) => void nav({ search: { filter: v === 'all' ? undefined : v }, replace: true })}><SelectTrigger className="w-32"><SelectValue /></SelectTrigger><SelectContent>{['all', 'active', 'done', 'failed'].map((s) => <SelectItem key={s} value={s}>{s}</SelectItem>)}</SelectContent></Select>
        <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', '/v1/server/data_rights/run'), 'Pending requests started')}>Run pending now</Button>
        <Button size="sm" onClick={() => { setForm({ account_id: '', kind: 'export', include_game_data: true }); setDlg(true) }}>New request</Button>
      </div>
      <Card><CardContent className="pt-6">
        <DataTable id="data-rights" rows={rows} loading={q.isPending} empty="No requests" rowTo={(r) => `/players/accounts/${r.account_id}`}
          cols={[{ key: 'kind', fmt: (r) => `${r.kind}${r.include_game_data === false ? ' (no game data)' : ''}` }, { key: 'account_id', label: 'account', mono: true }, { key: 'status', fmt: (r) => <span className={r.status === 'failed' ? 'text-destructive' : ''}>{r.status}</span> }, { key: 'requested_at', label: 'requested', dim: true, fmt: (r) => ts(r.requested_at ?? r.created_at) }, { key: 'completed_at', label: 'completed', dim: true, fmt: (r) => ts(r.completed_at) }, { key: 'requested_by', label: 'by', dim: true }, { key: 'error', dim: true }]}
          actions={(r) => (
            <span className="flex justify-end gap-1">
              {!!r.downloadable && <Button size="sm" variant="outline" onClick={() => void download(r)}>Download</Button>}
              {!done.includes(r.status) && <Button size="sm" variant="ghost" onClick={() => confirmThen('Cancel this request?', () => run(() => raw('POST', `/v1/server/data_rights/${r.id}/cancel`), 'Cancelled'))}>Cancel</Button>}
            </span>
          )} />
      </CardContent></Card>
      <FormDialog open={dlg} onOpenChange={setDlg} title="New data-rights request" submit="Create" pending={pending} disabled={!form.account_id}
        onSubmit={() => { const go = () => run(() => raw('POST', '/v1/server/data_rights', { account_id: String(form.account_id).trim(), kind: form.kind, include_game_data: !!form.include_game_data }), 'Request created'); if (form.kind === 'export') go(); else confirmThen('Erasure is irreversible. Continue?', go) }}>
        <Field label="Account id"><Input className="font-mono" value={form.account_id ?? ''} onChange={(e) => set('account_id', e.target.value)} /></Field>
        <Field label="Kind"><Select value={String(form.kind)} onValueChange={(v) => set('kind', v)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="export">export — produce a downloadable archive</SelectItem><SelectItem value="erase">erase — anonymise and delete personal data</SelectItem></SelectContent></Select></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!form.include_game_data} onCheckedChange={(v) => set('include_game_data', v === true)} /> include game data (inventory, progression, matches)</label>
      </FormDialog>
    </div>
  )
}
