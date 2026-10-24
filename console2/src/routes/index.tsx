import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'

export const Route = createFileRoute('/')({ component: Dashboard })

function Dashboard() {
  const node = useQuery({ queryKey: ['node'], queryFn: async () => (await api.GET('/v1/node')).data })
  return (
    <main className="p-6">
      <h1 className="text-xl font-semibold">Dashboard</h1>
      <pre className="mt-4 text-xs text-muted-foreground">{JSON.stringify(node.data, null, 2)}</pre>
    </main>
  )
}
