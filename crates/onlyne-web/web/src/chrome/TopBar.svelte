<script lang="ts">
  // The header, read as a sentence: which cluster this is, whether the link is
  // up, and how much of it is busy. A figure the payload does not carry is
  // left out rather than shown as a zero, so the bar never states a fact the
  // server did not send. A role count is the exception: how many boards there
  // are is worth knowing even when none of them is online.
  import ArrowCounterClockwise from 'phosphor-svelte/lib/ArrowCounterClockwise';
  import MagnifyingGlass from 'phosphor-svelte/lib/MagnifyingGlass';
  import SidebarSimple from 'phosphor-svelte/lib/SidebarSimple';
  import Warning from 'phosphor-svelte/lib/Warning';
  import { plural, uptime } from '../lib/format';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';

  /// The header the app grid places: `App.svelte` positions `> header`, so the
  /// element itself has to be the header rather than a div it renders.
  interface Props {
    class?: string;
  }

  let { class: className = '' }: Props = $props();

  const header = $derived(cluster.view.cluster);
  const name = $derived(header?.cluster ?? '');
  const stats = $derived(cluster.stats);
  const routes = $derived(cluster.routes.length);
  const reaches = $derived(cluster.reaches.length);
  const faults = $derived(cluster.openFaults.length);
  const up = $derived(header?.uptime_s ?? null);
  /// The version rides a monospace span of its own, so `v0.3.1` and `1.2.3`
  /// line up with the counts next to it instead of shifting the row.
  const version = $derived((header?.version ?? '').trim());
</script>

<header class="bar {className}">
  <div class="left">
    <span class="mark" aria-hidden="true"></span>
    <span class="wordmark">onlyne</span>
    {#if name}
      <span class="rule" aria-hidden="true"></span>
      <span class="name trunc" title={name}>{name}</span>
    {/if}
  </div>

  <div class="link" data-tone={cluster.status.tone} title={cluster.statusDetail}>
    <span class="dot" class:pulse={cluster.live} aria-hidden="true"></span>
    <span>{cluster.status.label}</span>
  </div>

  <div class="counts">
    <span class="figure">
      <span class="num">{stats.online}</span>
      of <span class="num">{stats.roles}</span> {stats.roles === 1 ? 'role' : 'roles'}
    </span>
    {#if stats.inFlight > 0}
      <span class="figure"><span class="num">{stats.inFlight}</span> in flight</span>
    {/if}
    {#if stats.sessions > 0}
      <span class="figure"><span class="num">{stats.sessions}</span> {stats.sessions === 1 ? 'session' : 'sessions'}</span>
    {/if}
    {#if routes > 0}
      <span class="figure">
        <span class="num">{routes}</span>
        {routes === 1 ? 'route' : 'routes'}
        {#if reaches > 0}<span class="reaches">+{reaches} reaching every role</span>{/if}
      </span>
    {/if}
  </div>

  {#if up != null || version}
    <div class="meta">
      {#if up != null}<span class="dim">up {uptime(up)}</span>{/if}
      {#if version}<span class="mono dim version">{version}</span>{/if}
    </div>
  {/if}

  {#if cluster.stale}
    <span class="chip line catching" data-tone="wait">catching up</span>
  {/if}

  <div class="right">
    <button class="btn icon ghost" type="button" title="Search" aria-label="Search" onclick={() => (ui.paletteOpen = true)}>
      <MagnifyingGlass />
    </button>
    <button
      class="btn icon ghost faults"
      type="button"
      data-tone={faults > 0 ? 'fail' : undefined}
      class:hot={faults > 0}
      title={faults > 0 ? plural(faults, 'open fault') : 'Faults'}
      aria-label={faults > 0 ? `Faults, ${plural(faults, 'open fault')}` : 'Faults'}
      onclick={() => ui.setDock({ tab: 'faults', open: true })}
    >
      <Warning />
      {#if faults > 0}<span class="badge num">{faults}</span>{/if}
    </button>
    <button class="btn icon ghost" type="button" title="Reset layout" aria-label="Reset layout" onclick={() => ui.resetLayout()}>
      <ArrowCounterClockwise />
    </button>
    <button
      class="btn icon ghost"
      type="button"
      title="Dock"
      aria-label="Dock"
      aria-expanded={ui.dockOpen}
      onclick={() => ui.setDock({ open: !ui.dockOpen })}
    >
      <SidebarSimple />
    </button>
  </div>
</header>

<style>
  .bar {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    gap: var(--s-3);
    height: var(--topbar-h);
    padding: 0 var(--s-3);
    border-bottom: 1px solid var(--line-soft);
    background: var(--panel);
  }
  .left {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  /* The mark is a shape rather than an image, so the header carries no request
   * of its own: a favicon-style asset would reach the token guard with no
   * token and be refused. */
  .mark {
    align-self: center;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--bone);
  }
  .wordmark {
    font-size: var(--fs-13);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--ink);
  }
  .rule {
    width: 1px;
    height: 12px;
    background: var(--line);
    align-self: center;
  }
  .name {
    color: var(--ink-2);
    max-width: 22ch;
  }
  .link {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    color: var(--ink-2);
    white-space: nowrap;
  }
  .counts {
    display: flex;
    align-items: baseline;
    gap: var(--s-3);
    min-width: 0;
    padding-left: var(--s-3);
    border-left: 1px solid var(--line-soft);
    overflow: hidden;
  }
  .figure {
    font-size: var(--fs-12);
    color: var(--ink-3);
    white-space: nowrap;
  }
  .figure .num {
    color: var(--ink-2);
  }
  .reaches {
    color: var(--ink-4);
  }
  .meta {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    min-width: 0;
    overflow: hidden;
  }
  .dim {
    font-size: var(--fs-11);
    color: var(--ink-4);
    white-space: nowrap;
  }
  .version {
    font-size: var(--fs-11);
  }
  .catching {
    flex: none;
  }
  .right {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 2px;
    flex: none;
  }
  .btn.icon {
    position: relative;
    color: var(--ink-3);
  }
  .btn.icon:hover {
    color: var(--ink);
  }
  .faults.hot {
    color: var(--fail);
  }
  .badge {
    position: absolute;
    top: 1px;
    right: 1px;
    min-width: 12px;
    height: 12px;
    padding: 0 2px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--r-1);
    background: var(--fail);
    color: var(--bone-ink);
    font-size: 9px;
    line-height: 1;
  }
  /* Under a narrow window the summary is the part that can be lost: the counts
   * and the uptime are what an operator glances at, so the cluster name goes
   * first. */
  @media (max-width: 860px) {
    .name,
    .reaches {
      display: none;
    }
  }
  @media (max-width: 640px) {
    .meta {
      display: none;
    }
  }
</style>
