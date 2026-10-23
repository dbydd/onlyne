<script lang="ts">
  // The session axis: what each role has open, what word the reducer gives it,
  // and what it last said. It opens on the sessions still running, because a
  // panel watching a cluster work is not interested in the ones that left.
  //
  // A session row is clickable through its role, so it is a button; a row the
  // wire sent without a role names nothing this surface can select, and is
  // drawn as the plain row it is rather than as a button that would do nothing.
  import MagnifyingGlass from 'phosphor-svelte/lib/MagnifyingGlass';
  import Pulse from 'phosphor-svelte/lib/Pulse';
  import Warning from 'phosphor-svelte/lib/Warning';
  import type { SessionView } from '../gen/View';
  import { parseTime, short } from '../lib/format';
  import { sessionOf } from '../lib/model';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import Ago from '../lib/ui/Ago.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import { liveSessions, matchesSessionQuery } from './rows';

  /// On by default, and the tab row's count is read from the same helper, so
  /// the number above the tab is the number of rows below it.
  let liveOnly = $state(true);

  const visible = $derived(liveOnly ? liveSessions(cluster.sessions) : cluster.sessions);
  const rows = $derived(visible.filter((session) => matchesSessionQuery(session, ui.query)));

  function reading(session: SessionView) {
    const base = sessionOf(cluster.sessionWord(session.session_id));
    /// A silent working row is a question rather than a state, so the warning
    /// outranks whatever word the reducer still holds for it.
    if (session.heartbeat_stale) return { tone: 'wait' as const, label: 'heartbeat stale', hollow: false };
    return base;
  }
</script>

<div class="controls">
  <div class="seg" role="group" aria-label="session visibility">
    <button aria-pressed={liveOnly} onclick={() => (liveOnly = true)}>live only</button>
    <button aria-pressed={!liveOnly} onclick={() => (liveOnly = false)}>including exited</button>
  </div>

  <label class="search">
    <MagnifyingGlass size="12" />
    <span class="sr-only">filter sessions</span>
    <input class="input" type="search" placeholder="session id, role, task id" bind:value={ui.query} />
  </label>

  <div class="spacer"></div>
  <span class="held num">{rows.length}</span>
</div>

<div class="rows">
  {#if rows.length > 0}
    <ul>
      {#each rows as session (session.session_id)}
        {@const read = reading(session)}
        {#snippet cells()}
          <span class="id mono">{short(session.session_id)}</span>
          <span class="role trunc">{session.role ?? 'unnamed session'}</span>
          <span class="chip" data-tone={read.tone}>
            {#if session.heartbeat_stale}
              <span class="warn">
                <Warning size="10" weight="bold" aria-hidden="true" />
                <span class="sr-only">heartbeat stale:</span>
              </span>
            {/if}
            {#if read.hollow}
              <span class="dot ring"></span>
            {/if}
            {read.label}
          </span>
          {#if session.task_id}
            <span class="task mono trunc" title="task {session.task_id}">{short(session.task_id)}</span>
          {:else}
            <span class="task muted">no task bound</span>
          {/if}
          <!-- The watermark a client owns: generation, then the sequence
               inside it. One cell, because a column of its own would be wider
               than the pair of figures it holds. -->
          <span class="gen mono num" title="generation {session.generation}, seq {session.seq}">
            g{session.generation}.{session.seq}
          </span>
          <Ago at={parseTime(session.last_seen) ?? 0} />
        {/snippet}
        <li data-tone={read.tone}>
          {#if session.role}
            <button
              class="row"
              class:selected={ui.selection?.kind === 'role' && ui.selection.role === session.role}
              onclick={() => ui.select({ kind: 'role', role: session.role ?? '' })}
            >
              {@render cells()}
            </button>
          {:else}
            <div class="row static">{@render cells()}</div>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if ui.query.trim() !== ''}
    <Empty
      icon={MagnifyingGlass}
      title="No session matches {ui.query.trim()}"
      hint="The filter reads the session id, the role and the task it is serving."
    >
      <button class="btn sm" onclick={() => (ui.query = '')}>Clear the filter</button>
    </Empty>
  {:else if liveOnly}
    <Empty
      icon={Pulse}
      title="No session is open"
      hint="Live sessions appear here as clients open them, and leave again when they exit."
    >
      <button class="btn sm" onclick={() => (liveOnly = false)}>Show exited sessions</button>
    </Empty>
  {:else}
    <Empty
      icon={Pulse}
      title="No session has been open"
      hint="Nothing on this link has opened a session yet."
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
  .search {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 120px;
    max-width: 320px;
    gap: 6px;
    padding-left: 8px;
    border-radius: var(--r-2);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--line);
    color: var(--ink-4);
  }
  .search:hover {
    box-shadow: inset 0 0 0 1px var(--line-strong);
  }
  .search:focus-within {
    box-shadow: inset 0 0 0 1px var(--focus);
  }
  .search :global(.input) {
    padding-left: 0;
    background: none;
    box-shadow: none;
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
  .row {
    display: grid;
    /* Fixed tracks for everything that is always there and one flexible track
       for the role, so a row carrying a task id and one without it keep their
       columns in the same places and the eye can run down the panel. */
    grid-template-columns: 72px minmax(0, 1fr) 132px 132px 54px 42px;
    align-items: center;
    gap: var(--s-2);
    width: 100%;
    height: 26px;
    padding: 0 var(--s-3) 0 calc(var(--s-3) - 2px);
    border: 0;
    background: transparent;
    text-align: left;
    /* A working row wears its own status on the leading edge, so the column
       the eye scans first is the one that says what the session is doing. */
    box-shadow: inset 2px 0 0 0 var(--tone-line, transparent);
  }
  .row:hover {
    background: var(--raised);
  }
  .row.selected {
    background: var(--raised);
    box-shadow: inset 2px 0 0 0 var(--ink);
  }
  .id {
    color: var(--ink-2);
    font-size: var(--fs-11);
  }
  .role {
    color: var(--ink);
  }
  .task {
    color: var(--ink-3);
    font-size: var(--fs-11);
  }
  .warn {
    display: inline-flex;
    color: var(--tone, var(--wait));
  }
  .gen {
    color: var(--ink-4);
    font-size: 10.5px;
  }
</style>