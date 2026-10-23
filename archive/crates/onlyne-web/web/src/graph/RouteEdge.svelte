<script lang="ts">
  import { BaseEdge, EdgeLabel, getBezierPath } from '@xyflow/svelte';
  import type { Edge, EdgeProps } from '@xyflow/svelte';
  import { activity } from './activity.svelte';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';

  type RouteData = { source: string; target: string };
  type RouteGraphEdge = Edge<RouteData, 'route'>;

  let {
    id,
    source,
    target,
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    markerEnd,
    interactionWidth,
  }: EdgeProps<RouteGraphEdge> = $props();

  let [path, labelX, labelY] = $derived(
    getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition }),
  );
  let load = $derived(cluster.routeLoad.get(id) ?? 0);
  let pulses = $derived(activity.pulseOf(id));
  let flowing = $derived(load > 0 || pulses.length > 0);
  let selected = $derived.by(() => {
    const selection = ui.selection;
    if (selection?.kind === 'role') return selection.role === source || selection.role === target;
    return selection?.kind === 'route' && selection.source === source && selection.target === target;
  });
  let inTrace = $derived(ui.trace?.edges.some((edge) => edge.id === id) ?? false);
  // Two reasons to recede: the graph is crowded, or a trace is drawn and this
  // line is not part of it. One flag rather than two, because a line cannot be
  // more dimmed than dimmed.
  let dimmed = $derived(cluster.dimEdges || (ui.trace !== null && !inTrace));
  let edgeClass = $derived({
    'route-edge': true,
    selected,
    flowing,
    crowded: cluster.dimEdges,
    'trace-dim': dimmed,
  });
</script>

<BaseEdge
  {id}
  {path}
  {markerEnd}
  {interactionWidth}
  class={edgeClass}
  aria-label={`${source} to ${target}`}
/>
{#if load > 0}
  <EdgeLabel x={labelX} y={labelY} class="route-load" selectEdgeOnClick>
    <span class="route-count mono">{load}</span>
  </EdgeLabel>
{/if}
