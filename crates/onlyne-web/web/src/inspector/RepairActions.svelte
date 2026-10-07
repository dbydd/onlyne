<script lang="ts">
  // The operator's authority over one task, and the only place on the surface
  // where work is moved or taken away. Which verb is offered is a function of
  // the row's reading, decided here once so the panel's busyness, the button
  // states and the two armed verbs cannot disagree.
  //
  // Two verbs take something away and both ask twice: settling failed writes a
  // verdict on work a session never concluded, and closing settles the task and
  // closes the session's resource. Neither fires on one click.
  import Crosshair from 'phosphor-svelte/lib/Crosshair';
  import Eye from 'phosphor-svelte/lib/Eye';
  import FlagBanner from 'phosphor-svelte/lib/FlagBanner';
  import Lightning from 'phosphor-svelte/lib/Lightning';
  import { untrack } from 'svelte';
  import type { Outcome } from '../gen/View';
  import { focusSession, inspectTask, repair, reportTask, type RepairKind } from '../lib/ops';
  import ConfirmButton from '../lib/ui/ConfirmButton.svelte';
  import Section from '../lib/ui/Section.svelte';

  interface Props {
    role: string;
    taskId: string | null;
    /// `workOf` for this delivery, label and tone together: the ledger's word
    /// and the board's column weighed against each other, which is the only
    /// reading that can say whether work is still owed.
    tone: string;
    label: string;
    ledger: string;
    outcome: string | null;
    /// An op of this panel is in flight, so the frame header can say so.
    onwork?: (pending: boolean) => void;
  }

  const { role, taskId, tone, label, ledger, outcome, onwork = () => {} }: Props = $props();

  /// Why the head cannot be empty: it is what the ledger shows in place of the
  /// body, so a blank one would leave the row without a line of text.
  const NEUTRAL_HEAD = 'concluded from onlyne-web';
  const OUTCOMES: readonly Outcome[] = ['done', 'failed', 'cancelled', 'blocked'];

  let reason = $state('from onlyne-web');
  let verdict = $state<Outcome>('done');
  let head = $state('');
  let busy = $state<RepairKind | 'inspect' | 'report' | 'focus' | null>(null);
  /// The reducer's own observation, as the server wrote it.
  let observed = $state<{ task: string; json: string } | null>(null);
  /// A refusal keeps its own line here. The toast is the announcement; this is
  /// what the operator is looking at when they go looking for the answer.
  let refusal = $state('');

  /// Work is owed while the ledger says the envelope is in hand, while the
  /// board has it in an unsettled column, or while the verdict is blocked,
  /// which is a delivery waiting on something outside it.
  const owed = $derived(tone === 'run' || tone === 'wait' || tone === 'queue' || label === 'blocked');
  const settled = $derived(!owed);

  // The documented table, as a set of what this state offers.
  const verbs = $derived.by(() => {
    const offered = new Set<RepairKind | 'inspect' | 'report' | 'focus'>();
    if (!taskId) return offered;
    if (ledger === 'in_flight' || ledger === 'queued') offered.add('focus');
    if (ledger === 'in_flight' || (ledger === 'acked' && outcome !== 'done')) offered.add('retry');
    // A verdict can still be filed on work nobody concluded, which includes an
    // envelope that expired. `settled` is what keeps a settled done off it.
    if (owed || ledger !== 'expired') {
      offered.add('fail');
      offered.add('close');
    }
    offered.add('inspect');
    offered.add('report');
    return offered;
  });

  $effect(() => {
    const working = busy !== null;
    // The parent hands a fresh closure every render, so the callback is read
    // untracked or this effect would re-run on the parent's own update.
    untrack(() => onwork(working));
  });

  async function file(kind: RepairKind) {
    busy = kind;
    await repair(kind, taskId as string, reason.trim() || 'from onlyne-web');
    busy = null;
  }

  async function focus() {
    busy = 'focus';
    await focusSession(role, taskId as string);
    busy = null;
  }

  async function inspect() {
    busy = 'inspect';
    refusal = '';
    observed = null;
    const answer = await inspectTask(taskId as string);
    busy = null;
    if (answer !== null) observed = { task: taskId as string, json: JSON.stringify(answer, null, 2) };
    else refusal = 'the reducer would not answer. The refusal above says why.';
  }

  async function fileReport() {
    busy = 'report';
    await reportTask(taskId as string, verdict, head.trim() || NEUTRAL_HEAD);
    busy = null;
  }
</script>

