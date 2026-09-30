<script lang="ts">
  // One ledger row per line, newest first, the delivery axis on the web's
  // right flank. The state word is the ledger's own — queued, in_flight,
  // acked, rejected, expired — because the panel is a reading of the ledger,
  // not a second opinion about it.
  import Panel from './Panel.svelte';
  import { app } from '../store.svelte';
  import type { DeliveryView, Principal } from '../gen/View';

  /// The panel opens on what is still owed: queued and in_flight. Settled
  /// history is one click away, because burying the work under it was the
  /// reading that made the panel useless.
  let liveOnly = $state(true);
  const rows = $derived(
    Object.values(app.view.deliveries ?? [])
      .reverse()
      .filter((d) => !liveOnly || d.state === 'queued' || d.state === 'in_flight'),
  );

  function name(p: Principal): string {
    if ('role' in p) {
      return p.role.session ? `${p.role.role}/${p.role.session.slice(0, 6)}` : p.role.role;
    }
    if ('gateway' in p) return p.gateway.gateway;
    return p.cluster.cluster;
  }

  function label(d: DeliveryView): string {
    const head = (d.out_head ?? '').trim();
    if (head) return head;
    if (d.task_id) return `task ${d.task_id.slice(0, 8)}`;
    return d.msg_id.slice(0, 8);
  }
</script>

<Panel title="ledger" count={rows.length}>
    {#snippet actions()}
      <button class="pill-toggle" class:off={!liveOnly} onclick={() => (liveOnly = !liveOnly)}>
        {liveOnly ? 'live' : 'all'}
      </button>
    {/snippet}
  {#if rows.length === 0}
    <p class="none">no deliveries</p>
  {:else}
    <ul>
      {#each rows as d (d.msg_id)}
        <li class={d.state}>
          <div class="row1">
            <span class="route">{name(d.from)} → {name(d.to)}</span>
            <span class="state">{d.state}</span>
          </div>
          <div class="row2">
            <span class="label">{label(d)}</span>
            <span class="tail">
              {#if d.hop != null}h{d.hop}{/if}
              {#if d.outcome}{d.outcome}{/if}
            </span>
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
    border-left: 2px solid #333b49;
    border-radius: 4px 6px 6px 4px;
  }
  li:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  li.in_flight {
    border-left-color: var(--accent, #5b9dff);
  }
  li.acked {
    border-left-color: var(--ok, #43c076);
  }
  li.rejected,
  li.expired {
    border-left-color: var(--bad, #e5604f);
  }
  .row1 {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
  }
  .route {
    font-size: 11.5px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .state {
    flex: none;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10px;
    color: var(--muted, #7d8593);
  }
  .row2 {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
    margin-top: 1px;
  }
  .label {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--muted, #7d8593);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tail {
    flex: none;
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10px;
    color: var(--faint, #565e6c);
  }
</style>
