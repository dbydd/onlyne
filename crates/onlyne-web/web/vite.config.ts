import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// The bundle lands in `assets/dist/`, which `rust-embed` embeds and the
// crate's `build.rs` demands — the shape `docs/v2-PLAN.md` line 391 draws.
export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: '../assets/dist',
    emptyOutDir: true,
    target: 'es2022',
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8787',
    },
  },
});
