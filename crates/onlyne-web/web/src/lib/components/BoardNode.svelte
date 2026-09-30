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
  //
  // The composer floats above the board rather than growing it. The board's box
  // is fixed — a board that resizes itself rewrites the layout the operator
  // arranged — so the form overlays the entries and goes away when it is done.
  import { Handle, Position } from '@xyflow/svelte';
  import type { BoardCard } from '../gen/BoardCard';
  import type { Board } from '../gen/Board';
  import { app, selectFamily, sendTask } from '../store.svelte';

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

  /// The composer's open state and draft. Per board, not shared: two boards
  /// compose at once without stealing each other's text.
  let composing = $state(false);
  let text = $state('');

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

  function submit(event: SubmitEvent) {
    event.preventDefault();
    const body = text.trim();
    if (!body) return;
    sendTask(data.board.role, body);
    text = '';
    composing = false;
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      composing = false;
      text = '';
    }
  }
</script>

<section class="node" class:selected data-node={id}>
  <Handle type="target" position={Position.Left} />
  <header>
    <span class="dot {data.board.presence}" title={data.board.presence}></span>
    <strong>{data.board.role}</strong>
    <span class="sessions">
      {data.board.counts?.busy ?? 0} busy · {data.board.counts?.idle ?? 0} idle
    </span>
    <button
      class="add"
      class:open={composing}
      title="send a task — the operator speaks"
      aria-label="send a task to {data.board.role}"
      onclick={() => (composing = !composing)}>+</button
    >
  </header>

  <div class="entries">
  {#if pending.length === 0 && queue.length === 0}
    <p class="empty">nothing in flight</p>
  {:else}
    {#if pending.length > 0}
      <p class="group pending">pending</p>
      <ul>
        {#each pending as card (card.msg_id)}
          <li
            class={card.column}
            class:settling={isSettled(card)}
            class:family={!!card.family}
            onclick={() => card.family && selectFamily(card.family)}
          >
            <span class="label">{label(card)}</span>
            <span class="meta">{#if card.hop != null}hop {card.hop}{/if}</span>
          </li>
        {/each}
      </ul>
    {/if}
    {#if queue.length > 0}
      <p class="group queue">queue · {queue.length}</p>
      <ul>
        {#each queue as card (card.msg_id)}
          <li
            class={card.column}
            class:settling={isSettled(card)}
            class:family={!!card.family}
            onclick={() => card.family && selectFamily(card.family)}
          >
            <span class="label">{label(card)}</span>
            <span class="meta">from {card.from ?? '—'}</span>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
  </div>

  {#if composing}
    <form class="compose" onsubmit={submit}>
      <textarea
        bind:value={text}
        rows="3"
        placeholder="task for {data.board.role}…"
        autofocus
      ></textarea>
      <footer>
        <span class="hint">as _supervisor</span>
        <button type="button" class="ghost" onclick={() => ((composing = false), (text = ''))}>
          cancel
        </button>
        <button type="submit" disabled={!text.trim()}>send</button>
      </footer>
    </form>
  {/if}

  {#if highlight}
    <p class="families">{families} famil{families === 1 ? 'y' : 'ies'} here</p>
  {/if}
  <Handle type="source" position={Position.Right} />
</section>

<svelte:window onkeydown={composing ? onKeydown : undefined} />

<style>
  /* A fixed box with a scrolling list inside. The alternative — a board that
     grows with its work and shrinks when the work settles — moves every other
     board on the canvas each time a delivery lands or leaves, so the layout an
     operator arranged is rewritten by the cluster's own activity. */
  section.node {
    width: 260px;
    height: 240px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    position: relative;
    background: linear-gradient(180deg, #171b22, #12151b);
    border: 1px solid var(--line, #232935);
    border-radius: 12px;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.03),
      0 10px 28px rgba(0, 0, 0, 0.35);
    padding: 0;
    font-size: 12px;
    transition: border-color 140ms ease, box-shadow 140ms ease;
  }
  section.node:hover {
    border-color: #2e3644;
  }
  section.node.selected {
    border-color: var(--accent, #5b9dff);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.03),
      0 10px 28px rgba(0, 0, 0, 0.35),
      0 0 0 3px rgba(91, 157, 255, 0.14);
  }

  /* The ports a dragged route starts and lands on. Quiet while the board is
     idle; lit while it is the one under the cursor, so the affordance says
     "from here" only when "here" is the board being looked at. */
  section.node :global(.svelte-flow__handle) {
    width: 8px;
    height: 8px;
    background: #0b0d12;
    border: 1.5px solid #3a414e;
    transition: border-color 140ms ease, box-shadow 140ms ease, transform 140ms ease;
  }
  section.node:hover :global(.svelte-flow__handle) {
    border-color: var(--accent, #5b9dff);
    box-shadow: 0 0 0 3px rgba(91, 157, 255, 0.16);
    transform: scale(1.2);
  }

  header {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 9px 10px 8px 12px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.045);
    flex: none;
  }
  header strong {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.01em;
  }
  .sessions {
    opacity: 0.55;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .add {
    flex: none;
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    padding: 0;
    background: transparent;
    color: var(--muted, #7d8593);
    border: 1px solid transparent;
    border-radius: 6px;
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
    transition: color 120ms ease, border-color 120ms ease, background 120ms ease;
  }
  .add:hover,
  .add.open {
    color: var(--accent, #5b9dff);
    border-color: rgba(91, 157, 255, 0.4);
    background: rgba(91, 157, 255, 0.1);
  }

  .entries {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 2px 6px 8px;
  }
  .entries::-webkit-scrollbar {
    width: 6px;
  }
  .entries::-webkit-scrollbar-thumb {
    background: #2b323e;
    border-radius: 3px;
  }
  .entries::-webkit-scrollbar-thumb:hover {
    background: #39414f;
  }

  p.group {
    margin: 8px 4px 3px;
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--faint, #565e6c);
    user-select: none;
  }
  p.group::before {
    content: '';
    width: 5px;
    height: 5px;
    border-radius: 50%;
  }
  p.group.pending::before {
    background: var(--warn, #e0a63f);
    box-shadow: 0 0 5px rgba(224, 166, 63, 0.6);
  }
  p.group.queue::before {
    background: var(--accent, #5b9dff);
    box-shadow: 0 0 5px rgba(91, 157, 255, 0.5);
  }

  p.empty {
    margin: 0;
    padding: 26px 0 0;
    text-align: center;
    opacity: 0.45;
    font-size: 11.5px;
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
    align-items: baseline;
    gap: 8px;
    padding: 4px 7px;
    border-left: 2px solid transparent;
    border-radius: 4px 6px 6px 4px;
    transition: background 120ms ease;
  }
  li:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  li.family {
    cursor: pointer;
  }
  li.running {
    border-left-color: var(--accent, #5b9dff);
  }
  li.waiting {
    border-left-color: var(--warn, #e0a63f);
  }
  li.queued {
    border-left-color: #333b49;
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
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 11.5px;
  }
  .meta {
    opacity: 0.5;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    flex: none;
  }

  /* The composer floats: the board's box stays fixed, so the form overlays the
     entries and leaves no layout behind when it closes. */
  form.compose {
    position: absolute;
    top: 38px;
    left: 8px;
    right: 8px;
    z-index: 6;
    display: flex;
    flex-direction: column;
    gap: 7px;
    padding: 8px;
    background: #171b22;
    border: 1px solid rgba(91, 157, 255, 0.45);
    border-radius: 10px;
    box-shadow: 0 14px 36px rgba(0, 0, 0, 0.55);
  }
  form.compose textarea {
    resize: none;
    width: 100%;
    padding: 7px 9px;
    background: #0b0d12;
    color: var(--text, #dbe0ea);
    border: 1px solid var(--line, #232935);
    border-radius: 7px;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 12px;
    line-height: 1.45;
  }
  form.compose textarea:focus {
    outline: none;
    border-color: var(--accent, #5b9dff);
    box-shadow: 0 0 0 2px rgba(91, 157, 255, 0.18);
  }
  form.compose textarea::placeholder {
    color: var(--faint, #565e6c);
  }
  form.compose footer {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .hint {
    flex: 1;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10px;
    color: var(--faint, #565e6c);
  }
  form.compose button {
    padding: 4px 10px;
    border-radius: 6px;
    font-size: 11.5px;
    cursor: pointer;
    border: 1px solid var(--line, #232935);
    background: transparent;
    color: var(--text, #dbe0ea);
    transition: background 120ms ease, border-color 120ms ease;
  }
  form.compose button.ghost {
    color: var(--muted, #7d8593);
  }
  form.compose button.ghost:hover {
    border-color: #39414f;
    background: rgba(255, 255, 255, 0.04);
  }
  form.compose button[type='submit'] {
    background: rgba(91, 157, 255, 0.16);
    border-color: rgba(91, 157, 255, 0.5);
    color: #a9c8ff;
    font-weight: 600;
  }
  form.compose button[type='submit']:hover:not(:disabled) {
    background: rgba(91, 157, 255, 0.26);
  }
  form.compose button[type='submit']:disabled {
    opacity: 0.4;
    cursor: default;
  }

  p.families {
    flex: none;
    margin: 6px 12px 8px;
    color: var(--accent, #5b9dff);
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #565e6c;
    flex: none;
  }
  .dot.online {
    background: var(--ok, #43c076);
    box-shadow: 0 0 6px rgba(67, 192, 118, 0.55);
  }
  .dot.draining {
    background: var(--warn, #e0a63f);
    box-shadow: 0 0 6px rgba(224, 166, 63, 0.5);
  }
</style>
