<script lang="ts">
  // The role's sessions, in the order the cluster already sorts them: working
  // first, then opening, then idle, then closed.
  //
  // A stale heartbeat is the one row in this list an operator scans for, so it
  // carries a mark and a word rather than only a colour. An exited row is
  // history that happened to stay on the board, and is dimmed so it reads as
  // such without being hidden.
  import Ago from '../lib/ui/Ago.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import Plugs from 'phosphor-svelte/lib/Plugs';
  import Warning from 'phosphor-svelte/lib/Warning';
  import { parseTime, short } from '../lib/format';
  import { cluster } from '../lib/state/cluster.svelte';
  import { sessionOf } from '../lib/model';

  interface Props {
    role: string;
  }

  let { role }: Props = $props();

  const rows = $derived(cluster.sessionsByRole.get(role) ?? []);
</script>

{#if rows.length === 0}
  <Empty icon={Plugs} title="No sessions" hint="One opens when work arrives for this role." />
{:else}
  <ul class="sessions">
    {#each rows as session (session.session_id)}
      {@const reading = sessionOf(cluster.sessionWord(session.session_id))}
      {@const last = parseTime(session.last_seen)}
      <li class="row" class:dim={session.lifecycle === 'exited'} class:stale={session.heartbeat_stale}>
        <span class="dot" class:ring={reading.hollow} data-tone={session.heartbeat_stale ? 'wait' : reading.tone}></span>
        <span class="id mono trunc">{short(session.session_id, 8)}</span>
        <span class="chip line" data-tone={reading.tone}>{reading.label}</span>
        {#if session.heartbeat_stale}
          <span class="stale-mark"><Warning size="11" weight="fill" /> stale</span>
        {/if}
        {#if session.task_id}
          <span class="task mono trunc" title={session.task_id}>{short(session.task_id, 8)}</span>
        {/if}
        {#if last != null}
          <Ago at={last} />
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .sessions {
    display: grid;
    gap: 1px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 6px;
    border-radius: var(--r-2);
  }
  .row.dim {
    opacity: 0.5;
  }
  .id {
    flex: none;
    font-size: 10.5px;
    color: var(--ink-2);
  }
  .stale-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    font-size: var(--fs-11);
    color: var(--wait);
  }
  .task {
    flex: 1;
    min-width: 0;
    font-size: 10.5px;
    color: var(--ink-3);
  }
  .row :global(.ago) {
    flex: none;
  }
</style>