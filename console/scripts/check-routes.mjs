// Fails the build when the console calls a /v1 path the node does not serve. Every string/template literal
// starting with /v1/ in src/ is normalised (${expr}, {param} and :param → one segment wildcard) and matched
// against src/api/routes.ts by path; query strings and trailing concatenations are ignored.
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve, join, relative } from 'node:path'
const here = dirname(fileURLToPath(import.meta.url))
const root = resolve(here, '../src')
const routes = readFileSync(resolve(root, 'api/routes.ts'), 'utf8')
const known = [...routes.matchAll(/\['[A-Z]+', '([^']+)'/g)].map((m) => m[1].split('/').slice(1))
const files = []
const walk = (d) => { for (const f of readdirSync(d)) { const p = join(d, f); statSync(p).isDirectory() ? walk(p) : /\.(ts|vue)$/.test(f) && !p.endsWith('routes.ts') && files.push(p) } }
walk(root)
const lit = /(['"`])(\/v1\/[^'"`]*)\1(\s*\+)?/g
// A literal that is continued (`'/v1/x' + …`) or ends in an interpolation (`/v1/x/${id}` kept in a const and
// extended later) only has to be a prefix of some route; everything else must match a whole route.
const seen = new Map()
for (const f of files) {
  const s = readFileSync(f, 'utf8')
  for (const m of s.matchAll(lit)) {
    const raw = m[2].split('?')[0].replace(/\/$/, '')
    const segs = raw.split('/').slice(1)
    if (segs.length < 3 || segs.some((x) => x === '')) continue
    const key = segs.map((x) => (/\$\{|^\{|^:/.test(x) ? '*' : x)).join('/')
    const prefix = !!m[3] || key.endsWith('*')
    const k = (prefix ? '^' : '') + key
    if (!seen.has(k)) seen.set(k, [])
    seen.get(k).push(relative(root, f))
  }
}
const matches = (want) => {
  const prefix = want.startsWith('^')
  const w = want.replace(/^\^/, '').split('/')
  return known.some((k) => (prefix ? k.length >= w.length : k.length === w.length) && w.every((seg, i) => seg === '*' || k[i].startsWith('{') || k[i] === seg))
}
const bad = [...seen].filter(([k]) => !matches(k))
for (const [k, fs] of bad) console.error(`check-routes: no node route for /${k.replace(/^\^/, '')}${k.startsWith('^') ? '/…' : ''}  (${[...new Set(fs)].join(', ')})`)
console.log(`check-routes: ${seen.size} distinct console paths, ${bad.length} unknown`)
if (bad.length) process.exit(1)
