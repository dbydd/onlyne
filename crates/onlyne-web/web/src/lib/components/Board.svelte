<script lang="ts">
  // One board: a role's header counts and its five columns, one card per
  // delivery. The card's column arrived with it — the Rust side read it off
  // the reducer — so this component only arranges and forwards clicks.
  import type { Board } from '../gen/Board';
  import { selectFamily, sendTask, app } from '../store.svelte';

  let { board }: { board: Board } = $props();

  let composing = $state(false);
  let text = $state('');

  const COLUMNS: Array<{ key: string; label: string }> = [
    { key: 'queued', label: 'queued' },
    { key: 'running', label: 'running' },
    { key: 'waiting', label: 'waiting' },
    { key: 'done', label: 'done' },
    { key: 'failed_or_blocked', label: 'failed / blocked' },
  ];

  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!text.trim()) return;
    sendTask(board.role, text.trim());
    text = '';
    composing = false;
  }

  function byColumn(column: string) {
    return (board.cards ?? []).filter((card) => card.column === column);
  }
</script>

<section class="board" data-board={board.role} class:operator={board.operator}>
  <header>
    <span class="dot {board.presence}" title={board.presence}></span>
    <h3>{board.operator ? `operator (${board.role})` : board.role}</h3>
    <span class="counts">
      busy {board.counts?.busy ?? 0} · idle {board.counts?.idle ?? 0} · suspended {board.counts?.suspended ?? 0}
    </span>
    {#if board.queued}
      <span class="queued">{board.queued} queued</span>
    {/if}
    <button class="send" onclick={() => (composing = !composing)}>send task</button>
  </header>

  {#if composing}
    <form class="compose" onsubmit={submit}>
      <textarea
        bind:value={text}
        rows="2"
        placeholder="write a task to {board.role} — sent as _supervisor"
      ></textarea>
      <button type="submit">send as _supervisor</button>
    </form>
  {/if}

  <div class="cols">
    {#each COLUMNS as column (column.key)}
      <div class="col" data-column={column.key}>
        <h4>{column.label}</h4>
        {#each byColumn(column.key) as card (card.msg_id)}
          <button
            class="card"
            class:selected={card.family && app.selectedFamily === card.family}
            data-family={card.family}
            data-hop={card.hop}
            onclick={() => card.family && selectFamily(card.family)}
          >
            <span class="route">{card.from ?? '?'} → {board.role}</span>
            <span class="head">{card.out_head ?? card.task_id ?? card.msg_id.slice(0, 8)}</span>
            <span class="state">{card.state}{card.outcome ? ` · ${card.outcome}` : ''}</span>
          </button>
        {/each}
      </div>
    {/each}
  </div>
</section>