<Section title="Repair" count={verbs.size}>
  {#if !taskId}
    <p class="none">This row carries no task id, so nothing on this panel has anything to name.</p>
  {:else}
    <div class="field">
      <label class="label" for="repair-reason">Reason, recorded with every verb</label>
      <input id="repair-reason" class="input" bind:value={reason} disabled={busy !== null} />
    </div>

    <div class="verbs">
      {#if verbs.has('focus')}
        <button class="btn sm" disabled={busy !== null} aria-busy={busy === 'focus'} onclick={focus} title="Point a session of {role} at this task">
          <Crosshair size="12" />
          Point a session at it
        </button>
      {/if}
      {#if verbs.has('retry')}
        <button class="btn sm" disabled={busy !== null} aria-busy={busy === 'retry'} onclick={() => file('retry')} title="Re-queue this task once">
          <Lightning size="12" />
          Re-queue once
        </button>
      {/if}
      {#if verbs.has('fail')}
        <ConfirmButton
          label="Settle failed"
          confirm="settle it failed"
          size="sm"
          disabled={busy !== null}
          busy={busy === 'fail'}
          title="Write a failed verdict on this task as the operator"
          onconfirm={() => file('fail')}
        />
      {/if}
      {#if verbs.has('close')}
        <ConfirmButton
          label="Close session and settle"
          confirm="close it and settle"
          size="sm"
          disabled={busy !== null}
          busy={busy === 'close'}
          title="Settle this task and close the serving session's resource"
          onconfirm={() => file('close')}
        />
      {/if}
      {#if verbs.has('inspect')}
        <button class="btn ghost sm" disabled={busy !== null} aria-busy={busy === 'inspect'} onclick={inspect} title="Read the serving session's reducer state without changing it">
          <Eye size="12" />
          Inspect
        </button>
      {/if}
    </div>

    <ul class="hints">
      {#if verbs.has('focus')}
        <li>Points a free session of {role} at this task, whichever one answers.</li>
      {/if}
      {#if verbs.has('retry')}
        <li>Re-queues the task once. What runs next is the client's own pull.</li>
      {/if}
      {#if verbs.has('fail')}
        <li>Settles this task failed. Nothing runs on it afterwards.</li>
      {/if}
      {#if verbs.has('close')}
        <li>Settles this task and closes the serving session's resource.</li>
      {/if}
    </ul>

    {#if busy === 'inspect'}
      <p class="pending" role="status">Reading the reducer, one round trip.</p>
    {:else if refusal}
      <p class="error" role="status">{refusal}</p>
    {/if}

    {#if observed}
      <!-- The reducer's own observation, kept as JSON: a shaped view would
           hide the fields this surface does not know about, and those are
           what the operator opened it for. -->
      <details class="answer">
        <summary>Reducer observation for task <span class="mono">{observed.task}</span></summary>
        <pre class="mono">{observed.json}</pre>
      </details>
    {/if}
  {/if}
</Section>

<Section title="File a report" open={false}>
  {#if !taskId}
    <p class="none">A conclusion is filed on a task id, and this row carries none.</p>
  {:else}
    <p class="none">
      Conclude work a session never concluded. The ledger shows the head where the body was.
    </p>
    <div class="report">
      <label class="sr-only" for="report-verdict">Verdict</label>
      <select id="report-verdict" class="select" bind:value={verdict} disabled={busy !== null}>
        {#each OUTCOMES as option (option)}
          <option value={option}>{option}</option>
        {/each}
      </select>
      <label class="sr-only" for="report-head">One-line head</label>
      <input
        id="report-head"
        class="input"
        bind:value={head}
        disabled={busy !== null}
        placeholder={NEUTRAL_HEAD}
        maxlength="160"
      />
      <button
        class="btn sm"
        disabled={busy !== null}
        aria-busy={busy === 'report'}
        onclick={fileReport}
        title="File this conclusion on the task as the operator"
      >
        <FlagBanner size="12" />
        File
      </button>
    </div>
    {#if busy === 'report'}
      <p class="pending" role="status">Filing {verdict} on this task.</p>
    {/if}
    {#if settled}
      <p class="none">This task already reads {outcome ?? label}. Filing again overwrites that verdict.</p>
    {/if}
  {/if}
</Section>

<style>
  .field {
    margin-bottom: var(--s-3);
  }
  .verbs {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-2);
  }
  .hints {
    display: grid;
    gap: 3px;
    margin: var(--s-2) 0 0;
    padding: 0;
    list-style: none;
  }
  .hints li,
  .none {
    font-size: var(--fs-11);
    line-height: 1.45;
    color: var(--ink-3);
  }
  .none {
    margin: var(--s-2) 0 0;
  }
  .pending {
    margin: var(--s-2) 0 0;
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .error {
    margin: var(--s-2) 0 0;
    font-size: var(--fs-11);
    color: var(--fail);
  }
  .answer {
    margin-top: var(--s-2);
    border-radius: var(--r-2);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .answer summary {
    padding: 6px 8px;
    cursor: pointer;
    font-size: var(--fs-11);
    font-weight: 500;
    color: var(--ink-2);
  }
  .answer pre {
    margin: 0;
    padding: 0 8px 8px;
    max-height: 260px;
    overflow: auto;
    font-size: 10.5px;
    line-height: 1.5;
    color: var(--ink-2);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .report {
    display: grid;
    grid-template-columns: 88px minmax(0, 1fr) auto;
    gap: var(--s-2);
    margin-top: var(--s-2);
  }
</style>
