import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { auth } from '@/lib/auth'
import { useAct, confirmThen } from '@/lib/act'
import { ts } from '@/lib/format'
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
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'

const TABS: [string, string][] = [
  ['account', 'Account'],
  ['wallet', 'Wallet & inventory'],
  ['heroes', 'Heroes'],
  ['storage', 'Storage'],
  ['social', 'Social'],
  ['sessions', 'Sessions & devices'],
  ['bans', 'Bans & reports'],
  ['security', 'Security'],
  ['notes', 'Notes'],
  ['timeline', 'Timeline'],
]
type Dlg = '' | 'ban' | 'kick' | 'note' | 'sanction' | 'edit' | 'limits'

export const Route = createFileRoute('/_app/players/accounts/$id')({
  validateSearch: (s: Record<string, unknown>): { tab?: string } => ({ tab: typeof s.tab === 'string' ? s.tab : undefined }),
  component: AccountPage,
})

interface Ctx { id: string; A: string; S: string; run: (fn: () => Promise<unknown>, ok: string) => void }

function AccountPage() {
  const { id } = Route.useParams()
  const { tab = 'account' } = Route.useSearch()
  const nav = useNavigate({ from: Route.fullPath })
  const A = `/v1/admin/accounts/${id}`
  const S = `/v1/server/accounts/${id}`
  const acctQ = useQuery({ queryKey: ['acct', id], queryFn: ({ signal }) => raw<Row>('GET', A, undefined, { signal }) })
  const banQ = useSub<Row>(`${A}/ban`)
  const [dlg, setDlg] = useState<Dlg>('')
  const [form, setForm] = useState<Row>({})
  const [limitsText, setLimitsText] = useState('')
  const { run, pending } = useAct([['acct', id], ['sub']], () => setDlg(''))
  const ctx: Ctx = { id, A, S, run }

  const acct = acctQ.data
  const a = (acct?.account ?? {}) as Row
  const status = (acct?.status ?? {}) as Row
  const online = ((acct?.online as unknown[] | undefined)?.length ?? 0) > 0
  const banned = !!banQ.data
  const state = status.deleted_at ? 'deleted' : status.disabled_at ? 'disabled' : banned ? 'banned' : 'active'
  const name = a.display_name || a.username || id

  function open(k: Dlg) {
    setDlg(k)
    if (k === 'ban') setForm({ reason: '', reason_code: '', days: '', shadow: false, message: '', devices: false })
    else if (k === 'kick') setForm({ reason: 'kicked by operator' })
    else if (k === 'note') setForm({ text: '' })
    else if (k === 'sanction') setForm({ kind: 'mute', reason: '', until: '', scope: '' })
    else if (k === 'edit') setForm({ display_name: a.display_name ?? '', lang: a.lang ?? '', metadata: JSON.stringify(a.metadata ?? {}, null, 2) })
  }
  const set = (k: string, v: unknown) => setForm((f) => ({ ...f, [k]: v }))

  async function exportJson() {
    const r = await fetch((auth.base || '') + `${A}/export`, { headers: { 'x-musubi-server-key': auth.key } })
    if (!r.ok) return toast.error(`Export failed: ${r.status}`)
    const u = URL.createObjectURL(await r.blob())
    const el = document.createElement('a')
    el.href = u
    el.download = `account-${id}.json`
    el.click()
    URL.revokeObjectURL(u)
  }

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Players', to: '/players' }, { title: 'Accounts', to: '/players/accounts' }, { title: String(name) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-xl font-semibold">{acctQ.isPending ? <Skeleton className="h-7 w-48" /> : name}</h1>
          <p className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">
            {id}
            {acct && <Badge variant={state === 'active' ? 'secondary' : 'destructive'}>{state}</Badge>}
            {online && <Badge variant="outline">online</Badge>}
          </p>
        </div>
        {acct && (
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" onClick={() => open('edit')}>Edit</Button>
            <Button size="sm" variant="outline" onClick={() => open('note')}>Add note</Button>
            <Button size="sm" variant="outline" disabled={!online} onClick={() => open('kick')}>Kick</Button>
            {banned ? (
              <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${A}/unban`), 'Unbanned')}>Unban</Button>
            ) : (
              <Button size="sm" variant="outline" onClick={() => open('ban')}>Ban</Button>
            )}
            <Button size="sm" variant="outline" onClick={() => open('sanction')}>Sanction</Button>
            <Button size="sm" variant="outline" onClick={() => void exportJson()}>Export</Button>
            {status.deleted_at ? (
              <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', `${A}/restore`), 'Restored')}>Restore</Button>
            ) : (
              <Button size="sm" variant="destructive" onClick={() => confirmThen('Soft-delete this account? Data rights erasure runs on the retention schedule.', () => run(() => raw('DELETE', A), 'Deleted'))}>Delete</Button>
            )}
          </div>
        )}
      </div>

      {acctQ.isError && <Alert variant="destructive"><AlertDescription>{String(acctQ.error)}</AlertDescription></Alert>}
      {acct && (
        <Tabs value={tab} onValueChange={(v) => void nav({ search: { tab: v === 'account' ? undefined : v }, replace: true })}>
          <div className="overflow-x-auto"><TabsList className="w-max">{TABS.map(([v, l]) => <TabsTrigger key={v} value={v}>{l}</TabsTrigger>)}</TabsList></div>
          <Card className="mt-3"><CardContent className="pt-6">
            <TabsContent value="account"><AccountTab ctx={ctx} a={a} /></TabsContent>
            <TabsContent value="wallet"><WalletTab ctx={ctx} onLimits={(cur) => { setLimitsText(JSON.stringify(cur ?? { daily_grant: null }, null, 2)); setDlg('limits') }} /></TabsContent>
            <TabsContent value="heroes"><HeroesTab ctx={ctx} /></TabsContent>
            <TabsContent value="storage"><StorageTab ctx={ctx} /></TabsContent>
            <TabsContent value="social"><SocialTab ctx={ctx} /></TabsContent>
            <TabsContent value="sessions"><SessionsTab ctx={ctx} /></TabsContent>
            <TabsContent value="bans"><BansTab ctx={ctx} ban={banQ.data ?? null} /></TabsContent>
            <TabsContent value="security"><SecurityTab ctx={ctx} /></TabsContent>
            <TabsContent value="notes"><NotesTab ctx={ctx} onAdd={() => open('note')} /></TabsContent>
            <TabsContent value="timeline"><TimelineTab ctx={ctx} /></TabsContent>
          </CardContent></Card>
        </Tabs>
      )}

      <FormDialog open={dlg === 'ban'} onOpenChange={(o) => !o && setDlg('')} title="Ban player" submit="Ban" destructive pending={pending}
        onSubmit={() => run(async () => {
          await raw('POST', `${A}/ban`, { reason: form.reason, reason_code: form.reason_code || undefined, until_secs: form.days ? Math.round(Number(form.days) * 86400) : undefined, shadow: !!form.shadow, message: form.message || undefined })
          if (form.devices) await raw('POST', `${A}/ban/devices`, { reason: form.reason })
        }, 'Banned')}>
        <Field label="Reason (shown to the player)"><Input value={form.reason ?? ''} onChange={(e) => set('reason', e.target.value)} /></Field>
        <Field label="Reason code"><Input value={form.reason_code ?? ''} placeholder="cheating, toxicity, …" onChange={(e) => set('reason_code', e.target.value)} /></Field>
        <Field label="Duration, days (empty = permanent)"><Input type="number" min={0} step={0.5} value={form.days ?? ''} onChange={(e) => set('days', e.target.value)} /></Field>
        <Field label="Internal message"><Input value={form.message ?? ''} onChange={(e) => set('message', e.target.value)} /></Field>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!form.shadow} onCheckedChange={(v) => set('shadow', v === true)} /> shadow ban (player is not told)</label>
        <label className="flex items-center gap-2 text-sm"><Checkbox checked={!!form.devices} onCheckedChange={(v) => set('devices', v === true)} /> also ban known devices</label>
      </FormDialog>
      <FormDialog open={dlg === 'kick'} onOpenChange={(o) => !o && setDlg('')} title="Kick player" submit="Kick" pending={pending} onSubmit={() => run(() => raw('POST', `${A}/kick`, { reason: form.reason }), 'Kicked')}>
        <Field label="Reason"><Input value={form.reason ?? ''} onChange={(e) => set('reason', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'note'} onOpenChange={(o) => !o && setDlg('')} title="Add note" submit="Add" pending={pending} disabled={!form.text} onSubmit={() => run(() => raw('POST', `${A}/notes`, { text: form.text }), 'Note added')}>
        <Field label="Note"><Textarea className="min-h-32" value={form.text ?? ''} onChange={(e) => set('text', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'sanction'} onOpenChange={(o) => !o && setDlg('')} title="Sanction" submit="Apply" pending={pending}
        onSubmit={() => run(() => raw('POST', `${A}/sanctions`, { kind: form.kind, scope: form.scope || undefined, reason: form.reason, until: form.until || undefined }), 'Sanctioned')}>
        <Field label="Kind">
          <Select value={String(form.kind ?? 'mute')} onValueChange={(v) => set('kind', v)}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>{['mute', 'chat_restricted', 'matchmaking_restricted', 'trade_restricted', 'ugc_restricted', 'warning'].map((k) => <SelectItem key={k} value={k}>{k}</SelectItem>)}</SelectContent>
          </Select>
        </Field>
        <Field label="Scope (optional)"><Input value={form.scope ?? ''} placeholder="channel, queue, …" onChange={(e) => set('scope', e.target.value)} /></Field>
        <Field label="Reason"><Input value={form.reason ?? ''} onChange={(e) => set('reason', e.target.value)} /></Field>
        <Field label="Until (ISO, empty = permanent)"><Input value={form.until ?? ''} placeholder="2026-12-31T00:00:00Z" onChange={(e) => set('until', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'edit'} onOpenChange={(o) => !o && setDlg('')} title="Edit account" pending={pending}
        onSubmit={() => {
          let metadata: unknown
          try { metadata = JSON.parse(String(form.metadata || '{}')) } catch { return toast.error('Metadata is not valid JSON') }
          run(() => raw('PATCH', A, { display_name: form.display_name || null, lang: form.lang || undefined, metadata }), 'Saved')
        }}>
        <Field label="Display name"><Input value={form.display_name ?? ''} onChange={(e) => set('display_name', e.target.value)} /></Field>
        <Field label="Language"><Input value={form.lang ?? ''} onChange={(e) => set('lang', e.target.value)} /></Field>
        <Field label="Metadata (JSON)"><Textarea className="min-h-40 font-mono text-xs" value={form.metadata ?? ''} onChange={(e) => set('metadata', e.target.value)} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'limits'} onOpenChange={(o) => !o && setDlg('')} title="Grant limit override" pending={pending}
        onSubmit={() => {
          let body: unknown
          try { body = JSON.parse(limitsText) } catch { return toast.error('Not valid JSON') }
          run(() => raw('PUT', `${A}/limits`, body), 'Limits saved')
        }}>
        <Field label="Limits (JSON)"><Textarea className="min-h-52 font-mono text-xs" value={limitsText} onChange={(e) => setLimitsText(e.target.value)} /></Field>
      </FormDialog>
    </div>
  )
}

function Json({ q, empty }: { q: { data?: unknown; isPending: boolean }; empty?: string }) {
  if (q.isPending) return <Skeleton className="h-16 w-full" />
  return <JsonView value={q.data} empty={empty} />
}

function AccountTab({ ctx: { id, A, S, run }, a }: { ctx: Ctx; a: Row }) {
  const geo = useSub<Row>(`${S}/geo`)
  const presence = useSub<Row>(`/v1/server/presence/${id}`)
  const experiments = useSub(`/v1/server/experiments/of/${id}`)
  const audiences = useSub(`/v1/server/audiences/of/${id}`)
  const attribution = useSub(`${S}/attribution`)
  const idents = (a.identities ?? []) as Row[]
  const g = (geo.data ?? {}) as Row
  const p = (presence.data ?? {}) as Row
  return (
    <>
      <KvGrid>
        <Kv k="Account ID" v={a.id} mono copy />
        <Kv k="Username" v={a.username} copy />
        <Kv k="Display name" v={a.display_name} />
        <Kv k="Created" v={ts(a.created_at)} />
        <Kv k="Updated" v={ts(a.updated_at)} />
        <Kv k="Language" v={a.lang} />
        <Kv k="Timezone" v={a.timezone} />
        <Kv k="Location" v={a.location} />
        <Kv k="Country" v={g.country} />
        <Kv k="City" v={g.city ? `${g.city}${g.region ? ', ' + g.region : ''}` : undefined} />
        <Kv k="Last IP" v={g.ip ?? g.last_ip} mono />
        <Kv k="Presence" v={p.status ?? (presence.data ? 'online' : undefined)} />
        <Kv k="Last seen" v={p.last_seen_at ? ts(p.last_seen_at) : undefined} />
      </KvGrid>
      <SubHead title="Identities" count={idents.length} />
      <DataTable id="acct-idents" tools={false} rows={idents} empty="No linked identities"
        cols={[{ key: 'provider' }, { key: 'subject', mono: true }, { key: 'verified', fmt: (r) => (r.verified ? 'yes' : 'no') }]}
        actions={(i) => <Button size="sm" variant="ghost" onClick={() => confirmThen(`Unlink ${i.provider}?`, () => run(() => raw('DELETE', `${A}/identities/${encodeURIComponent(i.provider)}/${encodeURIComponent(i.subject)}`), 'Unlinked'))}>Unlink</Button>} />
      <SubHead title="Experiments" count={listOf(experiments.data).length} />
      <DataTable id="acct-exp" tools={false} rows={listOf(experiments.data)} loading={experiments.isPending} empty="Not in any experiment"
        cols={[{ key: 'experiment_id', label: 'experiment', mono: true }, { key: 'variant' }, { key: 'assigned_at', label: 'assigned', dim: true, fmt: (r) => ts(r.assigned_at) }]} />
      <SubHead title="Audiences" count={listOf(audiences.data).length} />
      <p className="text-sm">{listOf(audiences.data).length ? listOf(audiences.data).map((x) => String(x.name ?? x.id ?? x)).join(', ') : <span className="text-muted-foreground">Not in any audience</span>}</p>
      <SubHead title="Attribution" /><Json q={attribution} empty="No attribution data yet" />
      <SubHead title="Metadata" /><JsonView value={a.metadata ?? {}} />
    </>
  )
}

function WalletTab({ ctx: { A, S, run }, onLimits }: { ctx: Ctx; onLimits: (cur: unknown) => void }) {
  const wallet = useSub(`${S}/wallet`)
  const inventory = useSub(`${S}/inventory`)
  const ledger = useSub(`${S}/ledger?limit=100`)
  const grants = useSub(`${A}/grants`)
  const limits = useSub(`${A}/limits`)
  const vip = useSub(`${S}/vip`)
  const ent = useSub(`${S}/entitlements`)
  return (
    <>
      <SubHead title="Wallet" />
      <DataTable id="acct-wallet" tools={false} rows={listOf(wallet.data, 'balances')} loading={wallet.isPending} empty="No balances yet" cols={[{ key: 'currency', mono: true }, { key: 'balance' }]} />
      <SubHead title="Ledger" count={listOf(ledger.data, 'entries').length} />
      <DataTable id="acct-ledger" rows={listOf(ledger.data, 'entries')} loading={ledger.isPending} empty="No transactions yet"
        cols={[{ key: 'created_at', label: 'at', dim: true, fmt: (r) => ts(r.created_at ?? r.at) }, { key: 'currency', mono: true }, { key: 'delta', fmt: (r) => (Number(r.delta) > 0 ? `+${r.delta}` : String(r.delta ?? '')) }, { key: 'balance' }, { key: 'reason' }, { key: 'ref', mono: true }]} />
      <SubHead title="Inventory" count={listOf(inventory.data).length} />
      <DataTable id="acct-inv" rows={listOf(inventory.data)} loading={inventory.isPending} empty="Empty inventory"
        cols={[{ key: 'id', mono: true }, { key: 'item_id', label: 'item', mono: true }, { key: 'quantity', label: 'qty' }, { key: 'slot' }, { key: 'bound' }, { key: 'acquired_at', dim: true, fmt: (r) => ts(r.acquired_at ?? r.created_at) }]} />
      <SubHead title="Entitlements" />
      <DataTable id="acct-ent" tools={false} rows={listOf(ent.data)} loading={ent.isPending} empty="No entitlements"
        cols={[{ key: 'provider' }, { key: 'external_id', mono: true }, { key: 'source' }, { key: 'active' }, { key: 'granted_at', dim: true, fmt: (r) => ts(r.granted_at) }]} />
      <SubHead title="VIP programs" />
      <DataTable id="acct-vip" tools={false} rows={listOf(vip.data, 'programs')} loading={vip.isPending} empty="Not enrolled" cols={[{ key: 'program' }, { key: 'tier' }, { key: 'points' }, { key: 'expires_at', dim: true }]} />
      <SubHead title="Grants" count={listOf(grants.data).length} />
      <DataTable id="acct-grants" rows={listOf(grants.data)} loading={grants.isPending} empty="No operator grants"
        cols={[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'amount' }, { key: 'actor' }, { key: 'reason' }, { key: 'created_at', dim: true, fmt: (r) => ts(r.created_at) }]} />
      <SubHead title="Grant limit override">
        <Button size="sm" variant="outline" onClick={() => onLimits(limits.data)}>Edit</Button>
        {!!limits.data && <Button size="sm" variant="ghost" onClick={() => run(() => raw('DELETE', `${A}/limits`), 'Override removed')}>Remove</Button>}
      </SubHead>
      <Json q={limits} empty="No override — global limits apply" />
    </>
  )
}

function HeroesTab({ ctx: { id, S } }: { ctx: Ctx }) {
  const heroes = useSub(`${S}/heroes`)
  const stats = useSub(`/v1/server/stats/${id}`)
  const effects = useSub(`/v1/server/effects/${id}`)
  const tutorials = useSub(`${S}/tutorials`)
  return (
    <>
      <SubHead title="Heroes" count={listOf(heroes.data).length} />
      <DataTable id="acct-heroes" rows={listOf(heroes.data)} loading={heroes.isPending} empty="No heroes"
        cols={[{ key: 'id', mono: true }, { key: 'class_id', label: 'class' }, { key: 'name' }, { key: 'level' }, { key: 'loadout', fmt: (r) => (r.loadout ? JSON.stringify(r.loadout) : '') }, { key: 'created_at', dim: true, fmt: (r) => ts(r.created_at) }]} />
      <SubHead title="Stats" />
      <DataTable id="acct-stats" tools={false} rows={listOf(stats.data)} loading={stats.isPending} empty="No stats" cols={[{ key: 'stat', mono: true }, { key: 'value' }, { key: 'updated_at', dim: true, fmt: (r) => ts(r.updated_at) }]} />
      <SubHead title="Active effects" />
      <DataTable id="acct-effects" tools={false} rows={listOf(effects.data)} loading={effects.isPending} empty="No active effects" cols={[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'value' }, { key: 'expires_at', dim: true, fmt: (r) => ts(r.expires_at) }]} />
      <SubHead title="Tutorials" /><Json q={tutorials} empty="No tutorial progress" />
    </>
  )
}

function StorageTab({ ctx: { S } }: { ctx: Ctx }) {
  const files = useSub(`${S}/files`)
  const baggage = useSub(`${S}/baggage`)
  const props = useSub(`${S}/event_properties`)
  const clock = useSub(`${S}/clock`)
  return (
    <>
      <SubHead title="Files" count={listOf(files.data).length} />
      <DataTable id="acct-files" rows={listOf(files.data)} loading={files.isPending} empty="No files" cols={[{ key: 'key', mono: true }, { key: 'size' }, { key: 'mime' }, { key: 'updated_at', dim: true, fmt: (r) => ts(r.updated_at) }]} />
      <SubHead title="Baggage" /><Json q={baggage} />
      <SubHead title="Event properties" /><Json q={props} />
      <SubHead title="Clock" /><Json q={clock} />
    </>
  )
}

function SocialTab({ ctx: { A, S } }: { ctx: Ctx }) {
  const privacy = useSub<Row>(`${S}/privacy`)
  const referrals = useSub(`${S}/referrals`)
  const donations = useSub(`${S}/donations`)
  const steam = useSub(`${A}/steam`)
  const dev = useSub(`${A}/developer`)
  const pv = (privacy.data ?? {}) as Row
  return (
    <>
      <KvGrid><Kv k="Profile visibility" v={pv.profile} /><Kv k="Inventory visibility" v={pv.inventory} /><Kv k="Show online" v={pv.show_online} /></KvGrid>
      <SubHead title="Referrals" /><Json q={referrals} empty="No referrals" />
      <SubHead title="Donations" />
      <DataTable id="acct-donations" rows={listOf(donations.data)} loading={donations.isPending} empty="No donations"
        cols={[{ key: 'id', mono: true }, { key: 'from', mono: true }, { key: 'amount' }, { key: 'currency' }, { key: 'created_at', dim: true, fmt: (r) => ts(r.created_at) }]} />
      <SubHead title="Steam" /><Json q={steam} empty="Not linked" />
      <SubHead title="Developer" /><Json q={dev} empty="Not a developer account" />
    </>
  )
}

function SessionsTab({ ctx: { A, run } }: { ctx: Ctx }) {
  const sessions = useSub(`${A}/sessions`)
  const devices = useSub(`${A}/devices`)
  const ips = useSub(`${A}/ips`)
  return (
    <>
      <SubHead title="Sessions" count={listOf(sessions.data, 'sessions').length} />
      <DataTable id="acct-sessions" rows={listOf(sessions.data, 'sessions')} loading={sessions.isPending} empty="No sessions"
        cols={[{ key: 'id', mono: true }, { key: 'device_id', mono: true }, { key: 'ip', mono: true }, { key: 'created_at', dim: true, fmt: (r) => ts(r.created_at) }, { key: 'expires_at', dim: true, fmt: (r) => ts(r.expires_at) }, { key: 'revoked_at', dim: true, fmt: (r) => ts(r.revoked_at) }]}
        actions={(s) => (s.revoked_at ? null : <Button size="sm" variant="ghost" onClick={() => run(() => raw('DELETE', `/v1/admin/sessions/${s.id}`), 'Session revoked')}>Revoke</Button>)} />
      <SubHead title="Devices" count={listOf(devices.data).length}>
        {listOf(devices.data).length > 0 && <Button size="sm" variant="destructive" onClick={() => confirmThen('Ban all devices of this account?', () => run(() => raw('POST', `${A}/ban/devices`, { reason: 'operator' }), 'Devices banned'))}>Ban all devices</Button>}
      </SubHead>
      <DataTable id="acct-devices" rows={listOf(devices.data)} loading={devices.isPending} empty="No devices"
        cols={[{ key: 'device_id', mono: true }, { key: 'logins' }, { key: 'banned' }, { key: 'last_ip', mono: true }, { key: 'user_agent', dim: true }, { key: 'first_seen_at', dim: true, fmt: (r) => ts(r.first_seen_at) }, { key: 'last_seen_at', dim: true, fmt: (r) => ts(r.last_seen_at) }]} />
      <SubHead title="IP history" count={listOf(ips.data).length} />
      <DataTable id="acct-ips" rows={listOf(ips.data)} loading={ips.isPending} empty="No logins recorded"
        cols={[{ key: 'ip', mono: true }, { key: 'country' }, { key: 'logins' }, { key: 'first_seen_at', dim: true, fmt: (r) => ts(r.first_seen_at) }, { key: 'last_seen_at', dim: true, fmt: (r) => ts(r.last_seen_at) }]} />
    </>
  )
}

function BansTab({ ctx: { id, A, run }, ban }: { ctx: Ctx; ban: Row | null }) {
  const sanctions = useSub(`${A}/sanctions`)
  const reports = useSub(`${A}/reports`)
  const anticheat = useSub(`${A}/anticheat`)
  const crashes = useSub(`${A}/crashes`)
  const fraud = useSub(`/v1/server/economy/fraud/${id}`)
  return (
    <>
      <SubHead title="Ban" />
      {ban ? (
        <KvGrid><Kv k="Reason" v={ban.reason} /><Kv k="Code" v={ban.reason_code} /><Kv k="Until" v={ban.until ? ts(ban.until) : 'permanent'} /><Kv k="Shadow" v={ban.shadow ? 'yes' : 'no'} /><Kv k="Since" v={ts(ban.created_at)} /><Kv k="By" v={ban.actor} /></KvGrid>
      ) : <p className="text-sm text-muted-foreground">Not banned</p>}
      <SubHead title="Sanctions" count={listOf(sanctions.data).length} />
      <DataTable id="acct-sanctions" rows={listOf(sanctions.data)} loading={sanctions.isPending} empty="No sanctions"
        cols={[{ key: 'id', mono: true }, { key: 'kind' }, { key: 'scope' }, { key: 'reason' }, { key: 'until', fmt: (r) => (r.until ? ts(r.until) : 'permanent') }, { key: 'created_at', dim: true, fmt: (r) => ts(r.created_at) }]}
        actions={(s) => (s.revoked_at ? null : <Button size="sm" variant="ghost" onClick={() => run(() => raw('DELETE', `/v1/admin/sanctions/${s.id}`), 'Sanction lifted')}>Lift</Button>)} />
      <SubHead title="Reports about this player" count={listOf(reports.data).length} />
      <DataTable id="acct-reports" rows={listOf(reports.data)} loading={reports.isPending} empty="No reports" rowTo={(r) => `/players/reports/${r.id}`}
        cols={[{ key: 'id', mono: true }, { key: 'category' }, { key: 'reporter_id', mono: true }, { key: 'status' }, { key: 'decision' }, { key: 'created_at', dim: true, fmt: (r) => ts(r.created_at) }]} />
      <SubHead title="Anti-cheat" /><Json q={anticheat} empty="No anti-cheat flags" />
      <SubHead title="Economy fraud" /><Json q={fraud} empty="No fraud flags" />
      <SubHead title="Crashes" count={listOf(crashes.data).length} />
      <DataTable id="acct-crashes" rows={listOf(crashes.data)} loading={crashes.isPending} empty="No crashes" rowTo={(r) => (r.fingerprint ? `/players/crashes/${r.fingerprint}` : null)}
        cols={[{ key: 'fingerprint', mono: true }, { key: 'version' }, { key: 'at', dim: true, fmt: (r) => ts(r.at ?? r.created_at) }]} />
    </>
  )
}

function SecurityTab({ ctx: { A } }: { ctx: Ctx }) {
  const sec = useSub(`${A}/security_events?limit=100`)
  const consents = useSub(`${A}/consents`)
  const pending = listOf(consents.data, 'pending')
  return (
    <>
      <SubHead title="Security events" count={listOf(sec.data).length} />
      <DataTable id="acct-sec" rows={listOf(sec.data)} loading={sec.isPending} empty="No security events"
        cols={[{ key: 'at', dim: true, fmt: (r) => ts(r.at) }, { key: 'kind' }, { key: 'ip', mono: true }, { key: 'summary', fmt: (r) => r.summary ?? (r.details ? JSON.stringify(r.details) : '') }]} />
      <SubHead title="Consents" count={listOf(consents.data, 'consents').length} />
      <DataTable id="acct-consents" tools={false} rows={listOf(consents.data, 'consents')} loading={consents.isPending} empty="No consents recorded"
        cols={[{ key: 'policy' }, { key: 'version' }, { key: 'granted' }, { key: 'at', dim: true, fmt: (r) => ts(r.at ?? r.granted_at) }]} />
      {pending.length > 0 && <p className="mt-2 text-sm text-muted-foreground">Pending: {pending.map((p) => String(p.policy ?? p)).join(', ')}</p>}
    </>
  )
}

function NotesTab({ ctx: { A, run }, onAdd }: { ctx: Ctx; onAdd: () => void }) {
  const notes = useSub(`${A}/notes`)
  return (
    <>
      <SubHead title="Operator notes" count={listOf(notes.data).length}><Button size="sm" onClick={onAdd}>Add note</Button></SubHead>
      <DataTable id="acct-notes" rows={listOf(notes.data)} loading={notes.isPending} empty="No notes yet"
        cols={[{ key: 'created_at', label: 'at', dim: true, fmt: (r) => ts(r.created_at) }, { key: 'author', fmt: (r) => r.author ?? r.actor }, { key: 'text', fmt: (r) => <span className="whitespace-pre-wrap">{r.text ?? r.body}</span> }]}
        actions={(n) => <Button size="sm" variant="ghost" onClick={() => run(() => raw('DELETE', `${A}/notes/${n.id}`), 'Note deleted')}>Delete</Button>} />
    </>
  )
}

function TimelineTab({ ctx: { A } }: { ctx: Ctx }) {
  const tl = useSub(`${A}/timeline?limit=200`)
  const rows = listOf(tl.data, 'items')
  return (
    <>
      <SubHead title="Timeline" count={rows.length} />
      <DataTable id="acct-timeline" rows={rows} loading={tl.isPending} empty="Nothing happened yet"
        cols={[{ key: 'at', dim: true, fmt: (r) => ts(r.at) }, { key: 'kind', mono: true }, { key: 'source', dim: true }, { key: 'summary', fmt: (r) => <span className="font-mono text-xs text-muted-foreground">{r.summary ?? (r.data ? JSON.stringify(r.data) : '')}</span> }]} />
    </>
  )
}
