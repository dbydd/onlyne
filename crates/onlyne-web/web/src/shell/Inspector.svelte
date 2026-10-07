<script lang="ts">
  // The inspector's router: one surface, five subjects. When the operator
  // clears the selection the content stays mounted while the panel slides
  // shut, because a panel that empties first reads as a glitch.
  import { ui } from '../lib/state/ui.svelte';
  import FaultInspector from '../inspector/FaultInspector.svelte';
  import RoleInspector from '../inspector/RoleInspector.svelte';
  import RouteInspector from '../inspector/RouteInspector.svelte';
  import TaskInspector from '../inspector/TaskInspector.svelte';

  let shown = $derived(ui.selection);
  $effect(() => {
    if (ui.selection) shown = ui.selection;
  });
</script>

{#if shown}
  {@const active = shown}
  {#if active.kind === 'role'}
    <RoleInspector role={active.role} />
  {:else if active.kind === 'task'}
    <TaskInspector msgId={active.msgId} role={active.role} />
  {:else if active.kind === 'fault'}
    <FaultInspector id={active.id} />
  {:else if active.kind === 'route'}
    <RouteInspector source={active.source} target={active.target} />
  {:else}
    <RoleInspector declaring />
  {/if}
{/if}
