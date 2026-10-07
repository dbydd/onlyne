import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// The bundle lands in `assets/dist/`, which `rust-embed` embeds and the
// crate's `build.rs` demands (`docs/v2-PLAN.md` §"网页前端 onlyne-web").
//
// `assetsInlineLimit` is a security-surface decision, not a size tweak. The
// guard in `src/lib.rs` admits a request only with the startup token, and the
// token is spliced into the document's own `/assets/` references and nowhere
// else. A font named by a `url()` inside the stylesheet would be fetched with
// no token and refused 401, so the two Geist subsets travel as `data:` URIs
// inside the CSS. 64 KiB clears the larger subset (29 KiB) with room, and no
// other asset in the bundle is that big.
export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: '../assets/dist',
    emptyOutDir: true,
    target: 'es2022',
    assetsInlineLimit: 65536,
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8787',
    },
  },
});
