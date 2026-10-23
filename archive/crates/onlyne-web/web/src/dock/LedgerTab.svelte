<script lang="ts">
  // The delivery axis, newest first. This is the panel that answers "what is
  // owed, and to whom", so it opens on the unsettled rows and offers the whole
  // link and the operator's own receipts one switch away.
  //
  // Three filters sit over the rows and every one of them is answerable: an
  // empty result names the filter that emptied it and offers to put it back,
  // because an empty panel that cannot say why is indistinguishable from a
  // broken one.
  import ArrowRight from 'phosphor-svelte/lib/ArrowRight';
  import MagnifyingGlass from 'phosphor-svelte/lib/MagnifyingGlass';
  import PaperPlaneTilt from 'phosphor-svelte/lib/PaperPlaneTilt';
  import Stack from 'phosphor-svelte/lib/Stack';
  import type { DeliveryView, MsgKind } from '../gen/View';
  import { plural, short } from '../lib/format';
  import { principalName, principalRole, workOf } from '../lib/model';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import type { LedgerScope } from '../lib/state/ui.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import { LEDGER_CAP, capRows, matchesLedgerQuery, rowTitle, scopeLedger } from './rows';

  const SCOPES: { scope: LedgerScope; label: string }[] = [
    { scope: 'live', label: 'Live' },
    { scope: 'all', label: 'All' },
    { scope: 'inbox', label: 'Inbox' },
  ];

  const KINDS: MsgKind[] = ['task', 'completion', 'note', 'control'];

  let kind = $state<MsgKind | ''>('');
  /// How much of the held-back tail this tab is showing, per filter setting.
  /// Keyed by the filters themselves, so changing one cannot leave a stale
  /// "showing more" behind it pointing at rows that are no longer there.
  let expanded = $state<Record<string, number>>({});

  const scoped = $derived(scopeLedger(cluster.deliveries, ui.ledgerScope));
  const signature = $derived([ui.ledgerScope, ui.query, kind].join('\u0000'));
  const kindTally = $derived.by(() => {
    const tally = new Map<MsgKind, number>();
    for (const delivery of scoped) tally.set(delivery.kind, (tally.get(delivery.kind) ?? 0) + 1);
    return tally;
  });
  const rows = $derived(
    scoped.filter(
      (delivery) => (kind === '' || delivery.kind === kind) && matchesLedgerQuery(delivery, ui.query),
    ),
  );
  const paged = $derived(capRows(rows, LEDGER_CAP + (expanded[signature] ?? 0)));

  /// One row's whole reading, taken from the same weighing the canvas card
  /// makes, so a row and its card can never disagree about what a row is.
  function reading(delivery: DeliveryView) {
    return workOf({
      state: delivery.state,
      outcome: delivery.outcome ?? null,
      column: cluster.columnById.get(delivery.msg_id) ?? null,
    });
  }

  function open(delivery: DeliveryView) {
    ui.select({ kind: 'task', msgId: delivery.msg_id, role: principalRole(delivery.to) ?? '' }, true);
  }

  function showMore() {
    expanded = { ...expanded, [signature]: (expanded[signature] ?? 0) + LEDGER_CAP };
  }

  function clearFilters() {
    ui.query = '';
    kind = '';
  }

  /// Which filter emptied the panel, said in the order the operator would reach
  /// for them.
  const emptiedBy = $derived(
    ui.query.trim() !== '' && kind !== ''
      ? `the text filter and the ${kind} kind`
      : ui.query.trim() !== ''
        ? `the text filter (${ui.query.trim()})`
        : kind !== ''
          ? `the ${kind} kind`
          : '',
  );
</script>

