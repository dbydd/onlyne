<script lang="ts">
  // The stream's own tail along the bottom edge: what just happened, newest
  // first, and — when the cluster has any — the open faults beside it in red.
  // The panel renders what each event says and nothing more; the ledger and
  // the sessions panels are where state lives.
  import { app } from '../store.svelte';

  type TailEvent = { type?: string } & Record<string, unknown>;

  const tail = $derived((app.view.event_tail ?? []) as TailEvent[]);
  const faults = $derived(Object.values(app.view.faults ?? {}));

  /// The words this build's events carry that name what moved. An event none
  /// of whose keys are here still shows its type.
  const NAMED: Array<string> = ['role', 'kind', 'state', 'msg_id', 'session_id', 'task_id'];

  function summary(event: TailEvent): string {
    const parts: Array<string> = [];
    for (const key of NAMED) {
      const value = event[key];
      if (typeof value === 'string' && value !== '') {
        parts.push(value.length > 16 ? value.slice(0, 16) : value);
      }
    }
    return parts.join(' · ');
  }
</script>

<aside class="panel">
  <header>
    <span class="title">events</span>
    <span class="count">{tail.length}</span>
  </header>
  <div class="cols">
    <ul class="feed">
      {#each tail as event, index (index)}
        <li>
          <span class="type">{event.type ?? 'event'}</span>
          <span class="sum">{summary(event)}</span>
        </li>
      {/each}
    </ul>
    {#if faults.length > 0}
      <ul class="faults">
        {#each faults as fault (fault.id ?? fault.seq ?? fault.kind)}
          <li>
            <span class="fkind">{fault.kind ?? 'fault'}</span>
            <span class="fwhere">{fault.role ?? fault.session_id?.slice(0, 8) ?? ''}</span>
            <span class="freason">{(fault.reason ?? '').slice(0, 90)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</aside>

<style>
  .panel {
    flex: none;
    height: 148px;
    display: flex;
    flex-direction: column;
    background: var(--panel, #10131a);
    border-top: 1px solid var(--line, #232935);
  }
  header {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px 4px;
  }
  .title {
    flex: 1;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--faint, #565e6c);
    user-select: none;
  }
  .count {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--muted, #7d8593);
  }
  .cols {
    flex: 1;
    min-height: 0;
    display: flex;
    gap: 12px;
    padding: 0 8px 6px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  ul.feed {
    flex: 1;
  }
  ul::-webkit-scrollbar {
    width: 6px;
  }
  ul::-webkit-scrollbar-thumb {
    background: #2b323e;
    border-radius: 3px;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 2px 6px;
    border-radius: 4px;
  }
  li:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  .type {
    flex: none;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    font-weight: 600;
    color: var(--accent, #5b9dff);
  }
  .sum {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--muted, #7d8593);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  ul.faults {
    flex: none;
    max-width: 46%;
    border-left: 1px solid var(--line, #232935);
    padding-left: 10px;
  }
  ul.faults li {
    border-left: 2px solid var(--bad, #e5604f);
  }
  .fkind {
    flex: none;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    font-weight: 600;
    color: var(--bad, #e5604f);
  }
  .fwhere {
    flex: none;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--muted, #7d8593);
  }
  .freason {
    font-size: 10.5px;
    color: var(--faint, #565e6c);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
