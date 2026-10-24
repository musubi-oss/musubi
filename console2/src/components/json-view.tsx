import { cn } from '@/lib/utils'

export function JsonView({ value, className, empty = 'No data' }: { value: unknown; className?: string; empty?: string }) {
  if (value === null || value === undefined) return <p className="text-sm text-muted-foreground">{empty}</p>
  return (
    <pre className={cn('max-h-[480px] overflow-auto rounded-md border bg-muted/40 p-3 font-mono text-xs', className)}>
      {JSON.stringify(value, null, 2)}
    </pre>
  )
}
