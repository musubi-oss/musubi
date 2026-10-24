// Refreshes openapi.json from a running node: MUSUBI_DEV_API (default http://127.0.0.1:7350),
// MUSUBI_DEV_KEY (default defaultkey). Strips the version so the snapshot only changes with the API.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)))
const api = process.env.MUSUBI_DEV_API ?? 'http://127.0.0.1:7350'
const r = await fetch(api + '/openapi.json', { headers: { 'x-musubi-server-key': process.env.MUSUBI_DEV_KEY ?? 'defaultkey' } })
if (!r.ok) throw new Error(`${api}/openapi.json → ${r.status}`)
const doc = await r.json()
if (doc.info) doc.info.version = '0.0.0'
fs.writeFileSync(path.join(root, 'openapi.json'), JSON.stringify(doc, null, 2) + '\n')
console.log(`sync-api: ${Object.keys(doc.paths).length} paths written to openapi.json`)
