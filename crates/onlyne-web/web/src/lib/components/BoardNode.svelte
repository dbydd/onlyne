<script lang="ts">
  // One role, drawn as a board on the graph: what it is doing now, and what is
  // waiting for it. The two handles a dragged line starts and lands on are the
  // only affordance that is not a reading.
  //
  // The board carries live work and nothing else. A delivery that settled while
  // this browser was watching stays for a moment and then goes, so a finish is
  // something you can see happen rather than something you notice missing; one
  // that was already settled when the page loaded lingers the same minute, which
  // is the cost of anchoring the fade to a transition the browser witnessed
  // rather than to a timestamp the wire does not carry. The ledger is where
  // settled work is read.
  import { Handle, Position } from '@xyflow/svelte';
  import type { BoardCard } from '../gen/BoardCard';
  import type { Board } from '../gen/Board';
  import { app } from '../store.svelte';

  interface Props {
    id: string;
    data: { board: Board };
    selected?: boolean;
  }

  let { id, data, selected = false }: Props = $props();

  /// How long a settled delivery stays before the board lets it go.
  const LINGER_MS = 60_000;

  /// The columns that are still work: a delivery in the queue, one a session is
  /// running, and one that waits on something outside the delivery.
  const LIVE = new Set(['queued', 'running', 'waiting']);
  /// The two the board lists under their own headings.
  const PENDING = new Set(['running', 'waiting']);

  /// Cards this board has watched settle, and the ones it has since let go. One
  /// timer per card rather than a heartbeat for the page: a settled card is a
  /// single event, and a clock that ticks every second to notice it puts a
  /// re-render behind every board on the canvas for as long as the tab is open.
  const settled = $state<Record<string, 'fading' | 'gone'>>({});

  function watch(card: BoardCard): void {
    if (LIVE.has(card.column)) {
      if (card.msg_id in settled) delete settled[card.msg_id];
      return;
    }
    if (card.msg_id in settled) return;
    settled[card.msg_id] = 'fading';
    setTimeout(() => {
      settled[card.msg_id] = 'gone';
    }, LINGER_MS);
  }

  $effect(() => {
    for (const card of data.board.cards ?? []) watch(card);
  });

  const rows = $derived(
    (data.board.cards ?? []).filter((card) => settled[card.msg_id] !== 'gone'),
  );
  const pending = $derived(rows.filter((card) => PENDING.has(card.column)));
  const queue = $derived(rows.filter((card) => card.column === 'queued'));
  const isSettled = $derived((card: BoardCard) => settled[card.msg_id] === 'fading');

  const families = $derived(
    new Set((data.board.cards ?? []).map((card) => card.family).filter(Boolean)).size,
  );
  const highlight = $derived(families > 0 && [...(data.board.cards ?? [])].some((c) => c.family === app.selectedFamily));

  /// The one line that says what this delivery is: the head the ledger kept when
  /// there is one, and the kind and sender when there is not.
  function label(card: BoardCard): string {
    const head = (card.out_head ?? '').trim();
    if (head) return head;
    return `${card.kind} from ${card.from ?? '—'}`;
  }
</script>

<section class="node" class:selected class:operator={data.board.operator} data-node={id}>
  <Handle type="target" position={Position.Left} />
  <header>
    <span class="dot {data.board.presence}"></span>
    <strong>{data.board.operator ? `operator (${data.board.role})` : data.board.role}</strong>
    <span class="sessions">
      {data.board.counts?.busy ?? 0} busy · {data.board.counts?.idle ?? 0} idle
    </span>
  </header>

  {#if pending.length === 0 && queue.length === 0}
    <p class="empty">nothing in flight</p>
  {:else}
    {#if pending.length > 0}
      <p class="group">pending</p>
      <ul>
        {#each pending as card (card.msg_id)}
          <li class:settling={isSettled(card)}>
            <span class="label">{label(card)}</span>
            <span class="meta">{#if card.hop != null}hop {card.hop}{/if}</span>
          </li>
        {/each}
      </ul>
    {/if}
    {#if queue.length > 0}
      <p class="group">queue · {queue.length}</p>
      <ul>
        {#each queue as card (card.msg_id)}
          <li class:settling={isSettled(card)}>
            <span class="label">{label(card)}</span>
            <span class="meta">from {card.from ?? '—'}</span>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  {#if highlight}
    <p class="families">{families} famil{families === 1 ? 'y' : 'ies'} here</p>
  {/if}
  <Handle type="source" position={Position.Right} />
</section>

<style>
  section.node {
    width: 260px;
    max-height: 320px;
    overflow: hidden;
    background: var(--panel, #16181d);
    border: 1px solid #2a2e37;
    border-radius: 8px;
    padding: 8px 10px;
    font-size: 12px;
  }
  section.node.selected {
    border-color: var(--accent, #4c8dff);
  }
  header {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  header strong {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sessions {
    opacity: 0.55;
    font-size: 11px;
    white-space: nowrap;
  }
  p.group {
    margin: 6px 0 2px;
    opacity: 0.5;
    font-size: 11px;
    text-transform: lowercase;
  }
  p.empty {
    margin: 8px 0 2px;
    opacity: 0.4;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  li {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  /* The fade is a duration on the card rather than a clock read per frame, so
     the board costs nothing until something on it settles. */
  li.settling {
    animation: settle-away 60s linear forwards;
  }
  @keyframes settle-away {
    from {
      opacity: 1;
    }
    to {
      opacity: 0.12;
    }
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    opacity: 0.5;
    font-size: 11px;
    white-space: nowrap;
    flex: none;
  }
  p.families {
    margin: 6px 0 0;
    color: var(--accent, #4c8dff);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #666;
    flex: none;
  }
  .dot.online {
    background: #3fb961;
  }
  .dot.draining {
    background: #d9a038;
  }
</style>
