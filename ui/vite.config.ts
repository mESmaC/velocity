import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ['VITE_', 'TAURI_ENV_*'],
  // The packaged font-src policy permits same-origin files, not data URLs.
  build: { target: 'es2022', assetsInlineLimit: 0 },
});
