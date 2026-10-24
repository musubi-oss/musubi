import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { tanstackRouter } from '@tanstack/router-plugin/vite'
import { fileURLToPath, URL } from 'node:url'

// Served by musubi-server under /console (embedded dist). In dev, /v1, /metrics and /openapi.json
// are proxied to a local node (MUSUBI_DEV_API overrides the default dev node).
const api = process.env.MUSUBI_DEV_API ?? 'http://127.0.0.1:7350'
export default defineConfig({
  base: '/console/',
  plugins: [
    tanstackRouter({ target: 'react', autoCodeSplitting: true, routesDirectory: './src/routes', generatedRouteTree: './src/routeTree.gen.ts' }),
    react(),
    tailwindcss(),
  ],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  server: { port: 5175, proxy: { '/v1': api, '/metrics': api, '/openapi.json': api } },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false },
  test: { environment: 'jsdom', globals: true },
})
