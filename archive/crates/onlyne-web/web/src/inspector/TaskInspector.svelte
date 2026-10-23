<script lang="ts">
  // One delivery, and the family it belongs to. The trace is why this panel
  // exists: a task moves as a chain across boards, and a row on its own says
  // nothing about where the work came from or what happened to it. The repair
  // verbs sit under it, because authority over a task follows from reading it,
  // not from reaching for a button.
  import ArrowRight from 'phosphor-svelte/lib/ArrowRight';
  import ClockCounterClockwise from 'phosphor-svelte/lib/ClockCounterClockwise';
  import Path from 'phosphor-svelte/lib/Path';
  import { clip, parseTime, short } from '../lib/format';
  import { principalName, principalRole, sessionOf, workOf, workTitle, type Tone } from '../lib/model';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import Section from '../lib/ui/Section.svelte';
  import Frame from '../shell/Frame.svelte';
  import RepairActions from './RepairActions.svelte';

  interface Props {
    msgId: string;
    /// The role this row sits on: the one a free session would be taken from.
    role: string;
  }

  let { msgId, role }: Props = $props();

  const delivery = $derived(cluster.deliveryById.get(msgId));
  const taskId = $derived(delivery?.task_id ?? null);
  const column = $derived(delivery ? (cluster.columnById.get(delivery.msg_id) ?? null) : null);
  const reading = $derived(delivery ? workOf({ state: delivery.state, outcome: delivery.outcome, column }) : null);

  // `ui.trace` is the family behind the current selection. This panel stays
  // mounted for a moment after a selection is cleared, so read the same trace by
  // id rather than watching the chain vanish under it.
  const trace = $derived(ui.trace?.selected === msgId ? ui.trace : cluster.traceFor(msgId));
  const hops = $derived(trace?.hops ?? []);
  const hopIndex = $derived(hops.findIndex((hop) => hop.msgId === msgId));

  // A row this tab first saw on the event stream carries no family and no hop;
  // the next ledger read fills them in. Until then the family is one row, and
  // saying so beats drawing a chain that is not there yet.
  const streamBorn = $derived(hops.length < 2 && !delivery?.family);

  /// One reading per hop, indexed by `msg_id`: the same weighing `workOf`
  /// gives a board card, so a step and the card it became agree on the word.
  const hopTones = $derived(
    new Map<string, { tone: Tone; label: string }>(
      hops.map((hop) => [
        hop.msgId,
        workOf({ state: hop.state, outcome: hop.outcome, column: cluster.columnById.get(hop.msgId) ?? null }),
      ]),
    ),
  );

  const title = $derived(
    delivery
      ? clip(workTitle({ out_head: delivery.out_head, kind: delivery.kind, from: principalRole(delivery.from) ?? undefined }), 60)
      : `task ${short(msgId)}`,
  );
  const subtitle = $derived.by(() => {
    if (hopIndex < 0 || hops.length < 2) return '';
    const position = `step ${hopIndex + 1} of ${hops.length}`;
    return delivery?.hop == null ? position : `${position}, ledger hop ${delivery.hop}`;
  });
  const fromName = $derived(delivery ? principalName(delivery.from) : '');
  const toName = $derived(delivery ? principalName(delivery.to) : '');

  const session = $derived(taskId ? cluster.sessionByTask.get(taskId) : undefined);
  let pending = $state(false);
</script>

