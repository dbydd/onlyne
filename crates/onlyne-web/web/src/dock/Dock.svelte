<script lang="ts">
  // The panel under the canvas: a row of tabs, one body, and a grip on the top
  // edge. Nothing here holds state of its own. Which tab is open, whether the
  // panel is folded and how tall it stands are all the operator's arrangement,
  // so every one of them is written through `ui.setDock` and the store keeps
  // the memory across a reload.
  //
  // The tab counts are what the row is for. The ledger's is the rows its scope
  // actually holds, so the number moves with the switch and the tab cannot
  // claim work the operator is not looking at; the faults count wears the fail
  // hue only while there is something to acknowledge.
  import CaretDown from 'phosphor-svelte/lib/CaretDown';
  import CaretUp from 'phosphor-svelte/lib/CaretUp';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import type { DockTab } from '../lib/state/ui.svelte';
  import EventsTab from './EventsTab.svelte';
  import FaultsTab from './FaultsTab.svelte';
  import LedgerTab from './LedgerTab.svelte';
  import { liveSessions, scopeLedger } from './rows';
  import SessionsTab from './SessionsTab.svelte';

  /// The dock's own bounds. The store clamps to them as well; clamping here
  /// too keeps a drag that leaves the window from asking for a height the
  /// panel cannot hold.
  const MIN_H = 120;
  const MAX_H = 560;
  const STEP = 16;

  const TABS: { tab: DockTab; label: string }[] = [
    { tab: 'ledger', label: 'Ledger' },
    { tab: 'sessions', label: 'Sessions' },
    { tab: 'events', label: 'Events' },
    { tab: 'faults', label: 'Faults' },
  ];

  const counts = $derived.by(() => ({
    ledger: scopeLedger(cluster.deliveries, ui.ledgerScope).length,
    sessions: liveSessions(cluster.sessions).length,
    events: cluster.events.length,
    faults: cluster.openFaults.length,
  }));

  const active = $derived(TABS.find((entry) => entry.tab === ui.dockTab) ?? TABS[0]);
  /// Folded, the panel is its tab row and nothing else, so the canvas gets the
  /// height back without a second control to learn.
  const height = $derived(ui.dockOpen ? `calc(${ui.dockHeight}px + var(--dock-tabs-h))` : 'var(--dock-tabs-h)');

  let list = $state<HTMLElement | null>(null);
  let grip = $state<HTMLElement | null>(null);
  let resizing = $state(false);
  let startY = 0;
  let startHeight = 0;

  /// Arrowing along the tab row is how a keyboard reaches the other three
  /// panels without tabbing through them one at a time.
  function onTabKey(event: KeyboardEvent) {
    const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
    if (step === 0 || !list) return;
    event.preventDefault();
    const index = TABS.findIndex((entry) => entry.tab === ui.dockTab);
    const next = (index + step + TABS.length) % TABS.length;
    ui.setDock({ tab: TABS[next].tab });
    list.querySelectorAll<HTMLButtonElement>('button')[next]?.focus();
  }

  function resizeTo(height: number) {
    ui.setDock({ height: Math.min(MAX_H, Math.max(MIN_H, height)) });
  }

  function grab(event: PointerEvent) {
    if (event.button !== 0 || !grip) return;
    grip.setPointerCapture(event.pointerId);
    startY = event.clientY;
    startHeight = ui.dockHeight;
    resizing = true;
    event.preventDefault();
  }

  function drag(event: PointerEvent) {
    if (!grip?.hasPointerCapture(event.pointerId)) return;
    resizeTo(startHeight - (event.clientY - startY));
  }

  function onGripKey(event: KeyboardEvent) {
    if (event.key === 'ArrowUp') resizeTo(ui.dockHeight + STEP);
    else if (event.key === 'ArrowDown') resizeTo(ui.dockHeight - STEP);
    else return;
    event.preventDefault();
  }
</script>

