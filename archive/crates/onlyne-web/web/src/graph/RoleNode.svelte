<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte';
  import type { Node, NodeProps } from '@xyflow/svelte';
  import { cluster } from '../lib/state/cluster.svelte';
  import { COLUMN_ORDER, presenceTone, sessionOf, type Tone } from '../lib/model';
  import { ui } from '../lib/state/ui.svelte';
  import { NODE_H, NODE_W } from './geometry';

  type BoardNode = Node<{ role: string }, 'board'>;
  type PipKind = 'busy' | 'idle' | 'suspended' | 'free';

  let { data, selected, isConnectable }: NodeProps<BoardNode> = $props();
  let role = $derived(data.role);
  let board = $derived(cluster.boardByRole.get(role));
  let info = $derived(cluster.roleInfo(role));
  let cards = $derived(board?.cards ?? []);
  let counts = $derived(board?.counts ?? {});
  let totalSessions = $derived((counts.busy ?? 0) + (counts.idle ?? 0) + (counts.suspended ?? 0));
  let maxSessions = $derived(Math.max(1, info?.max_sessions ?? 1));
  let tone = $derived.by<Tone>(() => {
    for (const column of COLUMN_ORDER) {
      if (cards.some((card) => card.column === column)) {
        if (column === 'failed_or_blocked') return 'fail';
        if (column === 'done') return 'done';
        if (column === 'running') return 'run';
        if (column === 'waiting') return 'wait';
        return 'queue';
      }
    }
    return 'plain';
  });
  let pips = $derived.by<PipKind[]>(() => {
    const busy = Math.min(counts.busy ?? 0, maxSessions);
    const suspended = Math.min(counts.suspended ?? 0, Math.max(0, maxSessions - busy));
    const idle = Math.min(counts.idle ?? 0, Math.max(0, maxSessions - busy - suspended));
    const free = Math.max(0, maxSessions - busy - suspended - idle);
    return [
      ...Array<PipKind>(busy).fill('busy'),
      ...Array<PipKind>(idle).fill('idle'),
      ...Array<PipKind>(suspended).fill('suspended'),
      ...Array<PipKind>(free).fill('free'),
    ];
  });
  let visiblePips = $derived(pips.slice(0, 6));
  let hiddenPips = $derived(Math.max(0, pips.length - visiblePips.length));
  let columnCounts = $derived.by(() => {
    const result: Record<'running' | 'waiting' | 'queued' | 'failed', number> = {
      running: 0,
      waiting: 0,
      queued: 0,
      failed: 0,
    };
    for (const card of cards) {
      if (card.column === 'running') result.running += 1;
      if (card.column === 'waiting') result.waiting += 1;
      if (card.column === 'queued') result.queued += 1;
      if (card.column === 'failed_or_blocked') result.failed += 1;
    }
    return result;
  });
  let traceStep = $derived.by<number | null>(() => {
    const trace = ui.trace;
    if (!trace) return null;
    const hop = trace.hops.find((candidate) => candidate.fromRole === role || candidate.toRole === role);
    return hop ? hop.index + 1 : null;
  });
  let roleSelected = $derived(selected || (ui.selection?.kind === 'role' && ui.selection.role === role));
  let traced = $derived(traceStep !== null);
  let dimmed = $derived(ui.focus !== null && !ui.focus.has(role));
  let onlineWorking = $derived(board?.presence === 'online' && (counts.busy ?? 0) > 0);
  let drive = $derived(info?.runtime?.drive ?? 'plugin');
  let presence = $derived(board?.presence ?? 'offline');

  function selectRole(event: MouseEvent): void {
    if (event.defaultPrevented) return;
    ui.select({ kind: 'role', role }, false);
  }
</script>

<div
  class="role-node"
  class:role-selected={roleSelected}
  class:trace-selected={traced}
  class:dimmed
  data-tone={tone}
  style={`width: ${NODE_W}px; height: ${NODE_H}px`}
  role="button"
  tabindex="0"
  aria-label={`Role ${role}`}
  onclick={selectRole}
  onkeydown={(event) => {
    if (event.key === 'Enter' || event.key === ' ') selectRole(event as unknown as MouseEvent);
  }}
