import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { FormDialog, Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { CampaignDialog, ts } from '../codes'

/** One code campaign: its codes (generated batches and shared codes, filterable by used/unused and batch) and the redemption log. */
export const Route = createFileRoute('/_app/economy/codes/$id')({
  validateSearch: (s: Record<string, unknown>): { unused?: string; batch?: string } => ({ unused: typeof s.unused === 'string' ? s.unused : undefined, batch: typeof s.batch === 'string' ? s.batch : undefined }),
  component: Campaign,
})

function Campaign() {
  const { id } = Route.useParams()
  const { unused = '', batch = '' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const C = `/v1/server/codes/campaigns/${encodeURIComponent(id)}`
  const camp = useSub<Row>(C)
  const qs = new URLSearchParams({ limit: '200', ...(unused ? { unused } : {}), ...(batch ? { batch } : {}) })
  const codes = useSub<Row>(`${C}/codes?${qs}`)
  const red = useSub<Row>(`${C}/redemptions?limit=200`)
  const c = (camp.data?.campaign ?? camp.data) as Row | undefined
  const [dlg, setDlg] = useState<'' | 'edit' | 'generate' | 'shared'>('')
  const [f, setF] = useState<Row>({})
  const [generated, setGenerated] = useState<unknown>(null)
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const gen = useAct([['sub']])
  const codeRows = listOf(codes.data, 'codes')
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Code campaigns', to: '/economy/codes' }, { title: String(c?.name ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{String(c?.name ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">{id}{c && <Badge variant={c.enabled ? 'default' : 'secondary'}>{c.enabled ? 'enabled' : 'disabled'}</Badge>}</div></div>
        <Button size="sm" variant="outline" onClick={() => setDlg('edit')}>Edit</Button>
        <Button size="sm" variant="outline" onClick={() => { setF({ code: '', max_uses: 100 }); setDlg('shared') }}>Add shared code</Button>
        <Button size="sm" onClick={() => { setF({ count: 100, length: 10, prefix: '', max_uses: 1, batch: new Date().toISOString().slice(0, 10) }); setGenerated(null); setDlg('generate') }}>Generate codes</Button>
      </div>
      {camp.isError && <Alert variant="destructive"><AlertDescription>{String(camp.error)}</AlertDescription></Alert>}
      {c && <Card><CardContent className="pt-6"><KvGrid>
        <Kv k="Reward" v={c.reward_table ? `reward table ${c.reward_table}` : c.key_group ? `key group ${c.key_group}` : 'none'} />
        <Kv k="Limits" v={`${c.per_account} per account · ${c.max_redemptions ?? 'unlimited'} total`} />
        <Kv k="Window" v={c.starts_at || c.ends_at ? `${ts(c.starts_at)} → ${ts(c.ends_at)}` : 'always'} />
        <Kv k="Audience" v={c.audience ?? 'everyone'} />
        <Kv k="Redeemed" v={c.redemptions ?? c.redeemed ?? red.data ? listOf(red.data, 'redemptions').length : '—'} />
      </KvGrid>{c.metadata && Object.keys(c.metadata as Row).length ? <div className="mt-3"><JsonView value={c.metadata} /></div> : null}</CardContent></Card>}
      <Card><CardContent className="pt-6">
        <SubHead title="Codes" count={codeRows.length}>
          <Select value={unused || 'all'} onValueChange={(v) => void nav({ search: (p) => ({ ...p, unused: v === 'all' ? undefined : v }), replace: true })}><SelectTrigger className="h-8 w-32"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="all">all</SelectItem><SelectItem value="true">unused</SelectItem><SelectItem value="false">used</SelectItem></SelectContent></Select>
          <Input className="h-8 w-40" placeholder="batch" defaultValue={batch} onKeyDown={(e) => e.key === 'Enter' && void nav({ search: (p) => ({ ...p, batch: (e.target as HTMLInputElement).value || undefined }), replace: true })} />
        </SubHead>
        {codes.isError ? <Alert variant="destructive"><AlertDescription>{String(codes.error)}</AlertDescription></Alert> :
          <DataTable id="campaign-codes" rows={codeRows} loading={codes.isPending} empty="No codes yet — generate a batch or add a shared code" cols={[{ key: 'code', mono: true }, { key: 'uses', fmt: (r) => `${r.uses ?? r.used ?? 0} / ${r.max_uses ?? 1}` }, { key: 'batch', dim: true }, { key: 'revoked_at', label: 'revoked', fmt: (r) => (r.revoked_at ? ts(r.revoked_at) : ''), dim: true }, { key: 'created_at', label: 'created', fmt: (r) => ts(r.created_at), dim: true }]}
            actions={(r) => !r.revoked_at ? <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Revoke code ${r.code}?`, () => run(() => raw('POST', `/v1/server/codes/${encodeURIComponent(String(r.code))}/revoke`), 'Code revoked'))}>Revoke</Button> : null} />}
        <SubHead title="Redemptions" count={listOf(red.data, 'redemptions').length} />
        {red.isError ? <Alert variant="destructive"><AlertDescription>{String(red.error)}</AlertDescription></Alert> :
          <DataTable id="campaign-redemptions" rows={listOf(red.data, 'redemptions')} loading={red.isPending} empty="Nobody has redeemed yet" rowTo={(r) => (r.account_id ? `/players/accounts/${r.account_id}` : null)} cols={[{ key: 'at', fmt: (r) => ts(r.at ?? r.redeemed_at ?? r.created_at), dim: true }, { key: 'code', mono: true }, { key: 'account_id', mono: true }, { key: 'result', fmt: (r) => r.result ?? r.status ?? r.key ?? '', dim: true }]} />}
      </CardContent></Card>
      <CampaignDialog open={dlg === 'edit'} onClose={() => setDlg('')} initial={c ?? null} />
      <FormDialog open={dlg === 'generate'} onOpenChange={(o) => !o && setDlg('')} title="Generate codes" submit="Generate" pending={gen.pending} disabled={!Number(f.count)}
        onSubmit={() => gen.run(async () => { const r = await raw('POST', `${C}/generate`, { count: Number(f.count), length: Number(f.length), prefix: f.prefix || undefined, max_uses: Number(f.max_uses) || 1, batch: f.batch || undefined }); setGenerated(r) }, 'Codes generated')}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Count"><Input type="number" value={String(f.count ?? '')} onChange={(e) => setF((x) => ({ ...x, count: e.target.value }))} /></Field>
          <Field label="Length"><Input type="number" value={String(f.length ?? '')} onChange={(e) => setF((x) => ({ ...x, length: e.target.value }))} /></Field>
          <Field label="Prefix"><Input className="font-mono" placeholder="LAUNCH-" value={String(f.prefix ?? '')} onChange={(e) => setF((x) => ({ ...x, prefix: e.target.value }))} /></Field>
          <Field label="Max uses per code"><Input type="number" value={String(f.max_uses ?? '')} onChange={(e) => setF((x) => ({ ...x, max_uses: e.target.value }))} /></Field>
        </div>
        <Field label="Batch label"><Input value={String(f.batch ?? '')} onChange={(e) => setF((x) => ({ ...x, batch: e.target.value }))} /></Field>
        {generated != null && <><p className="text-xs text-muted-foreground">Generated — copy now; they are also in the list.</p><JsonView value={generated} /></>}
      </FormDialog>
      <FormDialog open={dlg === 'shared'} onOpenChange={(o) => !o && setDlg('')} title="Add shared code" submit="Add" pending={pending} disabled={!f.code}
        onSubmit={() => run(() => raw('POST', `${C}/codes`, { code: f.code, max_uses: Number(f.max_uses) || 1 }), 'Code added')}>
        <p className="text-sm text-muted-foreground">One human-readable code many players redeem (streamer drops, newsletters).</p>
        <Field label="Code"><Input className="font-mono" placeholder="WELCOME2026" value={String(f.code ?? '')} onChange={(e) => setF((x) => ({ ...x, code: e.target.value.toUpperCase() }))} /></Field>
        <Field label="Max uses"><Input type="number" value={String(f.max_uses ?? '')} onChange={(e) => setF((x) => ({ ...x, max_uses: e.target.value }))} /></Field>
      </FormDialog>
    </div>
  )
}
