import { Bar, BarChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'

/** Compact bar chart for daily series; labels are ISO days, shown as MM-DD. */
export function Bars({ data, height = 160, format }: { data: { label: string; v: number }[]; height?: number; format?: (v: number) => string }) {
  if (!data.length) return <p className="py-6 text-center text-sm text-muted-foreground">No data for this period.</p>
  return (
    <ResponsiveContainer width="100%" height={height}>
      <BarChart data={data} margin={{ top: 4, right: 4, left: -20, bottom: 0 }}>
        <XAxis dataKey="label" tickFormatter={(l: string) => l.slice(5, 10)} fontSize={10} tickLine={false} axisLine={false} interval="preserveStartEnd" />
        <YAxis fontSize={10} tickLine={false} axisLine={false} tickFormatter={(v: number) => (format ? format(v) : String(v))} />
        <Tooltip cursor={{ fill: 'hsl(var(--muted))' }} contentStyle={{ fontSize: 12, borderRadius: 6 }} formatter={(v) => [format ? format(Number(v)) : String(v), '']} labelFormatter={(l) => String(l)} />
        <Bar dataKey="v" fill="hsl(var(--primary))" radius={[2, 2, 0, 0]} />
      </BarChart>
    </ResponsiveContainer>
  )
}
