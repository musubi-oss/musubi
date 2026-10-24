import { createFileRoute, notFound } from '@tanstack/react-router'
import { resource, sectionById } from '@/model/sections'
import { about } from '@/model/about'

export const Route = createFileRoute('/_app/$section/$page')({
  beforeLoad: ({ params }) => { if (!sectionById(params.section) || !resource(params.section, params.page)) throw notFound() },
  component: Page,
})

function Page() {
  const { section, page } = Route.useParams()
  const r = resource(section, page)!
  return (
    <div className="grid gap-2">
      <h1 className="text-xl font-semibold">{r.title}</h1>
      <p className="max-w-3xl text-sm text-muted-foreground">{about[`${section}/${page}`]}</p>
    </div>
  )
}
