// Global search (⌘K / Ctrl-K): pages from the catalog, then objects from the node's
// /v1/admin/search (accounts, worlds, matches, nodes, keys, an entity by uuid).
import { useEffect, useMemo, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from '@/components/ui/command'
import { sections, resources } from '@/model/sections'
import { raw, qs } from '@/lib/api'

export function useCommandMenu() {
  const [open, setOpen] = useState(false)
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); setOpen((o) => !o) }
    }
    window.addEventListener('keydown', on)
    return () => window.removeEventListener('keydown', on)
  }, [])
  return { open, setOpen }
}

interface Hit { group: string; title: string; sub?: string; to: string }
type SearchResult = {
  accounts?: { id: string; username?: string; display_name?: string | null }[]
  worlds?: { name: string; node_id?: string | null }[]
  matches?: { match_id: string; label?: string; module?: string }[]
  nodes?: { node_id: string; name?: string; region?: string }[]
  keys?: { id: string; name?: string; role?: string }[]
  entity?: { world: string; id: string } | null
}

function useDebounced<T>(v: T, ms: number): T {
  const [d, setD] = useState(v)
  useEffect(() => { const t = setTimeout(() => setD(v), ms); return () => clearTimeout(t) }, [v, ms])
  return d
}

export function CommandMenu({ open, setOpen }: { open: boolean; setOpen: (v: boolean) => void }) {
  const nav = useNavigate()
  const [q, setQ] = useState('')
  const term = useDebounced(q.trim(), 200)
  const pages = useMemo<Hit[]>(() => sections.flatMap((s) => [
    { group: 'Pages', title: s.title, sub: s.sub, to: `/${s.id}` },
    ...resources(s.id).map((r) => ({ group: 'Pages', title: r.title, sub: s.title, to: `/${s.id}/${r.id}` })),
  ]), [])
  const remote = useQuery({
    queryKey: ['search', term],
    enabled: open && term.length >= 2,
    queryFn: ({ signal }) => raw<SearchResult>('GET', '/v1/admin/search' + qs({ q: term, limit: 8 }), undefined, { signal }),
    staleTime: 10_000,
  })
  const hits = useMemo<Hit[]>(() => {
    const r = remote.data
    if (!r) return []
    const out: Hit[] = []
    for (const a of r.accounts ?? []) out.push({ group: 'Players', title: a.username || a.display_name || a.id, sub: a.id, to: `/players/accounts/${a.id}` })
    for (const w of r.worlds ?? []) out.push({ group: 'Worlds', title: w.name, sub: w.node_id ?? undefined, to: `/world/worlds/${encodeURIComponent(w.name)}` })
    for (const m of r.matches ?? []) out.push({ group: 'Matches', title: m.label || m.match_id, sub: m.module, to: `/social/matches/${m.match_id}` })
    for (const n of r.nodes ?? []) out.push({ group: 'Nodes', title: n.name || n.node_id, sub: n.region, to: `/cluster/nodes/${n.node_id}` })
    for (const k of r.keys ?? []) out.push({ group: 'Keys', title: k.name || k.id, sub: k.role, to: `/settings/keys/${k.id}` })
    if (r.entity) out.push({ group: 'Entities', title: r.entity.id, sub: r.entity.world, to: `/world/worlds/${encodeURIComponent(r.entity.world)}/entities/${r.entity.id}` })
    return out
  }, [remote.data])
  const groups = useMemo(() => {
    const m = new Map<string, Hit[]>()
    for (const h of hits) m.set(h.group, [...(m.get(h.group) ?? []), h])
    return [...m]
  }, [hits])
  function go(to: string) { setOpen(false); setQ(''); void nav({ to }) }
  return (
    <CommandDialog open={open} onOpenChange={setOpen} title="Search" description="Pages, players, worlds, matches, nodes, keys">
      <CommandInput placeholder="Search pages, players, worlds, ids…" value={q} onValueChange={setQ} />
      <CommandList>
        <CommandEmpty>{remote.isFetching ? 'Searching…' : remote.isError ? String(remote.error) : 'Nothing found.'}</CommandEmpty>
        {groups.map(([g, list]) => (
          <CommandGroup key={g} heading={g}>
            {list.map((h) => (
              <CommandItem key={h.to} value={`${h.title} ${h.sub ?? ''} ${h.to}`} onSelect={() => go(h.to)}>
                <span className="truncate">{h.title}</span>{h.sub && <span className="ml-auto truncate text-xs text-muted-foreground">{h.sub}</span>}
              </CommandItem>
            ))}
          </CommandGroup>
        ))}
        <CommandGroup heading="Pages">
          {pages.map((h) => (
            <CommandItem key={h.to} value={`${h.title} ${h.sub ?? ''}`} onSelect={() => go(h.to)}>
              <span className="truncate">{h.title}</span>{h.sub && <span className="ml-auto truncate text-xs text-muted-foreground">{h.sub}</span>}
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  )
}
