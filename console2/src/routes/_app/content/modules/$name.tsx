import { useRef, useState } from 'react'
import { createFileRoute, Link, useNavigate } from '@tanstack/react-router'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { Field, FormDialog } from '@/components/form-dialog'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

/** One WASM runtime module: stored bytes (upload a new build), version history with canary rollout / promote / rollback, and the environment variables the module reads (with the ones it declares but that are unset). */
export const Route = createFileRoute('/_app/content/modules/$name')({ component: Module })

const ts = (v: unknown) => (v ? new Date(typeof v === 'number' ? v * 1000 : String(v)).toISOString().replace('T', ' ').slice(0, 19) : '—')
const kb = (n: unknown) => (n ? (Number(n) / 1024).toFixed(1) + ' KB' : '—')

function Module() {
  const { name } = Route.useParams()
  const M = `/v1/admin/runtime/modules/${encodeURIComponent(name)}`
  const nav = useNavigate()
  const all = useSub<Row>('/v1/admin/runtime/modules')
  const mod = listOf(all.data, 'stored').find((m) => m.name === name) ?? ({ name } as Row)
  const loaded = ((all.data?.loaded as unknown[] | undefined) ?? []).some((m) => (typeof m === 'string' ? m : (m as Row).name) === name)
  const vq = useSub<Row>(`${M}/versions`)
  const versions = listOf(vq.data, 'versions')
  const canary = versions.find((x) => x.state === 'canary')
  const eq = useSub<Row>(`${M}/env`)
  const envRaw: Row[] = Array.isArray(eq.data?.env) ? (eq.data!.env as Row[]) : Object.entries((eq.data?.env as Row | undefined) ?? {}).map(([key, value]) => ({ key, value }) as Row)
  const envList = envRaw.map((x) => ({ k: String(x.key ?? x.name ?? x.k ?? ''), v: String(x.value ?? x.v ?? '') }))
  const missing = (eq.data?.missing as string[] | undefined) ?? []
  const [dlg, setDlg] = useState('')
  const [f, setF] = useState<Row>({})
  const { run, pending } = useAct([['sub']], () => setDlg(''))
  const del = useAct([['sub']], () => void nav({ to: '/$section/$page', params: { section: 'content', page: 'modules' } }))
  const [env, setEnv] = useState<{ k: string; v: string }[]>([])
  const file = useRef<HTMLInputElement>(null)
  const upload = async () => {
    const f = file.current?.files?.[0]
    if (!f) return
    const buf = new Uint8Array(await f.arrayBuffer())
    run(() => raw('PUT', M, buf, { headers: { 'content-type': 'application/wasm' } }), `Module uploaded and loaded (${kb(buf.length)})`)
  }
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Content', to: '/content' }, { title: 'WASM modules', to: '/content/modules' }, { title: name }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate font-mono text-xl font-semibold">{name}</h1><p className="mt-1 text-sm text-muted-foreground">{loaded || mod.loaded ? <Badge>loaded</Badge> : <Badge variant="outline">not loaded</Badge>}{mod.sha ? <span className="ml-2 font-mono text-xs">sha {String(mod.sha).slice(0, 12)}</span> : null}{mod.size ? ` · ${kb(mod.size)}` : ''}{mod.updated_at ? ` · ${ts(mod.updated_at)}` : ''}</p></div>
        <div className="flex flex-wrap gap-2">
          <input ref={file} type="file" accept=".wasm,application/wasm" className="hidden" onChange={() => void upload()} />
          <Button size="sm" disabled={pending} onClick={() => file.current?.click()}>Upload .wasm</Button>
          <Button size="sm" variant="outline" disabled={pending || !!canary} onClick={() => { setF({ version: String(versions[0]?.version ?? ''), percent: '10', accounts: '' }); setDlg('canary') }}>Start canary</Button>
          <Button size="sm" variant="destructive" disabled={pending} onClick={() => confirmThen(`Delete module ${name}? Hooks, RPCs and schedules it registers stop immediately.`, () => del.run(() => raw('DELETE', M), 'Module deleted'))}>Delete</Button>
        </div>
      </div>
      {all.isError && <Alert variant="destructive"><AlertDescription>{String(all.error)}</AlertDescription></Alert>}
      <p className="text-sm text-muted-foreground">Uploading replaces the stored bytes and hot-loads the module on every node; the previous build stays in the version list for rollback. A canary serves the chosen version to a percentage of accounts (or a fixed list) while the rest keep the current one; promote makes it current, stop drops it.</p>
      {canary && <Alert><AlertDescription className="flex flex-wrap items-center gap-2">Canary: version <b>{String(canary.version)}</b> at {String(canary.canary_percent ?? 0)}%{Array.isArray(canary.canary_accounts) && canary.canary_accounts.length ? ` for ${canary.canary_accounts.length} accounts` : ''}<span className="ml-auto flex gap-2"><Button size="sm" disabled={pending} onClick={() => run(() => raw('POST', `${M}/canary/promote`), 'Canary promoted to current')}>Promote</Button><Button size="sm" variant="outline" disabled={pending} onClick={() => run(() => raw('DELETE', `${M}/canary`), 'Canary stopped')}>Stop</Button></span></AlertDescription></Alert>}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card><CardHeader className="pb-2"><CardTitle className="text-base">Versions</CardTitle></CardHeader><CardContent>
          {vq.isPending ? <p className="text-sm text-muted-foreground">Loading…</p> : !versions.length ? <p className="text-sm text-muted-foreground">No versions recorded yet — upload a build.</p> :
            <ul className="divide-y text-sm">{versions.map((v) => <li key={String(v.version)} className="flex flex-wrap items-center gap-2 py-2"><span className="font-mono">v{String(v.version)}</span><Badge variant={v.state === 'active' ? 'default' : v.state === 'canary' ? 'secondary' : 'outline'}>{String(v.state ?? '')}</Badge><span className="text-xs text-muted-foreground">{ts(v.created_at)}{v.sha ? ` · ${String(v.sha).slice(0, 10)}` : ''}{v.size ? ` · ${kb(v.size)}` : ''}</span>{v.state !== 'active' && <Button size="sm" variant="ghost" className="ml-auto" disabled={pending} onClick={() => confirmThen(`Roll back ${name} to v${String(v.version)}? The active build is retired and this version is loaded on every node.`, () => run(() => raw('POST', `${M}/versions/${String(v.version)}/rollback`), `Rolled back to v${String(v.version)}`))}>Roll back</Button>}</li>)}</ul>}
        </CardContent></Card>
        <Card><CardHeader className="flex-row items-center justify-between pb-2"><CardTitle className="text-base">Environment</CardTitle><Button size="sm" variant="outline" onClick={() => { setEnv(envList.length ? envList : [{ k: '', v: '' }]); setDlg('env') }}>Edit</Button></CardHeader><CardContent>
          {missing.length > 0 && <Alert variant="destructive" className="mb-3"><AlertDescription>Declared by the module but unset: {missing.map((m) => <code key={m} className="mr-1">{m}</code>)}</AlertDescription></Alert>}
          {!envList.length ? <p className="text-sm text-muted-foreground">No variables set.</p> :
            <ul className="divide-y text-sm">{envList.map((e) => <li key={e.k} className="flex items-center gap-2 py-1.5"><code className="shrink-0">{e.k}</code><span className="min-w-0 flex-1 truncate text-muted-foreground">{e.v}</span><Button size="sm" variant="ghost" disabled={pending} onClick={() => run(() => raw('DELETE', `${M}/env/${encodeURIComponent(e.k)}`), `${e.k} removed`)}>×</Button></li>)}</ul>}
        </CardContent></Card>
      </div>
      <p className="text-sm"><Link className="underline" to="/$section/$page" params={{ section: 'content', page: 'schedules' }}>Schedules & timers</Link> registered by modules are listed under Content → Schedules.</p>
      <FormDialog open={dlg === 'canary'} onOpenChange={(o) => !o && setDlg('')} title={`Start canary · ${name}`} submit="Start" pending={pending} disabled={!f.version}
        onSubmit={() => run(() => raw('POST', `${M}/canary`, { version: Number(f.version), percent: Number(f.percent) || 0, accounts: String(f.accounts || '').split(',').map((x) => x.trim()).filter(Boolean) }), 'Canary started')}>
        <Field label="Version"><Select value={String(f.version ?? '')} onValueChange={(v) => setF((x) => ({ ...x, version: v }))}><SelectTrigger className="w-full"><SelectValue placeholder="pick a version" /></SelectTrigger><SelectContent>{versions.map((v) => <SelectItem key={String(v.version)} value={String(v.version)}>v{String(v.version)} ({String(v.state ?? '')})</SelectItem>)}</SelectContent></Select></Field>
        <Field label="Percent of accounts"><Input type="number" min={0} max={100} value={String(f.percent ?? '')} onChange={(e) => setF((x) => ({ ...x, percent: e.target.value }))} /></Field>
        <Field label="Pinned account ids (comma-separated)" hint="These accounts always get the canary version regardless of percent."><Textarea rows={2} value={String(f.accounts ?? '')} onChange={(e) => setF((x) => ({ ...x, accounts: e.target.value }))} /></Field>
      </FormDialog>
      <FormDialog open={dlg === 'env'} onOpenChange={(o) => !o && setDlg('')} title={`Environment · ${name}`} pending={pending} onSubmit={() => run(() => raw('PUT', `${M}/env`, { values: Object.fromEntries(env.filter((e) => e.k).map((e) => [e.k, e.v])) }), 'Environment saved')}>
        <div className="space-y-2">{env.map((e, i) => <div key={i} className="flex min-w-0 gap-2"><Input className="min-w-0 flex-1 font-mono" placeholder="KEY" value={e.k} onChange={(ev) => setEnv((a) => a.map((x, j) => (j === i ? { ...x, k: ev.target.value } : x)))} /><Input className="min-w-0 flex-1" placeholder="value" value={e.v} onChange={(ev) => setEnv((a) => a.map((x, j) => (j === i ? { ...x, v: ev.target.value } : x)))} /><Button type="button" size="sm" variant="ghost" onClick={() => setEnv((a) => a.filter((_, j) => j !== i))}>×</Button></div>)}
          <Button type="button" size="sm" variant="outline" onClick={() => setEnv((a) => [...a, { k: '', v: '' }])}>Add variable</Button>
          {missing.length > 0 && <Label className="block text-xs text-muted-foreground">Unset: {missing.join(', ')}</Label>}</div>
      </FormDialog>
    </div>
  )
}
