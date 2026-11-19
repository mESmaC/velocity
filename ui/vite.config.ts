import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// On Windows, Vite probes mapped drives by running `net use` through cmd.exe and keeps
// the build process alive until that child exits (#44, #500). cmd.exe looks `net` up
// through PATH and PATHEXT. Some installers add `.DLL` and `.CPL` to PATHEXT, and a
// JDK's `bin` folder holds a `net.dll`; when that folder comes before System32,
// cmd.exe "runs" net.dll, hidden and without output, and never exits, so the build
// prints "built in" and then hangs. Keeping only program extensions in PATHEXT makes
// `net` resolve to net.exe without depending on PATH order. This must run before
// Vite resolves its first path, which is why the package scripts load this file
// with `--configLoader native`: the default loader bundles the config and starts the
// probe while resolving its imports.
// The UI has no Node type definitions; declare the two fields used here.
declare const process: {
  platform: string;
  env: Record<string, string | undefined>;
};
const programExtensions = new Set(['.COM', '.EXE', '.BAT', '.CMD']);
if (process.platform === 'win32' && process.env.PATHEXT) {
  process.env.PATHEXT = process.env.PATHEXT.split(';')
    .filter((extension: string) =>
      programExtensions.has(extension.trim().toUpperCase()),
    )
    .join(';');
}

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ['VITE_', 'TAURI_ENV_*'],
  // The packaged font-src policy permits same-origin files, not data URLs.
  build: { target: 'es2022', assetsInlineLimit: 0 },
});
