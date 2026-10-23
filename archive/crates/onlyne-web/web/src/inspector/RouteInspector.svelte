<script lang="ts">
  // One declared route: what the graph draws between two boards, what is
  // travelling it right now, and the one gesture that withdraws it. Withdrawing
  // is a typed edit to `spec.toml`, so it goes through the same path a dragged
  // line uses and comes back with an undo.
  import ArrowRight from 'phosphor-svelte/lib/ArrowRight';
  import Path from 'phosphor-svelte/lib/Path';
  import { clip } from '../lib/format';
  import { presenceTone, principalRole, workOf, workTitle } from '../lib/model';
  import { removeRoute } from '../lib/ops';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import ConfirmButton from '../lib/ui/ConfirmButton.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import Section from '../lib/ui/Section.svelte';
  import Frame from '../shell/Frame.svelte';

  interface Props {
    source: string;
    target: string;
  }

  let { source, target }: Props = $props();

  /// `Frame` takes the title as a string, so the header names the pair in words
  /// and the glyph that draws the direction rides beside it. The bundled latin
  /// subset carries no arrow, and a title that wrapped to two lines would cost
  /// the panel its header anyway.
  const heading = $derived(`${source} to ${target}`);
  const declared = $derived(cluster.routes.some((route) => route.source === source && route.target === target));
  const sourceTone = $derived(cluster.boardByRole.get(source)?.presence);
  const targetTone = $derived(cluster.boardByRole.get(target)?.presence);

  /// The deliveries crossing this route right now, newest first, as the canvas
  /// ordered them.
  const traffic = $derived(
    cluster.deliveries.filter(
      (delivery) => principalRole(delivery.from) === source && principalRole(delivery.to) === target,
    ),
  );
  /// A card sits on the board it is addressed to, so the edge's load is the
  /// target board's unsettled cards that came from this source.
  const owed = $derived(
    (cluster.boardByRole.get(target)?.cards ?? []).filter(
      (card) => card.from === source && card.column !== 'done' && card.column !== 'failed_or_blocked',
    ).length,
  );

  let busy = $state(false);

  async function withdraw() {
    busy = true;
    await removeRoute(source, target);
    busy = false;
  }
</script>

{#snippet pair()}
  <span class="pair">
    <span class="trunc">{source}</span>
    <ArrowRight size="11" />
    <span class="trunc">{target}</span>
  </span>
{/snippet}

{#snippet footer()}
  <div class="withdraw">
    <ConfirmButton
      label="Withdraw this route"
      confirm="withdraw it"
      size="sm"
      disabled={busy}
      busy={busy}
      title="Remove {target} from the targets {source} may address"
      onconfirm={withdraw}
    />
    <p class="hint">
      A typed edit to `spec.toml`, applied through the same path a dragged line uses, and it comes back with an undo.
    </p>
  </div>
{/snippet}

<Frame
  eyebrow="route"
  title={heading}
  actions={pair}
  footer={footer}
>
  <p class="lead">
    {source} may address {target}, and a session of {source} owes {target} a delivery before it may report a
    terminal outcome.
  </p>
  <div class="chips">
    <span class="chip" data-tone={sourceTone ? presenceTone(sourceTone) : 'off'}>
      {sourceTone ?? 'no presence'}
    </span>
    <span class="chip" data-tone={targetTone ? presenceTone(targetTone) : 'off'}>
      {targetTone ?? 'no presence'}
    </span>
    <span class="chip">{owed} owed</span>
  </div>
  {#if !declared}
    <p class="dim">This route is not on the canvas. The rows below are what last crossed it.</p>
  {/if}

  <Section title="Travelling this route" count={traffic.length}>
    {#if traffic.length === 0}
      <Empty
        icon={Path}
        title="Nothing is on this route"
        hint={`No delivery is addressed from ${source} to ${target} in this view. One appears the moment a session of ${source} sends it.`}
      />
    {:else}
      <ul class="rows">
        {#each traffic as delivery (delivery.msg_id)}
          {@const reading = workOf({
            state: delivery.state,
            outcome: delivery.outcome,
            column: cluster.columnById.get(delivery.msg_id) ?? null,
          })}
          <li>
            <button
              class="row"
              data-tone={reading.tone}
              onclick={() => ui.select({ kind: 'task', msgId: delivery.msg_id, role: target })}
              title={workTitle({ out_head: delivery.out_head, kind: delivery.kind, from: source })}
            >
              <span class="dot" data-tone={reading.tone}></span>
              <span class="trunc head">{clip(workTitle({ out_head: delivery.out_head, kind: delivery.kind, from: source }), 34)}</span>
              <span class="chip line">{reading.label}</span>
              <Ago at={cluster.timeOf(delivery)} />
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </Section>
</Frame>

<style>
  .pair {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 132px;
    height: 20px;
    padding: 0 6px;
    border-radius: var(--r-1);
    box-shadow: inset 0 0 0 1px var(--line);
    background: var(--raised);
    color: var(--ink-3);
    font-size: var(--fs-11);
  }
  .lead {
    padding: 10px 12px 8px;
    font-size: var(--fs-12);
    line-height: 1.55;
    color: var(--ink-2);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    padding: 0 12px;
  }
  .dim {
    margin: 0;
    padding: 8px 12px 0;
    font-size: var(--fs-11);
    line-height: 1.45;
    color: var(--ink-4);
  }
  .rows {
    display: grid;
    gap: 1px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    min-width: 0;
    height: 24px;
    padding: 0 4px;
    border: 0;
    border-radius: var(--r-2);
    background: none;
    color: var(--ink-2);
    font-size: var(--fs-12);
    text-align: left;
    transition: background var(--t-fast) var(--ease);
  }
  .row:hover {
    background: var(--hover);
  }
  .head {
    flex: 1;
    min-width: 0;
  }
  .row :global(.ago) {
    flex: none;
  }
  .withdraw {
    display: grid;
    gap: 5px;
  }
  .withdraw :global(button) {
    justify-self: start;
  }
  .hint {
    font-size: var(--fs-11);
    line-height: 1.45;
    color: var(--ink-3);
  }
</style>
