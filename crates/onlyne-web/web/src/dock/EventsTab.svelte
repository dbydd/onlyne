<script lang="ts">
  // The stream's tail, newest first. Nothing here is clickable: an event is a
  // record of what happened, not a thing to act on, and the rows that can be
  // acted on are the ones the ledger and the faults panel hold.
  //
  // The arrival time is the one thing this panel cannot always read. A frame
  // carries the whole tail and no clock on its entries, so a tail this tab
  // first saw has no stamp at all; the store leaves those at zero rather than
  // dating them now, and a zero is drawn as nothing rather than as a lie.
  import Lightning from 'phosphor-svelte/lib/Lightning';
  import MagnifyingGlass from 'phosphor-svelte/lib/MagnifyingGlass';
  import { plural } from '../lib/format';
  import { describeEvent } from '../lib/model';
  import { cluster } from '../lib/state/cluster.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import { tailKinds } from './rows';

  /// One class at a time, because the tail is read as "what is happening" and a
  /// stack of filters over a 128-line stream hides more than it shows.
  let kind = $state('');

  /// Stamped before filtering and reversed, so a line keeps the arrival time of
  /// its own position in the tail rather than of the position it ended up in.
  const lines = $derived(
    cluster.events.map((event, index) => ({ line: describeEvent(event), at: cluster.stamps[index] ?? 0 })).reverse(),
  );
  const shown = $derived(lines.filter((entry) => kind === '' || entry.line.kinds === kind));
  const kinds = $derived(tailKinds(cluster.events));
</script>

<div class="controls">
  {#if kinds.length > 0}
    <div class="seg" role="group" aria-label="event class">
      <button aria-pressed={kind === ''} onclick={() => (kind = '')}>all</button>
      {#each kinds as entry (entry.kind)}
        <button aria-pressed={kind === entry.kind} onclick={() => (kind = kind === entry.kind ? '' : entry.kind)}>
          {entry.kind}<span class="tally num">{entry.count}</span>
        </button>
      {/each}
    </div>
  {/if}
  <div class="spacer"></div>
  <span class="held num">{shown.length}</span>
  <!-- The count is what a screen reader is told when something arrives; the
       list itself is not live, because announcing every line of a stream on
       every frame is noise rather than news. -->
  <span class="sr-only" aria-live="polite">{plural(shown.length, 'event')} in the tail</span>
</div>

<div class="rows">
  {#if shown.length > 0}
    <ul>
      <!-- Unkeyed on purpose: the tail grows at the front, so a line's position
           is its identity and nothing here is ever reordered. -->
      {#each shown as entry}
        <li data-tone={entry.line.tone}>
          <span class="mark"></span>
          <span class="chip">{entry.line.kinds}</span>
          <span class="text trunc">{entry.line.text}</span>
          {#if entry.at > 0}
            <Ago at={entry.at} />
          {/if}
        </li>
      {/each}
    </ul>
  {:else if kind !== ''}
    <Empty icon={MagnifyingGlass} title="No {kind} event in the tail" hint="The tail holds the last events this cluster published.">
      <button class="btn sm" onclick={() => (kind = '')}>Show every event</button>
    </Empty>
  {:else}
    <Empty
      icon={Lightning}
      title="Nothing has happened yet on this stream"
      hint="Events arrive here as the cluster publishes them. The tail is cut to the last few hundred."
    />
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
  .tally {
    color: var(--ink-4);
    font-size: 10px;
  }
  .spacer {
    flex: 1;
  }
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
    display: grid;
    grid-template-columns: 7px auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--s-2);
    height: 24px;
    padding: 0 var(--s-3);
    border-bottom: 1px solid var(--line-soft);
  }
  .mark {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--tone, var(--ink-4));
  }
  .text {
    color: var(--ink-2);
  }
</style>