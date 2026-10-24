import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { Field } from '@/components/form-dialog'
import { JsonView } from '@/components/json-view'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

/** Hands-on economy operations for support and LiveOps: grant an item, repair a durable, adjust a wallet (reason + idempotency key so a retry never pays twice), send mail to one account or an audience, and the per-account grant limits. */
export const Route = createFileRoute('/_app/economy/inventory_ops')({ component: InventoryOps })

type Set = React.Dispatch<React.SetStateAction<Row>>
const bind = (set: Set, k: string) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => set((x) => ({ ...x, [k]: e.target.value }))
const S = (v: unknown) => String(v ?? '')

function Op({ title, children, result }: { title: string; children: React.ReactNode; result?: unknown }) {
  return <Card className="min-w-0"><CardContent className="space-y-3 pt-6"><h3 className="text-sm font-semibold">{title}</h3>{children}{result != null ? <JsonView value={result} /> : null}</CardContent></Card>
}

function InventoryOps() {
  const limits = useSub<Row>('/v1/server/grant_limits')
  const [grant, setGrant] = useState<Row>({ account_id: '', item_id: '', quantity: 1, instance: '' })
  const [repair, setRepair] = useState<Row>({ account_id: '', id: '', amount: 10 })
  const [adj, setAdj] = useState<Row>({ account_id: '', currency: '', delta: 0, reason: '', reference: '', idempotency_key: '' })
  const [mail, setMail] = useState<Row>({ account_id: '', audience: '', title: '', body: '', rewards: '', expires_secs: 0 })
  const [lim, setLim] = useState<Row>({ kind: 'item', key: '', window_secs: 3600, max_per_account: 1 })
  const [out, setOut] = useState<Record<string, unknown>>({})
  const { run, pending } = useAct([['sub']])
  const go = (k: string, fn: () => Promise<unknown>, ok: string) => run(async () => { const r = await fn(); setOut((o) => ({ ...o, [k]: r })) }, ok)
  let instOk = true; try { if (S(grant.instance).trim()) JSON.parse(S(grant.instance)) } catch { instOk = false }
  const delta = Number(adj.delta)
  const mailBody = () => ({ account_ids: mail.account_id ? [mail.account_id] : [], all: false, audience: mail.audience || null, title: mail.title, body: mail.body, rewards: mail.rewards || null, metadata: {}, expires_in_seconds: Number(mail.expires_secs) || null })
  const count = (out.count as Row | undefined)?.count
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Economy', to: '/economy' }, { title: 'Grants & wallet' }]} />
      <div><h1 className="text-xl font-semibold">Grants & wallet</h1><p className="mt-1 text-sm text-muted-foreground">Support and LiveOps operations on one player's economy — every action is journaled with the operator and reason.</p></div>
      <div className="grid min-w-0 grid-cols-1 gap-4 lg:grid-cols-2">
        <Op title="Grant item" result={out.grant}>
          <Field label="Account id"><Input className="font-mono" value={S(grant.account_id)} onChange={bind(setGrant, 'account_id')} /></Field>
          <div className="grid grid-cols-3 gap-3"><div className="col-span-2"><Field label="Item id"><Input className="font-mono" value={S(grant.item_id)} onChange={bind(setGrant, 'item_id')} /></Field></div><Field label="Quantity"><Input type="number" value={S(grant.quantity)} onChange={bind(setGrant, 'quantity')} /></Field></div>
          <Field label={instOk ? 'Instance data (JSON, optional — durability, enchant…)' : 'Instance data — invalid JSON'}><Textarea className={`min-h-12 font-mono text-xs ${instOk ? '' : 'border-destructive'}`} value={S(grant.instance)} onChange={bind(setGrant, 'instance')} /></Field>
          <Button size="sm" disabled={pending || !grant.account_id || !grant.item_id || !instOk} onClick={() => go('grant', () => raw('POST', '/v1/server/inventory/grant', { account_id: grant.account_id, item_id: grant.item_id, quantity: Number(grant.quantity) || 1, instance: S(grant.instance).trim() ? JSON.parse(S(grant.instance)) : undefined }), 'Granted')}>Grant</Button>
        </Op>
        <Op title="Adjust wallet" result={out.adjust}>
          <Field label="Account id"><Input className="font-mono" value={S(adj.account_id)} onChange={bind(setAdj, 'account_id')} /></Field>
          <div className="grid grid-cols-2 gap-3"><Field label="Currency"><Input className="font-mono" value={S(adj.currency)} onChange={bind(setAdj, 'currency')} /></Field><Field label="Delta (negative to deduct)"><Input type="number" value={S(adj.delta)} onChange={bind(setAdj, 'delta')} /></Field></div>
          <Field label="Reason (required, goes to the ledger)"><Input value={S(adj.reason)} placeholder="compensation for outage 2026-10-14" onChange={bind(setAdj, 'reason')} /></Field>
          <div className="grid grid-cols-2 gap-3"><Field label="Reference (ticket, optional)"><Input value={S(adj.reference)} onChange={bind(setAdj, 'reference')} /></Field><Field label="Idempotency key"><div className="flex gap-2"><Input className="font-mono" value={S(adj.idempotency_key)} onChange={bind(setAdj, 'idempotency_key')} /><Button size="sm" variant="outline" onClick={() => setAdj((x) => ({ ...x, idempotency_key: 'console-' + Date.now().toString(36) }))}>New</Button></div></Field></div>
          <Button size="sm" variant={delta < 0 ? 'destructive' : 'default'} disabled={pending || !adj.account_id || !adj.currency || !delta || !adj.reason} onClick={() => confirmThen(`${delta > 0 ? 'Credit' : 'Debit'} ${Math.abs(delta)} ${adj.currency}?`, () => go('adjust', () => raw('POST', '/v1/server/wallet/adjust', { ...adj, delta, idempotency_key: adj.idempotency_key || undefined, reference: adj.reference || undefined }), 'Wallet adjusted'))}>Apply</Button>
        </Op>
        <Op title="Repair durable item" result={out.repair}>
          <Field label="Account id"><Input className="font-mono" value={S(repair.account_id)} onChange={bind(setRepair, 'account_id')} /></Field>
          <div className="grid grid-cols-3 gap-3"><div className="col-span-2"><Field label="Inventory entry id"><Input className="font-mono" value={S(repair.id)} onChange={bind(setRepair, 'id')} /></Field></div><Field label="Durability to restore"><Input type="number" value={S(repair.amount)} onChange={bind(setRepair, 'amount')} /></Field></div>
          <Button size="sm" disabled={pending || !repair.account_id || !repair.id} onClick={() => go('repair', () => raw('POST', '/v1/server/inventory/repair', { ...repair, amount: Number(repair.amount) }), 'Repaired')}>Repair</Button>
        </Op>
        <Op title="Send mail" result={out.mail ?? out.count}>
          <div className="grid grid-cols-2 gap-3"><Field label="Account id (one player)"><Input className="font-mono" value={S(mail.account_id)} onChange={bind(setMail, 'account_id')} /></Field><Field label="…or audience name (bulk)"><Input value={S(mail.audience)} onChange={bind(setMail, 'audience')} /></Field></div>
          <Field label="Title"><Input value={S(mail.title)} onChange={bind(setMail, 'title')} /></Field>
          <Field label="Body"><Textarea className="min-h-16" value={S(mail.body)} onChange={bind(setMail, 'body')} /></Field>
          <div className="grid grid-cols-2 gap-3"><Field label="Rewards (reward table, optional)"><Input className="font-mono" value={S(mail.rewards)} onChange={bind(setMail, 'rewards')} /></Field><Field label="Expires in (s, 0 = never)"><Input type="number" value={S(mail.expires_secs)} onChange={bind(setMail, 'expires_secs')} /></Field></div>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" disabled={pending || !mail.audience} onClick={() => go('count', () => raw('POST', '/v1/server/mail/audience/count', { audience: mail.audience }), 'Counted')}>Count recipients</Button>
            <Button size="sm" disabled={pending || !(mail.account_id || mail.audience) || !mail.title} onClick={() => (!mail.audience ? go('mail', () => raw('POST', '/v1/server/mail', mailBody()), 'Sent') : confirmThen(`Send to the whole audience ${mail.audience}${count != null ? ` (${count} accounts)` : ''}?`, () => go('mail', () => raw('POST', '/v1/server/mail', mailBody()), 'Sent')))}>Send</Button>
          </div>
        </Op>
      </div>
      <Card><CardContent className="pt-6">
        <SubHead title="Grant limits" count={listOf(limits.data, 'limits').length} />
        <p className="mb-3 text-sm text-muted-foreground">Caps how much of one item or currency any account can receive from server grants inside a window — a brake on a leaked server key or a runaway script, not on players.</p>
        {limits.isError && <Alert variant="destructive"><AlertDescription>{String(limits.error)}</AlertDescription></Alert>}
        <div className="mb-3 flex flex-wrap items-end gap-2">
          <Select value={S(lim.kind)} onValueChange={(v) => setLim((x) => ({ ...x, kind: v }))}><SelectTrigger className="w-32"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="item">item</SelectItem><SelectItem value="currency">currency</SelectItem></SelectContent></Select>
          <Input className="w-48 font-mono" placeholder="item / currency id" value={S(lim.key)} onChange={bind(setLim, 'key')} />
          <Input className="w-28" type="number" title="max per account" value={S(lim.max_per_account)} onChange={bind(setLim, 'max_per_account')} />
          <Input className="w-32" type="number" title="window seconds" value={S(lim.window_secs)} onChange={bind(setLim, 'window_secs')} />
          <Button size="sm" disabled={pending || !lim.key} onClick={() => run(() => raw('PUT', '/v1/server/grant_limits', { ...lim, max_per_account: Number(lim.max_per_account), window_secs: Number(lim.window_secs) }), 'Limit saved')}>Set limit</Button>
        </div>
        <DataTable id="grant-limits" rows={listOf(limits.data, 'limits')} loading={limits.isPending} empty="No limits — server grants are uncapped" cols={[{ key: 'kind' }, { key: 'key', mono: true }, { key: 'max_per_account', label: 'max per account' }, { key: 'window_secs', label: 'window', fmt: (r) => (Number(r.window_secs) >= 3600 ? `${Number(r.window_secs) / 3600} h` : `${r.window_secs} s`) }]}
          actions={(r) => <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen('Delete this limit?', () => run(() => raw('DELETE', `/v1/server/grant_limits/${r.kind}/${encodeURIComponent(String(r.key))}`), 'Deleted'))}>×</Button>} />
      </CardContent></Card>
    </div>
  )
}
