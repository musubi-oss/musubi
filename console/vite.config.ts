import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// served by musubi-server under /console; in dev, /v1 is proxied to a local node
export default defineConfig({
  base: '/console/',
  plugins: [vue()],
  server: { port: 5174, proxy: { '/v1': 'http://127.0.0.1:7350' } },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false },
})