>
  <Handle type="target" position={Position.Left} isConnectable={isConnectable} class="role-handle" />
  <Handle type="source" position={Position.Right} isConnectable={isConnectable} class="role-handle" />

  <div class="role-head">
    <span class="dot" class:pulse={onlineWorking} data-tone={presenceTone(presence)}></span>
    <span class="role-name trunc">{role}</span>
    {#if board?.admin}<span class="chip admin-chip">admin</span>{/if}
    {#if traceStep !== null}<span class="trace-step mono">{traceStep}</span>{/if}
  </div>

  <div class="role-meta">
    <span class="chip mono drive-chip">{drive}</span>
    <span class="session-count mono" title={`${totalSessions} sessions`}>{counts.busy ?? 0}/{totalSessions}</span>
  </div>

  <div class="slot-row" aria-label={`${maxSessions} session slots`}>
    {#each visiblePips as pip}
      <span class="slot-pip" class:busy={pip === 'busy'} class:idle={pip === 'idle'} class:suspended={pip === 'suspended'} class:free={pip === 'free'}></span>
    {/each}
    {#if hiddenPips > 0}<span class="slot-more mono">+{hiddenPips}</span>{/if}
  </div>

  <div class="column-row">
    {#if columnCounts.running > 0}<span class="chip" data-tone="run">{columnCounts.running} run</span>{/if}
    {#if columnCounts.waiting > 0}<span class="chip" data-tone="wait">{columnCounts.waiting} wait</span>{/if}
    {#if columnCounts.queued > 0}<span class="chip" data-tone="queue">{columnCounts.queued} queue</span>{/if}
    {#if columnCounts.failed > 0}<span class="chip" data-tone="fail">{columnCounts.failed} fail</span>{/if}
  </div>

  {#if presence === 'offline'}<div class="offline-note">declared but offline</div>{/if}
</div>

<style>
  .role-node {
    position: relative;
    display: grid;
    grid-template-rows: 22px 22px 18px 22px;
    gap: 3px;
    padding: 12px 14px 10px;
    border: 1px solid var(--tone-line, var(--line));
    border-radius: var(--r-3);
    background: var(--panel);
    box-shadow: var(--shadow-node);
    color: var(--ink);
    cursor: pointer;
    opacity: 1;
    transition: opacity var(--t-fast) var(--ease), box-shadow var(--t-fast) var(--ease);
  }
  .role-node::before {
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: var(--tone-wash, transparent);
    content: '';
    pointer-events: none;
  }
  .role-node.role-selected,
  .role-node.trace-selected {
    box-shadow: inset 0 0 0 1px var(--focus), var(--shadow-node);
  }
  .role-node.dimmed {
    opacity: 0.45;
  }
  .role-head,
  .role-meta,
  .slot-row,
  .column-row {
    position: relative;
    display: flex;
    align-items: center;
    min-width: 0;
  }
  .role-head {
    gap: 7px;
  }
  .role-name {
    min-width: 0;
    flex: 1;
    font-size: var(--fs-13);
    font-weight: 600;
    line-height: 1;
  }
  .admin-chip {
    flex: none;
    height: 17px;
    color: var(--ink-2);
    box-shadow: inset 0 0 0 1px var(--line-strong);
  }
  .trace-step {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: none;
    width: 17px;
    height: 17px;
    border-radius: 50%;
    background: var(--focus);
    color: var(--bone-ink);
    font-size: 10px;
    font-weight: 600;
  }
  .role-meta {
    justify-content: space-between;
    gap: var(--s-2);
  }
  .drive-chip {
    color: var(--ink-2);
  }
  .session-count {
    color: var(--ink-3);
    font-size: var(--fs-11);
  }
  .slot-row {
    gap: 4px;
  }
  .slot-pip {
    display: block;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--line);
    box-shadow: inset 0 0 0 1px var(--line-strong);
  }
  .slot-pip.busy {
    background: var(--run);
    box-shadow: none;
  }
  .slot-pip.idle {
    background: var(--off);
    box-shadow: none;
  }
  .slot-pip.suspended {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--queue);
  }
  .slot-pip.free {
    background: transparent;
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .slot-more {
    color: var(--ink-3);
    font-size: 10px;
  }
  .column-row {
    gap: 4px;
    overflow: hidden;
  }
  .column-row .chip {
    max-width: 74px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .offline-note {
    position: absolute;
    right: 14px;
    bottom: 8px;
    color: var(--ink-4);
    font-size: 10px;
  }
  :global(.role-handle) {
    width: 10px;
    height: 22px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-1);
    background: var(--raised);
    transition: width var(--t-fast) var(--ease), height var(--t-fast) var(--ease), background var(--t-fast) var(--ease);
  }
  :global(.role-handle:hover),
  :global(.role-handle.svelte-flow__handle-connecting) {
    width: 14px;
    height: 28px;
    background: var(--focus);
  }
</style>
