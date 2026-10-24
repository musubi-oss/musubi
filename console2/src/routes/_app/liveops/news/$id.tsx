import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { listOf, useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { JsonView } from '@/components/json-view'
import { Kv } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'

/** News article editor (`new` creates one): markdown body, schedule, audience, read stats and per-locale translations. */
export const Route = createFileRoute('/_app/liveops/news/$id')({ component: NewsPage })

interface L { locale: string; title: string; body: string; saved: boolean }
const toLocal = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().slice(0, 16) : '')
const fromLocal = (s: string) => (s ? Math.floor(new Date(s).getTime() / 1000) : null)
const tsu = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—')
const BLANK: Row = { title: '', body: '', pinned: false, enabled: true, link: null, image: null, audience: null, locales: {}, metadata: {} }

function NewsPage() {
  const { id } = Route.useParams()
  const isNew = id === 'new'
  const nav = useNavigate({ from: Route.fullPath })
  const N = `/v1/server/news/${encodeURIComponent(id)}`
  const q = useQuery({ queryKey: ['news', id], enabled: !isNew, queryFn: async ({ signal }) => { const r = await raw<Row>('GET', N, undefined, { signal }); return (r.news ?? r.item ?? r) as Row } })
  const stats = useSub<Row>(isNew ? null : `${N}/stats`)
  const audiences = listOf(useSub('/v1/server/audiences').data, 'audiences').map((a) => String(a.name))
  const n = isNew ? BLANK : q.data
  const [f, setF] = useState<Row>(BLANK); const [tags, setTags] = useState(''); const [pub, setPub] = useState(''); const [exp, setExp] = useState(''); const [meta, setMeta] = useState('{}'); const [locales, setLocales] = useState<L[]>([]); const [newLocale, setNewLocale] = useState('')
  useEffect(() => { if (!n) return; setF({ ...n }); setTags(((n.tags ?? []) as string[]).join(', ')); setPub(toLocal(n.published_at)); setExp(toLocal(n.expires_at)); setMeta(JSON.stringify(n.metadata ?? {}, null, 2)); setLocales(Object.entries((n.locales ?? {}) as Record<string, Row>).map(([locale, v]) => ({ locale, title: String(v.title ?? ''), body: String(v.body ?? ''), saved: true }))) }, [n])
  const set = (k: string, v: unknown) => setF((x) => ({ ...x, [k]: v }))
  const { run, pending } = useAct([['news', id], ['sub']])
  const now = Date.now() / 1000
  const state = !n ? '' : !n.enabled ? 'disabled' : n.published_at && now < Number(n.published_at) ? 'scheduled' : n.expires_at && now > Number(n.expires_at) ? 'expired' : 'live'
  function save() {
    let metadata: unknown; try { metadata = JSON.parse(meta || '{}') } catch { return toast.error('Metadata is not valid JSON') }
    const { locales: _l, ...rest } = f
    const b: Row = { ...rest, tags: tags.split(',').map((s) => s.trim()).filter(Boolean), published_at: fromLocal(pub), expires_at: fromLocal(exp), metadata, audience: f.audience || null, link: f.link || null, image: f.image || null }
    run(async () => {
      if (isNew) { const r = await raw<Row>('POST', '/v1/server/news', b); void nav({ to: '/liveops/news/$id', params: { id: String(r.id ?? (r.news as Row | undefined)?.id ?? '') }, replace: true }) }
      else await raw('PUT', N, b)
    }, 'Saved')
  }
  const updL = (i: number, k: 'title' | 'body', v: string) => setLocales((ls) => ls.map((l, j) => (j === i ? { ...l, [k]: v, saved: false } : l)))
  const s = stats.data
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'LiveOps', to: '/liveops' }, { title: 'News', to: '/liveops/news' }, { title: isNew ? 'new article' : String(n?.title ?? id) }]} />
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1"><h1 className="truncate text-xl font-semibold">{isNew ? 'New article' : String(n?.title ?? id)}</h1><div className="mt-1 flex flex-wrap items-center gap-2 font-mono text-xs text-muted-foreground">{!isNew && id}{state && <Badge variant={state === 'live' ? 'default' : 'secondary'}>{state}</Badge>}</div></div>
        {!isNew && n && <Button size="sm" variant="destructive" onClick={() => confirmThen('Delete this article?', () => run(async () => { await raw('DELETE', N); void nav({ to: '/$section/$page', params: { section: 'liveops', page: 'news' } }) }, 'Deleted'))}>Delete</Button>}
        <Button size="sm" disabled={!n || pending || !f.title} onClick={save}>{isNew ? 'Create' : 'Save'}</Button>
      </div>
      {q.isError && <Alert variant="destructive"><AlertDescription>{String(q.error)}</AlertDescription></Alert>}
      {n && (
        <Card><CardContent className="pt-6">
          <div className="grid min-w-0 gap-x-8 gap-y-1 xl:grid-cols-[2fr_1fr]">
            <div className="min-w-0">
              <Kv k="Title" v={<Input value={String(f.title ?? '')} onChange={(x) => set('title', x.target.value)} />} />
              <Kv k="Body" v={<><Textarea className="min-h-44" value={String(f.body ?? '')} onChange={(x) => set('body', x.target.value)} aria-label="body" /><p className="mt-1 text-xs text-muted-foreground">markdown as the client renders it</p></>} />
              <Kv k="Link" v={<Input className="font-mono" value={String(f.link ?? '')} placeholder="https:// or deep link" onChange={(x) => set('link', x.target.value)} />} />
              <Kv k="Image URL" v={<Input className="font-mono" value={String(f.image ?? '')} onChange={(x) => set('image', x.target.value)} />} />
              <Kv k="Tags (comma-separated)" v={<Input value={tags} onChange={(x) => setTags(x.target.value)} />} />
            </div>
            <div className="min-w-0">
              <Kv k="Publish at (empty = now)" v={<Input type="datetime-local" value={pub} onChange={(x) => setPub(x.target.value)} />} />
              <Kv k="Expires at" v={<Input type="datetime-local" value={exp} onChange={(x) => setExp(x.target.value)} />} />
              <Kv k="Audience" v={<Select value={String(f.audience || '*')} onValueChange={(v) => set('audience', v === '*' ? null : v)}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="*">everyone</SelectItem>{audiences.map((a) => <SelectItem key={a} value={a}>{a}</SelectItem>)}</SelectContent></Select>} />
              <Kv k="Flags" v={<div className="space-y-1.5"><label className="flex items-center gap-2"><Checkbox checked={!!f.enabled} onCheckedChange={(v) => set('enabled', v === true)} /> enabled</label><label className="flex items-center gap-2"><Checkbox checked={!!f.pinned} onCheckedChange={(v) => set('pinned', v === true)} /> pinned to the top</label></div>} />
              {s && <><Kv k="Reads" v={s.reads ?? 0} /><Kv k="First / last read" v={`${tsu(s.first_read_at)} · ${tsu(s.last_read_at)}`} /></>}
            </div>
          </div>
          {!isNew && <>
            <SubHead title="Localisations" count={locales.length}>
              <span className="self-center text-xs text-muted-foreground">clients get their locale, else the default above</span>
              <form className="flex gap-1.5" onSubmit={(e) => { e.preventDefault(); const l = newLocale.trim(); if (l && !locales.some((x) => x.locale === l)) { setLocales((ls) => [...ls, { locale: l, title: '', body: '', saved: false }]); setNewLocale('') } }}>
                <Input className="h-8 w-24 font-mono" value={newLocale} placeholder="de, ja…" onChange={(e) => setNewLocale(e.target.value)} aria-label="new locale" /><Button type="submit" size="sm" variant="outline" disabled={!newLocale.trim()}>Add</Button>
              </form>
            </SubHead>
            <div className="overflow-x-auto"><Table>
              <TableHeader><TableRow><TableHead className="w-24">Locale</TableHead><TableHead className="w-[30%]">Title</TableHead><TableHead>Body</TableHead><TableHead className="w-36" /></TableRow></TableHeader>
              <TableBody>
                {locales.map((l, i) => (
                  <TableRow key={l.locale}>
                    <TableCell className="font-mono">{l.locale}</TableCell>
                    <TableCell><Input className="h-8" value={l.title} onChange={(x) => updL(i, 'title', x.target.value)} aria-label={`${l.locale} title`} /></TableCell>
                    <TableCell><Textarea className="min-h-16 text-xs" value={l.body} onChange={(x) => updL(i, 'body', x.target.value)} aria-label={`${l.locale} body`} /></TableCell>
                    <TableCell><span className="flex justify-end gap-1"><Button size="sm" variant={l.saved ? 'ghost' : 'default'} disabled={l.saved} onClick={() => run(() => raw('PUT', `${N}/locales/${encodeURIComponent(l.locale)}`, { title: l.title, body: l.body }), `${l.locale} saved`)}>Save</Button><Button size="sm" variant="ghost" className="text-destructive" onClick={() => { if (!l.saved) return setLocales((ls) => ls.filter((_, j) => j !== i)); run(() => raw('DELETE', `${N}/locales/${encodeURIComponent(l.locale)}`), 'Removed') }}>×</Button></span></TableCell>
                  </TableRow>
                ))}
                {!locales.length && <TableRow><TableCell colSpan={4} className="py-4 text-center text-muted-foreground">Default language only</TableCell></TableRow>}
              </TableBody>
            </Table></div>
          </>}
          <SubHead title="Metadata (JSON)" /><Textarea className="min-h-20 font-mono text-xs" value={meta} onChange={(x) => setMeta(x.target.value)} aria-label="metadata" />
          {!isNew && <><SubHead title="Raw" /><JsonView value={n} /></>}
        </CardContent></Card>
      )}
    </div>
  )
}
