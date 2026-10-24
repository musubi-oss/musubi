import { Outlet, createRootRouteWithContext, redirect } from '@tanstack/react-router'
import type { QueryClient } from '@tanstack/react-query'
import { Toaster } from '@/components/ui/sonner'
import { auth } from '@/lib/auth'

export const Route = createRootRouteWithContext<{ queryClient: QueryClient }>()({
  beforeLoad: ({ location }) => {
    if (!auth.authed && location.pathname !== '/login') {
      throw redirect({ to: '/login', search: { to: location.pathname + location.searchStr } })
    }
  },
  component: () => (
    <>
      <Outlet />
      <Toaster richColors position="bottom-right" />
    </>
  ),
})
