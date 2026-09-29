<script lang="ts">
  // The app's one shell: the cluster header, the mode the operator chose
  // (or the density rule chose for them), the boards, and the notice line.
  import { onMount } from 'svelte';
  import GridView from './lib/components/GridView.svelte';
  import GraphView from './lib/components/GraphView.svelte';
  import { connect, app } from './lib/store.svelte';
  import { degraded, routesOf } from './lib/layout';
  import type { ClusterSummary } from './gen/View';

  let token = $state('');
  let dense = $state(false);

  $effect(() => {
    dense = degraded(app.boards);
  });

  onMount(() => {
    const given = new URLSearchParams(window.location.search).get('token');
    if (given) {
      token = given;
      connect(given);
    }
  });

  const cluster = $derived((app.view.cluster ?? {}) as ClusterSummary);
  // The header counts the edges the graph draws, not the server's `[[route]]`
  // table. The contract calls `allowed_targets` the allowed route
  // (`docs/v2-CONTRACT.md` §Slice 10), so a header reading the other table said
  // "0 routes" above a graph with two edges drawn on it.
  const routes = $derived(routesOf(app.boards).length);
  const effective = $derived(dense ? 'grid' : app.mode);
  const lastEvent = $derived(app.view.event_tail?.[0]);
</script>

{#if !token}
  <main class="gate">
    <h1>onlyne</h1>
    <p>
      this surface needs its startup token — open the URL the server printed
      (<code>http://127.0.0.1:&lt;port&gt;/?token=…</code>)
    </p>
  </main>
{:else}
  <header class="bar">
    <h1>onlyne{cluster.cluster ? ` · ${cluster.cluster}` : ''}</h1>
    <span class="status" class:live={app.connected}>
      {app.view.stale ? 'catching up…' : app.link}
    </span>
    <span class="meta">
      {cluster.connected_roles ?? 0}/{cluster.role_count ?? 0} roles ·
      {cluster.routes ?? 0} routes
    </span>
    <span class="modes">
      <button class:active={effective === 'graph'} onclick={() => (app.mode = 'graph')}>graph</button>
      <button class:active={effective === 'grid'} onclick={() => (app.mode = 'grid')}>boards</button>
    </span>
    {#if dense}
      <span class="dense">dense graph — boards as grid, edges hidden</span>
    {/if}
  </header>

  <main class="content" class:graph={effective === 'graph'}>
    {#if effective === 'graph' && app.boards.length > 0}
      <GraphView />
    {:else}
      <GridView />
    {/if}
  </main>

  <footer class="bar foot">
    <span class="notice">{app.notice}</span>
    {#if lastEvent}
      <span class="tail">
        last event · seq {lastEvent.seq ?? ''} {lastEvent.type ?? ''}
      </span>
    {/if}
    {#if Object.keys(app.view.faults ?? {}).length > 0}
      <span class="faults">{Object.keys(app.view.faults ?? {}).length} open faults</span>
    {/if}
    <span class="cursor">cursor {app.cursor}</span>
  </footer>
{/if}
