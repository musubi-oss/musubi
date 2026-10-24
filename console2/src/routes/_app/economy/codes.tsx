import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'

/** Redeem-code campaigns: a campaign names the reward (reward table or platform key group), its limits, window and audience; codes are generated in batches or added as shared codes. */
export const Route = createFileRoute('/_app/economy/codes')({
  validateSearch: (s: Record<string, unknown>): { q?: string } => ({ q: typeof s.q === 'string' ? s.q : undefined }),
  component: Codes,
})

const local = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().slice(0, 16) : '')
export const epoch = (s: unknown) => (s ? Math.floor(new Date(String(s)).getTime() / 1000) : null)
export const ts = (v: unknown) => (v ? new Date(Number(v) * (Number(v) < 1e12 ? 1000 : 1)).toISOString().replace('T', ' ').slice(0, 16) : '—')

export function CampaignDialog({ open, onClose, initial, onSaved }: { open: boolean; onClose: () => void; initial: Row | null; onSaved?: () => void }) {
  const [f, setF] = useState<Row>({})
  const [key, setKey] = useState('')
  const k = `${open}:${String(initial?.id ?? '')}`
  if (open && key !== k) { setKey(k); setF(initial ? { ...initial, metadata: JSON.stringify(initial.metadata ?? {}), starts_at: local(initial.starts_at), ends_at: local(initial.ends_at) } : { id: '', name: '', reward_table: '', key_group: '', per_account: 1, max_redemptions: '', starts_at: '', ends_at: '', audience: '', metadata: '{}', enabled: true }) }
  const { run, pending } = useAct([['sub']], () => { onClose(); onSaved?.() })
  let metaOk = true; try { JSON.parse(String(f.metadata || '{}')) } catch { metaOk = false }
  const set = (k: string) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => setF((x) => ({ ...x, [k]: e.target.value }))
  return (
    <FormDialog open={open} onOpenChange={(o) => !o && onClose()} title={initial ? `Campaign ${initial.id}` : 'New campaign'} submit="Save" pending={pending} disabled={!f.id || !f.name || !metaOk}
      onSubmit={() => run(() => raw('PUT', `/v1/server/codes/campaigns/${encodeURIComponent(String(f.id))}`, { name: f.name, reward_table: f.reward_table || null, key_group: f.key_group || null, per_account: Number(f.per_account) || 1, max_redemptions: f.max_redemptions === '' || f.max_redemptions == null ? null : Number(f.max_redemptions), starts_at: epoch(f.starts_at), ends_at: epoch(f.ends_at), audience: f.audience || null, enabled: !!f.enabled, metadata: JSON.parse(String(f.metadata || '{}')) }), 'Campaign saved')}>
      <div className="grid grid-cols-2 gap-3">
        <Field label="Id"><Input className="font-mono" value={String(f.id ?? '')} disabled={!!initial} placeholder="launch" onChange={set('id')} /></Field>
        <Field label="Name"><Input value={String(f.name ?? '')} onChange={set('name')} /></Field>
        <Field label="Reward table id (or empty)"><Input className="font-mono" value={String(f.reward_table ?? '')} onChange={set('reward_table')} /></Field>
        <Field label="Key group id (platform keys, or empty)"><Input className="font-mono" value={String(f.key_group ?? '')} onChange={set('key_group')} /></Field>
        <Field label="Per account"><Input type="number" value={String(f.per_account ?? '')} onChange={set('per_account')} /></Field>
        <Field label="Max redemptions (empty = unlimited)"><Input type="number" value={String(f.max_redemptions ?? '')} onChange={set('max_redemptions')} /></Field>
        <Field label="Starts (local, empty = always)"><Input type="datetime-local" value={String(f.starts_at ?? '')} onChange={set('starts_at')} /></Field>
        <Field label="Ends"><Input type="datetime-local" value={String(f.ends_at ?? '')} onChange={set('ends_at')} /></Field>
      </div>
      <Field label="Audience (name, empty = everyone)"><Input value={String(f.audience ?? '')} onChange={set('audience')} /></Field>
      <Field label={metaOk ? 'Metadata (JSON)' : 'Metadata — invalid JSON'}><Textarea className={`min-h-16 font-mono text-xs ${metaOk ? '' : 'border-destructive'}`} value={String(f.metadata ?? '')} onChange={set('metadata')} /></Field>
      <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!f.enabled} onCheckedChange={(v) => setF((x) => ({ ...x, enabled: !!v }))} /> Enabled</label>
    </FormDialog>
  )
}

function Codes() {
  const { q = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const list = useSub<Row>('/v1/server/codes/campaigns')
  const all = listOf(list.data, 'campaigns')
  const rows = all.filter((c) => !q || String(c.id).includes(q) || String(c.name ?? '').toLowerCase().includes(q.toLowerCase()))
  const [dlg, setDlg] = useState<Row | null | false>(false)
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Code campaigns' }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Code campaigns</h1><p className="mt-1 text-sm text-muted-foreground">{all.length} campaigns · {all.filter((c) => c.enabled).length} enabled. A player redeems at <code>POST /v1/codes/redeem</code>; the campaign's reward table is granted or one key from its key group is handed out — per-account and total caps, window and audience gate every redemption.</p></div>
        <Button size="sm" onClick={() => setDlg(null)}>New campaign</Button>
      </div>
      {list.isError && <Alert variant="destructive"><AlertDescription>{String(list.error)}</AlertDescription></Alert>}
      <Card><CardContent className="pt-6">
        <div className="mb-3 flex flex-wrap gap-2"><Input className="w-full sm:w-64" placeholder="id or name" value={q} onChange={(e) => void nav({ search: { q: e.target.value || undefined }, replace: true })} /></div>
        <DataTable id="code-campaigns" rows={rows} loading={list.isPending} empty={q ? 'No campaign matches' : 'No campaigns yet'} rowTo={(r) => `/economy/codes/${encodeURIComponent(String(r.id))}`}
          cols={[{ key: 'id', mono: true }, { key: 'name' }, { key: 'reward', fmt: (r) => (r.reward_table ? `table ${r.reward_table}` : r.key_group ? `keys ${r.key_group}` : '—') }, { key: 'per_account', label: 'per account' }, { key: 'max_redemptions', label: 'max', fmt: (r) => r.max_redemptions ?? '∞' }, { key: 'window', fmt: (r) => (r.starts_at || r.ends_at ? `${ts(r.starts_at)} → ${ts(r.ends_at)}` : 'always'), dim: true }, { key: 'audience', fmt: (r) => r.audience ?? 'everyone', dim: true }, { key: 'enabled', fmt: (r) => <Badge variant={r.enabled ? 'default' : 'secondary'}>{r.enabled ? 'on' : 'off'}</Badge> }]}
          actions={(r) => <Button size="sm" variant="ghost" onClick={() => setDlg(r)}>Edit</Button>} />
      </CardContent></Card>
      <CampaignDialog open={dlg !== false} onClose={() => setDlg(false)} initial={dlg === false ? null : dlg} />
    </div>
  )
}
