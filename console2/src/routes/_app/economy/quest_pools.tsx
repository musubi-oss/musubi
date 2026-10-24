import { createFileRoute, redirect } from '@tanstack/react-router'

/** Quest pools live on the Quests page (Pools tab); keep the old URL working. */
export const Route = createFileRoute('/_app/economy/quest_pools')({
  beforeLoad: () => { throw redirect({ to: '/economy/quests', search: { tab: 'pools' }, replace: true }) },
})
