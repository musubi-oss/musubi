import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { raw } from '@/lib/api'
import { useAct, confirmThen } from '@/lib/act'
import { mb } from '@/lib/metrics'
import { useSub } from '@/lib/sub'
import type { Row } from '@/model/resource'
import { Crumbs } from '@/components/crumbs'
import { DataTable } from '@/components/data-table'
import { JsonView } from '@/components/json-view'
import { Kv, KvGrid } from '@/components/kv'
import { SubHead } from '@/components/sub-head'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'

/** Build artifacts (content-chunked, deduplicated across versions) with manifests, pending delta uploads and chunk-store stats. */
export const Route = createFileRoute('/_app/servers/builds')({ component: Builds })

const tsu = (v: unknown) => (v ? new Date(Number(v) * 1000).toISOString().replace('T', ' ').slice(0, 16) : '—')

function Builds() {
  const q = useQuery({ queryKey: ['builds'], queryFn: ({ signal }) => raw<Row>('GET', '/v1/server/builds', undefined, { signal }) })
  const chunking = useSub<Row>('/v1/server/builds/chunking')
  const arts = (q.data?.artifacts ?? []) as Row[]
  const pending = (q.data?.pending_uploads ?? []) as Row[]
  const groups = useMemo(() => { const m = new Map<string, Row[]>(); for (const a of arts) m.set(String(a.name), [...(m.get(String(a.name)) ?? []), a]); return [...m.entries()].map(([name, vs]) => ({ name, vs: vs.sort((a, b) => Number(b.created_at ?? 0) - Number(a.created_at ?? 0)) })) }, [arts])
  const [open, setOpen] = useState<string | null>(null)
  const [sel, setSel] = useState<Row | null>(null)
  const [manifest, setManifest] = useState<Row | null>(null)
  const { run } = useAct([['builds'], ['sub']])
  const c = chunking.data
  async function showManifest(a: Row) { setSel(a); try { setManifest(await raw<Row>('GET', `/v1/server/builds/${encodeURIComponent(String(a.name))}/${encodeURIComponent(String(a.version))}/manifest`)) } catch (e) { toast.error(String(e)) } }
  const chunks = (manifest?.chunks ?? []) as unknown[]
  return (
    <div className="space-y-4">
      <Crumbs items={[{ title: 'Servers', to: '/servers' }, { title: 'Builds & artifacts' }]} />
      <div className="flex flex-wrap items-center gap-3">
        <div className="min-w-0 flex-1"><h1 className="text-xl font-semibold">Builds &amp; artifacts</h1><p className="mt-1 text-xs text-muted-foreground">{groups.length} builds · {arts.length} versions{c ? ` · dedup ${String((c.artifacts as Row | undefined)?.dedup_ratio ?? c.dedup_ratio ?? '—')}×` : ''}. Upload from CI with <code>musubi build push name@version file</code> (delta: only changed chunks travel). Launchers fetch the manifest and pull missing chunks; versions share identical chunks.</p></div>
        <Button size="sm" variant="outline" onClick={() => run(() => raw('POST', '/v1/server/builds/gc'), 'Chunk GC done')}>GC chunks</Button>
      </div>
      <Card><CardContent className="pt-6">
        {!groups.length && !q.isPending && <p className="text-sm text-muted-foreground">No artifacts uploaded</p>}
        {groups.map((g) => (
          <div key={g.name} className="mb-3">
            <button type="button" className="flex w-full flex-wrap items-baseline gap-2 text-left text-sm font-semibold hover:underline" onClick={() => setOpen(open === g.name ? null : g.name)}>
              <span>{open === g.name ? '▾' : '▸'}</span><span className="font-mono">{g.name}</span><span className="font-normal text-muted-foreground">({g.vs.length} versions · latest {String(g.vs[0]?.version)} · {tsu(g.vs[0]?.created_at)})</span>
            </button>
            {open === g.name && (
              <DataTable id={`builds-${g.name}`} tools={false} rows={g.vs} empty="No versions"
                cols={[{ key: 'version', mono: true }, { key: 'size', fmt: (a) => mb(a.size) }, { key: 'chunks', dim: true, fmt: (a) => (a.chunked ? String((a.delta as Row | undefined)?.chunks ?? a.chunks ?? '✓') : 'raw') }, { key: 'delta', label: 'delta from previous', dim: true, fmt: (a) => (a.delta ? `${(a.delta as Row).from} → ${(a.delta as Row).new_pct}% new (${mb((a.delta as Row).new_bytes)})` : '—') }, { key: 'sha256', mono: true, dim: true, fmt: (a) => String(a.sha256 ?? '').slice(0, 16) }, { key: 'created_at', label: 'created', dim: true, fmt: (a) => tsu(a.created_at) }]}
                actions={(a) => (
                  <span className="flex justify-end gap-1">
                    <Button size="sm" variant="ghost" onClick={() => void showManifest(a)}>Manifest</Button>
                    {!!a.download_url && <Button size="sm" variant="ghost" asChild><a href={String(a.download_url)} target="_blank" rel="noopener">Download</a></Button>}
                    <Button size="sm" variant="ghost" className="text-destructive" onClick={() => confirmThen(`Delete ${a.name} ${a.version}? Chunks only it uses are freed.`, () => run(() => raw('DELETE', `/v1/server/builds/${encodeURIComponent(String(a.name))}/${encodeURIComponent(String(a.version))}`), 'Deleted'))}>Delete</Button>
                  </span>
                )} />
            )}
          </div>
        ))}
        {pending.length > 0 && <>
          <SubHead title="Pending uploads" count={pending.length}><span className="self-center text-xs text-muted-foreground">delta sessions awaiting chunks / commit</span></SubHead>
          <DataTable id="builds-pending" tools={false} rows={pending} empty="" cols={[{ key: 'name', label: 'build', mono: true }, { key: 'version', mono: true }, { key: 'size', fmt: (p) => mb(p.size) }, { key: 'started_at', label: 'started', dim: true, fmt: (p) => tsu(p.started_at) }]}
            actions={(p) => <Button size="sm" variant="ghost" onClick={() => run(() => raw('DELETE', `/v1/server/builds/uploads/${encodeURIComponent(String(p.id ?? p.name))}`), 'Upload dropped')}>Drop</Button>} />
        </>}
        {manifest && sel && <>
          <SubHead title={`Manifest — ${sel.name} ${sel.version}`} count={chunks.length} />
          <KvGrid><Kv k="File" v={`${manifest.filename ?? sel.filename ?? '—'} · ${mb(Number(manifest.size ?? sel.size))}`} /><Kv k="sha256" v={manifest.sha256 ?? sel.sha256} mono copy /><Kv k="Chunk sizes" v={`min ${mb(Number(manifest.chunk_min ?? 0))} · avg ${mb(Number(manifest.chunk_avg ?? 0))} · max ${mb(Number(manifest.chunk_max ?? 0))}`} /></KvGrid>
          <JsonView value={{ ...manifest, chunks: chunks.slice(0, 20).concat(chunks.length > 20 ? [`… ${chunks.length - 20} more`] : []) }} />
        </>}
        {c && <><SubHead title="Chunk store" /><JsonView value={c} /></>}
      </CardContent></Card>
    </div>
  )
}
