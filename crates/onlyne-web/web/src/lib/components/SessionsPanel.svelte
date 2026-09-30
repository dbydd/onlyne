<script lang="ts">
  // One session per row, the TUI's session read on the web's left flank: who
  // owns it, where its lifecycle sits, and what it is serving. A session the
  // heartbeat has lost carries the amber bar, because that is the row an
  // operator scans this panel for.
  import Panel from './Panel.svelte';
  import { app } from '../store.svelte';

  /// The panel opens on the sessions that exist: an exited row is history,
  /// and one click shows it again. A heartbeat-stale row stays — that is a
  /// live fact an operator scans this panel for.
  let liveOnly = $state(true);
  const sessions = $derived(
    Object.values(app.view.sessions ?? [])
      .reverse()
      .filter((s) => !liveOnly || s.lifecycle !== 'exited'),
  );
</script>

<Panel title="sessions" count={sessions.length}>
    {#snippet actions()}
      <button class="pill-toggle" class:off={!liveOnly} onclick={() => (liveOnly = !liveOnly)}>
        {liveOnly ? 'live' : 'all'}
      </button>
    {/snippet}
  {#if sessions.length === 0}
    <p class="none">no sessions</p>
  {:else}
    <ul>
      {#each sessions as s (s.session_id)}
        <li class:stale={s.heartbeat_stale} class:exited={s.lifecycle === 'exited'}>
          <div class="row1">
            <span class="role">{s.role ?? '—'}</span>
            <span class="sid">{s.session_id.slice(0, 8)}</span>
          </div>
          <div class="row2">
            {s.lifecycle} · {s.agent} · {s.delivery}{#if s.task_id}
              · task {s.task_id.slice(0, 8)}{/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</Panel>

<style>
  .none {
    margin: 0;
    padding: 14px 12px;
    color: var(--faint, #565e6c);
    font-size: 11.5px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 4px;
    display: grid;
    gap: 2px;
  }
  li {
    padding: 5px 7px;
    border-left: 2px solid transparent;
    border-radius: 4px 6px 6px 4px;
  }
  li:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  li.stale {
    border-left-color: var(--warn, #e0a63f);
  }
  li.exited {
    opacity: 0.5;
  }
  .row1 {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
  }
  .role {
    font-size: 12px;
    font-weight: 600;
  }
  .sid {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    color: var(--faint, #565e6c);
  }
  .row2 {
    margin-top: 1px;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--muted, #7d8593);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
