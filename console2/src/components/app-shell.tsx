import { Link, Outlet, useNavigate, useRouterState } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { LogOut, Menu, Search } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Sheet, SheetContent, SheetTitle } from '@/components/ui/sheet'
import { Badge } from '@/components/ui/badge'
import { CommandMenu, useCommandMenu } from '@/components/command-menu'
import { sections, navGroups } from '@/model/sections'
import { nodeQuery } from '@/lib/node'
import { auth } from '@/lib/auth'
import { raw } from '@/lib/api'
import { cn } from '@/lib/utils'

function useSection(): string {
  const path = useRouterState({ select: (s) => s.location.pathname })
  return path.split('/').filter(Boolean)[0] ?? ''
}

function SectionNav({ onNavigate }: { onNavigate?: () => void }) {
  const section = useSection()
  const path = useRouterState({ select: (s) => s.location.pathname })
  const page = path.split('/').filter(Boolean)[1] ?? ''
  return (
    <nav className="grid gap-4 text-sm">
      <div className="grid gap-0.5">
        <Link to="/" onClick={onNavigate} className={cn('rounded-md px-2 py-1.5 hover:bg-sidebar-accent', section === '' && 'bg-sidebar-accent font-medium')}>Dashboard</Link>
        {sections.map((s) => (
          <Link key={s.id} to="/$section" params={{ section: s.id }} onClick={onNavigate}
            className={cn('rounded-md px-2 py-1.5 hover:bg-sidebar-accent', section === s.id && 'bg-sidebar-accent font-medium')}>{s.title}</Link>
        ))}
      </div>
      {section && navGroups(section).map((g) => (
        <div key={g.title || '_'} className="grid gap-0.5">
          {g.title && <div className="px-2 pb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">{g.title}</div>}
          {g.items.map((r) => (
            <Link key={r.id} to="/$section/$page" params={{ section, page: r.id }} onClick={onNavigate}
              className={cn('rounded-md px-2 py-1 text-muted-foreground hover:bg-sidebar-accent hover:text-foreground', page === r.id && 'bg-sidebar-accent text-foreground')}>{r.title}</Link>
          ))}
        </div>
      ))}
    </nav>
  )
}

export function AppShell() {
  const [open, setOpen] = useState(false)
  const cmd = useCommandMenu()
  const node = useQuery(nodeQuery)
  const nav = useNavigate()
  async function logout() {
    try { await raw('DELETE', '/v1/operator/logout') } catch { /* server keys have no session */ }
    auth.clear()
    await nav({ to: '/login', search: {} })
  }
  return (
    <div className="grid min-h-dvh lg:grid-cols-[15rem_minmax(0,1fr)]">
      <aside className="hidden lg:block border-r bg-sidebar text-sidebar-foreground">
        <div className="sticky top-0 h-dvh overflow-y-auto p-3">
          <div className="px-2 pb-4 pt-1 font-semibold">Musubi</div>
          <SectionNav />
        </div>
      </aside>
      <Sheet open={open} onOpenChange={setOpen}>
        <SheetContent side="left" className="w-72 overflow-y-auto p-3">
          <SheetTitle className="px-2 pb-2">Musubi</SheetTitle>
          <SectionNav onNavigate={() => setOpen(false)} />
        </SheetContent>
      </Sheet>
      <div className="min-w-0">
        <header className="sticky top-0 z-20 flex h-12 items-center gap-2 border-b bg-background/90 px-3 backdrop-blur">
          <Button variant="ghost" size="icon" className="lg:hidden" aria-label="Menu" onClick={() => setOpen(true)}><Menu /></Button>
          <Button variant="outline" size="sm" className="min-w-0 flex-1 shrink max-w-xs justify-start text-muted-foreground" onClick={() => cmd.setOpen(true)}>
            <Search /> <span className="truncate">Search…</span> <kbd className="ml-auto hidden rounded border px-1 text-xs sm:inline">⌘K</kbd>
          </Button>
          <div className="ml-auto flex shrink-0 items-center gap-2 text-sm">
            {node.data && <span className="hidden truncate text-muted-foreground md:inline">{node.data.name}{node.data.region ? ` · ${node.data.region}` : ''}</span>}
            {node.data?.environment && <Badge variant="secondary">{node.data.environment}</Badge>}
            <Button variant="ghost" size="icon" aria-label="Sign out" onClick={logout}><LogOut /></Button>
          </div>
        </header>
        <main className="p-4 md:p-6"><Outlet /></main>
      </div>
      <CommandMenu open={cmd.open} setOpen={cmd.setOpen} />
    </div>
  )
}
