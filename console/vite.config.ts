import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// served by musubi-server under /console; in dev, /v1 and /metrics are proxied to a local node
// (MUSUBI_DEV_API overrides the default dev node)
const api = process.env.MUSUBI_DEV_API ?? 'http://127.0.0.1:7350'
export default defineConfig({
  base: '/console/',
  plugins: [vue()],
  server: { port: 5174, proxy: { '/v1': api, '/metrics': api } },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false },
})
