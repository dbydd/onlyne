<script lang="ts">
  // The faults the reducer filed, open ones first, with the acknowledge in the
  // row itself: a fault is worth clearing where it is read, not from a panel
  // somewhere else in the margin.
  //
  // The reason each row will be acknowledged with is kept by fault id rather
  // than as a field on the row, because a row is replaced by every frame and a
  // half-typed reason must outlive the frame that showed it.
  import FirstAidKit from 'phosphor-svelte/lib/FirstAidKit';
  import { clip, short } from '../lib/format';
  import { ackFault } from '../lib/ops';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import { faultReading, faultRows } from './rows';
  import type { FaultRow } from './rows';

  /// What an acknowledge says when nobody writes a better reason. Naming the
  /// surface is the point: the next reader needs to know a person, not a
  /// watch, cleared it.
  const ACK_REASON = 'acknowledged from onlyne-web';

  let openOnly = $state(true);
  let reasons = $state<Record<string, string>>({});
  let acking = $state<Record<string, boolean>>({});

  const open = $derived(new Set(cluster.openFaults.map((fault) => String(fault.id))));
  const rows = $derived(faultRows(cluster.faults, open, openOnly));

  function reasonFor(key: string): string {
    return reasons[key] ?? ACK_REASON;
  }

  function setReason(key: string, value: string) {
    reasons = { ...reasons, [key]: value };
  }

  async function acknowledge(fault: FaultRow) {
    const key = String(fault.id);
    if (acking[key]) return;
    acking = { ...acking, [key]: true };
    try {
      await ackFault(fault.id, reasonFor(key).trim() || ACK_REASON);
    } finally {
      acking = { ...acking, [key]: false };
    }
  }
</script>

<div class="controls">
  <div class="seg" role="group" aria-label="fault visibility">
    <button aria-pressed={openOnly} onclick={() => (openOnly = true)}>open only</button>
    <button aria-pressed={!openOnly} onclick={() => (openOnly = false)}>all</button>
  </div>
  <div class="spacer"></div>
  <span class="held num">{rows.length}</span>
</div>

<div class="rows">
  {#if rows.length > 0}
    <ul>
      {#each rows as fault (fault.id)}
        {@const read = faultReading(fault.state)}
        {@const key = String(fault.id)}
        {@const isOpen = fault.state === 'open'}
        {@const attempt = fault.attempt ?? 0}
        {@const named = fault.role ?? fault.session_id ?? ''}
        <li data-tone={read.tone} class:selected={ui.selection?.kind === 'fault' && ui.selection.id === fault.id}>
          <button class="row" onclick={() => ui.select({ kind: 'fault', id: fault.id })}>
            <span class="kind trunc">{fault.kind ?? 'fault'}</span>
            {#if named}
              <span class="who mono trunc" title={named}>{short(named)}</span>
            {:else}
              <span class="who muted">no subject</span>
            {/if}
            <span class="chip" data-tone={read.tone}>{read.label}</span>
            <span class="reason trunc" title={fault.reason ?? ''}>{clip(fault.reason ?? '', 96)}</span>
            {#if attempt > 0}
              <span class="attempt mono num">try {attempt}</span>
            {/if}
            <Ago at={(fault.created_at ?? 0) * 1000} />
          </button>
          {#if isOpen}
            <div class="act">
              <input
                class="input"
                value={reasonFor(key)}
                aria-label="reason for acknowledging fault {fault.id}"
                placeholder={ACK_REASON}
                oninput={(event) => setReason(key, event.currentTarget.value)}
              />
              <button
                class="btn sm"
                aria-busy={acking[key] === true}
                disabled={acking[key] === true}
                onclick={() => acknowledge(fault)}
              >
                Ack
              </button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if openOnly}
    <!-- No open fault is a clean bill of health, so it is phrased as one. -->
    <Empty
      icon={FirstAidKit}
      title="No open faults"
      hint="Every fault this cluster has filed has been handled. New ones arrive here as the reducer files them."
    >
      <button class="btn sm" onclick={() => (openOnly = false)}>Read handled faults</button>
    </Empty>
  {:else}
    <Empty
      icon={FirstAidKit}
      title="No fault has been filed"
      hint="Nothing this cluster has seen needed a person yet. Faults are recorded against a task, a role or a session."
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
    border-bottom: 1px solid var(--line-soft);
  }
  /* Selection is bone: a row an operator opened is not a status. */
  li.selected {
    background: var(--raised);
    box-shadow: inset 2px 0 0 0 var(--ink);
  }
  .row {
    display: grid;
    /* Fixed tracks, one flexible for the reason: a fault with no attempt and
       one on its fourth read alike, or the columns cannot be scanned down. */
    grid-template-columns: minmax(0, 1fr) 84px 108px minmax(0, 2fr) 52px 42px;
    align-items: center;
    gap: var(--s-2);
    width: 100%;
    height: 26px;
    padding: 0 var(--s-3) 0 calc(var(--s-3) - 2px);
    border: 0;
    background: transparent;
    text-align: left;
  }
  .row:hover {
    background: var(--raised);
  }
  .kind {
    color: var(--ink);
  }
  .who {
    color: var(--ink-3);
    font-size: var(--fs-11);
  }
  .reason {
    color: var(--ink-3);
  }
  .attempt {
    color: var(--ink-4);
    font-size: 10.5px;
  }
  /* The acknowledge lives under its own row rather than inside it, because a
     button inside a button is not a button. */
  .act {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: 0 var(--s-3) var(--s-2) calc(var(--s-3) + var(--s-1));
  }
  .act :global(.input) {
    max-width: 320px;
  }
</style>