<div class="controls">
  <div class="seg" role="group" aria-label="ledger scope">
    {#each SCOPES as entry (entry.scope)}
      <button aria-pressed={ui.ledgerScope === entry.scope} onclick={() => (ui.ledgerScope = entry.scope)}>
        {entry.label}
      </button>
    {/each}
  </div>

  <label class="search">
    <MagnifyingGlass size="12" />
    <span class="sr-only">filter deliveries</span>
    <input class="input" type="search" placeholder="route, title, msg id, task id" bind:value={ui.query} />
  </label>

  <div class="seg" role="group" aria-label="delivery kind">
    <button aria-pressed={kind === ''} onclick={() => (kind = '')}>All kinds</button>
    {#each KINDS as name (name)}
      <button aria-pressed={kind === name} onclick={() => (kind = kind === name ? '' : name)}>
        {name}<span class="tally num">{kindTally.get(name) ?? 0}</span>
      </button>
    {/each}
  </div>

  <div class="spacer"></div>

  <span class="held num">{rows.length}</span>
</div>

<div class="rows">
  {#if paged.shown.length > 0}
    <ul>
      {#each paged.shown as delivery (delivery.msg_id)}
        {@const read = reading(delivery)}
        {@const hop = delivery.hop ?? 0}
        <li data-tone={read.tone}>
          <button
            class="row"
            class:selected={ui.selection?.kind === 'task' && ui.selection.msgId === delivery.msg_id}
            onclick={() => open(delivery)}
          >
            <span class="route">
              <span class="trunc">{principalName(delivery.from)}</span>
              <ArrowRight size="11" />
              <span class="trunc">{principalName(delivery.to)}</span>
            </span>
            <span class="chip">{delivery.kind}</span>
            <span class="chip" data-tone={read.tone}>{read.label}</span>
            {#if delivery.outcome && delivery.outcome !== read.label}
              <!-- Only where the reading's word is the ledger's rather than the
                   verdict's: a rejected or expired row is the one case where the
                   task's own outcome is still worth saying. -->
              <span class="chip" data-tone={read.tone}>{delivery.outcome}</span>
            {:else}
              <span class="slot"></span>
            {/if}
            <span class="title trunc">{rowTitle(delivery)}</span>
            <span class="hop mono num">{hop > 0 ? `h${hop}` : ''}</span>
            {#if delivery.family}
              <span class="chip mono" title="family {delivery.family}">{short(delivery.family)}</span>
            {:else}
              <!-- The empty slots keep every row's columns in the same place. A
                   table whose cells shift row to row cannot be scanned down,
                   which is the only thing a table of deliveries is for. -->
              <span class="slot"></span>
            {/if}
            <Ago at={cluster.timeOf(delivery)} />
          </button>
        </li>
      {/each}
    </ul>

    {#if paged.held > 0}
      <div class="more">
        <button class="btn ghost sm" onclick={showMore}>
          show {LEDGER_CAP} more, {plural(paged.held, 'row')} held back
        </button>
      </div>
    {/if}
  {:else if emptiedBy !== ''}
    <Empty
      icon={MagnifyingGlass}
      title="No delivery matches {emptiedBy}"
      hint="Clear the filters to read every row this scope holds."
    >
      <button class="btn sm" onclick={clearFilters}>Clear filters</button>
    </Empty>
  {:else if ui.ledgerScope === 'live'}
    <!-- An empty live scope is the good state, so it is phrased as one. -->
    <Empty
      icon={Stack}
      title="Nothing is owed right now"
      hint="Every delivery this link holds has settled. Switch to All to read the ones that did."
    >
      <button class="btn sm" onclick={() => (ui.ledgerScope = 'all')}>Show all deliveries</button>
    </Empty>
  {:else if ui.ledgerScope === 'inbox'}
    <Empty
      icon={PaperPlaneTilt}
      title="Nothing has been sent from this surface yet"
      hint="A task written on a board lands here as its receipt, beside the reply it drew."
    />
  {:else}
    <Empty icon={Stack} title="No delivery on this link yet" hint="Rows appear here as the relay enqueues them." />
  {/if}
</div>

<style>
  .controls {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3);
    border-bottom: 1px solid var(--line-soft);
  }
  .search {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 120px;
    max-width: 320px;
    gap: 6px;
    padding-left: 8px;
    border-radius: var(--r-2);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--line);
    color: var(--ink-4);
  }
  .search:hover {
    box-shadow: inset 0 0 0 1px var(--line-strong);
  }
  .search:focus-within {
    box-shadow: inset 0 0 0 1px var(--focus);
  }
  .search :global(.input) {
    padding-left: 0;
    background: none;
    box-shadow: none;
  }
  .tally {
    color: var(--ink-4);
    font-size: 10px;
  }
  .spacer {
    flex: 1;
  }
  /* The count of what the filters hold, held to the far end so it reads as the
     header's own figure rather than as a fourth control. */
  .held {
    color: var(--ink-4);
    font-size: 10.5px;
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
  }
  li {
    border-bottom: 1px solid var(--line-soft);
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) 76px 88px 76px minmax(0, 2fr) 34px 68px 42px;
    align-items: center;
    gap: var(--s-2);
    width: 100%;
    height: 26px;
    padding: 0 var(--s-3) 0 calc(var(--s-3) - 2px);
    border: 0;
    background: transparent;
    text-align: left;
    /* The delivery's own status runs down the leading edge: the column the eye
       scans first is the one that says what state the work is in. */
    box-shadow: inset 2px 0 0 0 var(--tone-line, transparent);
  }
  .row:hover {
    background: var(--raised);
  }
  /* Selection is bone, never a hue, so it cannot be read as a status. */
  .row.selected {
    background: var(--raised);
    box-shadow: inset 2px 0 0 0 var(--ink);
  }
  .route {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    color: var(--ink-2);
    font-size: var(--fs-11);
  }
  .route :global(svg) {
    flex: none;
    color: var(--ink-4);
  }
  .title {
    color: var(--ink-3);
  }
  .hop {
    color: var(--ink-4);
    font-size: 10.5px;
  }
  .slot {
    /* Holds a cell's place when the row has nothing to put in it. */
    display: block;
  }
  .more {
    display: flex;
    justify-content: center;
    padding: var(--s-2);
  }
</style>