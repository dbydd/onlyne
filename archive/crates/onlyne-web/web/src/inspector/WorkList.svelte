<script lang="ts">
  // The board's live work, arranged in the reducer's columns.
  //
  // This panel reads what is moving; the ledger dock is where settled history
  // lives. So the two settled columns stop at their eight most recent rows and
  // say how many more there are, because a board list that grows without bound
  // is a list an operator has to scroll past to find the row that is stuck.
  import Empty from '../lib/ui/Empty.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import ListBullets from 'phosphor-svelte/lib/ListBullets';
  import type { BoardCard, BoardColumn } from '../gen/Board';
  import { COLUMN_ORDER, workOf, workTitle } from '../lib/model';
  import { cluster } from '../lib/state/cluster.svelte';
  import { short } from '../lib/format';
  import { ui } from '../lib/state/ui.svelte';

  interface Props {
    role: string;
  }

  let { role }: Props = $props();

  /// Settled work is history, and history has a home. Eight is where the two
  /// settled columns stop competing with the live ones for the same panel.
  const SHOWN = 8;

  const COLUMN_WORDS: Record<BoardColumn, string> = {
    running: 'running',
    waiting: 'waiting',
    queued: 'queued',
    done: 'done',
    failed_or_blocked: 'failed or blocked',
  };

  interface Group {
    column: BoardColumn;
    rows: BoardCard[];
    /// How many settled rows are behind the fold.
    hidden: number;
  }

  let expanded = $state<Record<string, boolean>>({});

  const cards = $derived(cluster.boardCards(role));
  const groups = $derived.by<Group[]>(() => {
    const byColumn = new Map<BoardColumn, BoardCard[]>();
    for (const card of cards) {
      const group = byColumn.get(card.column);
      if (group) group.push(card);
      else byColumn.set(card.column, [card]);
    }
    return COLUMN_ORDER.map((column) => {
      const rows = byColumn.get(column) ?? [];
      const settled = column === 'done' || column === 'failed_or_blocked';
      const open = expanded[column] === true;
      const hidden = settled && !open ? Math.max(0, rows.length - SHOWN) : 0;
      return { column, rows: hidden > 0 ? rows.slice(0, SHOWN) : rows, hidden };
    }).filter((group) => group.rows.length > 0);
  });

  /// The moment a row is read by, when the payload carries one. A stream-born
  /// row has no clock of its own until the next snapshot, and the empty span
  /// reads as "not yet known" rather than as a wrong time.
  function atOf(card: BoardCard): number | null {
    const delivery = cluster.deliveryById.get(card.msg_id);
    if (!delivery) return null;
    const at = cluster.timeOf(delivery);
    return at > 0 ? at : null;
  }

  function open(column: BoardColumn) {
    expanded = { ...expanded, [column]: !expanded[column] };
  }
</script>

{#if groups.length === 0}
  <Empty icon={ListBullets} title="Nothing on this board" hint="A task sent to this role lands here." />
{:else}
  <div class="work">
    {#each groups as group (group.column)}
      <div class="group">
        <div class="ghead">
          <span class="glabel">{COLUMN_WORDS[group.column]}</span>
          <span class="gcount num">{group.rows.length}</span>
        </div>
        <ul>
          {#each group.rows as card (card.msg_id)}
            {@const reading = workOf(card)}
            {@const at = atOf(card)}
            <li>
              <button class="row" onclick={() => ui.select({ kind: 'task', msgId: card.msg_id, role }, false)}>
                <span class="dot" data-tone={reading.tone}></span>
                <span class="head trunc" title={workTitle(card)}>{workTitle(card)}</span>
                {#if card.from}
                  <span class="from trunc">{card.from}</span>
                {/if}
                {#if card.hop != null}
                  <span class="chip line" title="hop">h{card.hop}</span>
                {/if}
                {#if card.family}
                  <span class="chip mono" title="family">{short(card.family, 8)}</span>
                {/if}
                {#if at != null}
                  <Ago {at} />
                {/if}
              </button>
            </li>
          {/each}
        </ul>
        {#if group.hidden > 0}
          <button class="more" onclick={() => open(group.column)}>
            show all <span class="num">{group.rows.length + group.hidden}</span>
          </button>
        {:else if group.column === 'done' || group.column === 'failed_or_blocked'}
          {#if expanded[group.column]}
            <button class="more" onclick={() => open(group.column)}>show less</button>
          {/if}
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .work {
    display: grid;
    gap: var(--s-3);
  }
  .ghead {
    display: flex;
    align-items: baseline;
    gap: 6px;
    margin-bottom: 3px;
  }
  .glabel {
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .gcount {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-4);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 6px;
    border: 0;
    border-radius: var(--r-2);
    background: none;
    text-align: left;
    transition: background var(--t-fast) var(--ease);
  }
  .row:hover {
    background: var(--raised);
  }
  .head {
    flex: 1;
    min-width: 0;
    font-size: var(--fs-12);
    color: var(--ink);
  }
  .from {
    flex: none;
    max-width: 11ch;
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .row :global(.ago) {
    flex: none;
  }
  .more {
    margin-top: 3px;
    padding: 3px 6px;
    border: 0;
    border-radius: var(--r-2);
    background: none;
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .more:hover {
    background: var(--raised);
    color: var(--ink-2);
  }
</style>