import { Link, createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { qs, raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { short, ts } from '@/lib/format'
import { listOf } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'

/** Moderation desk: player reports, chat reports, bans and the automatic rules that issue them. */
export const Route = createFileRoute('/_app/players/reports')({
  validateSearch: (s: Record<string, unknown>): { tab?: string; status?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined, status: typeof s.status === 'string' ? s.status : undefined }),
  component: Moderation,
})

function Sel({ value, onChange, options, className }: { value: string; onChange: (v: string) => void; options: [string, string][]; className?: string }) {
  return <Select value={value} onValueChange={onChange}><SelectTrigger className={className ?? 'w-44'}><SelectValue /></SelectTrigger><SelectContent>{options.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}</SelectContent></Select>
}

function Moderation() {
  const { tab = 'reports', status = 'open' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const [category, setCategory] = useState('')
  const [banQ, setBanQ] = useState(''); const [banApplied, setBanApplied] = useState('')
  const [shadow, setShadow] = useState('all')
  const [dlg, setDlg] = useState<'' | 'decide' | 'chat' | 'rule'>('')
  const [sel, setSel] = useState<Row | null>(null)
  const [form, setForm] = useState<Row>({})
  const set = (k: string, v: unknown) => setForm((f) => ({ ...f, [k]: v }))
  const { run, pending } = useAct([['mod']], () => setDlg(''))

  const reportsQ = useQuery({ queryKey: ['mod', 'reports', status, category], enabled: tab === 'reports', queryFn: ({ signal }) => raw<Row>('GET', '/v1/admin/reports' + qs({ limit: 200, status: status === 'all' ? undefined : status, category: category || undefined }), undefined, { signal }) })
  const chatQ = useQuery({ queryKey: ['mod', 'chat'], queryFn: ({ signal }) => raw<unknown>('GET', '/v1/admin/chat/reports', undefined, { signal }) })
  const bansQ = useQuery({ queryKey: ['mod', 'bans', banApplied], enabled: tab === 'bans', queryFn: ({ signal }) => raw<Row>('GET', '/v1/admin/bans' + qs({ limit: 200, q: banApplied || undefined }), undefined, { signal }) })
  const rulesQ = useQuery({ queryKey: ['mod', 'rules'], enabled: tab === 'rules', queryFn: ({ signal }) => raw<Row>('GET', '/v1/admin/bans/rules', undefined, { signal }) })
  const reports = listOf(reportsQ.data, 'reports')
  const chat = listOf(chatQ.data)
  const bans = listOf(bansQ.data, 'bans').filter((b) => shadow === 'all' || (shadow === 'shadow') === !!b.shadow)
  const rules = listOf(rulesQ.data, 'rules')
  const go = (patch: Record<string, string | undefined>) => void nav({ search: (p) => ({ ...p, ...patch }), replace: true })
  const acct = (id: unknown, tab?: string) => (id ? <Link to="/players/accounts/$id" params={{ id: String(id) }} search={tab ? { tab } : {}} className="font-mono text-xs hover:underline">{short(id)}</Link> : '—')

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Players', to: '/players' }, { title: 'Moderation' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Moderation</h1><p className="mt-1 text-xs text-muted-foreground">Player reports, chat reports, bans and the automatic rules that issue them. Rows link to the player's hub.</p></div>
        {tab === 'rules' && <Button size="sm" onClick={() => { setForm({ kind: 'reports', threshold: 5, window_secs: 86400, action: 'ban', until_secs: 604800, reason: '', shadow: false, enabled: true }); setDlg('rule') }}>Add rule</Button>}
      </div>
      <Tabs value={tab} onValueChange={(v) => go({ tab: v === 'reports' ? undefined : v })}>
        <div className="overflow-x-auto"><TabsList className="w-max"><TabsTrigger value="reports">Player reports</TabsTrigger><TabsTrigger value="chat">Chat reports ({chat.length})</TabsTrigger><TabsTrigger value="bans">Bans</TabsTrigger><TabsTrigger value="rules">Automatic rules</TabsTrigger></TabsList></div>
        <Card className="mt-3"><CardContent className="pt-6">
          <TabsContent value="reports">
            <div className="mb-3 flex flex-wrap gap-2">
              <Sel value={status} onChange={(v) => go({ status: v === 'open' ? undefined : v })} options={[['open', 'open'], ['resolved', 'resolved'], ['dismissed', 'dismissed'], ['all', 'all']]} />
              <Input className="w-48" value={category} placeholder="category" onChange={(e) => setCategory(e.target.value)} />
            </div>
            <DataTable id="mod-reports" rows={reports} loading={reportsQ.isPending} empty="No reports match"
              cols={[{ key: 'created_at', label: 'created', dim: true, fmt: (r) => ts(r.created_at) }, { key: 'target_id', label: 'target', fmt: (r) => acct(r.target_id, 'bans') }, { key: 'reporter_id', label: 'reporter', fmt: (r) => acct(r.reporter_id) }, { key: 'category' }, { key: 'details', dim: true, fmt: (r) => (typeof r.details === 'string' ? r.details : JSON.stringify(r.details ?? '')) }, { key: 'status' }, { key: 'decision', dim: true }]}
              actions={(r) => r.status !== 'open' ? null : (
                <span className="flex justify-end gap-1">
                  <Button size="sm" onClick={() => { setSel(r); setForm({ decision: 'warned', note: '', ban: false, reason: r.category ?? '', until_days: 7, shadow: false }); setDlg('decide') }}>Decide</Button>
                  <Button size="sm" variant="ghost" onClick={() => run(() => raw('POST', `/v1/admin/reports/${r.id}`, { status: 'dismissed', decision: 'no evidence' }), 'Dismissed')}>Dismiss</Button>
                </span>
              )} />
          </TabsContent>
          <TabsContent value="chat">
            <DataTable id="mod-chat" rows={chat} loading={chatQ.isPending} empty="No chat reports"
              cols={[{ key: 'created_at', label: 'created', dim: true, fmt: (r) => ts(r.created_at) }, { key: 'channel_id', label: 'channel', mono: true, dim: true, fmt: (r) => short(r.channel_id) }, { key: 'target_id', label: 'target', fmt: (r) => acct(r.target_id) }, { key: 'reporter_id', label: 'reporter', fmt: (r) => acct(r.reporter_id) }, { key: 'reason' }, { key: 'status' }, { key: 'context', dim: true, fmt: (r) => (typeof r.context === 'string' ? r.context : JSON.stringify(r.context ?? r.message ?? '')) }]}
              actions={(r) => r.status !== 'open' && r.status ? null : <Button size="sm" onClick={() => { setSel(r); setForm({ action: 'mute', duration_h: 1, note: '' }); setDlg('chat') }}>Decide</Button>} />
          </TabsContent>
          <TabsContent value="bans">
            <form className="mb-3 flex flex-wrap gap-2" onSubmit={(e) => { e.preventDefault(); setBanApplied(banQ) }}>
              <Input className="w-64" value={banQ} placeholder="username / account id" onChange={(e) => setBanQ(e.target.value)} />
              <Sel value={shadow} onChange={setShadow} options={[['all', 'all bans'], ['shadow', 'shadow only'], ['visible', 'visible only']]} className="w-36" />
              <Button type="submit" size="sm" variant="outline">Search</Button>
              <span className="self-center text-xs text-muted-foreground">{String(bansQ.data?.total ?? bans.length)} bans</span>
            </form>
            <DataTable id="mod-bans" rows={bans} loading={bansQ.isPending} empty="No bans" rowTo={(b) => `/players/accounts/${b.account_id}?tab=bans`}
              cols={[{ key: 'username', label: 'player', fmt: (b) => b.username ?? short(b.account_id) }, { key: 'shadow', label: 'kind', fmt: (b) => (b.shadow ? 'shadow' : 'ban') }, { key: 'reason' }, { key: 'until', fmt: (b) => (b.until ? ts(b.until) : 'permanent') }, { key: 'actor', label: 'by', dim: true }, { key: 'created_at', label: 'created', dim: true, fmt: (b) => ts(b.created_at) }]}
              actions={(b) => <Button size="sm" variant="ghost" onClick={() => confirmThen('Lift this ban?', () => run(() => raw('POST', `/v1/admin/accounts/${b.account_id}/unban`), 'Unbanned'))}>Unban</Button>} />
          </TabsContent>
          <TabsContent value="rules">
            <p className="mb-3 text-xs text-muted-foreground">Rules act automatically when a player crosses a threshold inside the window: ban, shadow-ban or mute, for the given duration.</p>
            <DataTable id="mod-rules" tools={false} rows={rules} loading={rulesQ.isPending} empty="No automatic rules — every ban is manual"
              cols={[{ key: 'kind', label: 'signal' }, { key: 'threshold' }, { key: 'window_secs', label: 'window', dim: true, fmt: (r) => `${Math.round(Number(r.window_secs ?? 0) / 3600)} h` }, { key: 'action' }, { key: 'until_secs', label: 'duration', dim: true, fmt: (r) => (r.until_secs ? `${Math.round(Number(r.until_secs) / 86400)} d` : 'permanent') }, { key: 'reason' }, { key: 'enabled', fmt: (r) => (r.enabled === false ? 'off' : 'on') }]}
              actions={(r) => <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen('Delete this rule?', () => run(() => raw('DELETE', `/v1/admin/bans/rules/${r.id}`), 'Rule deleted'))}>Delete</Button>} />
          </TabsContent>
        </CardContent></Card>
      </Tabs>

      <FormDialog open={dlg === 'decide'} onOpenChange={(o) => !o && setDlg('')} title="Decide report" submit="Resolve" pending={pending}
        onSubmit={() => run(() => { const b: Row = { status: 'resolved', decision: form.decision, note: form.note }; if (form.ban) b.ban = { reason: form.reason, until_secs: Number(form.until_days) * 86400 || null, shadow: !!form.shadow }; return raw('POST', `/v1/admin/reports/${sel?.id}`, b) }, 'Resolved')}>
        {sel && <JsonView value={{ target: sel.target_id, category: sel.category, details: sel.details, evidence: sel.evidence }} className="max-h-40" />}
        <Field label="Decision"><Sel className="w-full" value={String(form.decision ?? 'warned')} onChange={(v) => set('decision', v)} options={[['warned', 'warned'], ['banned', 'banned'], ['muted', 'muted'], ['no action', 'no action']]} /></Field>
        <Field label="Note"><Input value={form.note ?? ''} onChange={(e) => set('note', e.target.value)} /></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!form.ban} onCheckedChange={(v) => set('ban', v === true)} /> also ban the target</label>
        {!!form.ban && <>
          <Field label="Ban reason"><Input value={form.reason ?? ''} onChange={(e) => set('reason', e.target.value)} /></Field>
          <div className="grid grid-cols-2 gap-3"><Field label="Days (0 = permanent)"><Input type="number" min={0} value={form.until_days ?? 7} onChange={(e) => set('until_days', e.target.value)} /></Field><label className="flex items-center gap-2 self-end pb-2 text-sm"><Checkbox checked={!!form.shadow} onCheckedChange={(v) => set('shadow', v === true)} /> shadow</label></div>
        </>}
      </FormDialog>
      <FormDialog open={dlg === 'chat'} onOpenChange={(o) => !o && setDlg('')} title="Decide chat report" submit="Resolve" pending={pending}
        onSubmit={() => run(() => raw('POST', `/v1/admin/chat/reports/${sel?.id}`, { status: 'resolved', action: form.action, duration_secs: Number(form.duration_h ?? 0) * 3600 || undefined, note: form.note }), 'Resolved')}>
        <Field label="Action"><Sel className="w-full" value={String(form.action ?? 'mute')} onChange={(v) => set('action', v)} options={[['mute', 'mute in chat'], ['kick', 'kick from channel'], ['ban', 'ban account'], ['none', 'no action']]} /></Field>
        {(form.action === 'mute' || form.action === 'ban') && <Field label="Duration (hours)"><Input type="number" min={0} value={form.duration_h ?? 1} onChange={(e) => set('duration_h', e.target.value)} /></Field>}
        <Field label="Note"><Input value={form.note ?? ''} onChange={(e) => set('note', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'rule'} onOpenChange={(o) => !o && setDlg('')} title="Automatic rule" submit="Add" pending={pending}
        onSubmit={() => run(() => raw('POST', '/v1/admin/bans/rules', { ...form, threshold: Number(form.threshold), window_secs: Number(form.window_secs), until_secs: Number(form.until_secs) || null }), 'Rule added')}>
        <Field label="Signal"><Sel className="w-full" value={String(form.kind ?? 'reports')} onChange={(v) => set('kind', v)} options={[['reports', 'player reports received'], ['chat_reports', 'chat reports received'], ['anticheat', 'anti-cheat flags'], ['fraud', 'economy fraud flags']]} /></Field>
        <div className="grid grid-cols-2 gap-3"><Field label="Threshold"><Input type="number" min={1} value={form.threshold ?? 5} onChange={(e) => set('threshold', e.target.value)} /></Field><Field label="Window (s)"><Input type="number" min={60} value={form.window_secs ?? 86400} onChange={(e) => set('window_secs', e.target.value)} /></Field></div>
        <div className="grid grid-cols-2 gap-3"><Field label="Action"><Sel className="w-full" value={String(form.action ?? 'ban')} onChange={(v) => set('action', v)} options={[['ban', 'ban'], ['shadow_ban', 'shadow ban'], ['mute', 'mute']]} /></Field><Field label="Duration (s, 0 = permanent)"><Input type="number" min={0} value={form.until_secs ?? 0} onChange={(e) => set('until_secs', e.target.value)} /></Field></div>
        <Field label="Reason shown to the player"><Input value={form.reason ?? ''} onChange={(e) => set('reason', e.target.value)} /></Field>
      </FormDialog>
    </div>
  )
}
