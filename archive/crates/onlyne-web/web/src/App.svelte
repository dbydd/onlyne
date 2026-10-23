<script lang="ts">
  // The shell: a header, the graph with its dock under it, and the inspector
  // beside both. Everything here is one of those four, so the file stays the
  // wiring and the reading order.
  import IconContext from 'phosphor-svelte/lib/IconContext';
  import Gate from './chrome/Gate.svelte';
  import Palette from './chrome/Palette.svelte';
  import Toasts from './chrome/Toasts.svelte';
  import TopBar from './chrome/TopBar.svelte';
  import Dock from './dock/Dock.svelte';
  import GraphCanvas from './graph/GraphCanvas.svelte';
  import { cluster } from './lib/state/cluster.svelte';
  import { spec } from './lib/state/spec.svelte';
  import { ui } from './lib/state/ui.svelte';
  import Inspector from './shell/Inspector.svelte';
  import Shortcuts from './shell/Shortcuts.svelte';

  /// One stroke weight and one size for every glyph on the surface.
  const ICONS = { size: '1.05em', weight: 'regular' } as const;

  /// The token rides the query string of the URL the process printed. It stays
  /// in the address bar on purpose: a reload has no other way to get it, and
  /// the bind is loopback unless the operator widened it (`--bind`).
  const token = new URLSearchParams(window.location.search).get('token') ?? '';
  if (token) cluster.connect(token);

  /// The spec cache: read once, then again whenever the cluster says the file
  /// moved — a typed edit from this surface, or someone editing `spec.toml`.
  $effect(() => {
    const hash = cluster.view.cluster?.spec_hash ?? '';
    if (cluster.live && hash !== '' && !spec.current(hash)) void spec.refresh(cluster.token, hash);
  });

  /// The tab's title carries the two facts worth reading from another window.
  $effect(() => {
    const name = cluster.view.cluster?.cluster ?? '';
    const faults = cluster.openFaults.length;
    document.title = ['onlyne', name, faults > 0 ? `${faults} faults` : ''].filter(Boolean).join(' · ');
  });

  /// Nothing has arrived yet: the frame draws its own shape rather than an
  /// empty canvas that looks like a cluster with no roles.
  const booting = $derived(cluster.lastFrameAt === 0);
</script>

{#if !token}
  <Gate mode="no-token" detail="" />
{:else if cluster.transport === 'refused'}
  <Gate mode="refused" detail={cluster.detail} />
{:else}
  <IconContext values={ICONS}>
    <div class="app">
      <TopBar />
      <main class="stage">
        <section class="canvas">
          {#if booting}
            <div class="boot" aria-label="connecting">
              <div class="skeleton" style="width: 232px; height: 116px"></div>
              <div class="skeleton" style="width: 232px; height: 116px"></div>
              <div class="skeleton" style="width: 232px; height: 116px"></div>
            </div>
          {:else}
            <GraphCanvas />
          {/if}
        </section>
        <Dock />
      </main>
      <aside class="inspector" class:open={ui.selection !== null} inert={ui.selection === null}>
        <div class="inner">
          <Inspector />
        </div>
      </aside>
    </div>
    <Palette />
    <Toasts />
    <Shortcuts />
  </IconContext>
{/if}

<style>
  .app {
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-rows: var(--topbar-h) 1fr;
    height: 100%;
    overflow: hidden;
  }
  .app :global(> header) {
    grid-column: 1 / -1;
  }
  .stage {
    display: grid;
    grid-template-rows: 1fr auto;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  .canvas {
    position: relative;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
  }
  .boot {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--s-4);
    opacity: 0.5;
  }
  /* The panel slides; the column width is what moves, so the canvas resizes
     with it and nothing overlaps. */
  .inspector {
    width: 0;
    overflow: hidden;
    border-left: 1px solid transparent;
    background: var(--panel);
    transition:
      width var(--t-med) var(--ease),
      border-color var(--t-med) var(--ease);
  }
  .inspector.open {
    width: var(--inspector-w);
    border-left-color: var(--line);
  }
  .inner {
    width: var(--inspector-w);
    height: 100%;
  }
  @media (max-width: 900px) {
    .inspector {
      position: fixed;
      top: var(--topbar-h);
      right: 0;
      bottom: 0;
      z-index: var(--z-inspector);
      box-shadow: var(--shadow-pop);
      width: min(var(--inspector-w), 92vw);
      transform: translateX(100%);
      transition: transform var(--t-med) var(--ease);
    }
    .inspector.open {
      width: min(var(--inspector-w), 92vw);
      transform: none;
    }
    .inner {
      width: 100%;
    }
  }
</style>
