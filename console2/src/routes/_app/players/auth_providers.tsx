import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
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
import { Textarea } from '@/components/ui/textarea'

/** Identity providers: each one is a login button in the client (`/v1/auth/{name}`). Secrets are stored sealed and never shown again. */
export const Route = createFileRoute('/_app/players/auth_providers')({ component: Providers })

const KINDS: [string, string][] = [['oidc', 'OpenID Connect (discovery by issuer)'], ['oauth2', 'OAuth 2 (explicit endpoints)'], ['steam', 'Steam'], ['epic', 'Epic'], ['xbox', 'Xbox Live'], ['psn', 'PlayStation Network'], ['nintendo', 'Nintendo'], ['apple', 'Apple'], ['google', 'Google Play Games'], ['gamecenter', 'Game Center'], ['facebook', 'Facebook'], ['discord', 'Discord'], ['twitch', 'Twitch']]
const BLANK: Row = { name: '', kind: 'oidc', display_name: '', enabled: true, allow_signup: true, issuer: '', authorize_url: '', token_url: '', userinfo_url: '', jwks_url: '', client_id: '', auth_style: 'body', claims: {}, scopes: [], client_ids: [] }

function Providers() {
  const q = useQuery({ queryKey: ['providers'], queryFn: ({ signal }) => raw<Row>('GET', '/v1/admin/auth/providers', undefined, { signal }) })
  const provs = listOf(q.data, 'providers')
  const [open, setOpen] = useState(false)
  const [isNew, setIsNew] = useState(false)
  const [p, setP] = useState<Row>(BLANK)
  const [scopes, setScopes] = useState(''); const [clientIds, setClientIds] = useState(''); const [claims, setClaims] = useState('{}'); const [secret, setSecret] = useState('')
  const [test, setTest] = useState<Row | null>(null)
  const set = (k: string, v: unknown) => setP((x) => ({ ...x, [k]: v }))
  const { run, pending } = useAct([['providers']], () => setOpen(false))
  function edit(x: Row | null) {
    setIsNew(!x); const v = x ? { ...x } : { ...BLANK }; setP(v)
    setScopes(((v.scopes ?? []) as string[]).join(' ')); setClientIds(((v.client_ids ?? []) as string[]).join(', ')); setClaims(JSON.stringify(v.claims ?? {}, null, 2)); setSecret(''); setTest(null); setOpen(true)
  }
  function save() {
    let cl: unknown; try { cl = JSON.parse(claims || '{}') } catch { return toast.error('Claim mapping is not valid JSON') }
    const { name, has_client_secret: _h, ...rest } = p
    const b: Row = { ...rest, scopes: scopes.split(/\s+/).filter(Boolean), client_ids: clientIds.split(',').map((s) => s.trim()).filter(Boolean), claims: cl }
    if (secret) b.client_secret = secret
    run(() => raw('PUT', `/v1/admin/auth/providers/${encodeURIComponent(String(name))}`, b), 'Provider saved')
  }
  const runTest = async (name: string) => { try { const r = await raw<Row>('POST', `/v1/admin/auth/providers/${encodeURIComponent(name)}/test`); setTest(r); r.ok === false ? toast.error('Test failed') : toast.success('Test ok') } catch (e) { toast.error(String(e)) } }
  const web = p.kind === 'oidc' || p.kind === 'oauth2'

  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Players', to: '/players' }, { title: 'Identity providers' }]} />
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Identity providers</h1><p className="mt-1 text-xs text-muted-foreground">{provs.filter((x) => x.enabled).length} enabled of {provs.length} · {String(q.data?.open_flows ?? 0)} open login flows. Each provider is a login button in the client: clients call <code>/v1/auth/{'{name}'}</code>. Secrets are stored sealed and never shown again.</p></div>
        <Button size="sm" onClick={() => edit(null)}>Add provider</Button>
      </div>
      <Card><CardContent className="pt-6">
        <DataTable id="providers" rows={provs} loading={q.isPending} empty="No providers — device / e-mail login only"
          cols={[{ key: 'name', mono: true, fmt: (x) => <button type="button" className="font-mono hover:underline" onClick={() => edit(x)}>{x.name}{x.display_name ? <span className="ml-1 font-sans text-muted-foreground">— {x.display_name}</span> : null}</button> }, { key: 'kind' }, { key: 'issuer', label: 'issuer / authorize URL', mono: true, dim: true, fmt: (x) => x.issuer || x.authorize_url || '' }, { key: 'allow_signup', label: 'signup', fmt: (x) => (x.allow_signup ? 'yes' : 'link only') }, { key: 'has_client_secret', label: 'secret', fmt: (x) => (x.has_client_secret ? 'set' : '—') }, { key: 'enabled', fmt: (x) => (x.enabled ? 'on' : <span className="text-muted-foreground">off</span>) }]}
          actions={(x) => (
            <span className="flex justify-end gap-1">
              <Button size="sm" variant="ghost" onClick={() => edit(x)}>Edit</Button>
              <Button size="sm" variant="ghost" onClick={() => void runTest(String(x.name))}>Test</Button>
              <Button size="sm" variant="ghost" onClick={() => run(() => raw('PUT', `/v1/admin/auth/providers/${encodeURIComponent(String(x.name))}`, { ...x, enabled: !x.enabled }), x.enabled ? 'Disabled' : 'Enabled')}>{x.enabled ? 'Disable' : 'Enable'}</Button>
              <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Delete provider ${x.name}?`, () => run(() => raw('DELETE', `/v1/admin/auth/providers/${encodeURIComponent(String(x.name))}`), 'Deleted'))}>Delete</Button>
            </span>
          )} />
        {test && !open && <div className="mt-3"><JsonView value={test} className="max-h-52" /></div>}
      </CardContent></Card>

      <FormDialog open={open} onOpenChange={setOpen} title={isNew ? 'New identity provider' : `Provider ${p.name}`} pending={pending} disabled={!p.name} onSubmit={save}>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Name (URL slug)"><Input className="font-mono" value={String(p.name ?? '')} disabled={!isNew} placeholder="steam, google, studio_sso" onChange={(e) => set('name', e.target.value)} /></Field>
          <Field label="Display name"><Input value={String(p.display_name ?? '')} onChange={(e) => set('display_name', e.target.value)} /></Field>
        </div>
        <Field label="Kind"><Select value={String(p.kind)} onValueChange={(v) => set('kind', v)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent>{KINDS.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}</SelectContent></Select></Field>
        <div className="flex flex-wrap gap-4 text-sm"><label className="flex items-center gap-2"><Checkbox checked={!!p.enabled} onCheckedChange={(v) => set('enabled', v === true)} /> enabled</label><label className="flex items-center gap-2"><Checkbox checked={!!p.allow_signup} onCheckedChange={(v) => set('allow_signup', v === true)} /> allow new accounts (else link to existing only)</label></div>
        {web ? (
          <>
            <Field label="Issuer (OIDC discovery)"><Input className="font-mono" value={String(p.issuer ?? '')} placeholder="https://accounts.example.com" onChange={(e) => set('issuer', e.target.value)} /></Field>
            {p.kind === 'oauth2' && <Field label="Authorize / token / userinfo / JWKS URLs"><div className="space-y-1.5">{(['authorize_url', 'token_url', 'userinfo_url', 'jwks_url'] as const).map((k) => <Input key={k} className="font-mono" value={String(p[k] ?? '')} placeholder={k.replace('_url', '')} onChange={(e) => set(k, e.target.value)} />)}</div></Field>}
            <div className="grid grid-cols-2 gap-3">
              <Field label="Client id"><Input className="font-mono" value={String(p.client_id ?? '')} onChange={(e) => set('client_id', e.target.value)} /></Field>
              <Field label={`Client secret ${p.has_client_secret ? '(set — leave empty to keep)' : ''}`}><Input type="password" autoComplete="new-password" value={secret} onChange={(e) => setSecret(e.target.value)} /></Field>
              <Field label="Scopes (space-separated)"><Input className="font-mono" value={scopes} placeholder="openid email profile" onChange={(e) => setScopes(e.target.value)} /></Field>
              <Field label="Client auth"><Select value={String(p.auth_style ?? 'body')} onValueChange={(v) => set('auth_style', v)}><SelectTrigger><SelectValue /></SelectTrigger><SelectContent><SelectItem value="body">client_secret_post (body)</SelectItem><SelectItem value="basic">client_secret_basic (header)</SelectItem></SelectContent></Select></Field>
            </div>
            <Field label="Claim mapping (JSON: our field → their claim)"><Textarea className="min-h-28 font-mono text-xs" value={claims} onChange={(e) => setClaims(e.target.value)} /></Field>
          </>
        ) : (
          <>
            <Field label="Accepted client / app ids (comma-separated)"><Input className="font-mono" value={clientIds} onChange={(e) => setClientIds(e.target.value)} /></Field>
            <Field label={`Platform secret / API key ${p.has_client_secret ? '(set — leave empty to keep)' : ''}`}><Input type="password" autoComplete="new-password" value={secret} onChange={(e) => setSecret(e.target.value)} /></Field>
          </>
        )}
        {test && <JsonView value={test} className="max-h-40" />}
        {!isNew && <Button type="button" size="sm" variant="outline" onClick={() => void runTest(String(p.name))}>Test</Button>}
      </FormDialog>
    </div>
  )
}
