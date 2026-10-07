<script lang="ts">
  // Who this role is allowed to address, edited as the spec declares it.
  //
  // The two lists are the two halves of one edge, and they are edited from two
  // different ends: this panel owns `allowed_targets`, the mirror role's panel
  // owns the `allowed_senders` that admits this role. So senders are read here
  // rather than written, because a full editor for the inbound half would be a
  // second place to get the same permission wrong.
  //
  // The truth is the spec, because the graph draws the board's `edges` and the
  // board's edges are one reload behind a file that has just been edited. The
  // board is read while the spec has not arrived, so the list does not read as
  // empty on a slow link.
  import ArrowRight from 'phosphor-svelte/lib/ArrowRight';
  import Prohibit from 'phosphor-svelte/lib/Prohibit';
  import X from 'phosphor-svelte/lib/X';
  import { addRoute, removeRoute } from '../lib/ops';
  import { cluster } from '../lib/state/cluster.svelte';
  import { spec } from '../lib/state/spec.svelte';

  interface Props {
    role: string;
  }

  let { role }: Props = $props();

  const client = $derived(spec.clientOf(role));
  const targets = $derived(client?.allowedTargets ?? cluster.boardByRole.get(role)?.edges ?? []);
  const senders = $derived(client?.allowedSenders ?? []);
  /// A role may not route to itself here, so the operator's own board is the
  /// one name that is never offered.
  const choices = $derived(cluster.canvasRoles.has(role) ? cluster.canvasRoles : new Set(cluster.boardByRole.keys()));
  const addable = $derived([...choices].filter((name) => name !== role && !targets.includes(name)).sort());
  /// Every edit here is a file write and a reload behind the op's answer, so a
  /// pending state per control is what keeps a slow reload from reading as a
  /// surface that stopped working.
  let pending = $state<string | null>(null);
  let adding = $state(false);
  let chosen = $state('');
  const pickable = $derived(chosen === '' || addable.includes(chosen) ? addable : [...addable, chosen]);

  async function drop(target: string) {
    if (pending !== null) return;
    pending = target;
    try {
      await removeRoute(role, target);
    } finally {
      pending = null;
    }
  }

  async function add(target: string) {
    if (adding || pending !== null) return;
    adding = true;
    try {
      await addRoute(role, target);
    } finally {
      adding = false;
    }
  }

  function word(name: string): string {
    return name === '*' ? 'every role' : name;
  }
</script>

<div class="routes">
  <p class="rule">
    <code>allowed_targets</code> is the permission and the obligation both: a session owes every role listed here a delivery
    before it may report a terminal outcome. An empty list means this role addresses nobody.
  </p>

  <ul class="chips">
    {#each targets as target (target)}
      <li>
        <span class="chip line" data-tone={target === '*' ? 'plain' : 'queue'}>
          <ArrowRight size="10" />
          {word(target)}
        </span>
        {#if target === '*'}
          <span class="why">the wildcard is the only way to say it, so it is not withdrawn here</span>
        {:else}
          <button
            class="btn icon ghost sm"
            title="withdraw {role} to {target}"
            aria-label="withdraw {role} to {target}"
            aria-busy={pending === target}
            disabled={pending !== null}
            onclick={() => void drop(target)}
          >
            <X size="10" weight="bold" />
          </button>
        {/if}
      </li>
    {/each}
  </ul>

  {#if addable.length > 0}
    <div class="add">
      <span class="addlabel">add target</span>
      <select class="select" bind:value={chosen} disabled={adding || pending !== null}>
        <option value="" disabled>choose a role</option>
        {#each pickable as target (target)}
          <option value={target}>{target}</option>
        {/each}
      </select>
      <button
        class="btn"
        disabled={adding || pending !== null || chosen === ''}
        aria-busy={adding}
        onclick={() => void add(chosen)}
      >
        {adding ? 'adding' : 'add'}
      </button>
    </div>
  {:else if targets.length === 0}
    <p class="none">
      <Prohibit size="11" />
      no other role is on the board to reach
    </p>
  {/if}

  <div class="senders">
    <span class="addlabel">allowed senders</span>
    {#if senders.length === 0}
      <span class="dim">no role may send to it</span>
    {:else}
      {#each senders as sender (sender)}
        <span class="chip line">{word(sender)}</span>
      {/each}
    {/if}
    <p class="rule">
      read-only here. It is the inbound half of the same edge and belongs to the sending role's panel.
    </p>
  </div>
</div>

<style>
  .routes {
    display: grid;
    gap: var(--s-2);
  }
  .rule {
    font-size: var(--fs-11);
    color: var(--ink-3);
    text-wrap: pretty;
  }
  .rule code {
    color: var(--ink-2);
  }
  .chips {
    display: grid;
    gap: 3px;
  }
  .chips li {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .why {
    font-size: var(--fs-11);
    color: var(--ink-4);
  }
  .add {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
  }
  .add :global(.select) {
    flex: 1;
    min-width: 0;
    width: auto;
  }
  .addlabel {
    font-size: var(--fs-11);
    font-weight: 500;
    color: var(--ink-2);
  }
  .none {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: var(--fs-11);
    color: var(--ink-4);
  }
  .senders {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: var(--s-1);
    padding-top: var(--s-2);
    border-top: 1px solid var(--line-soft);
  }
  .senders .rule {
    flex-basis: 100%;
  }
  .dim {
    font-size: var(--fs-11);
    color: var(--ink-4);
  }
</style>