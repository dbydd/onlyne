<script lang="ts">
  // One fault: the reason the cluster raised it, the two things it compared to
  // raise it, and the one verb that marks it handled. The reason is read here
  // in full rather than clipped, because this is the one place on the surface
  // a person reads what went wrong.
  import Check from 'phosphor-svelte/lib/Check';
  import Stack from 'phosphor-svelte/lib/Stack';
  import Warning from 'phosphor-svelte/lib/Warning';
  import { short } from '../lib/format';
  import { ackFault } from '../lib/ops';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import Section from '../lib/ui/Section.svelte';
  import Frame from '../shell/Frame.svelte';

  interface Props {
    id: number;
  }

  let { id }: Props = $props();

  const fault = $derived(cluster.faults.find((candidate) => candidate.id === id));
  /// A missing word reads as open, which is what the reducer means by it: the
  /// fault is in the table and no verb has moved it.
  const word = $derived(fault?.state?.trim() || 'open');
  const handled = $derived(word !== 'open');
  const created = $derived(fault?.created_at == null ? 0 : fault.created_at * 1000);

  const named = $derived(
    [
      fault?.role ?? '',
      fault?.session_id ? short(fault.session_id) : '',
      fault?.task_id ? short(fault.task_id) : '',
    ].filter(Boolean),
  );

  /// The delivery carrying this fault's task, while one is still in the view.
  /// The jump is hidden rather than dead once the row has left.
  const taskMsgId = $derived(
    fault?.task_id
      ? cluster.deliveries.find((delivery) => delivery.task_id === fault.task_id)?.msg_id
      : undefined,
  );
  const boardRole = $derived(fault?.role && cluster.boardByRole.has(fault.role) ? fault.role : undefined);
  const payloadCount = $derived(
    (fault?.observed != null ? 1 : 0) + (fault?.desired != null ? 1 : 0),
  );

  let reason = $state('acknowledged from onlyne-web');
  let busy = $state(false);

  function json(value: unknown): string {
    try {
      return JSON.stringify(value, null, 2) ?? String(value);
    } catch {
      return String(value);
    }
  }

  async function acknowledge() {
    busy = true;
    await ackFault(id, reason.trim() || 'acknowledged from onlyne-web');
    busy = false;
  }
</script>

{#snippet state()}
  <span class="chip" data-tone={handled ? 'off' : 'fail'}>{word}</span>
{/snippet}

{#snippet jumps()}
  {#if taskMsgId}
    <button
      class="btn ghost sm"
      onclick={() => ui.select({ kind: 'task', msgId: taskMsgId, role: fault?.role ?? '' })}
    >
      Open its task
    </button>
  {/if}
  {#if boardRole}
    <button class="btn ghost sm" onclick={() => ui.select({ kind: 'role', role: boardRole })}>
      Show its board
    </button>
  {/if}
{/snippet}

<Frame
  eyebrow="fault"
  title={fault?.kind ?? `fault ${id}`}
  subtitle={named.join(' · ')}
  busy={busy}
  actions={jumps}
>
  {#if !fault}
    <Empty
      icon={Warning}
      title="This fault is no longer in this view"
      hint="A resync replaces the fault table with what the cluster holds now. The faults tab says what is open."
    >
      <button class="btn sm" onclick={() => ui.clear()}>Clear the selection</button>
    </Empty>
  {:else}
    <div class="chips">
      {@render state()}
      {#if fault.intent}
        <span class="chip">{fault.intent}</span>
      {/if}
      <span class="chip mono">fault {id}</span>
    </div>

    <Section title="Reason">
      <p class="reason">{fault.reason ?? 'the reducer raised this fault without a reason'}</p>
      <dl class="facts">
        <div>
          <dt>attempt</dt>
          <dd class="num">{fault.attempt ?? 0}</dd>
        </div>
        <div>
          <dt>generation</dt>
          <dd class="num">{fault.generation ?? 0}</dd>
        </div>
        <div>
          <dt>raised</dt>
          <dd><Ago at={created} /></dd>
        </div>
      </dl>
    </Section>

    {#if payloadCount > 0}
      <Section title="Payloads" count={payloadCount} open={false}>
        {#if fault.observed != null}
          <details>
            <summary>observed</summary>
            <pre class="mono">{json(fault.observed)}</pre>
          </details>
        {/if}
        {#if fault.desired != null}
          <details>
            <summary>desired</summary>
            <pre class="mono">{json(fault.desired)}</pre>
          </details>
        {/if}
      </Section>
    {/if}

    <Section title="Handle it">
      {#if handled}
        <p class="dim">This fault reads {word}, so a verb has already moved it.</p>
      {:else}
        <div class="field">
          <label class="label" for="fault-reason">Reason, recorded with the acknowledgement</label>
          <input id="fault-reason" class="input" bind:value={reason} disabled={busy} />
        </div>
        <button class="btn sm" disabled={busy} aria-busy={busy} onclick={acknowledge} title="Mark fault {id} handled">
          <Check size="12" />
          Acknowledge
        </button>
        <p class="hint">Marks the fault handled. It moves no task.</p>
      {/if}
    </Section>
  {/if}
</Frame>

<style>
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .reason {
    font-size: var(--fs-12);
    line-height: 1.55;
    color: var(--ink);
    overflow-wrap: anywhere;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: var(--s-2);
    margin: var(--s-3) 0 0;
  }
  .facts > div {
    display: grid;
    gap: 1px;
    padding: 5px 7px;
    border-radius: var(--r-2);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--line-soft);
  }
  .facts dt {
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .facts dd {
    margin: 0;
    font-family: var(--font-mono);
    font-size: var(--fs-12);
    color: var(--ink);
  }
  .field {
    margin-bottom: var(--s-3);
  }
  .dim {
    font-size: var(--fs-11);
    line-height: 1.45;
    color: var(--ink-4);
  }
  .hint {
    margin-top: var(--s-2);
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  details {
    margin-bottom: var(--s-2);
    border-radius: var(--r-2);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  summary {
    padding: 6px 8px;
    cursor: pointer;
    font-size: var(--fs-11);
    font-weight: 500;
    color: var(--ink-2);
  }
  pre {
    margin: 0;
    padding: 0 8px 8px;
    max-height: 240px;
    overflow: auto;
    font-size: 10.5px;
    line-height: 1.5;
    color: var(--ink-2);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
