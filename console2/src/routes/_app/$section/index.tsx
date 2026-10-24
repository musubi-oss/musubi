import { Link, createFileRoute, notFound } from '@tanstack/react-router'
import { Card, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { sectionById, resources } from '@/model/sections'
import { about } from '@/model/about'

export const Route = createFileRoute('/_app/$section/')({
  beforeLoad: ({ params }) => { if (!sectionById(params.section)) throw notFound() },
  component: SectionIndex,
})

function SectionIndex() {
  const { section } = Route.useParams()
  const s = sectionById(section)!
  return (
    <div className="grid gap-4">
      <div><h1 className="text-xl font-semibold">{s.title}</h1><p className="text-sm text-muted-foreground">{s.sub}</p></div>
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        {resources(section).map((r) => (
          <Link key={r.id} to="/$section/$page" params={{ section, page: r.id }} className="block">
            <Card className="h-full transition-colors hover:bg-accent/40">
              <CardHeader><CardTitle className="text-base">{r.title}</CardTitle>
                <CardDescription className="line-clamp-3">{about[`${section}/${r.id}`] ?? ''}</CardDescription></CardHeader>
            </Card>
          </Link>
        ))}
      </div>
    </div>
  )
}