{#snippet path()}
  {#if delivery}
    <span class="path">
      <span class="trunc">{fromName}</span>
      <ArrowRight size="11" />
      <span class="trunc">{toName}</span>
    </span>
  {/if}
{/snippet}

{#snippet spine()}
  <ol class="spine">
    {#each hops as hop (hop.msgId)}
      {@const hopTone = hopTones.get(hop.msgId)}
      <li class="step" class:on={hop.msgId === msgId} data-tone={hopTone?.tone ?? 'plain'}>
        <div class="line">
          <span class="idx num mono">{hop.index + 1}</span>
          <span class="names trunc">
            {hop.fromName}
            <ArrowRight size="10" />
            {hop.toName}
          </span>
          <span class="chip">{hop.kind}</span>
          {#if hopTone}
            <span class="chip line">{hopTone.label}</span>
          {/if}
          <Ago at={hop.at} />
        </div>
        <p class="title trunc">{clip(hop.title, 56)}</p>
        {#if !hop.drawn}
          <p class="dim">off canvas: one end is a gateway, the cluster, or the operator</p>
        {/if}
      </li>
    {/each}
  </ol>
{/snippet}

<Frame eyebrow="task" {title} {subtitle} busy={pending} actions={path}>
  {#if !delivery}
    <Empty
      icon={Path}
      title="This row is no longer in this view"
      hint="A settled row can leave the ledger when the link takes its next read. What became of it is still on the boards."
    >
      <button class="btn sm" onclick={() => ui.clear()}>Clear the selection</button>
    </Empty>
  {:else}
    <div class="chips">
      <span class="chip">{delivery.kind}</span>
      {#if reading}
        <span class="chip" data-tone={reading.tone}>{reading.label}</span>
      {/if}
      {#if delivery.outcome && delivery.outcome !== reading?.label}
        <span class="chip" data-tone={reading?.tone}>{delivery.outcome}</span>
      {/if}
      {#if taskId}
        <span class="chip mono" title={taskId}>task {short(taskId)}</span>
      {/if}
      {#if delivery.family}
        <span class="chip mono" title={delivery.family}>family {short(delivery.family)}</span>
      {/if}
    </div>

    <Section title="Trace" count={hops.length}>
      {#if hops.length === 0}
        <Empty
          icon={Path}
          title="No trace for this row"
          hint="Nothing has been read for this family yet. The next snapshot from the link carries its hops."
        />
      {:else}
        {#if streamBorn}
          <p class="dim">This row arrived on the stream. Its family is filled in on the next ledger read.</p>
        {/if}
        {@render spine()}
      {/if}
    </Section>

    <Section title="Serving session" count={session ? 1 : 0}>
      {#if !taskId}
        <Empty
          icon={ClockCounterClockwise}
          title="This row names no task"
          hint="Nothing a session could serve is named here, so there is nothing to point a session at."
        />
      {:else if !session}
        <Empty
          icon={ClockCounterClockwise}
          title="No session is serving this task"
          hint="A queued delivery has not been pulled yet, and a settled one may have left the sessions view."
        />
      {:else}
        {@const word = cluster.sessionWord(session.session_id)}
        {@const wordReading = sessionOf(word)}
        <div class="ident">
          <span class="mono trunc" title={session.session_id}>{short(session.session_id, 12)}</span>
          <span class="chip" data-tone={wordReading.tone}>{wordReading.label}</span>
          {#if session.lifecycle !== word}
            <span class="chip">{session.lifecycle}</span>
          {/if}
          <Ago at={parseTime(session.last_seen) ?? 0} />
        </div>
        <dl class="phases">
          <div><dt>agent</dt><dd class="mono">{session.agent}</dd></div>
          <div><dt>delivery</dt><dd class="mono">{session.delivery}</dd></div>
          <div><dt>resource</dt><dd class="mono">{session.resource}</dd></div>
          <div><dt>recovery</dt><dd class="mono">{session.recovery}</dd></div>
        </dl>
      {/if}
    </Section>

    <RepairActions
      {role}
      {taskId}
      tone={reading?.tone ?? 'plain'}
      label={reading?.label ?? ''}
      ledger={delivery.state}
      outcome={delivery.outcome ?? null}
      onwork={(working) => (pending = working)}
    />
  {/if}
</Frame>

<style>
  .path {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 148px;
    height: 20px;
    padding: 0 6px;
    border-radius: var(--r-1);
    box-shadow: inset 0 0 0 1px var(--line);
    background: var(--raised);
    color: var(--ink-3);
    font-size: var(--fs-11);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line-soft);
  }

  /* One spine down the family: the steps are rows on a shared line rather than
     a stack of cards, so the order the work arrived in is the shape. */
  .spine {
    display: grid;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .step {
    position: relative;
    margin-left: -10px;
    padding: 7px 0 8px 10px;
    border-left: 1px solid var(--line);
  }
  .step.on {
    /* Bone, not a hue: this is the row the operator opened, not a state. */
    border-left: 2px solid var(--bone);
  }
  .step::before {
    content: '';
    position: absolute;
    left: -3.5px;
    top: 12px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--tone, var(--ink-4));
    box-shadow: 0 0 0 3px var(--panel);
  }
  .line {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }
  .idx {
    flex: none;
    width: 12px;
    font-size: 10.5px;
    color: var(--ink-4);
  }
  .names {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .line :global(.chip) {
    flex: none;
  }
  .line :global(.ago) {
    margin-left: auto;
  }
  .title {
    margin-top: 3px;
    font-size: var(--fs-12);
    color: var(--ink-2);
  }
  .dim {
    margin-top: 4px;
    font-size: var(--fs-11);
    color: var(--ink-4);
  }

  .ident {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: var(--s-3);
  }
  .ident :global(.ago) {
    margin-left: auto;
  }
  .phases {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px var(--s-3);
    margin: 0;
  }
  .phases > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--s-2);
    padding-bottom: 3px;
    border-bottom: 1px solid var(--line-soft);
  }
  .phases dt {
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .phases dd {
    margin: 0;
    font-size: var(--fs-11);
    color: var(--ink-2);
  }
</style>
