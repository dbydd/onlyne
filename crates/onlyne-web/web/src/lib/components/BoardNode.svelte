<script lang="ts">
  // One board as a graph node: the same header and counts, a column census in
  // place of the full columns, and the two handles a dragged line starts and
  // lands on. A dragged line becomes an allowed route — never a local drawing.
  import { Handle, Position } from '@xyflow/svelte';
  import type { Board } from '../gen/Board';
  import { app } from '../store.svelte';

  interface Props {
    id: string;
    data: { board: Board };
    selected?: boolean;
  }

  let { id, data, selected = false }: Props = $props();

  const COLUMNS = ['queued', 'running', 'waiting', 'done', 'failed_or_blocked'];

  function count(column: string): number {
    return (data.board.cards ?? []).filter((card) => card.column === column).length;
  }

  function families(): string[] {
    const seen = new Set<string>();
    for (const card of data.board.cards ?? []) {
      if (card.family) seen.add(card.family);
    }
    return [...seen];
  }

  const highlight = $derived(families().includes(app.selectedFamily ?? ''));
</script>

<section class="node" class:selected class:operator={data.board.operator} data-node={id}>
  <Handle type="target" position={Position.Left} />
  <header>
    <span class="dot {data.board.presence}"></span>
    <strong>{data.board.operator ? `operator (${data.board.role})` : data.board.role}</strong>
  </header>
  <p class="counts">
    busy {data.board.counts?.busy ?? 0} · idle {data.board.counts?.idle ?? 0} ·
    suspended {data.board.counts?.suspended ?? 0}
    {#if data.board.queued}· {data.board.queued} queued{/if}
  </p>
  <ul>
    {#each COLUMNS as column (column)}
      <li class:lit={count(column) > 0}>
        <span>{column.replace('failed_or_blocked', 'failed')}</span>
        <b>{count(column)}</b>
      </li>
    {/each}
  </ul>
  {#if highlight}
    <p class="families">{families().length} famil{families().length === 1 ? 'y' : 'ies'} here</p>
  {/if}
  <Handle type="source" position={Position.Right} />
</section>

<style>
  section.node {
    min-width: 210px;
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
  p.counts {
    margin: 4px 0 6px;
    opacity: 0.8;
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
    opacity: 0.5;
  }
  li.lit {
    opacity: 1;
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