<section class="dock" class:resizing aria-label="cluster panels" style:height>
  {#if ui.dockOpen}
    <div
      class="grip"
      bind:this={grip}
      role="slider"
      aria-label="dock height in pixels"
      aria-orientation="horizontal"
      aria-valuemin={MIN_H}
      aria-valuemax={MAX_H}
      aria-valuenow={ui.dockHeight}
      tabindex="0"
      onpointerdown={grab}
      onpointermove={drag}
      onlostpointercapture={() => (resizing = false)}
      onkeydown={onGripKey}
    ></div>
  {/if}

  <header class="tabs">
    <!-- The tablist holds the tabs and nothing else: the fold control sits
         beside it, because a button inside a tablist is a control the arrow
         keys would walk over. -->
    <div
      class="list"
      bind:this={list}
      role="tablist"
      aria-label="cluster panels"
      tabindex="-1"
      onkeydown={onTabKey}
    >
      {#each TABS as entry (entry.tab)}
        {@const count = counts[entry.tab]}
        <button
          class="tab"
          role="tab"
          class:active={ui.dockTab === entry.tab}
          aria-selected={ui.dockTab === entry.tab}
          aria-controls={ui.dockOpen ? 'dock-body' : undefined}
          data-tone={entry.tab === 'faults' && count > 0 ? 'fail' : undefined}
          onclick={() => ui.setDock({ tab: entry.tab })}
        >
          <span>{entry.label}</span>
          <span class="count num">{count}</span>
        </button>
      {/each}
    </div>

    <div class="spacer"></div>

    <button
      class="btn icon ghost sm"
      aria-label={ui.dockOpen ? 'collapse the dock' : 'expand the dock'}
      aria-expanded={ui.dockOpen}
      aria-controls={ui.dockOpen ? 'dock-body' : undefined}
      onclick={() => ui.setDock({ open: !ui.dockOpen })}
    >
      {#if ui.dockOpen}
        <CaretDown size="12" weight="bold" />
      {:else}
        <CaretUp size="12" weight="bold" />
      {/if}
    </button>
  </header>

  {#if ui.dockOpen}
    <div class="body" id="dock-body" role="tabpanel" aria-label={active.label} tabindex="-1">
      {#if ui.dockTab === 'ledger'}
        <LedgerTab />
      {:else if ui.dockTab === 'sessions'}
        <SessionsTab />
      {:else if ui.dockTab === 'events'}
        <EventsTab />
      {:else}
        <FaultsTab />
      {/if}
    </div>
  {/if}
</section>

<style>
  /* The switch controls the tab bodies share. Global because the bodies are
     separate components, and a control that looks like one thing in one tab
     and another in the next reads as two controls. */
  :global(.seg) {
    display: inline-flex;
    flex: none;
    padding: 2px;
    border-radius: var(--r-2);
    box-shadow: inset 0 0 0 1px var(--line-soft);
  }
  :global(.seg > button) {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 20px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--r-1);
    background: transparent;
    color: var(--ink-3);
    font-size: var(--fs-11);
    font-weight: 500;
    line-height: 1;
    white-space: nowrap;
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  :global(.seg > button:hover) {
    color: var(--ink);
  }
  /* The chosen word wears bone, the way selection wears bone everywhere else
     on this surface: it is a choice, not a status. */
  :global(.seg > button[aria-pressed='true']) {
    background: var(--raised);
    box-shadow: inset 0 0 0 1px var(--line);
    color: var(--ink);
  }
  :global(.seg > button:focus-visible) {
    outline: 2px solid var(--focus);
    outline-offset: -1px;
  }
  .dock {
    position: relative;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--panel);
    border-top: 1px solid var(--line);
    transition: height var(--t-med) var(--ease);
  }
  /* A transition under a drag reads as lag, so the drag turns it off. */
  .dock.resizing {
    transition: none;
  }
  /* The grip sits on the seam rather than inside it, so the whole hairline is
     grabbable and the panel still measures what it is told to. */
  .grip {
    position: absolute;
    top: -4px;
    left: 0;
    right: 0;
    height: 8px;
    z-index: 1;
    cursor: ns-resize;
  }
  .grip:hover,
  .grip:focus-visible {
    background: var(--line-strong);
  }
  .tabs {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--s-1);
    height: var(--dock-tabs-h);
    padding: 0 var(--s-1) 0 var(--s-2);
    border-bottom: 1px solid var(--line);
  }
  .list {
    display: flex;
    align-items: center;
    gap: var(--s-1);
    min-width: 0;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--r-2);
    background: transparent;
    color: var(--ink-3);
    font-size: var(--fs-12);
    font-weight: 500;
    white-space: nowrap;
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .tab:hover {
    background: var(--raised);
    color: var(--ink);
  }
  /* Bone, never a hue: which tab is open is a choice, not a status. */
  .tab.active {
    background: var(--raised);
    box-shadow: inset 0 0 0 1px var(--line);
    color: var(--ink);
  }
  /* A count rides the hue of the thing it counts, and only when that thing
     carries one, so the faults tab goes red when there is work to acknowledge
     and stays quiet when there is none. */
  .count {
    color: var(--tone, var(--ink-4));
  }
  .spacer {
    flex: 1;
  }
  /* The body scrolls; the panel itself never grows the page. */
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .body:focus-visible {
    outline-offset: -2px;
  }
</style>