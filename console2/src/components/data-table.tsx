// The one table: TanStack Table for sorting + column visibility (remembered per table id),
// CSV export, skeleton rows, empty state, cross-links by field name, first cell → the row's page.
import { useMemo, useState } from 'react'
import { Link } from '@tanstack/react-router'
import { type ColumnDef, type SortingState, type VisibilityState, flexRender, getCoreRowModel, getSortedRowModel, useReactTable } from '@tanstack/react-table'
import { ArrowDown, ArrowUp, ArrowUpDown, Columns3, Download } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { Skeleton } from '@/components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { refFor } from '@/model/refs'
import { fmtVal, isMonoKey, toCsv, type Row } from '@/model/resource'
import { cn } from '@/lib/utils'

export interface Col { key: string; label?: string; mono?: boolean; dim?: boolean; fmt?: (r: Row) => React.ReactNode }
interface Props {
  id: string
  cols: Col[]
  rows: Row[]
  loading?: boolean
  empty?: React.ReactNode
  rowTo?: (r: Row) => string | null
  actions?: (r: Row) => React.ReactNode
  tools?: boolean
}

function useRemembered(id: string): [VisibilityState, (v: VisibilityState) => void] {
  const key = `dt.cols.${id}`
  const [v, setV] = useState<VisibilityState>(() => { try { return JSON.parse(localStorage.getItem(key) || '{}') } catch { return {} } })
  return [v, (n) => { setV(n); localStorage.setItem(key, JSON.stringify(n)) }]
}

export function DataTable({ id, cols, rows, loading, empty, rowTo, actions, tools = true }: Props) {
  const [sorting, setSorting] = useState<SortingState>([])
  const [visibility, setVisibility] = useRemembered(id)
  const columns = useMemo<ColumnDef<Row>[]>(() => cols.map((c, i) => ({
    id: c.key,
    accessorFn: (r) => r[c.key],
    header: c.label ?? c.key.replace(/_/g, ' '),
    cell: ({ row }) => {
      const r = row.original
      const text = c.fmt ? c.fmt(r) : fmtVal(c.key, r[c.key])
      const to = i === 0 && rowTo ? rowTo(r) : null
      const ref = !to && typeof r[c.key] !== 'object' ? refFor(c.key, r[c.key], r) : null
      const cls = cn('block max-w-[28rem] truncate', (c.mono ?? isMonoKey(c.key)) && 'font-mono text-xs', c.dim && 'text-muted-foreground')
      if (to) return <Link to={to} className={cn(cls, 'font-medium hover:underline')}>{text}</Link>
      if (ref) return <Link to={ref} className={cn(cls, 'hover:underline')}>{text}</Link>
      return <span className={cls} title={typeof text === 'string' ? text : undefined}>{text}</span>
    },
  })), [cols, rowTo])
  const table = useReactTable({
    data: rows, columns, state: { sorting, columnVisibility: visibility },
    onSortingChange: setSorting, onColumnVisibilityChange: (u) => setVisibility(typeof u === 'function' ? u(visibility) : u),
    getCoreRowModel: getCoreRowModel(), getSortedRowModel: getSortedRowModel(),
  })
  function exportCsv() {
    const keys = table.getVisibleLeafColumns().map((c) => c.id)
    const blob = new Blob([toCsv(keys, rows)], { type: 'text/csv' })
    const a = document.createElement('a'); a.href = URL.createObjectURL(blob); a.download = `${id}.csv`; a.click(); URL.revokeObjectURL(a.href)
  }
  return (
    <div className="grid gap-2">
      {tools && (
        <div className="flex items-center justify-end gap-1">
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="ghost" size="sm" aria-label="Columns"><Columns3 /> <span className="hidden sm:inline">Columns</span></Button></DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="max-h-80 overflow-y-auto">
              {table.getAllLeafColumns().map((c) => (
                <DropdownMenuCheckboxItem key={c.id} checked={c.getIsVisible()} onCheckedChange={(v) => c.toggleVisibility(!!v)} onSelect={(e) => e.preventDefault()}>{c.id.replace(/_/g, ' ')}</DropdownMenuCheckboxItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <Button variant="ghost" size="sm" aria-label="Export CSV" onClick={exportCsv} disabled={!rows.length}><Download /> <span className="hidden sm:inline">CSV</span></Button>
        </div>
      )}
      <div className="overflow-x-auto rounded-md border">
        <Table>
          <TableHeader>
            {table.getHeaderGroups().map((hg) => (
              <TableRow key={hg.id}>
                {hg.headers.map((h) => {
                  const dir = h.column.getIsSorted()
                  return (
                    <TableHead key={h.id} className="h-9 whitespace-nowrap text-xs">
                      <button type="button" className="inline-flex items-center gap-1 hover:text-foreground" onClick={h.column.getToggleSortingHandler()}>
                        {flexRender(h.column.columnDef.header, h.getContext())}
                        {dir === 'asc' ? <ArrowUp className="size-3" /> : dir === 'desc' ? <ArrowDown className="size-3" /> : <ArrowUpDown className="size-3 opacity-40" />}
                      </button>
                    </TableHead>
                  )
                })}
                {actions && <TableHead className="h-9 w-0" />}
              </TableRow>
            ))}
          </TableHeader>
          <TableBody>
            {loading && !rows.length ? [0, 1, 2, 3, 4].map((i) => (
              <TableRow key={i}>{table.getVisibleLeafColumns().map((c) => <TableCell key={c.id}><Skeleton className="h-4 w-full" /></TableCell>)}{actions && <TableCell />}</TableRow>
            )) : !rows.length ? (
              <TableRow><TableCell colSpan={cols.length + (actions ? 1 : 0)} className="h-20 text-center text-sm text-muted-foreground">{empty ?? 'No rows.'}</TableCell></TableRow>
            ) : table.getRowModel().rows.map((r) => (
              <TableRow key={r.id}>
                {r.getVisibleCells().map((c) => <TableCell key={c.id} className="py-1.5 text-sm">{flexRender(c.column.columnDef.cell, c.getContext())}</TableCell>)}
                {actions && <TableCell className="py-1 text-right whitespace-nowrap">{actions(r.original)}</TableCell>}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </div>
  )
}
