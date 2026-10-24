import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { z } from 'zod'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ApiError, api, raw } from '@/lib/api'
import { auth, type LoginMode } from '@/lib/auth'

const searchSchema = z.object({ to: z.string().optional() })
export const Route = createFileRoute('/login')({ validateSearch: searchSchema, component: Login })

const passwordSchema = z.object({ email: z.email(), password: z.string().min(1, 'required') })
const keySchema = z.object({ key: z.string().min(1, 'required'), base: z.string().optional() })

function explain(e: unknown): string {
  if (e instanceof ApiError) return e.status === 401 ? 'wrong e-mail or password' : e.message
  const m = e instanceof Error ? e.message : String(e)
  return /fetch|network|cors|load failed/i.test(m)
    ? `${m} — the node did not answer. If it runs on another origin, add this one to [http] cors_origins in its config.`
    : m
}

function Login() {
  const nav = useNavigate()
  const { to } = Route.useSearch()
  const [mode, setMode] = useState<LoginMode>(auth.mode)
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)

  async function done(ok: boolean) {
    if (!ok) return
    auth.mode = mode
    await nav({ to: to || '/' })
  }
  async function probe(): Promise<boolean> {
    const r = await api.GET('/v1/node')
    if (r.error) { auth.clear(); throw new ApiError(r.response.status, String(r.response.status), r.response.status === 401 ? 'key rejected' : 'node did not answer', r.error) }
    return true
  }

  const pw = useForm({ resolver: zodResolver(passwordSchema), defaultValues: { email: '', password: '' } })
  const key = useForm({ resolver: zodResolver(keySchema), defaultValues: { key: '', base: auth.base } })

  const onPassword = pw.handleSubmit(async (v) => {
    setErr(''); setBusy(true)
    try {
      auth.clear()
      const r = await raw<{ key: string }>('POST', '/v1/operator/login', v)
      auth.key = r.key
      await done(await probe())
    } catch (e) { setErr(explain(e)) } finally { setBusy(false) }
  })
  const onKey = key.handleSubmit(async (v) => {
    setErr(''); setBusy(true)
    try {
      auth.base = v.base ?? ''
      auth.key = v.key.trim()
      await done(await probe())
    } catch (e) { setErr(explain(e)) } finally { setBusy(false) }
  })
  function sso() {
    const ret = location.origin + '/console/login'
    location.href = (auth.base || '') + '/v1/admin/sso/login?return_to=' + encodeURIComponent(ret)
  }

  return (
    <main className="min-h-dvh grid place-items-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>Musubi Console</CardTitle>
          <CardDescription>Sign in to this node</CardDescription>
        </CardHeader>
        <CardContent>
          <Tabs value={mode} onValueChange={(v) => setMode(v as LoginMode)}>
            <TabsList className="w-full">
              <TabsTrigger value="password" className="flex-1">E-mail</TabsTrigger>
              <TabsTrigger value="key" className="flex-1">Key</TabsTrigger>
            </TabsList>
            <TabsContent value="password">
              <form onSubmit={onPassword} className="grid gap-3">
                <div className="grid gap-1.5"><Label htmlFor="email">E-mail</Label><Input id="email" type="email" autoComplete="username" {...pw.register('email')} /></div>
                <div className="grid gap-1.5"><Label htmlFor="password">Password</Label><Input id="password" type="password" autoComplete="current-password" {...pw.register('password')} /></div>
                <Button type="submit" disabled={busy}>Sign in</Button>
              </form>
            </TabsContent>
            <TabsContent value="key">
              <form onSubmit={onKey} className="grid gap-3">
                <div className="grid gap-1.5"><Label htmlFor="key">Operator key</Label><Input id="key" type="password" autoComplete="off" {...key.register('key')} /></div>
                <div className="grid gap-1.5"><Label htmlFor="base">API base <span className="text-muted-foreground">(blank = this origin)</span></Label><Input id="base" placeholder="https://node.example.com" {...key.register('base')} /></div>
                <Button type="submit" disabled={busy}>Sign in</Button>
              </form>
            </TabsContent>
          </Tabs>
          <Button type="button" variant="outline" className="w-full mt-3" onClick={sso}>Single sign-on</Button>
          {err && <p role="alert" className="mt-3 text-sm text-destructive">{err}</p>}
        </CardContent>
      </Card>
    </main>
  )
}
